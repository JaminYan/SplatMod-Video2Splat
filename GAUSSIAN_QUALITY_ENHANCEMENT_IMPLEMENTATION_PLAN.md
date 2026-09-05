# OOOSplat Gaussian 模型质量提升实施文档

> 文档版本：1.1
> 编写日期：2026-08-24  
> 最近更新：2026-09-05
> 适用项目：`A:\project\splat`  
> 文档性质：后续开发、实验、验收和回退依据  
> 当前状态（2026-09-05）：M0 基线、M1 PPISP、M2 延迟增殖和 M3 的 MCMC/AbsGS 显式选择已实现；三类真实素材的同输入 A/B 已完成，Brush 继续是产品默认后端。WD-R 10k 已在小桌、游戏桌完成指标和固定视角复核，作为 gsplat 推荐实验预设。M3.5 仍处于只读诊断阶段，尚未启用自动裁剪、增殖门控或恢复训练。

## 1. 技术结论

OOOSplat 的 M0 基础补齐、PPISP、延迟增殖以及 MCMC/AbsGS 显式选择已经进入实现；本文前半部分仍保留这些设计，作为代码审计、回归和重建基线的依据。MCMC/AbsGS 三类真实素材 A/B 与 WD-R 10k 两类素材复核已完成，当前下一阶段应解决现有 opacity-only 导出过滤无法识别多视角不一致浮点的问题，不继续扩大 splat cap 或用 WD-R 掩盖输入质量问题。

推荐的主线顺序为：

1. 修正相机、初始化、SH、损失和验证基线；
2. 接入 PPISP 光度补偿；
3. 接入轻量动态区域降权和延迟增殖；
4. 对 MCMC 与 AbsGS 进行同输入 A/B（已完成三类素材）；
5. 增加多视角一致性增殖门控、导出前多条件浮点清理和自动质量回退；
6. 实验 WD-R 感知损失（10k 已完成两类素材复核）；
7. 实验 ResGS 式粗到细训练与 residual split；
8. 仅在检测到对应问题时启用 3DGUT、BAD-Gaussians 或深度正则；
9. 最后再改善查看器的抗锯齿和运动一致性。

这条路线的核心约束是：

- Brush 在完整质量基准完成前继续作为默认训练后端；
- gsplat 必须保持显式安装、版本、CUDA 能力和失败原因；
- 默认产物仍是普通第三方查看器可读取的标准 Gaussian PLY；
- 训练时附加模块不能被误认为已经写入 PLY；
- 不用更大的 splat cap、更多训练步数或更大的文件代替质量证明；
- 不用单一 opacity 阈值代替多视角证据，避免把细杆、叶片和边缘细节与浮点一起删除；
- GLOMAP 自动质量回退和预置 ONNX/LightGlue 恢复模式继续保留为备选，不在本质量主线中实施。

## 2. 质量目标与范围

### 2.1 本文所称“模型质量”

模型质量至少包含四个相互独立的维度：

| 维度 | 含义 | 主要观测方法 |
| --- | --- | --- |
| 视图重建质量 | 留出相机视角与原图的一致程度 | PSNR、SSIM、LPIPS、DISTS |
| 运动观看质量 | 沿连续轨迹观察时是否闪烁、跳变、出现透明裂缝 | 固定轨迹视频、帧间感知差、人工检查 |
| 几何可信度 | 结构是否稳定，是否存在漂浮点、错误薄片和明显深度断裂 | COLMAP 指标、深度/法线一致性、轨迹检查 |
| 交付兼容性 | 导出的 PLY 在目标查看器中是否保持颜色、尺度、坐标和透明度 | 跨查看器 smoke、PLY 属性检查 |

PSNR 或训练 loss 只能覆盖其中一部分。单一指标升高不能单独证明整体质量提升。

### 2.2 目标场景

主线面向：

- 单个普通 RGB 视频；
- 静态场景为主体；
- 手机或运动相机常见的自动曝光、白平衡和滚动快门；
- COLMAP/CASPAR 生成相机和稀疏点；
- Brush 或本地 gsplat 生成 Gaussian PLY；
- Windows 桌面离线运行。

### 2.3 非目标和独立路线

以下能力不应混入默认静态 PLY 主线：

- 4D 动态人物或动态场景建模；
- 生成式补视角和内容幻觉；
- 必须使用专用神经查看器才能显示的外观表示；
- 高精度 mesh 输出。2DGS、PGSR、GOF 应作为独立“表面/网格重建”产品路线；
- 自动切换 GLOMAP 并重跑的质量回退；
- 内置 ONNX LightGlue 权重的最终恢复模式；
- VGGT、Dense-SfM 等大型模型替代默认 COLMAP。

## 3. 当前代码审计：M0 阻断项

### 3.1 相机模型与训练图像不一致

`src-tauri/src/engines/colmap.rs` 当前固定使用：

```text
ImageReader.camera_model = SIMPLE_RADIAL
ImageReader.single_camera = 1
```

但 `engines/gsplat/adapter/train_adapter.py` 读取 `SIMPLE_RADIAL`、`RADIAL`、`OPENCV` 等模型时，只保存 `fx/fy/cx/cy`，其余径向、切向和鱼眼畸变参数被丢弃。同时 `prepare_standard_colmap_dataset` 直接复制原 JPEG 和稀疏模型，没有执行 COLMAP `image_undistorter`。

因此当前 gsplat 实际使用“有畸变像素 + 无畸变针孔投影”训练。图像中心可能看似正常，但边缘会积累系统性重投影误差，训练器可能通过扩大 Gaussian、产生漂浮点或错误透明度去拟合该误差。

**M0 要求：**默认选择 COLMAP 去畸变后的针孔训练目录。3DGUT 原始畸变训练只作为后续条件路线。

### 3.2 Gaussian 表达不完整

当前 adapter 只优化：

- means；
- 单个 RGB/DC 颜色；
- opacity；
- scale；
- quaternion。

PLY 只包含 `f_dc_0..2`，没有 `f_rest_*`，即没有高阶 spherical harmonics。模型无法表达随观察方向变化的高光、反射和细微色彩变化。

**M0 要求：**实现 SH degree 从 0 逐步提升至 3，并按标准 3DGS PLY 属性导出全部 SH 系数。

### 3.3 初始化不符合标准 3DGS

当前实现使用固定：

- `scales = -3.0`；
- `opacities = 0.0`，经过 sigmoid 后为 0.5；
- means 学习率 `0.002`；
- 没有位置学习率指数衰减。

标准 3DGS/gsplat 通常根据稀疏点近邻距离初始化尺度，以约 0.1 的初始 opacity 开始，并使用更保守、随训练下降的位置学习率。固定尺度会忽略场景单位和点云稀疏度；过高的初始透明度会造成早期遮挡；过大的固定位置学习率可能导致点快速离开可靠的 COLMAP 初始化。

**M0 要求：**复用 gsplat 官方 `init_utils`、kNN 尺度初始化和位置学习率调度，不自行维护另一套未经验证的常量。

### 3.4 损失函数不足

当前 reconstruction loss 只有 L1：

```text
loss = L1(render, target)
```

没有 DSSIM、感知损失或图像边缘约束。L1 对像素偏差稳定，但单独使用容易得到平均化纹理。

**M0 要求：**先实现官方基线 `0.8 × L1 + 0.2 × DSSIM`。WD-R 等新损失必须在该基线通过后再实验。

### 3.5 验证集有视频相邻帧泄漏

当前只把最后一张图作为验证帧，其余所有帧训练。连续视频相邻帧高度相似，单张末帧既可能与训练帧近似重复，也无法覆盖整条相机轨迹。

**M0 要求：**建立时间分块留出集，而不是随机散点或单末帧留出。验证帧及其相邻保护区不得参与训练。

### 3.6 图像缩放和采样仍属最小实现

当前：

- 使用 PIL 双线性缩放；
- 训练样本按固定顺序循环；
- 不按场景覆盖、相机位置或困难区域分层采样；
- 不记录每个验证区段的独立指标。

**M0 要求：**缩小图像使用 area/Lanczos；训练使用固定 seed 的 shuffle 或覆盖分层采样；验证必须分区报告，不能只报告全局平均数。

## 4. 目标训练架构

```text
选中关键帧
  -> COLMAP/CASPAR 稀疏重建
  -> 稀疏模型质量门禁
  -> COLMAP image_undistorter
  -> training-input-undistorted
  -> 时间分块 train/validation 清单
  -> 标准 3DGS 初始化
  -> 光度补偿/动态像素权重
  -> MCMC 或 AbsGS densification
  -> L1 + DSSIM + 可选 WD-R
  -> 留出视图和轨迹质量评估
  -> 标准 PLY 导出
  -> 跨查看器验证
```

建议训练器内部按职责拆分：

| 模块 | 职责 |
| --- | --- |
| `dataset.py` | 相机、图像、train/validation manifest、缩放和缓存 |
| `initialization.py` | COLMAP 稀疏点、kNN scale、opacity、SH 初始化 |
| `photometric.py` | PPISP、曝光和颜色校正 |
| `sampling.py` | shuffle、时间分层、困难区域采样 |
| `losses.py` | L1、DSSIM、WD-R、正则和 mask 加权 |
| `strategies.py` | MCMC、AbsGS、后续 ResGS 策略 |
| `evaluation.py` | PSNR、SSIM、LPIPS、DISTS、分区指标 |
| `export.py` | 完整标准 Gaussian PLY、属性和一致性检查 |
| `train_adapter.py` | 配置、JSONL 事件、取消、异常边界和编排 |

不要继续把所有训练逻辑堆积在单个最小 adapter 文件中。

## 5. 里程碑 M0：完整且可信的 gsplat 基线

### 5.1 建立去畸变训练输入

在 COLMAP sparse model 通过门禁后新增显式步骤：

```text
colmap image_undistorter
  --image_path <selected-frames>
  --input_path <sparse-model>
  --output_path <training-input-temp>
  --output_type COLMAP
```

实现要求：

- 写入临时目录，完成后原子替换正式 `training-input`；
- 不能覆盖原始选中帧或原始 sparse model；
- 输出图像、`cameras.bin`、`images.bin`、`points3D.bin` 必须相互一致；
- 记录输入和输出相机模型、图像数量、分辨率和缩放；
- 去畸变失败时默认停止 gsplat，而不是静默忽略畸变；
- Brush 若已正确处理原模型，可以继续使用原 backend contract；后续再统一两个后端的输入。

### 5.2 完整 SH 表达和导出

实现：

- `sh0` 与 `shN` 分开存储；
- 初始只启用 degree 0；
- 每 1,000 步提高一级，最大 degree 3；
- 未启用的高阶系数保持零或不参与梯度；
- 导出 `f_dc_*` 和全部 `f_rest_*`；
- 用已有 Gaussian PLY validator 检查属性、数组长度和有限值。

回退：

- 若某查看器不支持高阶 SH，不修改主 PLY；另行提供明确标记的 DC-only 兼容导出。

### 5.3 标准初始化和优化器

实现：

- 使用稀疏点 kNN 距离初始化各向同性尺度；
- 初始 opacity 默认 0.1；
- SH DC 由点云颜色初始化；
- means LR 采用 scene scale 感知值和指数衰减；
- SH0、SHN、opacity、scale、rotation 使用各自学习率；
- 保存所有实际生效参数到训练历史；
- 初始化结果记录中位尺度、P95 尺度和场景半径。

保护：

- 检测零距离、非有限点和极端离群点；
- 对异常点先过滤，不允许 kNN 结果产生 NaN/Inf；
- 不把 `maxSplats` 用作稀疏点初始化采样质量目标。

### 5.4 基线损失

默认：

```text
L_rgb = 0.8 * L1 + 0.2 * DSSIM
L_total = L_rgb + 0.01 * opacity_reg + 0.01 * scale_reg
```

正则权重仅在 MCMC 模式使用论文/gsplat 推荐值；其他 strategy 必须有独立配置。

### 5.5 时间分块留出集

建议：

- 总帧数不少于 40 时，留出约 8%–10%；
- 将留出帧分布在时间轴前部、中部、后部及闭环区域；
- 每个验证中心帧向前后设置保护半径，保护区不进入训练；
- 验证段不能全是模糊帧、动态帧或纯旋转帧；
- 训练和验证文件清单持久化，后续 A/B 完全复用；
- 少于 40 帧时降低留出比例，但至少覆盖 3 个不同位置；
- 极少视图项目不自动声称具有可靠量化验证。

输出：

```text
logs/training-split.json
logs/quality/validation-metrics.json
logs/quality/validation-renders/
```

### 5.6 M0 交付条件

- 相机和图像投影契约一致；
- PLY 包含完整 SH 属性；
- 初始化、损失和学习率与官方 gsplat 基线可解释对齐；
- 留出集不是单帧，也不与训练集近重复；
- 相同输入可复现训练清单和参数；
- Brush 与 gsplat 的质量比较使用同一去畸变图像、相机、验证清单和分辨率；
- 在 M0 完成前，UI 中 gsplat 继续标注为实验后端。

## 6. 里程碑 M1：PPISP 光度一致性

### 6.1 采用原因

视频输入常见的自动曝光、白平衡、暗角和相机响应变化会破坏多视图光度一致性。普通 3DGS 可能让几何、opacity 或 SH 去拟合相机处理造成的颜色差异。CVPR 2026 PPISP 将相机固有和随帧变化的 ISP 效应显式分解，并已进入 gsplat 官方 trainer。

本地 gsplat 源码已经含有 PPISP 接口，因此优先复用上游实现，不重新实现论文模型。

### 6.2 接入方式

新增配置：

```json
{
  "photometricMode": "none | affine | bilateral_grid | ppisp",
  "ppispController": true,
  "ppispControllerDistillation": true,
  "canonicalExposure": "median"
}
```

默认策略：

- 快速：`none`；
- 均衡：先保持 `none`，真实基准通过后再考虑 `ppisp`；
- 精细：PPISP 通过三类素材门禁后可默认开启；
- 检测到曝光变化、暗角或白平衡漂移时，UI 提示 PPISP 的预期收益。

### 6.3 PLY 边界

PPISP 训练时能防止场景几何吸收光度误差，但普通 PLY 无法存放完整 PPISP 控制器。因此必须区分：

- 场景的 canonical radiance/SH：可写入普通 PLY；
- 每帧曝光、暗角、CRF 和 controller：写入项目 checkpoint/metadata；
- PPISP 查看效果：需要支持该模块的渲染器；
- 普通 PLY 最终质量：必须在不运行 PPISP 后处理的第三方查看器中复验。

### 6.4 门禁与回退

接受条件：

- 普通 PLY 的留出视图 PSNR/SSIM 不下降；
- LPIPS 或 DISTS 至少一项有稳定改善；
- 暗角区域和曝光切换位置不再产生额外漂浮点；
- canonical 外观无明显整体偏色；
- 三次运行无 controller 崩溃或不可恢复 checkpoint。

失败时：关闭 PPISP，保留完整 M0 基线；不能回退整个 gsplat 后端或重跑 COLMAP。

## 7. 里程碑 M2：动态干扰抑制和延迟增殖

### 7.1 第一阶段不引入大型模型

先复用现有自适应抽帧产生的背景运动、内点、覆盖率和清晰度数据，并结合训练中的跨视图残差生成像素或图像权重。

建议权重来源：

- 光流/RANSAC 判断的非背景运动；
- COLMAP 未注册或高重投影误差区域；
- 多视图颜色残差持续异常；
- 饱和、欠曝和模糊区域；
- 图像边缘无可靠共同可见区域。

训练方式：

```text
L_masked = sum(weight * pixel_loss) / max(sum(weight), epsilon)
```

不能把低权重区域完全删除，否则可能误伤真实反射、细小结构或暂时遮挡后的背景。

### 7.2 延迟增殖

参考 RobustSplat：

- 前 10%–15% 步数只优化现有 Gaussian 或显著降低 densification 频率；
- 先让静态大结构收敛；
- 动态残差稳定后再允许 split/relocation；
- densification 分数乘以静态可信权重；
- 不让短暂行人、车辆或树叶运动在早期生成永久 Gaussian。

### 7.3 第二阶段备选

