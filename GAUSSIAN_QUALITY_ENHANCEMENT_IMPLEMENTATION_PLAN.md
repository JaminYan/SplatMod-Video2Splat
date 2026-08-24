# OOOSplat Gaussian 模型质量提升实施文档

> 文档版本：1.0  
> 编写日期：2026-08-24  
> 适用项目：`A:\project\splat`  
> 文档性质：后续开发、实验、验收和回退依据  
> 当前状态：仅完成代码审计与技术研究，本文所列质量算法尚未实施  

## 1. 技术结论

OOOSplat 下一阶段的质量提升不应从替换 COLMAP 或直接叠加最新论文开始，而应先把实验 gsplat 后端从“能够生成标准 PLY 的最小训练器”补齐为“可与 Brush 公平比较的完整 3D Gaussian Splatting 质量基线”。当前 gsplat adapter 在相机投影、Gaussian 初始化、视角相关颜色、损失函数和验证集设计上都存在基础缺口；这些缺口足以掩盖 MCMC、AbsGS、PPISP 或感知损失带来的真实收益。

推荐的主线顺序为：

1. 修正相机、初始化、SH、损失和验证基线；
2. 接入 PPISP 光度补偿；
3. 接入轻量动态区域降权和延迟增殖；
4. 对 MCMC 与 AbsGS 进行同输入 A/B；
5. 实验 WD-R 感知损失；
6. 实验 ResGS 式粗到细训练与 residual split；
7. 仅在检测到对应问题时启用 3DGUT、BAD-Gaussians 或深度正则；
8. 最后再改善查看器的抗锯齿和运动一致性。

这条路线的核心约束是：

- Brush 在完整质量基准完成前继续作为默认训练后端；
- gsplat 必须保持显式安装、版本、CUDA 能力和失败原因；
- 默认产物仍是普通第三方查看器可读取的标准 Gaussian PLY；
- 训练时附加模块不能被误认为已经写入 PLY；
- 不用更大的 splat cap、更多训练步数或更大的文件代替质量证明；
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

## 21. 尚未解决的问题

开始 M0 开发前还需在真实素材上确认：

1. Brush 当前是否自行完成图像去畸变，还是同样依赖已去畸变 COLMAP 输入；
2. OOOSplat 目标 PLY 查看器支持的最大 SH degree 和属性顺序；
3. PPISP canonical radiance 导出在目标查看器中的颜色表现；
4. RTX 5090D 上本地 gsplat 1.6.0 MCMC 在 2M/3M cap 的稳定边界；
5. 时间分块留出对短视频的最低可用帧数；
6. LPIPS、DISTS 权重和模型的许可证/离线打包方式；
7. 是否需要为质量评估单独保留不参与 COLMAP 的原视频帧。

这些问题不会阻止先完成 M0 的相机、初始化、SH、损失和验证框架，但会影响后续默认预设和发布门禁。

## 22. 最终推荐

最合理的第一批开发不是实现新的大型模型，而是完成以下闭环：

```text
去畸变相机契约
  + 标准 kNN/opacity/LR 初始化
  + SH degree 0 -> 3
  + L1 + DSSIM
  + 时间分块验证
  + PSNR/SSIM/LPIPS/DISTS
  + 标准 PLY 跨查看器验收
```

在这一闭环通过后，优先实施：

```text
PPISP
  -> 动态区域软降权与延迟增殖
  -> MCMC / AbsGS A/B
  -> WD-R
  -> ResGS coarse-to-fine
```

这套组合同时覆盖视频采集最常见的光度不一致、动态干扰、细节不足和 densification 缺陷，并且大部分改动仍可导出标准 Gaussian PLY。3DGUT、BAD-Gaussians、深度正则、VGGT/Dense-SfM、GLOMAP 与 LightGlue 应继续作为有明确触发条件的恢复或研究路线，而不是默认增加运行时、安装体积和失败面。