只有轻量 mask 在真实动态素材上无法通过门禁时，才建立独立 WildGaussians/RobustSplat 特征模型实验。模型权重、许可证、显存、离线分发和启动时间必须单独评估。

## 8. 里程碑 M3：MCMC 与 AbsGS 双策略

### 8.1 产品策略

MCMC 与 AbsGS 解决的重点不同：

- MCMC：对初始化更稳健，Gaussian 数量可控，适合普通和复杂场景；
- AbsGS：通过绝对/同向梯度减少 gradient collision，更容易恢复细线和高频细节。

两者不应在同一次训练中简单叠加。

建议首轮预设：

| OOOSplat 预设 | strategy | cap | 说明 |
| --- | --- | ---: | --- |
| 快速 | MCMC | 1M | 保持低资源和稳定性 |
| 均衡 | MCMC | 2M | 默认质量/资源折中 |
| 精细-A | MCMC | 3M | 稳健高质量基线 |
| 精细-B | AbsGS | 由门禁决定 | 细线、文字、叶片和结构纹理实验 |

### 8.2 不采用固定场景规则直接切换

第一阶段只做显式 A/B，不根据“室内/室外”等粗分类自动切换。待样本量足够后，才可以基于以下可观测量推荐 strategy：

- 高频边缘面积占比；
- 稀疏点覆盖和 track length；
- 相机视差分布；
- M0 前期验证 improvement 曲线；
- splat 增殖率和显存增长；
- 细节区 LPIPS/DISTS。

### 8.3 质量和资源门禁

AbsGS 或新 MCMC 参数必须同时满足：

- 全局和分区指标不回退；
- 关键细节 crop 有可见改善；
- Gaussian 数不无界增长；
- RTX 8 GB 安全配置不 OOM；
- RTX 5090D 不出现上游 MCMC 非法内存访问；
- 导出 PLY 属性与普通查看器兼容。

### 8.4 当前实现与未完成验收（2026-08-25）

已实现以下最小可回退闭环：

- `GsplatDensificationStrategy` 持久化为 `mcmc`（默认）或 `absgrad`；旧设置和旧项目通过 serde 默认值安全读取为 MCMC；
- 设置页仅在选择 gsplat 后端时显示“gsplat 增殖策略”，避免影响 Brush 预设与默认路径；
- 任务创建时将 strategy 写入 `project.json`、历史详情和 adapter `request.json`，从而与质量档、splat cap、seed=42 一起可追溯；
- adapter 的 AbsGS 路径采用本地 gsplat `DefaultStrategy(absgrad=True)`，并向 rasterizer 请求绝对屏幕空间梯度；MCMC 保持既有 `MCMCStrategy` 路径；
- 延迟增殖、验证集冻结和标准 PLY 导出继续对两条策略生效；AbsGS 的首轮比较应关闭 PPISP，防止把光度控制器影响混入策略结论。

首个真实配对素材（均衡、15,000 steps、1024px、2M cap、seed=42、PPISP 关闭）中，MCMC 得到 PSNR 15.0476 / SSIM 0.6388、1,728,358 splats；AbsGS（`absgradGrowGrad2d=0.0008`）得到 PSNR 14.9612 / SSIM 0.6246、1,447,324 splats。AbsGS 虽少 16.3% splats、训练快 8.9%，但 SSIM 下降 0.01425，未通过门槛。随后在 chair 素材试验 `0.0004`，AbsGS 的 splat 增加 34.9%，但 PSNR 下降 0.4731 dB、SSIM 下降 0.03419，仍未通过门槛。为排除提前冻结，chair 又以 `0.0008` 完整增殖至第 12,000 步；splat 从 546,157 增至 654,522，但相对冻结版 PSNR 再下降 0.4706 dB，故该调度实验已回退。检查本地 gsplat `DefaultStrategy` 后确认它会按 `n_cameras` 缩放梯度并按可见相机数平均，当前 batch=4 不是梯度统计错误。桌面请求继续显式记录 `absgradGrowGrad2d=0.0008`；这仅影响 AbsGS 实验，不改变 MCMC 或 Brush 默认。

已完成静态验证：Python adapter 语法编译、Rust 单元测试和桌面/命令行 Rust 编译、前端生产构建。尚未完成的真实验收是固定同一素材、同一质量档、同一 cap 和 seed 的 MCMC 与 AbsGS 成对运行；必须记录 PSNR、SSIM、最终 splat 数、峰值显存、训练时间和标准 PLY 检查结果，再决定是否扩大实验范围或调整默认。

## 8A. 里程碑 M3.5：多视角浮点抑制与安全裁剪

### 8A.1 结论与实施定位

当前模型浮点的主要工程缺口不是训练步数不足，而是只被少数视角支持、尺度异常、空间孤立或拟合了动态/反光/模糊残差的 Gaussian 仍能增殖并进入最终 PLY。现有 gsplat adapter 导出时主要依据 `sigmoid(opacity) > 0.005` 保留点；MCMC 会重定位低 opacity Gaussian，但没有完整的多视角支持、渲染贡献、尺度和孤立度联合门禁。AbsGS/DefaultStrategy 虽增加超大世界尺度裁剪，仍不能识别“高于 opacity 阈值但只解释单个视角”的浮点。

因此 M3.5 的默认路线确定为：

```text
同输入基线诊断
  -> 多视角一致性增殖门控
  -> 导出前多条件安全裁剪
  -> 禁止增殖的短程恢复训练
  -> 留出视角与关键 crop 复验
  -> 不达门槛自动回退裁剪前 checkpoint/PLY
```

该阶段借鉴 FastGS 的多视角一致性增殖/裁剪、Taming 3DGS 的跨视角贡献度和 TIDI-GS 的空间关系与细节保护，但不要求改变标准 3DGS PLY，也不把尚无成熟本地集成的整套论文代码直接作为产品依赖。

### 8A.2 先区分五类问题

训练或裁剪前必须先生成固定轨迹视频并分类，避免把查看器问题误当模型浮点：

| 现象 | 优先诊断 | 默认处理 |
| --- | --- | --- |
| 只在单个输入视角附近出现半透明团块 | 单视角残差、近相机 screen-space artifact | 多视角支持与深度冲突门禁 |
| 多个视角都能看到漂浮薄片/云团 | 位姿误差、低视差、异常尺度 | 先检查 COLMAP，再做尺度/贡献联合裁剪 |
| 集中在人、车辆、树叶、阴影周围 | 动态或瞬态内容 | 动态软 mask，禁止其驱动增殖 |
| 集中在玻璃、金属、镜面或积水周围 | 反射/透射外观歧义 | 独立反射恢复路线，不提高全局裁剪强度 |
| 静止时正常，仅运动或转动时 popping | 排序、抗锯齿或查看器问题 | StopThePop/Mip 路线，不删除模型点 |

若 COLMAP 注册率、轨迹连通性、重投影误差或相机簇检查失败，M3.5 不得通过强裁剪掩盖重建错误，应回到 SfM/输入质量阶段。

### 8A.3 第一轮无代码 A/B 与资源边界

在开发新裁剪器前，先固定抽帧 manifest、COLMAP sparse model、训练/验证划分、seed、步数、分辨率和查看轨迹，仅改变一个变量：

1. gsplat 比较 MCMC 与 AbsGS，首轮关闭 PPISP；
2. 均衡路线优先测试 1.5M–2M cap，精细路线优先测试 2.5M–3M cap；
3. 暂不把 5M–8M 作为浮点问题素材的默认解法，因为更大 cap 可能继续拟合单视角噪声；
4. Brush 先只降低 `max_splats`；若仍明显过度增长，再分别测试把 `growth-select-fraction` 从 0.10 降至约 0.05，或把增长停止点从 15,000 提前至约 12,000；
5. 上述数值只是场景归一化前的实验起点，不得同时修改 cap、增长比例和停止点后宣称单项收益。

若只降低 cap 就产生明显孔洞，说明问题不是单纯冗余点过多，不应继续压缩 cap，应进入多视角门控。

### 8A.4 多视角增殖门控

对每个候选 split/clone/relocation Gaussian，除原 strategy 梯度条件外，按分层采样的训练视角累计：

- `viewSupportCount`：真正投影到有效像素且贡献超过噪声底的视角数；
- `accumulatedContribution`：跨视角累积的 `alpha * transmittance` 或等价可见性贡献；
- `mean/maxProjectedRadius`：平均与最大屏幕投影半径；
- `depthConsistency`：与主表面深度或多视图重投影深度的一致程度；
- `cameraProximity`：是否异常靠近单个相机；
- `staticConfidence`：现有动态/瞬态软 mask 给出的静态可信度。

首版门禁采用可解释的联合条件：

```text
allowDensification =
    originalStrategyPass
    AND sufficientViewSupport
    AND contributionAboveNoiseFloor
    AND depthConflictBelowLimit
    AND staticConfidenceAboveLimit
```

`sufficientViewSupport` 不能固定写死为三个视角。建议默认至少 2–3 个有效视角，并按总训练视角数、可见视角数和场景覆盖自动降低，防止短视频或边缘区域永远不能增殖。高贡献但低支持的候选应进入“保护/观察”队列，而不是立即删除。

### 8A.5 导出前多条件安全裁剪

必须把训练期 `trainingMinOpacity` 与导出期 `exportPruningPolicy` 分开。`trainingMinOpacity=0.005` 可继续作为首轮基线，但不能简单全局提高到 0.02。导出前按场景半径和统计分位归一化后执行：

```text
prune =
    opacity <= trainingMinOpacity
    OR (lowViewSupport AND lowContribution)
    OR (spatiallyIsolated AND lowContribution)
    OR (oversizedInWorldSpace AND lowContribution)
    OR (singleCameraNearField AND depthInconsistent)
```

其中：

- `spatiallyIsolated` 使用 kNN 距离/邻居数并按 scene radius 归一化；
- `oversizedInWorldSpace` 参考 DefaultStrategy 的场景尺度规则，但必须与低贡献联合，避免误删真实大平面；
- `singleCameraNearField` 只处理靠近单一相机、缺乏其他视角支持的半透明 Gaussian；
- 高累积贡献、高边缘响应、细杆/叶片/文字 crop 内的 Gaussian 进入细节保护集合；
- 第一版只做保守联合裁剪，不使用“满足任一弱信号即删除”的激进规则；
- 输出裁剪原因计数和被删 Gaussian ID/索引映射，保证可审计。

建议首轮把低支持、低贡献和孤立度阈值定义为场景内分位数，并通过三类素材标定；在没有真实 A/B 之前，不把某个绝对距离、绝对尺度或固定删除比例设为产品常量。

### 8A.6 裁剪后恢复训练

安全裁剪后进行 1,000–3,000 步短程恢复：

- 禁止 split、clone、relocation 和新增 Gaussian；
- 关闭或显著降低 MCMC mean noise；
- 仅优化已有 Gaussian 的位置、尺度、旋转、opacity 和 SH；
- 学习率沿用原训练末期水平或更低；
- 恢复训练前后都导出标准 PLY、checkpoint 和验证渲染。

恢复阶段的目的只是修复裁剪造成的小范围光度/边界变化，不能重新生成一批未经多视角门禁的点。

### 8A.7 自动质量回退

每次裁剪必须先保留 `pre-prune` checkpoint/PLY，生成 `post-prune` 候选后在完全相同的留出视角、关键 crop 和固定轨迹上比较。首轮自动回退门槛采用比一般新算法更严格的保护值：

- PSNR 下降超过 0.15–0.20 dB；
- SSIM 下降超过 0.002；
- LPIPS 增加超过 0.005；
- 关键 crop 出现孔洞、细线消失或结构断裂；
- 最差验证区段明显退化；
- 删除比例超过实验配置的安全上限，首轮建议警戒值 20%；
- 普通第三方 PLY 查看器中的运动质量反而变差。

触发后按以下顺序处理：

1. 回退到 `pre-prune` 产物；
2. 将裁剪强度降一级，只放宽低贡献/孤立度分位；
3. 最多自动重试一次；
4. 仍失败则保留原模型，将原因写入历史，不静默返回退化模型。

### 8A.8 诊断产物与完成条件

每次实验至少持久化：

- `floater-diagnostics.json`：阈值、分位数、各类候选/删除数、视角支持和贡献分布；
- `prune-manifest.json`：裁剪前后 splat 数、删除原因、checkpoint/PLY 哈希；
- `pre-prune.ply`、`post-prune.ply` 和恢复后的候选 PLY；
- 固定轨迹与关键 crop 的裁剪前后渲染；
- 自动回退是否触发、触发指标、最终 effective configuration；
- 训练、统计、裁剪和恢复各阶段耗时与峰值显存。

M3.5 进入产品预设前必须满足：至少三类真实素材同输入 A/B；浮点等级和固定轨迹明显改善；PSNR/SSIM/LPIPS 通过严格门槛；薄结构不出现系统性损失；标准 PLY 跨查看器通过；关闭开关可完全恢复原训练/导出路径。

### 8A.9 实施状态（2026-08-26）

已完成第一阶段的**只读诊断**，未开始任何裁剪、增殖门控或恢复训练：

- adapter 在训练和最终验证完成后，均匀采样最多 12 个训练视角，写入
  `logs/floater-diagnostics.json`；
- 记录活跃 Gaussian 的采样视角支持数、投影半径和
  `opacity × projectedRadius²` 投影面积代理，以及最大世界尺度相对场景半径的分位数；
- 第二批只读统计已加入：双错位体素网格的邻域支持代理、最近相机距离和“低视角支持且处于本场景最近相机距离 P10 内”的联合计数。实现为线性内存诊断，不使用全量 kNN 矩阵；因此不会把该体素代理误称为精确 kNN 孤立度，也不会用于自动裁剪。
- `projectedFootprintProxy` 明确只是可见面积代理，不能误称为精确的
  `alpha × transmittance` 累积贡献；
- 文件写明当前未覆盖的空间孤立、深度冲突、近相机冲突和动态可信度，避免将不完整统计用于自动删除；
- 此阶段不更改 strategy、opacity、导出选择或 PLY，因而可作为 MCMC、AbsGS、WD-R
  的同输入基线证据。
- 诊断的 opacity 向量必须在与 `radii` 相乘前压平成一维；否则 `[N,1] × [N]` 会被广播为 `N×N`，在大模型完成训练后造成非必要的显存失败。gsplat 返回每轴半径时还必须先将 `[...,N,2]` 沿轴取最大值，得到严格的 `[N]`。诊断代码还必须独立容错：统计失败写入 `status: failed` 和告警，但不得阻断已经完成的验证与标准 PLY 导出。

已开始第二阶段的**仅限新增点**门控，默认关闭且只支持 MCMC：启用时将最多 12 个均匀训练视角的可见性保存在每个 Gaussian 的紧凑位集里；MCMC 仅从至少两个采样视角可见的父点生成新点，新点继承父点的支持记录。已有 Gaussian、MCMC relocation、导出 opacity 过滤和 PLY 均不变。配置及增长审计会写入 `training-split.json` 与 `floater-diagnostics.json`。AbsGS 不显示此开关，切换到 AbsGS 会自动关闭它。

接下来的实现顺序固定为：对这份门控在反光、低纹理和常规素材做同输入 A/B；只在质量门槛通过后才考虑扩大范围；最后才在 `pre-prune` 备份、短程禁止增殖恢复和自动回退均完成后测试保守导出裁剪。

## 9. 里程碑 M4：WD-R 感知质量实验

### 9.1 实施定位

2026 年 Drop-In Perceptual Optimization 工作在大规模主观评价中报告 WD-R 能恢复更好的感知纹理，并可作为不同 3DGS 框架的 drop-in loss。它不是生成式补纹理方法，但仍可能牺牲像素指标或放大输入压缩伪影，因此必须晚于 M0–M3。

### 9.2 实验方案

先采用后期渐进权重：

```text
0%–60% steps: L1 + DSSIM
60%–80% steps: 逐步增加 WD-R
80%–100% steps: 固定低权重 WD-R
```

首轮只测试小权重集合，不直接默认：

```text
lambda_wdr in {0.02, 0.05, 0.10}
```

具体权重必须根据论文代码的归一化范围校准，不能仅凭数值照搬。

### 9.2.1 2026-08-26 官方实现核对与首轮固定边界

已在隔离 `engines/gsplat/python` 运行时验证官方
`balle-lab/wasserstein-distortion` 0.1.0（Apache-2.0）、`torchvision`
0.26.0 与现有 `torch 2.11.0+cu130` 可以共同导入，且预训练
ImageNet VGG-16 权重已固定缓存于 `engines/gsplat/wdr-cache`。128×128
CUDA 前向/反向烟雾测试输出有限损失与有限梯度；这只证明运行时可用，
不构成任何真实素材质量结论。该缓存、Python 包和模型权重均为可选运行时
payload，禁止作为源码提交或默认安装包内容。

首轮不采用上方临时的 `lambda_wdr` 近似方案，而按论文定义实现真实目标：

```text
warm-up: 原始 0.8 L1 + 0.2 DSSIM
WD-R: gamma * (WD_VGG16(sigma=4) + (1 / 0.09) * original_loss)
```

- `sigma=4` 对应官方 Python 实现的常量 `log2_sigma=2`；不引入未经验证的
  saliency/depth map；
- 界面提供 WD-R 15k 与 WD-R 10k 两个固定步数选项；10k 只缩短训练预算，保持
  WD-R 损失参数与同一 seed/cap，便于在质量和耗时之间做可重复 A/B；
- 论文使用 3k（大场景 5k）步 warm-up，并针对素材调节 `gamma` 使 Gaussian
  数量可比；本项目首轮将 `gamma=0.025` 固定为实验记录字段，后续只能通过
  同输入、同 cap、同 seed 的 A/B 校准；
- WD-R 必须默认关闭、仅 gsplat 可选、强制 batch=1，并在任务元数据、`ready`
  JSONL 与 `validation-metrics.json` 写出模式、warm-up、gamma、beta、sigma 和
  权重版本；
- 官方论文报告未优化 WD 约增加 4.5× 单步时间，缓存 GT 特征后仍约 2.8×；在
  8–24GB 显存设备上先以实际峰值、OOM 回退和真实 A/B 为准，不能承诺加速；
- WD-R 只改变训练损失，导出仍为标准 PLY，不把 VGG、WD-R 状态写入模型格式。

### 9.3 验收重点

- 人工盲测必须包含文字、栅栏、叶片、纹理墙面和远景；
- 同时观察 PSNR、SSIM、LPIPS、DISTS；
- 不能只挑选最清晰 crop；
- 检查 JPEG block、ringing 和锐化噪声是否被放大；
- 检查运动轨迹中细节是否稳定，而不是单帧锐、连续帧闪烁；
- 不增加 splat cap 作为 WD-R 对照的一部分。

## 10. 里程碑 M5：ResGS 式粗到细训练

### 10.1 目标

ResGS 使用图像金字塔、分阶段监督和 residual split，让训练先解决场景覆盖，再恢复细节。该路线用于解决：

- 大 Gaussian 长期覆盖复杂区域造成的模糊；
- 降低 split 阈值后出现覆盖不足；
- 纹理区域与无纹理区域使用相似尺度 Gaussian 的冗余。

### 10.2 最小实验拆分

不要一次实现完整论文。按三步隔离收益：

1. 仅加入多分辨率 coarse-to-fine schedule；
2. 在相同 schedule 下加入 progressive densification threshold；
3. 最后加入 residual split 和 opacity compensation。

每一步都复用同一训练/验证清单和随机种子。若第一步已经取得大部分收益，可延后更侵入的 residual split。

### 10.3 标准 PLY 兼容性

ResGS 的临时 level 属性只用于训练，不应导出。最终 means、scale、rotation、opacity 和 SH 仍按普通 Gaussian PLY 导出。

## 11. 条件恢复路线

### 11.1 3DGUT：畸变和滚动快门

触发条件：

- 明显鱼眼或广角畸变；
- 视频边缘在去畸变后损失过大；
- 检测到快速横移导致的倾斜直线和滚动快门；
- 普通针孔/去畸变基线在边缘区域持续失败。

要求：

- 仍需可靠相机标定；
- 记录 distortion 和 rolling-shutter 参数；
- 训练场景 PLY 可以保持普通属性，但专用相机效果不是 PLY 属性；
- 与去畸变 M0 基线单独 A/B；
- 不因 gsplat 支持 3DGUT 就默认绕过 COLMAP 去畸变。

### 11.2 BAD-Gaussians：严重运动模糊

触发条件：

- 清晰度筛选后剩余帧无法覆盖场景；
- 大部分候选帧存在曝光期相机运动；
- COLMAP 位姿和训练重建均在模糊区域不稳定。

不触发条件：

- 仅少量帧模糊且可以安全丢弃；
- 视频内容本身动态；
- 问题来源是错误内参或曝光变化。

BAD-Gaussians 需要联合优化曝光期间相机轨迹，工程和显存成本较高，应作为独立恢复模式，不并入普通精细预设。

### 11.3 深度正则：少视图和低视差

参考 DNGaussian，在以下条件同时出现时考虑：

- 已注册帧数少；
- 相机基线/视差不足；
- 稀疏点覆盖低；
- 普通 3DGS 出现成片漂浮结构；
- 补帧或重新拍摄不可用。

实现边界：

- 深度只作为相对/局部约束；
- 必须与 COLMAP 稀疏点对齐尺度和偏移；
- 使用置信度 mask；
- 反光、透明和天空区域降低权重；
- 不把单目深度直接当真实几何；
- 预训练权重、许可证和打包单独评审。

### 11.4 VGGT 与 Dense-SfM

这两条路线只作为更远期低纹理/注册失败实验：

- VGGT 可预测相机、深度、点图和轨迹，并可配合 BA；
- Dense-SfM 通过密集一致匹配和多视图 track refinement 改善弱纹理 SfM；
- 两者都引入新的大型模型、显存、运行时、许可证和分发边界；
- 不能替代当前已经规划的独立 GLOMAP 和最终 LightGlue 恢复路线。

## 12. 查看器质量与模型本体必须分开

### 12.1 Mip-Splatting/antialiasing

Mip-Splatting 的 3D smoothing 和 2D Mip filter 主要解决缩放和不同观察距离下的 aliasing、dilation 和 erosion。gsplat 已支持 antialiased rasterization。

必须分别验证：

- 训练时 3D filter 是否已经 bake 到 scale/opacity；
- 普通 PLY 查看器是否能复现训练器效果；
- 仅更换 renderer 时模型文件本身是否没有变化。

### 12.2 StopThePop

StopThePop 通过更准确的 per-pixel/hierarchical sorting 减少相机旋转时的 popping。它提高运动观看一致性，但不会修复错误相机、缺失几何或模糊纹理。

因此它只能进入查看器路线，不能作为训练模型质量提升的证明。

## 13. 质量评估设计

### 13.1 固定测试集

至少准备三类真实项目：

| 测试集 | 必含问题 | 用途 |
| --- | --- | --- |
| 室内细节 | 文字、细线、重复纹理、暗角 | SH、AbsGS、WD-R |
| 室外大范围 | 曝光变化、天空、树叶、闭环 | PPISP、动态 mask、轨迹稳定性 |
| 困难素材 | 低光、模糊、反光或低视差 | 条件恢复和失败归因 |

每类至少一个短项目和一个中等/长项目。所有 A/B 固定：

- 原视频；
- 选中帧 manifest；
- COLMAP sparse model 或明确记录的重建版本；
- train/validation manifest；
- 分辨率；
- GPU 和驱动；
- 随机种子；
- 训练步数；
- splat cap；
- 查看轨迹。

### 13.2 指标

训练后必须输出：

```json
{
  "psnr": {},
  "ssim": {},
  "lpips": {},
  "dists": {},
  "validationSegments": [],
  "splatCount": 0,
  "prePruneSplatCount": 0,
  "prunedSplatCount": 0,
  "pruneReasonCounts": {},
  "viewSupportHistogram": [],
  "contributionPercentiles": {},
  "spatialIsolationPercentiles": {},
  "pruneRollback": null,
  "plyBytes": 0,
  "trainingMs": 0,
  "peakVramMb": 0,
  "failure": null
}
```

每个图像指标同时提供：

- 全体平均；
- 中位数；
- P10/P90；
- 最差验证区段；
- 每个预定义细节 crop。

不要只给平均数，因为少数严重失败视角可能被平均值掩盖。

### 13.3 几何代理指标

没有真实扫描几何时，至少记录：

- COLMAP 注册率；
- sparse points 数；
- mean/median reprojection error；
- track length 分布；
- 相机轨迹连通性；
- 渲染 depth 的跨视图重投影一致性；
- 低 opacity/超大 scale Gaussian 比例；
- 相机包围盒外 Gaussian 比例；
- 低视角支持且低贡献 Gaussian 比例；
- kNN 空间孤立度分布；
- 单相机近场且深度冲突 Gaussian 比例；
- 裁剪前后各原因的 Gaussian 数和关键细节保护数；
- 人工标注的 floaters/holes 等级。

这些是代理指标，不能被描述为真实几何精度。

### 13.4 运动轨迹验收

固定一条包含以下动作的相机轨迹：

- 沿输入轨迹插值；
- 横向穿过细线/遮挡边界；
- 轻微外插；
- 缩放靠近高频区域；
- 原地旋转观察反光区域。

保存完全相同的 MP4 参数，并进行：

- 并排盲测；
- 帧间 LPIPS/DISTS 波动；
- popping、透明裂缝和闪烁检查；
- 普通 PLY 查看器和训练器 viewer 双重检查。

## 14. 接受门槛

### 14.1 相对门槛

相对当前默认 Brush 和通过 M0 的 gsplat 基线分别比较。新方法进入产品预设应满足：

- PSNR 中位数不得下降超过 0.3 dB；
- SSIM 中位数不得下降超过 0.005；
- LPIPS 不得增加超过 0.01；
- DISTS 不得出现统计稳定的回退；
- 最差验证区段不能出现新的结构性失败；
- 关键 crop 至少有一致的视觉收益；
- 普通查看器不能出现训练器 viewer 中不存在的明显退化；
- 三个随机种子中至少两个达到相同方向的收益。

这些阈值是首轮工程门禁，不是永久真理。积累真实项目分布后应重新标定。

### 14.2 绝对失败条件

任一项出现即拒绝进入默认：

- PLY 缺失标准属性或数组长度不一致；
- NaN/Inf；
- 坐标、比例、旋转或纵横比错误；
- 关键前景出现明显空洞；
- 大面积漂浮点、透明度崩坏或黑块；
- 训练退出成功但验证渲染失败；
- PPISP 等训练模块关闭后普通 PLY 明显变色；
- 仅在一个挑选样本上有效；
- OOM、CUDA illegal memory access 或无法取消；
- 恢复/继续训练后质量明显不同且没有解释。

## 15. 回退与功能开关

每项优化必须独立关闭：

| 开关 | 默认初始值 | 回退目标 |
| --- | --- | --- |
| `gsplatFullBaseline` | 开发完成后 true | 旧 adapter 只保留迁移期，不作为质量后端 |
| `photometricMode` | none | M0 标准光度路径 |
| `dynamicLossMask` | false | 无 mask 的 M0/M1 |
| `delayedDensification` | false | strategy 默认调度 |
| `densificationStrategy` | mcmc | MCMC 基线 |
| `multiViewDensificationGate` | false | 原 strategy 增殖判定 |
| `floaterPruning` | false | 仅保留既有 opacity 导出过滤 |
| `postPruneRecoverySteps` | 0 | 不进行裁剪后恢复训练 |
| `floaterPruningAutoRollback` | true（启用裁剪时） | `pre-prune` checkpoint/PLY |
| `perceptualLoss` | none | L1+DSSIM |
| `coarseToFine` | false | 单分辨率训练 |
| `cameraProjectionMode` | undistorted | COLMAP 去畸变针孔 |
| `depthRegularization` | false | 无预训练深度依赖 |

功能回退不应触发以下动作：

- 覆盖已有 sparse model；
- 删除已生成 PLY；
- 重跑无关阶段；
- 静默切换训练后端；
- 把实验失败写成 Brush 失败；
- 把质量问题误分类为 CUDA 不可用。

## 16. 历史、日志和可复现性

项目历史至少持久化：

- training backend 与精确版本/commit；
- Python、PyTorch、CUDA、gsplat 版本；
- 相机投影模式和去畸变参数；
- train/validation manifest 哈希；
- strategy 和全部参数；
- SH degree schedule；
- loss 组成和权重；
- PPISP/mask/depth 等模块状态；
- seed；
- stage timing；
- peak VRAM；
- 最终 splat 数和 PLY SHA-256；
- 裁剪前后 splat 数、裁剪原因分布、视角支持/贡献/孤立度统计；
- `pre-prune` 与最终候选 checkpoint/PLY 哈希；
- 恢复步数、禁止增殖状态和自动回退判定；
- 全局、分区和 crop 指标；
- 回退原因和 effective configuration。

已存在的旧历史必须通过 serde/default 或等效兼容层继续读取。

## 17. 建议代码改动顺序

### 阶段 A：不改变产品默认

1. 重构 gsplat adapter 模块边界；
2. 增加去畸变 training-input；
3. 增加完整初始化、SH、L1+DSSIM；
4. 增加时间分块验证和指标导出；
5. 完成 Brush/gsplat M0 基准。

交付条件：gsplat 可作为完整实验后端公平比较，但仍不自动推荐。

### 阶段 B：低风险质量增强

1. PPISP；
2. 动态 loss mask；
3. 延迟增殖；
4. MCMC/AbsGS 策略选择；
5. 完成三类真实素材 A/B。

交付条件：至少三组素材通过相对门槛，失败可单项回退。

### 阶段 B.5：多视角浮点抑制

1. 在不改代码的同输入 A/B 中先标定 cap 与 MCMC/AbsGS 基线；
2. 增加 `floater-diagnostics.json`，只统计不裁剪；
3. 接入多视角支持、累积贡献、深度冲突和静态可信度统计；
4. 将多视角证据加入 split/clone/relocation 门禁；
5. 增加导出前多条件保守裁剪和细节保护；
6. 增加禁止增殖的短程恢复训练；
7. 增加 `pre-prune` 产物、严格质量门禁和最多一次自动降级重试；
8. 完成普通静态、动态干扰、低纹理三类素材的同输入 A/B。

交付条件：浮点在固定轨迹中可重复减少，细杆/叶片/边缘不出现系统性损失，失败自动回到裁剪前标准 PLY。该阶段通过前，不开始用 WD-R 或 ResGS 掩盖结构伪影。

### 阶段 C：感知和致密化研究

1. WD-R；
2. coarse-to-fine；
3. progressive threshold；
4. residual split；
5. 评估是否形成新的“精细”预设。

交付条件：质量提升不能依赖更高 cap 或不同验证集。

### 阶段 D：条件恢复和查看器

1. 3DGUT；
2. BAD-Gaussians；
3. 深度正则；
4. 查看器 Mip/StopThePop；
5. VGGT/Dense-SfM 调研实验。

交付条件：每条路线都有明确触发器，不污染普通项目。

## 18. 测试计划

### 18.1 单元测试

- 相机模型参数解析和畸变不能静默丢失；
- SH degree 和 PLY 属性数量；
- kNN scale 无 NaN/Inf；
- opacity 初始化正确；
- loss 权重和 mask 归一化；
- train/validation 不重叠；
- 时间保护区生效；
- seed 可复现 manifest；
- strategy 配置互斥；
- 旧 history 兼容；
- 回退保留 effective configuration。

### 18.2 集成测试

- COLMAP 原模型到去畸变 training-input；
- M0 训练、指标、PLY 导出；
- PPISP checkpoint 和 canonical PLY；
- MCMC/AbsGS 两种 strategy；
- 中途取消、恢复和磁盘不足；
- 8 GB 显卡保护；
- RTX 5090D CUDA smoke；
- 跨查看器 PLY 加载。

### 18.3 真实运行验收

- 每类素材至少三次运行；
- 固定设备、驱动、输入、分辨率和 manifest；
- 报告中位耗时和质量，不只报告最快一次；
- 训练成功、指标成功、PLY 成功和查看成功分别记录；
- UI build 或 `exitCode=0` 不能代替真实质量验收。

## 19. 风险清单

| 风险 | 影响 | 缓解 |
| --- | --- | --- |
| 去畸变改变分辨率/FOV | Brush 与 gsplat 输入不一致 | 固定统一 training-input 并记录内参 |
| SH 增加显存和文件体积 | 低显存失败 | degree schedule、分辨率和 cache 保护 |
| PPISP 提升训练指标但普通 PLY 变差 | 错误宣传质量 | canonical 导出和第三方查看器复验 |
| 动态 mask 误伤反射/细节 | 结构缺失 | 软权重、置信度和关闭开关 |
| AbsGS splat 增长异常 | OOM/冗余 | cap、显存门禁和增长速率监控 |
| 单纯提高 opacity 阈值 | 细杆、叶片和边缘同时消失 | 训练/导出阈值分离，多视角联合裁剪 |
| 低视角支持规则写死 | 短视频与画面边缘无法增殖 | 按可见视角数自适应，保护高贡献候选 |
| 空间孤立或大尺度单条件裁剪 | 大平面、远景和真实薄结构被误删 | 必须与低贡献/深度冲突联合，按 scene radius 归一化 |
| 裁剪后继续增殖 | 浮点被重新生成 | 恢复阶段硬禁 split/clone/relocation |
| 强裁剪掩盖错误位姿 | 训练指标正常但几何仍错误 | SfM 门禁优先，失败返回重建阶段 |
| 浮点其实来自查看器排序 | 删除有效 Gaussian 仍不解决 popping | 固定轨迹分类，分流到 StopThePop/Mip 路线 |
| WD-R 放大 JPEG 伪影 | 看似锐利但不真实 | crop、轨迹和盲测 |
| ResGS 改动过大 | 难以归因 | 三步消融，不一次合并 |
| pose 优化漂移 | 相机和几何共同作弊 | 强正则、小范围、条件触发 |
| 单目深度错误 | 几何被错误先验拉偏 | 对齐、置信度和反光/天空 mask |
| 新模型权重影响分发 | 安装包和许可证风险 | 独立可选 payload，不内置默认 |

## 20. 论文和官方实现依据

以下链接用于后续开发时核对原始方法和官方实现：

- [gsplat 官方训练器](https://github.com/nerfstudio-project/gsplat/blob/main/examples/simple_trainer.py)：SH schedule、L1+DSSIM、pose/app optimization、PPISP 和评估实现参考。
- [gsplat 官方评估](https://github.com/nerfstudio-project/gsplat/blob/main/docs/source/tests/eval.rst)：Default、AbsGrad、MCMC 和 antialiasing 的统一基准。
- [3D Gaussian Splatting as Markov Chain Monte Carlo，NeurIPS 2024](https://proceedings.neurips.cc/paper_files/paper/2024/hash/93be245fce00a9bb2333c17ceae4b732-Abstract-Conference.html)。
- [AbsGS，ACM MM 2024](https://arxiv.org/abs/2404.10484)。
- [Mip-Splatting，CVPR 2024](https://openaccess.thecvf.com/content/CVPR2024/html/Yu_Mip-Splatting_Alias-free_3D_Gaussian_Splatting_CVPR_2024_paper.html)。
- [WildGaussians，NeurIPS 2024](https://proceedings.neurips.cc/paper_files/paper/2024/hash/25c0fe7b157821dd3140727dc07461da-Abstract-Conference.html)。
- [RobustSplat，ICCV 2025](https://openaccess.thecvf.com/content/ICCV2025/papers/Fu_RobustSplat_Decoupling_Densification_and_Dynamics_for_Transient-Free_3DGS_ICCV_2025_paper.pdf)。
- [FastGS，CVPR 2026](https://openaccess.thecvf.com/content/CVPR2026/html/Ren_FastGS_Training_3D_Gaussian_Splatting_in_100_Seconds_CVPR_2026_paper.html) 与[官方实现](https://github.com/fastgs/FastGS)：多视角一致性增殖、重要度统计和裁剪的首选工程参考。
- [Taming 3DGS，SIGGRAPH Asia 2024](https://humansensinglab.github.io/taming-3dgs/)：跨视角贡献度驱动的增殖与预算控制参考。
- [PUP 3D-GS，CVPR 2025](https://openaccess.thecvf.com/content/CVPR2025/papers/Hanson_PUP_3D-GS_Principled_Uncertainty_Pruning_for_3D_Gaussian_Splatting_CVPR_2025_paper.pdf)：基于不确定性/Fisher 信息的高质量离线裁剪备选。
- [SparseGS](https://openreview.net/pdf?id=O9GMl5UJbe)：近相机半透明浮点与深度差异检测参考。
- [TIDI-GS，2026](https://arxiv.org/abs/2601.09291)：多视角一致性、空间关系、重要度与细节保护的研究参考；在官方实现和本地验证成熟前不作为直接运行时依赖。
- [SSA-3DGS，2026](https://arxiv.org/abs/2607.05598)：输入 screen-space artifact 烘焙为近相机浮点的专项研究；列入观察路线，不进入首版实现。
- [PPISP，CVPR 2026](https://research.nvidia.com/labs/sil/projects/ppisp/)。
- [Drop-In Perceptual Optimization / WD-R，2026](https://machinelearning.apple.com/research/drop-in)。
- [ResGS，ICCV 2025](https://openaccess.thecvf.com/content/ICCV2025/papers/Lyu_ResGS_Residual_Densification_of_3D_Gaussian_for_Efficient_Detail_Recovery_ICCV_2025_paper.pdf)。
- [3DGUT in gsplat](https://github.com/nerfstudio-project/gsplat/blob/main/docs/3dgut.md)。
- [BAD-Gaussians，ECCV 2024](https://arxiv.org/abs/2403.11831)。
- [DNGaussian，CVPR 2024](https://openaccess.thecvf.com/content/CVPR2024/html/Li_DNGaussian_Optimizing_Sparse-View_3D_Gaussian_Radiance_Fields_with_Global-Local_Depth_CVPR_2024_paper.html)。
- [VGGT，CVPR 2025](https://openaccess.thecvf.com/content/CVPR2025/html/Wang_VGGT_Visual_Geometry_Grounded_Transformer_CVPR_2025_paper.html)。
- [Dense-SfM，CVPR 2025](https://openaccess.thecvf.com/content/CVPR2025/html/Lee_Dense-SfM_Structure_from_Motion_with_Dense_Consistent_Matching_CVPR_2025_paper.html)。
- [Faster-GS，CVPR 2026 官方实现](https://github.com/nerficg-project/faster-gaussian-splatting)。
- [StopThePop，SIGGRAPH 2024 官方实现](https://github.com/r4dl/StopThePop)。
- [2DGS](https://arxiv.org/abs/2403.17888)、[PGSR](https://github.com/zju3dv/PGSR)、[GOF](https://github.com/autonomousvision/gaussian-opacity-fields)：独立表面/网格路线。

## 20A. M3.5 文献复核与 MCMC 参数结论（2026-08-27）

本节将本地的两组 M3.5 同输入运行与公开论文、gsplat 当前官方实现对照。它是下一轮设计依据，**不是**把论文中的参数直接宣布为产品默认。

### 20A.1 可复用的官方 MCMC 基线

gsplat 当前 `MCMCStrategy` 的公开默认值为：`cap_max=1,000,000`、`noise_lr=500,000`、`refine_start_iter=500`、`refine_stop_iter=25,000`、`refine_every=100`、`min_opacity=0.005`。官方的 Mip-NeRF 360 MCMC 脚本同样使用 1M cap，并按室内/室外场景做输入分辨率下采样。官方训练器会在改变总步数时按同一比例缩放开始、停止与间隔，而不是把绝对步数原样照搬。

| 项目 | 官方 MCMC 基线 | OOOSplat 当前 15,000 步基线 | 本轮结论 |
| --- | --- | --- | --- |
| splat cap | 1,000,000 | 1,000,000（可选更高） | cap 是显存/模型体积上限，不是质量目标；不应因为可到 3M 就提高默认值。 |
| `min_opacity` | 0.005 | 0.005 | 保持；单独提高它会误删细杆、叶片和边缘。 |
| `refine_every` | 100 | 100 | 保持。 |
| 增殖起止 | 500–25,000（官方长训练默认） | `max(500, 0.10 * total)` 至 `0.80 * total`，即 1,500–12,000 | 这是已独立验证过的延迟增殖变量；不能和 M3.5 门控同时改回官方绝对步数。后续若改总步数，按比例比较。 |
| MCMC noise | `noise_lr=500,000` | 沿用策略默认值 | 保持，除非做单变量稳定性实验。 |
| MCMC 正则 | 论文强调未使用 Gaussian 的移除正则 | `opacity=0.01`、`scale=0.01` | 保持为当前基线；它们不是反光/低纹理的万能修复。 |
| 多视角控制 | 原生 MCMC 无“至少 N 视角才新增”的参数 | 12 个固定采样视图、至少 2 个可见视图的实验门控 | 不可作为通用成熟参数；必须重设计为多证据、相对阈值策略。 |

来源：[gsplat MCMC 策略实现](https://github.com/nerfstudio-project/gsplat/blob/main/gsplat/strategy/mcmc.py)、[官方 MCMC 基准脚本](https://github.com/nerfstudio-project/gsplat/blob/main/examples/benchmarks/mcmc.sh)、[官方训练器的步数缩放](https://github.com/nerfstudio-project/gsplat/blob/main/examples/simple_trainer.py)、[MCMC 原论文](https://arxiv.org/abs/2404.09591)。

### 20A.2 为什么当前“2/12 可见性”不是论文认可的成熟参数

[MVG-Splatting](https://arxiv.org/abs/2407.11840) 使用多视角深度和光度几何一致性，并按深度/场景分布采用分位数自适应增殖；[MVGS](https://arxiv.org/abs/2410.02103) 也采用选定视图上的多视角监督与 cross-ray densification。两者都不是把某个 Gaussian 父点的原始可见次数设成固定阈值。反光、重复纹理、曝光变化和画面边缘都可能让“可见次数少”成为真实细节而非浮点。

因此，下一版门控必须先只做 shadow diagnostics，再在一个观察窗口内对候选同时要求：

1. 低多视角支持；
2. 低渲染贡献（真实 alpha/compositing contribution，而不是只看 opacity 或投影半径代理）；
3. 孤立或相对异常尺度；
4. 在可用时存在多视角深度/重投影冲突。

四项应按场景分位数和可见训练视图数自适应。只满足一项或两项时保留候选；没有可靠深度/残差的反光素材只记录诊断，不触发新增点否决或裁剪。

### 20A.3 本地结果对文献结论的复核

| 同输入素材 | 对照结果 | 结论 |
| --- | --- | --- |
| `20260826_room`（25 张、低纹理柜门） | 实验门控：PSNR `18.9480 -> 20.0291`，SSIM `0.77809 -> 0.77772`，导出 splat `850,489 -> 839,855`，训练时间约 `+0.36%`。 | 单例有潜力，但不足以设默认；需要重复运行和固定轨迹查看。 |
| `20260826_grass`（19 张、反光压力素材） | 实验门控：PSNR `12.1534 -> 11.7250`，SSIM `0.56663 -> 0.55840`，导出 splat `198,962 -> 198,174`，训练时间约 `+2%`。 | 质量门槛失败，说明硬可见性门控会伤害这类素材；不应继续在 `gamedesk` 上验证此版本。 |

`grass` 的低基线分数不能单独证明拍摄失败，但它确实是几何一致性弱、反射/视差歧义强的压力样本，不能拿来标定通用阈值。`room` 可以用于低纹理分支的后续复测，但必须固定输入、seed、训练计划、验证集和查看轨迹。

### 20A.4 当前产品决策与下一步门槛

- 保持“多视角新增点门控（实验）”默认关闭；不为 AbsGS 或 Brush 显示/复用该参数。
- MCMC 默认继续采用 1M cap、`min_opacity=0.005`、100 步 refine 和现有 10%/80% 的 15k 训练节奏；先不要叠加调高 cap、修改正则、缩短/延长增殖窗口。
- 已实现 shadow candidate diagnostics：`floater-diagnostics.json` 现在按 active Gaussian 的场景 p10/p90 输出低可见支持、低 projected-footprint proxy、空间孤立、高尺度四项信号及其交集数量；它严格为 observe-only，不改变训练、迁移、裁剪或 PLY。当前 gsplat 栅格化接口没有公开逐 Gaussian 累计 alpha-transmittance，故 `projectedFootprintProxy` 仍明确是代理值，不能伪称真实贡献。
- 已实现 shadow group ablation：对四项弱信号交集的完整候选组，训练结束后仅在诊断渲染中临时置零其 opacity，并报告相对完整渲染的平均与 p99 RGB 差异。它测量的是**该组**的实际最终画面影响，不声称可归因到单个 Gaussian；任何诊断错误都只写入状态，不影响已完成的训练或 PLY。
- 下一个代码阶段不是继续试 `2/12`、`3/12` 等阈值，而是为上述 shadow report 接入可选深度/重投影冲突，并在 fixed trajectory 上将 group ablation 与质量门槛共同复核；没有深度冲突证据的素材只允许诊断。
- 只有普通静态、低纹理、反光/动态干扰三类素材各至少 3 次、以中位 PSNR/SSIM、固定轨迹细节和导出 splat 共同通过，才允许把新策略从实验开关提升为候选预设。

### 20A.5 冰箱实拍运行记录（`20260826_fridge`）

该运行导入 73 张 Splatcam RGB/位姿和 247,509 个初始化点，73/73 注册成功；使用 gsplat MCMC、balanced、`maxResolution=1024`、15,000 步、1M cap。最终 logical Gaussian 达到 1,000,000，导出 895,871，训练 322.2 秒，峰值显存 2,649 MB，验证 PSNR `18.9077`、SSIM `0.71505`。

它启用了旧的 `multiViewDensificationGate`，因此不能与标准 MCMC 或 `room`/`grass` 的开关结果做质量归因。新的 observe-only shadow report 记录：12 个采样训练视图中，p10 支持度为 1、p10 projected-footprint proxy 为 `0.7632`、p90 归一化最大尺度为 `0.01919`；四项弱信号交集为 354 个（约占已导出 Gaussian 的 0.04%）。这只说明严格交集很小，绝不说明这 354 个就是可删除浮点。

原始素材包含低纹理冰箱门、金属/玻璃反射、室内不同深度和部分运动模糊帧。它可保留为复杂压力素材，但不用于拟合通用阈值。若需要测旧门控的净效应，唯一有效的补跑是同一输入、seed、1M cap、15k、1024、MCMC，**只关闭** `multiViewDensificationGate`；更推荐把后续新素材作为门控关闭的默认基线运行。

### 20A.6 小桌实拍基线记录（`20260826_small_desk`）

该运行是当前应采用的默认基线：35/35 Splatcam RGB/位姿成功导入，78,174 个初始化点，gsplat MCMC、balanced、`maxResolution=1024`、15,000 步、1M cap，且 `multiViewDensificationGate=false`。最终 logical Gaussian 为 431,165、导出 417,623，未触及 cap；训练 281.1 秒，验证 PSNR `18.9086`、SSIM `0.69536`。

验证图中中心的碗、包装和笔记本仍可辨识，但桌面边缘、地板和高反光/遮挡区域出现明显拖影与漂浮感。这与仅 35 张、复杂遮挡、光亮木面及包装反射的素材特性一致；不能因这些伪影就把 cap 从 1M 调高，因为本次只用了约 43% cap。shadow report 的四项弱信号交集为 535 个（约占导出 Gaussian 的 0.13%），同样只能作为后续 attribution/depth-conflict 设计的观测样本，不能直接裁剪。

该案例应固定为“默认 MCMC、门控关闭、反光和遮挡压力”基线。下一轮任何 M3.5 干预都必须以它做同输入 A/B：保持 seed、训练/验证划分、分辨率、15k 与 cap 不变，并同时复核上述四张验证图；否则不能把视觉改善归因于浮点治理。

### 20A.7 椅子实拍与 group-ablation 首次结果（`20260826_chair 2`）

该运行使用 50/50 Splatcam RGB/位姿、159,360 初始化点，gsplat MCMC、`multiViewDensificationGate=false`、balanced、1M cap、15,000 步。最终 logical Gaussian 为 878,989、导出 796,445，未打满 cap；训练 375.2 秒，验证 PSNR `21.6683`、SSIM `0.78312`，是目前这批默认 MCMC 实拍基线中质量最稳定的一组。

shadow candidate analysis 选出 851 个四项弱信号交集候选（约占导出 Gaussian 的 0.11%）。首次 `shadowGroupAblation` 已完成：12 个采样视图中，候选组整体置零的平均 RGB 绝对差均值为 `1.06e-6`，其视图级 p99 绝对差均为 0。这说明**这一完整候选组在这些采样视图的最终画面中几乎没有贡献**，并证明 group-level ablation 诊断可运行；它不证明组内每个 Gaussian 在所有未采样视图均无贡献，也不授权自动删除。

后续若做保守裁剪研究，应先在同一输入上仅对该组执行“预导出副本 + 固定轨迹 + 验证/PLY 对照”的一次性实验，并设置任何可见差异或指标回退即回滚；在此之前，默认产品和当前训练输出保持不裁剪。

### 20A.8 已实现的保守导出副本裁剪（默认关闭）

gsplat MCMC 设置中新增“保守浮点导出裁剪（实验）”，仅在新增点门控关闭时可开启。训练完成后它会：

1. 保留原参数并导出 `pre-prune.ply`；
2. 仅对已完成 group-ablation 的四项交集候选临时置零 opacity；
3. 保存 `prune-candidate.ply` 和固定验证视图的候选渲染；
4. 比较裁剪前后验证指标和 group RGB 影响；
5. 仅在 PSNR 下降不超过 `0.01 dB`、SSIM 下降不超过 `0.0001`、group mean absolute RGB delta 不超过 `1e-5` 时采用候选 PLY；否则恢复原 opacity 并导出原 PLY。

每次启用均写入 `logs/prune-manifest.json`，保留 pre/candidate 产物和接受、拒绝或失败理由。该路径不做恢复训练、不删训练 checkpoint/参数，且默认关闭；真实 `chair 2` 同输入 A/B 与固定轨迹复核尚未执行，因而不能将它表述为已验证质量收益。

### 20A.9 椅子裁剪副本首轮运行（`20260826_chair 3`）

该次运行正确启用 MCMC、关闭新增点门控并开启导出副本裁剪。它产出 1,062 个候选，`pre-prune.ply` 为 909,105 splat、`prune-candidate.ply` 为 908,043 splat。候选验证为 PSNR `21.70709 -> 21.70752`、SSIM `0.780876 -> 0.780875`，即 PSNR 不降、SSIM 下降约 `0.00000163`；group mean absolute RGB delta 为 `8.60e-7`，均在门槛内。

但该首轮在接受分支恢复内存中 opacity 参数时触发 PyTorch leaf-variable 原地写入异常，manifest 标记 `failed`，并按安全路径导出原始 PLY。这个恢复操作已改为 `torch.no_grad()`；须以相同设置重跑一次，确认 manifest 为 `accepted` 后，才能将候选 PLY 作为最终导出。首轮产生的 pre/candidate PLY 和验证渲染保留作对照，不能通过手工替换项目最终文件绕开新的验证记录。

### 20A.10 椅子裁剪副本确认运行（`20260826_chair 4`）

修复后以同类设置重新运行，manifest 为 `accepted`、`finalOutput=candidate`。本次有 856 个候选；`pre-prune.ply` 为 793,448 splat，候选/最终 `chair.ply` 为 792,592，减少 856（约 0.108%）。最终 PLY 与 `prune-candidate.ply` 的 SHA-256 完全一致，确认没有错误导出原模型。

候选验证 PSNR `21.728047 -> 21.727668`，下降 `0.000379 dB`；SSIM `0.7828166 -> 0.7827997`，下降 `0.0000169`；group mean absolute RGB delta `4.95e-7`。均严格低于当前门槛。固定验证图的人工快速复核未见由这组删除产生的可见差异。该结果只验证这一个场景、这一个极小且多证据重叠候选组的**导出副本裁剪安全性**；它不构成一般浮点消除效果或默认开启的证据。

### 20A.11 小桌压力素材裁剪确认（`20260826_small_desk 2`）

该次运行同样为 MCMC、门控关闭、裁剪开启；manifest 为 `accepted`、最终 PLY 与 `prune-candidate.ply` SHA-256 一致。530 个候选从 417,530 splat 裁至 417,000（约 0.127%）。候选验证 PSNR `18.896955 -> 18.897027`、SSIM `0.6944605 -> 0.6944614`，两项均有极小正向变化；group mean absolute RGB delta 为 `3.44e-6`，低于 `1e-5` 门槛。固定验证图快速对比未出现该候选组删除导致的新增可见差异。

这使导出副本裁剪在“较稳定椅子”和“反光/遮挡小桌”两类素材上各通过一次，但两个运行都只删除约 0.1% splat，不能宣称它已实质性解决小桌原有拖影或浮点。下一步仍须以低纹理柜门或冰箱压力素材验证安全回退/接受行为，并把开关继续保留为实验、默认关闭。

### 20A.12 冰箱低纹理/反光压力结果与门槛收紧（`20260826_fridge 2`）

该运行以当时的旧门槛 `0.05 dB / 0.001` 获得 `accepted`：351 个候选从 898,566 裁至 898,215（约 0.039%）。然而候选验证 PSNR `18.975171 -> 18.931461`，下降 `0.04371 dB`；SSIM `0.7160034 -> 0.7156383`，下降 `0.0003652`。虽然表面上没有越过旧门槛，但这已消耗其约 87% PSNR 余量，而删除比例极小；固定验证图亦可见细微局部变化。因此该场景不应被视为安全接受证据。

随后默认门槛收紧为 PSNR 下降不超过 `0.01 dB`、SSIM 下降不超过 `0.0001`，RGB 组级门槛维持 `1e-5`。`chair 4` 与 `small_desk 2` 仍会通过该更严格条件；`fridge 2` 会被拒绝并自动导出 pre-prune 原模型。既有 `fridge 2` 不应手工替换文件；如需正式产物，应按相同设置重跑，让新门槛生成完整回退记录。

### 20A.13 冰箱自动回退确认（`20260826_fridge 3`）

按收紧后的门槛重跑后，319 个候选从 900,908 形成 900,589 splat 的候选 PLY。候选 PSNR 下降 `0.004148 dB`，仍在 `0.01 dB` 内；但 SSIM 下降 `0.00010486`，刚好超过 `0.0001` 门槛，因此 manifest 正确写入 `rejected`、`finalOutput=original`。最终 `fridge.ply` 与同次 `pre-prune.ply` 的 SHA-256 完全一致，且与 `prune-candidate.ply` 不同，证明程序实际回退而不是仅修改状态。

至此，导出副本裁剪在当前门槛下已完成两类“安全接受”（椅子、小桌）和一类“安全回退”（冰箱）的真实 Splatcam 验证。功能保持实验、默认关闭；下一阶段不应提高裁剪比例，而应补充深度/重投影冲突证据和固定轨迹查看器复核。

### 20A.14 重投影一致性诊断（已接入，尚待真实素材验收）

adapter 现使用 gsplat `RGB+ED` 的期望投影深度，在最多四对固定诊断视图间执行低分辨率（8 px 网格）重投影：目标像素按期望深度反投影到世界、投影到来源相机，并仅在来源视野内、两侧 alpha 足够、来源期望深度相对误差不超过 5% 时比较真实训练 RGB。`floater-diagnostics.json` 新增 `reprojectionConsistency`，包含每对的可靠样本比例、光度残差分位数及残差大于 `0.15` 的比例。

该项严格 observe-only：渲染深度只用于过滤遮挡/不对应样本，不冒充外部深度真值，也不把场景级光度残差归因到单个 Gaussian。其失败只写入诊断状态，绝不阻止训练、候选副本或 PLY 导出。下一次 MCMC 运行必须先确认其状态为 `completed`，再讨论如何把可靠的多视图冲突作为更高阶裁剪证据。

### 20A.15 小桌重投影诊断首轮结果（`20260827_small_desk`）

`reprojectionConsistency.status=completed`，共采样四对视图。聚合可靠重投影样本的光度残差为 p50 `0.0605`、p90 `0.2451`、p99 `0.4956`，高于 `0.15` 的比例为 `24.42%`。各对可靠样本比例为 40.58%、27.25%、9.10%、24.67%；其中 `0018 -> 0023` 与 `0030 -> 0032` 的高残差比例分别为 38.62% 和 40.72%。

这证明诊断管线可以在真实 Splatcam 数据上正确完成，但也说明该反光、遮挡与复杂视差素材的光度重投影冲突较高，不能把它作为“候选无冲突”的证据，更不能将场景级高残差直接归因到候选 Gaussian。下一步应以椅子这类较稳定素材建立对照分布，再决定是否需要候选级 conflict attribution；当前裁剪仍只依赖已验证的 group ablation 与严格验证门槛。

### 20A.16 椅子重投影对照结果（`20260827_chair`）

该诊断同样完成四对视图。聚合光度残差 p50 `0.0458`、p90 `0.1949`、p99 `0.4315`、均值 `0.0798`，高残差比例为 `13.68%`；均低于小桌的 p50 `0.0605`、p90 `0.2451`、均值 `0.1028` 和 24.42%。因此椅子可作为当前“相对稳定”对照，而不是零冲突真值。

四对中 `0000 -> 0003`、`0028 -> 0031`、`0043 -> 0046` 的可靠样本比例分别为 41.99%、23.26%、10.87%；`0012 -> 0018` 为 0。后者不是训练失败，而是固定索引配对在该轨迹段缺少满足可见、视野内与深度一致条件的重叠。下一实现迭代应先按实测可靠样本/重叠度自适应选择来源视图，替换固定索引相邻配对；在此之前，场景分数应忽略零可靠对且报告其数量。

### 20A.17 自适应重投影来源选择（已接入，尚待真实素材验收）

重投影诊断不再把每个目标视图固定配对到下一个诊断索引。它先为 12 个有界诊断视图各渲染一次 `RGB+ED`，再对每个目标在其余 11 个候选来源中计算网格化的深度一致重叠数，选择重叠样本最多的来源。最终 JSON 的每对新增 `sourceSelection`、`candidateSourceCount` 与 `selectedOverlapSamples`。

这会消除“固定轨迹索引导致零可靠对”的采样伪影，但仍可能偏向较近视角；因此它只提高诊断覆盖，不构成多视角几何验证或候选裁剪授权。下一次椅子/小桌运行须检查 selected overlap 是否显著高于旧零样本对，并继续以真实验证质量门槛约束任何裁剪。

### 20A.18 椅子自适应配对验收（`20260827_chair 2`）

自适应来源选择成功完成四对，每个目标考察 11 个来源。旧固定配对中的零可靠对已消失：四对可靠比例为 42.14%、38.89%、28.34%、52.66%，选中重叠样本分别为 3,884、3,584、2,612、4,853。特别是原来的 `0012 -> 0018` 零样本对改选为 `0012 -> 0043`，获得 3,584 个可靠样本。

聚合残差为 p50 `0.04575`、p90 `0.14295`、p99 `0.32842`、均值 `0.06762`，高残差比例 `9.23%`。相较旧固定配对的 p90 `0.1949` 和 13.68%，它表明选中的对具有更好的重叠；由于选择规则已改变，这不是训练质量提升的可比 A/B 结论。该数据只验证诊断覆盖改进，候选裁剪继续不使用重投影残差。

### 20A.19 候选级重投影归因（已接入，尚待真实素材验收）

对于已有的四项弱信号候选，诊断现在将其中心投影到已通过深度一致/遮挡过滤的重投影残差网格，只在候选自身深度也与目标期望深度相符时累积残差。`reprojectionConsistency.candidateAttribution` 新增候选总数、具有可靠样本的候选数、采样视图支持分布、候选平均光度残差分布及超过 `0.15` 的候选数。

该方法是中心投影代理，不是 rasterizer 的逐像素 Gaussian ownership，也不能证明高残差来自该候选；它仅避免用整个场景的残差平均值惩罚无关候选。当前路径只写诊断，绝不改变 group ablation、严格验证门槛或候选裁剪。待椅子/小桌真实运行确认状态与分布合理后，才讨论将“低贡献 + 多证据 + 候选级冲突”作为更严格的拒绝条件，而不是作为删除条件。

### 20A.20 候选归因覆盖修正（`20260827_chair 3`）

该次椅子运行的重投影与导出副本裁剪均正常：805 个候选，候选 PLY 由 797,533 裁至 796,728，manifest `accepted`，PSNR 下降 `0.000355 dB`、SSIM 下降 `0.00000503`。但候选级归因只抽取四个目标视图，805 个低支持候选均未落入可靠网格，`candidatesWithReliableSamples=0`。这不能解释为候选无冲突，而是稀疏目标抽样对低支持候选的覆盖不足。

归因路径已修正为遍历全部 12 个诊断目标视图，并复用已缓存的 `RGB+ED` 深度/alpha 渲染，避免为每对重复栅格化。场景级对仍会自适应选择重叠最多的来源；新的 `sampledPairCount` 预期为 12。待下一次运行确认候选具有合理的可靠样本覆盖后，才可分析候选级残差分布。

### 20A.21 全视角候选归因复核（`20260827_chair 4`）

本次已实际覆盖全部 12 个目标视图，且每个目标均从其余 11 个视图中自适应选取重叠最大的来源；`sampledPairCount=12`，各对都有 1,357–5,330 个选中重叠样本。场景级聚合残差为 p50 `0.04434`、p90 `0.15812`、高残差比例 `10.86%`，因此配对覆盖本身已不再是 `chair 3` 的“四视角抽样不足”问题。

但 741 个四项交集候选的 `candidatesWithReliableSamples` 仍为 0；与此同时 group ablation 的平均 RGB delta 为 `4.33e-7`，并非严格的零影响，导出副本裁剪也按严格门槛安全接受（798,149 -> 797,408，PSNR 下降 `0.0000435 dB`，SSIM 微升）。因此不得把候选归因的全零结果解释成“候选没有重投影冲突”或“它们已被多视角确认无影响”。

现有归因仅测试 Gaussian **中心**是否同时落在目标可见表面、深度一致位置和可靠残差网格上。低贡献候选可能因中心被前景遮挡、但投影足迹边缘仍有微小贡献而全部被排除；也可能确实全部属于遮挡/不可观测成分。下一步必须先输出候选在 `inTarget`、中心深度一致、重投影可靠及三者交集四个阶段的计数，再决定是否以有限投影足迹采样替代中心代理。在得到该分解前，该字段继续为 observe-only，不能参与裁剪、densification 或 MCMC 策略判断。

### 20A.22 候选归因过滤分解（已接入，待下一次真实运行）

`candidateAttribution.coverageBreakdown` 现为每个阶段保存候选数、跨 target/source pair 的命中总数，以及每候选的视图支持分位数：

1. `inTargetFrustum`：候选中心在目标图像范围内且位于相机前方；
2. `centerDepthConsistent`：在入画前提下，中心深度与目标期望渲染深度相符；
3. `reprojectionReliable`：在前两项前提下，候选位置所在网格同时满足 target/source 可见、来源视野内及来源深度一致；
4. `residualAttributed`：实际累加光度残差的最终交集，当前与第 3 项相同，显式保留以防未来引入额外残差有效性过滤。

`pairObservationCount` 是所有候选在全部采样 pair 中的累计命中数，因此可大于 `candidateCount`；不能把它与候选数量混为同一比例。下一次同类素材必须先读取这四层分解：若第一层已有覆盖、第二层归零，优先判定中心大多被遮挡；若第三层才归零，优先检查自适应来源对、可靠网格和阈值；只有覆盖合理且有最终样本后，才可讨论有限足迹采样。该扩展仅写入诊断 JSON，不修改任何训练、裁剪、导出或接受/回退条件。

### 20A.23 过滤分解首轮验收（`20260828_chair`）

本次 MCMC 训练完成，使用 50 张 Splatcam 图像、`1m` cap，最终为 927,286 splat，训练耗时约 383 秒；未启用导出副本裁剪，因此没有 `prune-manifest.json`，不能把此 PLY 用作裁剪接受/回退证据。12 个自适应 pair 全部完成，场景级残差 p50 `0.04712`、p90 `0.17523`、高残差比例 `13.04%`。

四段覆盖给出了可解释的全零原因：1,332 个候选中仅 212 个曾在采样目标图像中入画（累计也是 212，表示每个均只命中一个 target），10 个通过中心深度一致，而 `reprojectionReliable` 和最终归因均为 0。故这次不能归因为“中心深度代理本身把所有候选错误排除”：主要限制首先是候选只在极少数诊断目标可见；剩余 10 个则在其场景级最大重叠来源对上不满足完整 source/target 可靠条件。

这也说明场景级最大重叠来源选择不等于某个候选中心的最佳可观测来源。若后续要提高候选归因覆盖，应新增一个**候选诊断专用、仍 observe-only**的来源选择：仅对已入画且深度一致的候选，从有界诊断相机中寻找其中心附近满足可见性与深度一致的来源，而不能把场景级配对或零支持视为浮点结论。在该候选专用覆盖实现并以新素材复核前，任何 MCMC/裁剪决策继续不得使用此归因。

### 20A.24 候选专用来源选择（已接入，待真实素材验收）

在保留原有场景级 `candidateAttribution` 的同时，新增 `candidateSpecificCorrespondence`。它对每个目标视图中已落在渲染表面上的候选中心，遍历其余 11 个有界诊断相机；仅接受候选在来源相机前方、入画、来源 alpha 大于 0.5 且来源期望深度相对误差不超过既有遮挡阈值的来源。多个有效来源按 `alpha - relativeDepthError` 选择最高者，再直接双线性采样两张真实训练 RGB 图像计算中心光度残差。

JSON 会记录目标表面覆盖、拥有候选专用有效来源的候选数、各自视图支持分布、中心残差分位数、高残差候选数，以及各来源被选中的候选 pair 数。它复用已有的 12 个 `RGB+ED` 缓存和诊断图像缓存，不增加 rasterization 次数；额外工作限制为 `12 × 11 × 候选数` 的中心投影/采样。该选择不能被理解为所有像素的 Gaussian ownership，也不得用于删除、densification、MCMC relocation、接受或自动回退；只有先以同类 chair 和至少一类压力素材确认覆盖与残差分布合理，才可讨论其作为更严格的诊断证据。

### 20A.25 候选专用来源选择首轮验收（`20260828_chair 2`）

新路径实际执行但没有产生候选级残差：1,226 个候选中，9 个候选中心落在某个采样目标的渲染表面，然而在其余 11 个诊断来源里均不存在同时入画、alpha 有效、且深度一致的来源，因此 `candidatesWithValidSource=0`、`selectedSources=[]`。原场景级归因同样为零。这排除了“仅因场景级最大重叠来源选错而造成全零”的解释；当前 12 个诊断相机对这些低支持候选的有效双视图覆盖不足。

本次裁剪另有独立的安全证据：manifest 为 `accepted`、候选由 909,440 至 908,214（删除 1,226，约 0.135%）；PSNR 下降 `0.00006294 dB`、SSIM 下降 `0.000000823`、group RGB delta `7.45e-7`，均在严格门槛内。最终 `chair.ply` SHA-256 与 `prune-candidate.ply` 完全一致，且与 `pre-prune.ply` 不同，确认采用候选导出而非状态误报。

下一项若继续候选重投影研究，应仅对已落在目标表面的少量候选扩展来源相机集合（例如使用全部已导入相机），并按需渲染来源的 `RGB+ED`；不能无界地对所有 candidate/所有图像做两两比较。该路径会提高训练后诊断开销，必须先记录额外渲染数与耗时，再以 chair 加一类压力素材验证覆盖收益。若仍没有双视图可见候选，应接受“该候选集合不适合中心重投影归因”的结论，转向有限投影足迹或继续依赖严格导出副本验证，而不应把零样本包装成质量信号。

### 20A.26 全导入相机的候选专用来源（已接入，待真实素材验收）

候选专用来源池现从 12 个诊断相机扩展为全部已导入、可训练的相机；目标仍限制为原有 12 个均匀诊断视图。每个目标首先仅以全体候选中心进行一次目标表面过滤；只有至少一个中心确实可见时，才遍历全来源池。这样避免 `候选数 × 图像数 × 图像数` 的无界比较，实际中心采样量受“目标表面候选数”约束。

为维持 8–24 GB 显存安全，原 12 个诊断来源的 RGB+ED 与图像缓存可复用；扩展来源按需流式 rasterize 和采样后释放，不会把全部相机的 RGB+ED 结果常驻 GPU。`candidateSpecificCorrespondence.sourcePool` 会记录总来源相机数、已缓存诊断相机数、实际额外 RGB+ED rasterization 次数和候选诊断耗时；额外 rasterization 数可因多个目标均有候选而大于“扩展相机数量”，这是流式实现的可审计时间/显存取舍。

该路径依旧只产生 `floater-diagnostics.json`。下次运行的验收条件不是“必须得到高冲突候选”，而是确认 source pool 覆盖、额外渲染数/耗时合理，并能区分“全部相机也无双视图中心”与“此前 12 相机采样不足”。无论哪一种结果，都不得自动改变 MCMC、AbsGS、裁剪、导出或回退。

### 20A.27 全导入相机首轮验收（`20260828_chair 3`）

该次 50 张 Splatcam 相机的来源池成功得到候选级对应：749 个候选中，5 个落在采样目标表面，2 个在全来源池中找到有效来源（`0013.jpg` 与 `0019.jpg` 各被选中一次）。其中心 RGB 残差 p50 `0.03808`、p90 `0.05413`，均低于当前 `0.15` 的 observe-only 高冲突阈值。由此可确认此前 12 相机来源池的零样本至少部分来自采样覆盖不足，而不是所有候选都必然没有双视图对应。

但这不是“候选组已证明无冲突”：只有 2/749 个候选具备中心级双视图样本，且中心残差不是逐像素 ownership。场景级 p50 `0.04573`、p90 `0.17442`、高残差比例 `12.94%` 也不能转嫁到其他 747 个候选。裁剪的独立验证为 accepted：795,825 -> 795,076，PSNR 下降 `0.000983 dB`、SSIM 下降 `0.0000101`、group RGB delta `7.46e-7`；最终 PLY SHA-256 与 candidate 一致、与 pre-prune 不同。

`extraRgbEdRasterizations=152` 对应 4 个含目标表面候选的目标视图各流式渲染 38 个非诊断来源，符合显存受限的设计。`elapsedMs=423` 目前只表示 Python/CUDA dispatch 时间，未在 CUDA stream 上同步，不能作为真实 wall-clock 性能数据；在据此评估“额外开销可接受”之前，计时必须在候选专用诊断前后进行 GPU synchronize 或使用 CUDA event。下一步代码修正应只提升计时可信度，不改变来源选择或任何训练/裁剪决策。

### 20A.28 候选全相机诊断的真实计时（已接入，待下一次运行）

`candidateSpecificCorrespondence.sourcePool.elapsedMs` 现于共享 12 目标重投影循环前后调用 `torch.cuda.synchronize(device)` 后测得，`timingScope` 明确写为“CUDA-synchronized wall time for the shared 12-target reprojection loop including candidate-specific source search”。这避免将异步 CUDA kernel 的 CPU dispatch 时间误作为 GPU 实际耗时。

该时间包含既有场景级 pair 重投影和新增候选专用来源搜索，不能被解读为后者单独开销；但同一训练设置下可用于比较“12 来源池”与“全导入来源池”的总诊断成本。同步只发生训练已完成后的 observe-only 诊断边界，不改变训练迭代、MCMC、裁剪验证、PLY 导出或接受/回退路径。下一次验收应同时报告 `extraRgbEdRasterizations`、同步后的 `elapsedMs` 和训练总时长，避免将后处理成本隐藏在训练阶段之外。

### 20A.29 攀岩墙压力素材验收（`20260828_climbwall`）

该素材有 63 张 Splatcam 图像，MCMC 1m cap 训练完成，最终原始模型为 906,860 splat。候选归因在 12 个目标视图中没有任何候选中心落在渲染可见表面，因此全相机来源搜索未启动：`candidatesWithTargetSurface=0`、`extraRgbEdRasterizations=0`。CUDA 同步后的共享重投影循环 wall time 为 125 ms，表明新计时字段正常工作，也证明来源池扩展不会在无目标候选时产生无谓额外 rasterization。

这不等于 614 个候选已被确认安全或无冲突；它们不适合当前中心投影归因。场景级残差仍为 p50 `0.04469`、p90 `0.17289`、高残差比例 `12.42%`，不得归因给该候选组。

裁剪 manifest 正确拒绝候选：906,860 -> 906,246 的候选虽然 PSNR 提升 `0.21749 dB`，但 SSIM 下降 `0.0016424`，远超 `0.0001` 门槛；最终 `climbwall.ply` SHA-256 与 `pre-prune.ply` 一致，且与 candidate 不同。该压力素材证明严格回退不偏向单一 PSNR 指标，当前裁剪仍应保持实验、默认关闭。

### 20A.30 小桌反光/遮挡压力素材验收（`20260828_small_desk`）

35 张图像的小桌素材首次提供了较多候选级双视图样本：565 个候选中，15 个中心落在采样目标表面，12 个在 35 相机来源池中找到有效来源；中心残差 p50 `0.06064`、p90 `0.26258`，其中 4 个超过 `0.15`。这证明全相机候选专用来源路径能够在反光/遮挡压力素材发现候选级高残差，且不是 chair 的偶发零样本。`extraRgbEdRasterizations=92`，CUDA 同步后的共享诊断耗时 270 ms。

该结果仍只覆盖约 2.1% 候选，不允许把 4 个高残差中心直接映射为逐点责任、删除依据或全组质量结论；它仅是下一阶段“保留高冲突候选、再做固定轨迹验证”的候选性证据。当前导出副本裁剪不使用该字段。

裁剪本身以独立严格门槛安全接受：417,642 -> 417,077（删除 565），PSNR 增加 `0.000926 dB`、SSIM 增加 `0.0000115`、group RGB delta `3.85e-6`。最终 `small_desk.ply` SHA-256 与 candidate 一致、与 pre-prune 不同。结合 climbwall 的 SSIM 回退拒绝，这一对压力素材验证了接受和回退两条路径；但没有证明应提高裁剪比例或默认开启。

### 20A.31 冰箱反光/低纹理压力复核（`20260828_fridge`）

73 张图像的冰箱素材场景级重投影冲突较高（p50 `0.05496`、p90 `0.28726`、高残差比例 `21.57%`），但 369 个候选中没有一个中心落在 12 个采样目标的可见表面。因此全相机候选来源没有额外渲染，CUDA 同步共享诊断耗时 115 ms；不能把高场景残差归因给任何候选，也不能据此触发裁剪。

候选 PLY 由 901,950 至 901,581。PSNR 下降 `0.005628 dB` 尚在 0.01 dB 内，但 SSIM 下降 `0.0001924` 超过 0.0001 门槛，manifest 正确为 `rejected/finalOutput=original`。最终 `fridge.ply` SHA-256 与 pre-prune 相同、与 candidate 不同。它独立复现了“PSNR 未越界也必须因 SSIM 回退”的严格安全路径，支持保持默认关闭，而不应依据小桌的单次接受扩大裁剪范围。

### 20A.32 草地小样本复核（`20260828_grass`）

该素材仅有 19 张图像，最终模型为 96,958 splat、候选仅 8 个。两个候选中心落在采样目标表面，但在全部 19 相机来源中均无有效双视图对应；9 次额外 RGB+ED 流式渲染后的同步共享诊断耗时为 103 ms。因此它不补充候选冲突分布证据，也不应与小桌的 12 个有效来源作覆盖率比较。

导出副本裁剪由 96,958 至 96,950，仅删除 8 个。PSNR 下降 `0.000621 dB`、SSIM 下降 `0.0000216`、group RGB delta `8.32e-8`，在严格门槛内；最终 `grass.ply` SHA-256 与 candidate 一致、与 pre-prune 不同。该结果只证明极小候选组的裁剪路径正确。该素材验证 PSNR 约 11.76 dB，低于其他压力素材，说明输入覆盖/重建质量本身有限，不能用于判断当前 M3.5 诊断或裁剪带来模型质量改善。

### 20A.33 游戏桌复杂遮挡复核（`20260828_gamedesk`）

46 张图像的游戏桌素材进一步复现候选级冲突可观测性：105 个候选中 5 个中心在目标表面可见，4 个在全相机来源池得到有效来源；残差 p50 `0.09215`、p90 `0.25981`，其中 1 个超过 `0.15`。68 次扩展 RGB+ED 流式渲染的 CUDA 同步共享诊断耗时为 221 ms。与小桌的 12 个有效来源/4 个高冲突点一起，这证明复杂桌面素材可以产生少量、但非零的候选级冲突证据。

此覆盖仍不足以把高冲突数接入策略：4/105 个候选的中心样本不代表其余候选，且中心观测不是像素 ownership。裁剪继续由独立固定验证门槛决定。本次 453,346 -> 453,241 的候选因 PSNR 下降 `0.001908 dB`、SSIM 下降 `0.0002471` 而被拒绝；最终 `gamedesk.ply` SHA-256 与 pre-prune 一致、与 candidate 不同。该结果和冰箱、climbwall 的回退共同说明 SSIM 门槛在复杂场景中持续发挥保护作用。

### 20A.34 自行车细结构/遮挡复核（`20260828_bike`）

43 张图像的自行车素材具有当前最高的场景级重投影冲突（p50 `0.11336`、p90 `0.35049`、高残差比例 `38.75%`）。但 154 个候选中只有 3 个取得有效双视图中心，且其残差 p50 `0.03676`、p90 `0.09072`，均低于 `0.15`。这为“高场景级残差不能直接归因到候选”增加了细结构场景证据；同时也再次显示中心级覆盖不足，不能将 3 个低残差中心推广到全部候选。

本次全相机路径为 93 次额外 RGB+ED 渲染、同步共享诊断耗时 320 ms。裁剪候选由 439,795 至 439,641，PSNR 反而增加 `0.004196 dB`，但 SSIM 下降 `0.0002638`，故严格门槛正确拒绝；最终 `bike.ply` SHA-256 与 pre-prune 一致、与 candidate 不同。该独立案例进一步确认 PSNR 改善不能覆盖结构相似度退化。

### 20A.35 高冲突候选的保留式裁剪否决（已接入，待真实复核）

小桌与游戏桌已各出现候选专用全相机中心残差超过 `0.15` 的样本。为使用这一**仅有的正向证据**而不扩大删除风险，导出副本裁剪现采用最保守的单向规则：四项弱信号候选若存在至少一次有效全相机中心对应且该次 RGB 残差超过阈值，就从本次有效裁剪 mask 中排除、保留其 opacity。它不会把未覆盖、低残差或高冲突候选加入删除集合，也不改变 MCMC、densification、训练参数或默认关闭的产品状态。

`prune-manifest.json` 新增 `allFourSignalCandidateCount`、`excludedHighConflictCandidateCount`，其中 `candidateCount` 表示实际被临时置零并送入固定验证的有效数量；`candidateSpecificCorrespondence.pruningVeto` 会保存被保留的数和理由。group ablation 仍以完整原候选组的 RGB delta 作为更严格的上界，不因排除少数点而放宽现有 PSNR/SSIM/RGB 门槛。

若重投影诊断失败，程序将把所有原候选作为冲突未知而全部保留，使有效裁剪 mask 为空，而不是退回旧的未筛选裁剪路径。这一规则的验收仅需一份具有高冲突候选的桌面素材：确认 manifest 的有效 `candidateCount` 比 `allFourSignalCandidateCount` 小、排除数匹配诊断、最终 PLY 及既有门槛仍正确接受或回退。它不证明高冲突中心就是 floaters，也不能成为增加删除比例的理由。

### 20A.36 保留式否决首轮验收（`20260828_small_desk 2`）

小桌重跑完整验证了保留式否决接线。全相机候选路径发现 11 个有效来源候选，其中 3 个平均中心残差超过 `0.15`；`candidateSpecificCorrespondence.pruningVeto.excludedCandidateCount=3` 与 manifest 的 `excludedHighConflictCandidateCount=3` 一致。四项弱信号原始候选为 545，实际置零验证的 `candidateCount=542`，精确满足 545 - 3。

裁剪后的验证仍通过：417,466 -> 416,924，PSNR 下降 `0.000844 dB`、SSIM 下降 `0.000000507`、group RGB delta `2.89e-6`；最终 `small_desk.ply` SHA-256 与 candidate 一致、与 pre-prune 不同。该运行证明保留式否决不会绕开既有固定验证门槛，也不会让高冲突点被静默删除。它只验证此保守规则的实现正确；训练存在随机性，不能将其 splat 数或指标与前一轮小桌运行做直接质量 A/B 比较。

### 20A.37 游戏桌重跑的负样本（`20260828_gamedesk 2`）

该次训练的候选分布与上一轮游戏桌不同：589 个四项候选中仅 1 个获得有效中心来源，残差 `0.00944`，没有高冲突候选，故否决数为 0、实际裁剪数仍为 589。固定验证安全接受（958,332 -> 957,743，PSNR 增加 `0.001122 dB`、SSIM 下降 `0.0000441`），最终 PLY SHA-256 与 candidate 一致、与 pre-prune 不同。

这证明零否决时新路径保持原有行为，但**不**验证“高冲突保留后仍发生质量回退”的组合分支。不能把它与上一轮 gamedesk 的 1 个高冲突/SSIM 回退做直接 A/B：请求本已固定 `seed=42`，但 gsplat/CUDA 执行仍不保证 bitwise deterministic，最终 splat 数和候选覆盖会变化。现有小桌 2 已覆盖否决+接受；旧 gamedesk/冰箱/climbwall 已覆盖无否决+拒绝。若必须追求该组合分支的运行证据，应先单独验证可用的确定性 CUDA 模式及其性能代价；否则不应靠连续随机重跑追逐偶发条件。

### 20A.38 裁剪清单的有效配置溯源（已接入，待下一次运行）

每份 `prune-manifest.json` 现新增 `effectiveConfiguration`，记录实际使用的 `seed`、`maxSteps`、配置 cap 与按显存安全收敛后的 effective cap、最大训练分辨率、batch size、增殖策略、多视角新增点门控、photometric mode 和 perceptual mode。该信息来自已经传入 adapter 的现有变量，不新增 UI、配置项或训练分支。

此字段用于首先排除“请求配置不同”造成的伪 A/B；它不承诺 CUDA/gsplat 的逐 bit 可重复性，也不应被误作质量指标。后续比较接受/拒绝或高冲突否决时，应同时核对该字段、输入图像/相机数、manifest 门槛及 PLY 哈希；若 effective configuration 不同，结果只能并列参考，不能归因给单一代码改动。

### 20A.39 固定验证视图的清单索引（已接入，待下一次运行）

adapter 已经在 `logs/quality/validation-renders` 保存裁剪前的固定验证渲染，并在 `candidateValidationRenders` 保存同一 validation indices 的候选渲染。`prune-manifest.json.fixedValidation` 现仅索引已有产物：`frameCount`、带图像名的 frame index 列表和 `originalRenders` 目录。它不增加 rasterization、文件副本、UI 或额外模型依赖。

审查裁剪时应以同一 manifest 的 `fixedValidation.frames` 为顺序，配对查看 `originalRenders` 和既有 `candidateValidationRenders` 中同名 PNG，再结合 PSNR/SSIM/RGB 门槛和 PLY 哈希判断。该索引解决“看见候选图但不确定是否同一轨迹”的审计问题；它不是自动图像质量判定，也不替代人工关键区域复核。

### 20A.40 椅子素材 AbsGS 对照验收（`20260904_chair`）

本次为 Splatcam 椅子素材的 AbsGS 运行：`balanced` 质量档、1M splat cap、seed 42、50 张输入图像。训练完成，`trainingMs=276,308`、总耗时 `281,033 ms`（约 276.3 s / 281.0 s），导出标准 PLY 为 512,215 splat、127,030,851 bytes。该运行未启用 floater pruning，因此没有 `prune-manifest.json`；本次只能作为训练策略 A/B，不作为裁剪接受证据。

固定验证指标为 PSNR `20.9557 dB`、SSIM `0.765075`。诊断共发现 414 个四项弱信号候选，5 个候选取得有效全相机来源，1 个候选出现高冲突中心残差；覆盖率仍不足以把候选级信号推广为全模型结论。

与同一 `A:\\tmp\\chair`、同为均衡/1M/seed42 的 MCMC 基线 `20260827_chair`（411.1 s 训练、416.0 s 总耗时、907,525 splat、PSNR `21.9396 dB`、SSIM `0.783376`）相比，AbsGS 快 134.9 s（约 32.4%），模型少 43.6%，但 PSNR 低 `0.9839 dB`、SSIM 低 `0.018301`。由于 MCMC/AbsGS 的增殖与 CUDA 执行不保证逐 bit 相同，这不是严格可重复性证明；方向性结论足够明确：当前均衡档下 AbsGS 是“更快、更小”的配置，不是 MCMC 的质量等价替代。后续默认质量基线继续使用 MCMC；AbsGS 仅适合作为速度/体积优先的显式选项，除非再完成参数调优并在三类素材上复核。

### 20A.41 AbsGS 三素材门禁的下一步

椅子只完成了 AbsGS 的第一类真实素材。为满足“先完成三类 MCMC/AbsGS A/B，再决定是否进入动态 mask 或进一步的 WD-R/ResGS 调参”的门禁，下一轮只补两次 AbsGS：`A:\\tmp\\small_desk`（反光/遮挡）和 `A:\\tmp\\gamedesk`（复杂细节/遮挡）。两次均固定 `quality=balanced`、`cap=1m`、`seed=42`、`photometricMode=none`、`perceptualMode=none`、`multiViewDensificationGate=false`、`floaterPruning=false`，不要同时改分辨率、步数或 `absgradGrowGrad2d`。

每次记录总耗时、训练耗时、最终 splat 数、PSNR、SSIM、峰值显存和 `floater-diagnostics.json` 的候选覆盖；分别与已有同素材 MCMC 基线（`20260828_small_desk 2`、`20260828_gamedesk 2`）对照。门禁只回答“AbsGS 的速度/体积代价是否在不同素材保持方向一致”，不把候选诊断或单次 SSIM 变化转成自动裁剪规则。两次完成后再决定是否进入下一开发阶段；在此之前不接动态软 mask，也不扩大 cap。

### 20A.42 小桌素材 AbsGS 对照验收（`20260904_small_desk`）

本次为 35 张图像的小桌素材，配置固定为 AbsGS、`balanced`、1M cap、seed 42，未启用 PPISP/WD-R、多视角增殖门控或 floater pruning。训练耗时 `569,841 ms`、总耗时 `572,610 ms`（约 569.8 s / 572.6 s），最终输出 441,246 splat、109,430,539 bytes；PSNR `18.9481 dB`、SSIM `0.685969`。

与同输入、同声明配置的 MCMC 基线 `20260828_small_desk 2` 的**裁剪前**模型（训练约 296.4 s、总耗时约 298.8 s、417,466 splat、PSNR `18.8930 dB`、SSIM `0.692696`）比较，AbsGS 慢约 283.4 s（总耗时约 +98.0%），模型多 5.7%，PSNR 仅高 `0.0551 dB`，SSIM 反而低 `0.006727`。因此 AbsGS 的“更快”只在椅子样本成立，不能作为通用结论；在该反光/遮挡素材上，它既没有体积优势，也没有结构相似度优势。

本次诊断发现 1,993 个四项弱信号候选，但 12 对重投影中没有候选中心取得目标表面或有效来源（`candidatesWithTargetSurface=0`、`candidatesWithValidSource=0`），故没有高冲突否决，也没有任何裁剪证据。场景级残差 p50 `0.04613`、p90 `0.18369` 只能说明输入/位姿存在一定不一致，不能归因到候选点。

该首轮结果受 GPU 外部任务占用影响，不能单独作为性能结论；其质量指标仍保留为同 seed 下的随机性参考。性能判断以 `20A.43` 空闲 GPU 复跑为准。三素材门禁还缺 `gamedesk` AbsGS；完成后应按“每素材分别判断”决定是否保留 AbsGS 速度档。

### 20A.43 小桌 AbsGS 空闲 GPU 复跑（`20260904_small_desk 2`）

为排除上一轮 GPU 被其他任务占用的影响，在完全相同配置（AbsGS、`balanced`、1M、seed 42、`absgradGrowGrad2d=0.0008`、其余实验开关关闭）下复跑。训练耗时降至 `262,683 ms`、总耗时 `265,160 ms`（约 262.7 s / 265.2 s），相对上一轮 572.6 s 缩短 53.7%，证明上一轮耗时不能作为 AbsGS 性能结论。日志记录 RTX 5090 D v2 总显存 24,427 MB、峰值分配 1,604 MB；没有连续 GPU utilization 采样，因此不据此宣称平均利用率。

本次输出 439,724 splat、约 104.0 MiB，PSNR `18.7751 dB`、SSIM `0.685910`。相对 MCMC `20260828_small_desk 2`（298.8 s、416,924 最终 splat、PSNR `18.8930 dB`、SSIM `0.692696`），AbsGS 快 33.6 s（约 11.2%），但模型多 5.5%，PSNR 低 `0.1179 dB`、SSIM 低 `0.006786`。因此在空闲 GPU 下 AbsGS 对该素材确有有限的速度优势，但仍不是质量等价方案，默认质量后端继续使用 MCMC。

本次诊断有 1,773 个四项弱信号候选，但目标表面和有效全相机来源均为 0；没有高冲突否决，也没有裁剪证据。该结果只修正性能判断，不改变 M3.5 的 observe-only 边界。

### 20A.44 游戏桌素材 AbsGS 对照验收（`20260904_gamedesk`）

本次为 46 张图像的游戏桌素材，配置为 AbsGS、`balanced`、1M cap、seed 42，`absgradGrowGrad2d=0.0008`，未启用 PPISP/WD-R、多视角增殖门控或 floater pruning。训练耗时 `255,674 ms`、总耗时 `257,914 ms`（约 255.7 s / 257.9 s），最终输出 545,317 splat、约 129.0 MiB；峰值显存 1,822 MB（RTX 5090 D v2，总显存 24,427 MB）。

与同输入、同声明配置的 MCMC 基线 `20260828_gamedesk 2` 裁剪前模型（训练约 367.6 s、总耗时约 369.9 s、958,332 splat、PSNR `18.4408 dB`、SSIM `0.728485`）相比，AbsGS 快 112.0 s（约 30.3%），模型少 43.1%，但 PSNR 低 `1.3114 dB`、SSIM 低 `0.049280`。这确认 AbsGS 在该复杂遮挡素材上虽然更快且更小，但质量损失已经明显超过椅子和小桌，不能作为默认质量方案。

诊断发现 177 个四项弱信号候选；其中 2 个进入目标表面，但没有取得有效全相机来源，故高冲突候选为 0，额外 RGB+ED 渲染 34 次、同步诊断耗时 173 ms。该覆盖仍不足以支持逐点归因或自动裁剪。

至此三素材 AbsGS/MCMC A/B 已完成：AbsGS 的速度优势在空闲 GPU 下方向一致，但 splat 数和质量代价随素材变化，且复杂场景质量退化显著。下一步进入 M3.5 固定轨迹/关键区域人工复核；自动裁剪、动态软 mask 和新的增殖策略继续关闭。

### 20A.45 三素材固定验证帧人工复核

对三组 AbsGS 输出与对应 MCMC 输出的固定验证帧做了同索引抽查：椅子 `0024.png`、小桌 `0020.png`、游戏桌 `0022.png`。MCMC 在椅子腿、桌沿和游戏桌细杆等边缘处整体略清晰、拖影略少；AbsGS 的差异在椅子和小桌上较小，在游戏桌细结构上更明显，与三素材 SSIM 下降方向一致。

但三组输入/重建本身都存在明显运动模糊、径向拖影和反光重影，游戏桌最严重。固定帧只能说明当前输出的可视差异，不能把全部模糊归因给训练策略，也不能替代原始帧清晰度、相机轨迹和 COLMAP 注册质量检查。现有 `floater-diagnostics.json` 仍缺少足够的候选有效来源，因此 M3.5 的自动裁剪、动态软 mask 和新增殖门控继续保持关闭。

下一步优先级调整为：先做输入清晰度/运动模糊门禁和轨迹覆盖报告，再决定是否需要动态 mask；不以 WD-R 或提高 cap 掩盖素材采集问题。

### 20A.46 Splatcam 输入质量与轨迹报告（已接入）

Splatcam 只读导入检查现复用既有 Laplacian 方差实现，写入 `import-report.json` 和检查界面的 `imageQuality`：清晰度 p10/中位/p90、低清晰度帧比例及相对分布告警。同时新增 `trajectoryCoverage`：相机路径长度、路径/包围范围比、中位相邻间隔和 p90 相邻间隔。该报告与几何投影门禁分离，清晰度告警只提示、不阻断 Brush 或 gsplat 训练，避免把未校准的绝对阈值误当成失败条件。

当前相对清晰度筛查使用“低于清晰度中位数 35% 的帧超过 35%”作为可解释的预警线；它只能发现清晰度分布中明显的低尾部，不能证明全片不存在均匀运动模糊。后续若要升级为真正阻断门禁，必须先收集不同相机、曝光和运动速度的真实样本，校准绝对阈值并验证误报/漏报，再单独接入任务创建按钮。

### 20A.47 WD-R 10k 跨素材复核

在同一 `balanced`、MCMC、`1m` cap、seed 42 配置下，WD-R 10k 已完成小桌和游戏桌两类 Splatcam 素材复核。小桌 PSNR/SSIM 为 `19.3422/0.7226`，普通 MCMC 为 `18.9262/0.6950`；游戏桌为 `18.7358/0.7481`，普通 MCMC 为 `18.4408/0.7285`。两次运行均提升 PSNR 与 SSIM，且人工固定视角复核确认细节观感更好；训练耗时约为普通 MCMC 的 `2.8–3.7` 倍，因此界面将其标为 **推荐实验**，不改变 Brush 默认后端，也不自动开启浮点裁剪或动态增殖。

### 20A.48 M3.5 多视角新增点门控 A/B（下一项）

门控实现已接入且默认关闭，下一轮使用同一 `gamedesk` Splatcam 输入与 `20260905_gamedesk 2` 作为控制组：`balanced`、gsplat、MCMC、`1m` cap、seed 42、WD-R 关闭、floater pruning 关闭；处理组只打开“多视角新增点门控”。验收必须确认 request 的 `multiViewDensificationGate=true`、`training-split.json` 写入采样训练视角、`floater-diagnostics.json` 记录 `growthAttempted/growthBlocked/lastEligibleParents`，并比较 PSNR、SSIM、最终 splat 数、训练耗时、显存和固定视角画面。该轮只验证新增点父点约束，不改变已有 Gaussian、导出裁剪或恢复训练；未通过质量门槛前保持实验开关和默认关闭。

### 20A.49 M3.5 游戏桌门控首轮 A/B

`20260905_gamedesk 3` 已确认门控配置生效：`multiViewDensificationGate=true`、12 个采样训练视角、最小支持 2 视角。相对同日关闭门控的 `20260905_gamedesk 2`，PSNR `18.4563 -> 18.5443`（+0.0880 dB）、SSIM `0.7263 -> 0.7283`、L1 `0.08476 -> 0.08417`；最终 splat `742,079 -> 579,775`（-21.9%），总耗时 `317.9 -> 289.0` 秒，峰值显存 `2122 -> 1879 MB`。固定视角复核仍需补充，且 `growthBlocked=0`、`lastEligibleParents=573,591`，所以本次只能说明父点选择约束可运行并出现正向初步结果，不能解释为已证明“阻止了大量错误增殖”或授权默认开启。下一轮应在小桌或椅子上复现同输入 A/B。

### 20A.50 M3.5 小桌门控第二轮 A/B

`20260905_small_desk 5` 使用与 `20260905_small_desk 2` 相同的 `balanced`、MCMC、`1m` cap、seed 42、WD-R 关闭配置，仅打开多视角新增点门控。门控为真且采样 12 个训练视角；PSNR `18.9262 -> 18.9431`（+0.0169 dB），L1 `0.076657 -> 0.076506`，但 SSIM `0.694973 -> 0.693784`（-0.00119）。最终 splat `417,331 -> 417,467`、总耗时 `289.3 -> 280.4` 秒、峰值显存 `1558 -> 1561 MB`，基本没有资源变化；`growthBlocked=0`。本轮应判定为**中性结果**，不能把游戏桌的正向变化推广为通用收益。下一步补椅子素材或重复小桌 A/B，仍保持默认关闭。

### 20A.51 椅子门控运行（待配对控制）

`20260905_chair` 已确认多视角门控生效：50 张输入、12 个采样训练视角、最小支持 2 视角，最终 `904,263` splat，PSNR/SSIM/L1 为 `21.7240/0.7820/0.058231`，`growthBlocked=0`。当前可找到的旧椅子 MCMC 运行使用 `absgradGrowGrad2d=0.0004`，而本次为 `0.0008`，不能作为严格控制组。因此本次只记录门控运行健康，不下质量结论；下一步必须用相同输入、质量、cap、seed、步数和 `absgradGrowGrad2d=0.0008` 关闭门控重跑，再比较固定视角与诊断字段。

### 20A.52 椅子门控严格 A/B 结论

`20260905_chair 2` 完成了与 `20260905_chair` 的严格控制：两者均为 `balanced`、MCMC、`1m` cap、seed 42、15,000 步、`absgradGrowGrad2d=0.0008`，唯一差异是门控关闭/开启。门控开启后 PSNR `21.86095 -> 21.72401`（-0.13694 dB）、SSIM `0.784187 -> 0.782003`（-0.002184）、L1 `0.057963 -> 0.058231`；最终 splat `798,454 -> 904,263`（+13.3%），训练耗时 `358.9 -> 387.0` 秒，峰值显存 `2363 -> 2553 MB`，高残差比例 `12.49% -> 14.58%`。导入/训练输入阶段耗时差异较大，不用于性能结论。结合游戏桌正向、小桌中性，本门控首版跨素材验收未通过，继续默认关闭；下一步不调高 cap，应先分析父点支持阈值和素材差异，或暂缓该策略。

### 20A.53 自动策略选择：单次训练内短分支评分

继续让用户逐个完成完整 A/B 的收益过低。下一版应在增密中点保存一次 checkpoint，并在同一任务内对候选策略做短分支：标准 MCMC、最小支持 2 视角、最小支持 3 视角。三个分支共享输入、seed、验证帧和 GPU 缓存，各运行有限步数后按固定指标自动评分，再让 Pareto 最优分支继续到目标步数；不把分支结果直接写成默认配置，先保留完整审计记录。

评分至少包含 PSNR、SSIM、L1、固定验证帧的 RGB 残差、最终 splat 预测、峰值显存和单位质量耗时。硬门槛为 SSIM 不得下降超过 `0.001`、PSNR 不得下降超过 `0.05 dB`，并优先剔除高残差比例上升且模型更大的分支。当前已有证据的自动先验为：WD-R 10k 可作为 gsplat 推荐实验；多视角新增点门控暂不推荐。该方案减少用户手工完整运行次数，但仍承认短分支不能完全替代最终轨迹复核。

### 20A.54 只读历史策略评分器（已接入）

新增 `scripts/score-gsplat-policies.ps1`，扫描已有项目目录中的 `project.json`、`validation-metrics.json`、`floater-diagnostics.json` 和 gsplat 日志，按素材分组输出候选策略、质量/资源综合分数和当前推荐。脚本只读、不修改设置、不替代固定轨迹复核；历史运行配置不完全一致时，推荐仅作为排序线索。已在 Windows PowerShell 5.1 与现有 Splatcam 运行目录上通过自检。

### 20A.55 训练 checkpoint/resume 基础（已接入，默认关闭）

`engines/gsplat/adapter/train_adapter.py` 现支持显式配置 `checkpointStep` + `checkpointPath` 保存训练状态，以及 `resumeCheckpoint` 从该状态继续。checkpoint 可落在增密前或增密后，保存 Gaussian 参数、Adam/均值学习率调度器、策略状态、多视角 bitset、PPISP 状态、CPU/CUDA/NumPy 随机状态，并写出同名 JSON 元数据。resume 会按 checkpoint 的参数形状重建 ParameterDict/optimizer；增密后的 checkpoint 只允许同策略恢复，避免把已经不同的轨迹伪装成公平 A/B。另加 `stopAfterCheckpoint` 和 `multiViewMinSupport`，可在预筛步数干净停止，并区分 gate2/gate3。

新增 `scripts/run-gsplat-policy-sweep.ps1`：从一份 request 先生成共享 warmup checkpoint，再自动运行 `mcmc`、`gate2`、`gate3` 分支，并写出 `policy-sweep.json`。新增 `scripts/run-gsplat-autoselect.ps1`：共享 warmup 后只把 MCMC/gate3 跑到预筛步数，按固定质量/体积/显存门槛选中分支，再从预筛 checkpoint 继续到目标步数。两者当前只在 adapter/脚本层启用，桌面默认请求不传这些字段，因此既不改变 Brush 默认，也不改变现有 gsplat 训练；自动选择仍需保留摘要和固定验证帧复核。

本轮仅完成代码级验证：bundled Python 可编译 adapter，Windows PowerShell 5.1 解析 sweep 脚本；真实素材运行需在能读取 Splatcam 输入目录的本机 PowerShell 中执行，不能把沙箱对 `A:\tmp` 的访问失败当作训练失败。

### 20A.56 游戏桌自动策略 sweep 首次实测（`20260905_gamedesk policy-sweep`）

使用 `20260905_gamedesk 3` 原始输入、seed 42、15,000 步，从同一个第 1,000 步 checkpoint 分支运行标准 MCMC、gate2（最小 2 视角）和 gate3（最小 3 视角）。第一次运行发现 PowerShell 5.1 的 UTF-8 BOM 与 adapter 的无 BOM JSON 读取不兼容，已修正脚本并成功重跑；随后又修正了 CUDA RNG 状态恢复的 CPU 类型转换。

结果如下：

| 分支 | PSNR | SSIM | L1 | 最终 splat | 训练耗时 | 峰值显存 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| MCMC | 18.5820 | 0.728679 | 0.084473 | 741,854 | 312.1 s | 2,171 MB |
| gate2 | 18.4429 | 0.725589 | 0.085738 | 742,794 | 312.4 s | 2,166 MB |
| gate3 | 18.3877 | 0.727346 | 0.085068 | 947,321 | 349.9 s | 2,557 MB |

本素材上 MCMC 在三项质量指标均最好；gate2 几乎没有资源收益，gate3 反而增加约 27.7% 模型体积、约 12.1% 训练时间和约 386 MB 峰值显存，同时 PSNR 下降 `0.1943 dB`。因此多视角门控首版不应进入默认策略，自动 sweep 目前只负责减少重复实验并保留候选证据，不自动切换后端或预设。完整产物在 `A:\tmp\Splatcam\20260905_gamedesk policy-sweep`，摘要为 `policy-sweep.json`。

### 20A.57 小桌自动策略 sweep 第二轮（`20260905_small_desk policy-sweep`）

使用 `20260905_small_desk 2` 的标准 MCMC request，保持 35 张输入、seed 42、15,000 步、1M cap、无 WD-R，从同一第 1,000 步 checkpoint 运行三分支。结果为：MCMC `18.9156/0.692023/0.077381`（PSNR/SSIM/L1）、417,733 splat、286.1 s、1,614 MB；gate2 `18.6580/0.689834/0.079872`、418,097 splat、288.9 s、1,619 MB；gate3 `19.0805/0.693531/0.075884`、418,082 splat、288.7 s、1,618 MB。

本素材 gate3 相对 MCMC 提升 PSNR `0.1649 dB`、SSIM `0.001508`、L1 改善 `0.001497`，模型体积仅增加约 `0.08%`，耗时增加 `2.6 s`、显存增加 `4 MB`；gate2 则明确变差。该结果满足“质量提升且资源近似不变”的候选门槛，但与游戏桌结果相反，不能直接设为默认。下一轮只需在椅子素材上比较 MCMC 与 gate3；若椅子也通过，才考虑把 gate3 做成质量档的可选策略，否则继续保持 MCMC 默认。完整产物在 `A:\tmp\Splatcam\20260905_small_desk policy-sweep`。

### 20A.58 椅子自动策略 sweep 第三轮（`20260905_chair policy-sweep`）

使用 `20260905_chair 2` 严格控制 request，保持 50 张输入、seed 42、15,000 步、1M cap、无 WD-R，只比较 MCMC 与 gate3。MCMC 为 PSNR/SSIM/L1 `21.8262/0.782293/0.058945`、最终 920,386 splat、385.0 s、2,651 MB；gate3 为 `21.9634/0.783917/0.057066`、798,741 splat、378.4 s、2,483 MB。

gate3 相对 MCMC 提升 PSNR `0.1372 dB`、SSIM `0.001624`、L1 改善 `0.001879`，模型减少约 `13.2%`，耗时减少 `6.6 s`，峰值显存减少 `168 MB`。结合小桌 gate3 的正向结果和游戏桌 gate3 的负向结果，三素材结论是**素材相关**：gate3 适合当前椅子/小桌这类输入，但不适合直接作为所有素材默认。下一步不再继续扩大 gate3 全局实验，而是设计“短 warmup 质量预筛 + gate3/MCMC 自动选择”的明确回退门槛；Brush 默认保持不变。

### 20A.59 自动预筛与续训验证（`20260906_small_desk autoselect`）

新增 `scripts/run-gsplat-autoselect.ps1`，固定门槛为 gate3 相对 MCMC 至少提升 PSNR `0.05 dB`、SSIM `0.001`、L1 不变差，且 splat/显存增幅分别不超过 `5%/10%`。真实 small_desk 测试完成了 warmup `1000` -> 两个候选预筛到 `5000` -> 选择分支继续到 `15000` 的完整流程。预筛 MCMC 为 `19.2492/0.711738/0.073735`，gate3 为 `19.2574/0.712661/0.074553`；gate3 虽 PSNR略高，但 SSIM 未达 `0.001` 且 L1 变差，因此自动回退 MCMC。最终 `selected` 请求确认使用 MCMC，完整输出 PSNR/SSIM/L1 为 `18.9177/0.685278/0.077212`。

该次还验证了增密后参数形状恢复和 optimizer/checkpoint 续训路径。最终 splat 数受 CUDA 随机性影响达到约 `969,517`，与历史完整 MCMC 运行的数量不同；这属于非 bitwise deterministic 的运行差异，不能单独视为恢复错误。

### 20A.60 自动策略接入应用（2026-09-06）

`GsplatDensificationStrategy` 新增持久化的 `auto` 值。设置页仅在 gsplat 后端下显示“自动预筛（实验）”；Brush 默认、MCMC 和 AbsGS 的现有路径不变。选择该值后，Rust 训练入口调用随应用资源打包的 `scripts/run-gsplat-autoselect.ps1`，完成共享 warmup、MCMC/gate3 预筛和胜者续训，再把 `auto-select/selected/final.ply` 交给现有 PLY 校验与发布流程。旧设置通过 serde 默认仍为 MCMC。

该接入依赖 PowerShell 和已通过 CUDA 健康检查的 gsplat 运行时；预筛阶段仍保留脚本的完整审计目录和阈值，不改变 Brush 默认，也不把 gate3 固化为全局默认。当前自动分支的实时进度仍以外层任务心跳为主，下一步若需要细粒度阶段进度，应让脚本转发每个 adapter JSONL，而不是另写一套进度协议。

### 20A.61 自动预筛运行时路径修复（2026-09-06）

首次从桌面开发构建选择 `auto` 时，任务 `20260905_bar 2` 在训练开始即退出。`logs/gsplat.log` 记录的根因是脚本位于 `src-tauri/target/debug/scripts`，旧逻辑从 `$PSScriptRoot` 推导 `$repoRoot`，因此无法找到 gsplat Python，尚未进入 CUDA 训练。Rust 现在把已经通过健康检查的 `engines\\gsplat` 根目录显式传给脚本；脚本仍兼容独立命令行调用，并在未提供参数时才回退到自身路径推导。该失败不是 CUDA 或素材质量结论。

### 20A.62 自动预筛扩展路径兼容修复（2026-09-06）

桌面构建的 PowerShell 入口可能收到 Windows 扩展路径 `\\?\\A:\\...`；PowerShell 5.1 会将其误判为无效盘符，训练尚未启动即返回退出码 `1`。Rust 调用端和 `run-gsplat-autoselect.ps1` 现在都会清理该前缀；扩展路径回归测试已能继续到请求文件读取阶段。源码 `cargo check`、67 个 Rust 单元测试、TypeScript 构建和 PowerShell 解析检查均通过。实际 CUDA 训练仍需在重启后的开发构建中复验，旧任务日志不能当作修复后的通过证据。

## 21. 尚未解决的问题

后续真实素材验收仍需确认：

1. Brush 当前是否自行完成图像去畸变，还是同样依赖已去畸变 COLMAP 输入；
2. OOOSplat 目标 PLY 查看器支持的最大 SH degree 和属性顺序；
3. PPISP canonical radiance 导出在目标查看器中的颜色表现；
4. RTX 5090D 上本地 gsplat 1.6.0 MCMC 在 2M/3M cap 的稳定边界；
5. 时间分块留出对短视频的最低可用帧数；
6. LPIPS、DISTS 权重和模型的许可证/离线打包方式；
7. 是否需要为质量评估单独保留不参与 COLMAP 的原视频帧；
8. 多视角支持的有效贡献噪声底如何按分辨率和场景自适应；
9. MCMC relocation 是否应接受同一套多视角门禁，还是首版只限制新增点；
10. 三类真实素材上低贡献、孤立度和安全删除比例的稳定分位；
11. Brush 是否能提供逐 Gaussian 可见性/贡献统计；若不能，M3.5 首版应限定为 gsplat 实验后端。

这些问题不否定已经完成的 M0 基础实现，但会影响 M3/M3.5 的阈值标定、默认预设和发布门禁。

## 22. 最终推荐

M0–M3 已部分进入实现与验证。当前最优先的新开发不是继续增加大型模型或扩大 splat cap，而是补齐以下浮点治理闭环：

```text
同输入 MCMC / AbsGS 与 cap 基线
  -> 多视角支持/贡献/深度/孤立度诊断
  -> 多视角一致性增殖门控
  -> 多条件保守裁剪与细节保护
  -> 禁止增殖的 1,000–3,000 步恢复训练
  -> 严格指标、关键 crop 和固定轨迹复验
  -> 不达标自动回退 pre-prune 标准 PLY
```

整体实施顺序更新为：

```text
完成 MCMC / AbsGS 三类素材 A/B
  -> M3.5 多视角浮点抑制与自动回退
  -> 动态区域软 mask 完整接入
  -> WD-R
  -> ResGS coarse-to-fine
```

M3.5 首版优先在 gsplat 实验后端落地，因为当前 adapter 可直接访问 rasterization 统计和 Gaussian 参数；Brush 保持产品默认与回退，不在缺少等价逐点统计接口时强行复刻。该方案保持标准 Gaussian PLY，不新增大型模型权重，并直接针对“高于 opacity 阈值但缺乏多视角证据”的浮点。3DGUT、BAD-Gaussians、深度正则、VGGT/Dense-SfM、GLOMAP 与 LightGlue 继续作为有明确触发条件的恢复或研究路线，而不是默认增加运行时、安装体积和失败面。
