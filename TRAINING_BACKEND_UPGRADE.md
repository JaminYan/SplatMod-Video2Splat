# OOOSplat 训练后端加速改造设计

> 状态：M1 已实施；M2 隔离运行时、CUDA smoke、adapter 与资源保护已实施；完整质量/速度基准尚未完成  
> 日期：2026-08-22  
> 适用版本：SplatMod-Video2Splat 0.47.x
> 目标平台：Windows 10/11 x64，NVIDIA CUDA；现有 Brush/WGPU 路径继续支持  
> 上游参考：[speedy-splat](https://github.com/j-alex-hanson/speedy-splat)、[gsplat](https://github.com/nerfstudio-project/gsplat)

## 1. 文档结论

本次改造不直接用 Speedy-Splat 替换 Brush，而是把当前单一 Brush 训练路径改造成可切换的训练后端：

1. `Brush` 继续作为默认、稳定和跨显卡回退后端。
2. 新增 `gsplat` CUDA 实验后端，仅在运行时、显卡和引擎健康检查全部通过时开放。
3. 先使用 gsplat 自带的 Default/MCMC Strategy 建立同数据基准。
4. 只有基准证明 gsplat 在目标设备上明显快于 Brush，才把它提升为可推荐选项。
5. Speedy-Splat 只用于理解 AccuTile 和训练中裁剪方法；不得直接复制其受非商业许可证约束的源代码。
6. 若后续需要 Speedy-Splat 式裁剪，应依据论文在 Apache-2.0 代码中独立实现，并先完成许可证和潜在专利审查。

本设计优先追求可验证的端到端加速，而不是只比较训练迭代速度。

## 实施记录（2026-08-22）

- 已完成 M1 的 Rust 基础：`TrainingBackend`、统一训练请求/结果、标准 `work/training-input`、Brush 适配器迁移和设置 schema 6（默认 `brush`）。0.47 继续保持旧设置迁移到 Brush，用户之后可显式选择 gsplat。
- 已将后端、训练输入完成状态和阶段计时字段写入项目元数据/状态；PLY 发布前现在校验标准 Gaussian 属性与二进制布局长度。
- `gsplat` 在未安装隔离 Python/CUDA 运行时或未通过三级健康检查时明确禁用并提示切回 Brush；没有静默回退。当前开发机三级检查已通过，但产品结论仍须完整基准。
- 已具备固定 Python/PyTorch/gsplat 版本矩阵、`sm_120` CUDA smoke 与可复现 adapter；仍缺三组同输入的 Brush/gsplat 完整质量与耗时基线，因此不能宣称 gsplat 优于 Brush。

### M2 实验记录（2026-08-22）

- 已在 `engines/gsplat/python` 创建隔离 Python 3.12 环境；没有修改系统 Python。
- 锁定实验矩阵：PyTorch `2.11.0+cu130`、CUDA Toolkit `13.2.51`、`TORCH_CUDA_ARCH_LIST=12.0`、gsplat `1.6.0` commit `90d7b4b349e379ccf9ee6a8cef76aa40f48bb32e`。详细记录见 `engines/gsplat/adapter/version.json`。
- 核心扩展已在 RTX 5090D `sm_120` 上完成 Python 导入和最小 CUDA rasterization smoke；构建仅包含 3DGS、Adam 和 relocation，未构建与训练无关的 experimental renderer。
- 上游 `examples/simple_trainer.py` 依赖另一套会替换当前 PyTorch 的示例依赖矩阵，不能作为产品 runtime 的直接入口；产品 adapter 必须保持最小依赖并自行实现 JSONL、COLMAP 输入和 PLY 导出契约。
- 最小 adapter 已通过合成 COLMAP fixture：2 步 CUDA 优化、JSONL 事件、`binary_little_endian` Gaussian PLY 的属性与字节长度检查均通过。该结果不替代真实项目或 Brush 查看器验收。
- 设置界面的 gsplat 选项已接入三级运行时门禁：隔离 Python、adapter/manifest 文件、以及最小 CUDA rasterization 必须同时成功；Brush 仍保持默认后端。
- 真实项目 `training-input` 的 2 步诊断已通过：COLMAP `SIMPLE_RADIAL` 内参可读取，5,935 个稀疏点完成 CUDA 优化并导出 PLY。完整步数训练和 Brush 查看器验收仍待执行。
- adapter 已接入原生 gsplat `MCMCStrategy`，在相同真实输入的 600 步测试中从 5,920 个初始点增长到 6,216 个 splats；默认每步批量渲染 4 个视图。图片完整集保留在系统内存，GPU 仅使用按显存（总显存的约 1/12，范围 256--1,024 MB）约束的 LRU 缓存，避免 8 GB 显卡在训练开始前就被输入图像占满。
- MCMC 的实际 cap 同时受用户选择和显存安全上限约束；设置可选自动安全、100 万、200 万或 400 万。cap 仅是硬天花板，绝不代表质量目标：MCMC 每轮会按当前数量约增加 5%，因此 adapter 采用论文/gsplat 的 opacity 与 scale 正则（默认各 `0.01`）、在训练后 20% 停止增殖/注噪，并在固定验证视图连续四次没有至少 0.5% 改善时提前冻结增殖。导出时会移除 opacity 不高于 `0.005` 的无效 splat。两个后端的 splat 数并非相同质量的定义，仍须在相同输入上完成三组质量/耗时基准后才可声称等效。
- 实时进程仅显示阶段、进度、训练 JSONL 指标、心跳和可操作错误；COLMAP `I...` 焦距/相机等 INFO 行仍写入项目日志，但不再刷入界面。
- PyTorch CUDA smoke 通过：驱动 610.88 下识别 RTX 5090 D v2 / compute capability 12.0，并完成 GPU Tensor 运算。
- gsplat 源码 wheel 编译已通过 VS 环境检查并进入 CUDA 编译，但最终失败；日志保存在 `engines/gsplat/gsplat-build-retry.stdout.log`。在完成 `import gsplat` 和最小 rasterization smoke 前，后端继续禁用。

## 2. 背景与现状

### 2.1 当前流水线

```text
视频
  -> FFprobe
  -> FFmpeg 有界候选抽帧
  -> pHash 去连续近重复 + Laplacian 清晰帧选择
  -> COLMAP 特征提取 / 匹配 / 稀疏重建
  -> 打包 work/brush/dataset.zip
  -> Brush GPU 训练
  -> 校验 PLY
  -> 发布 <视频文件名>.ply
```

当前训练调用集中在：

- `src-tauri/src/engines/brush.rs`：Brush 数据集打包、CLI 参数和输出文件约定；
- `src-tauri/src/pipeline/runner.rs`：训练阶段编排、进度、状态和最终发布；
- `src-tauri/src/presets/quality.rs`：7k / 15k / 30k 训练步数、最大分辨率和 splat 上限；
- `src-tauri/src/reconstruction/ply.rs`：PLY 魔数、头部和顶点数量检查；
- `engines/manifest.json`：随应用分发的原生引擎及哈希。

### 2.2 当前数据契约

Brush 输入 ZIP 内部布局已经与标准 COLMAP 数据集接近：

```text
dataset.zip
├── images/
│   └── *.jpg
└── sparse/0/
    ├── cameras.bin
    ├── images.bin
    └── points3D.bin
```

gsplat 的 COLMAP trainer 需要同样的目录结构，但不能直接使用当前 Brush ZIP。因此数据准备层应先生成一个标准目录，再由不同后端决定是否压缩。

### 2.3 本机历史任务证据

以下数字来自本机项目元数据、日志中的 `elapsed_ms` 和文件时间戳。时间戳数据只能用于近似判断，不能替代新增的阶段计时器。

| 样本 | 总耗时 | 图像数 | 训练耗时 | 训练占比 | 结论 |
| --- | ---: | ---: | ---: | ---: | --- |
| 旧 balanced 任务 | 2812.9 秒 | 631 | 147.3 秒 | 约 5.2% | 旧全帧抽取和 COLMAP 是主要瓶颈 |
| high 任务 | 453.5 秒 | 64 | 约 344.5 秒 | 约 76% | 训练是主要瓶颈 |
| balanced 任务 | 122.0 秒 | 42 | 约 65.1 秒 | 约 53% | 训练和 COLMAP 都值得优化 |

因此，训练后端改造对当前 1 / 2 / 4 FPS 新流水线有价值，但必须同时记录 COLMAP、数据准备和 PLY 发布耗时，不能用训练 FPS 代替端到端结果。

### 2.4 本机运行环境

设计时确认的目标机器：

| 项目 | 当前状态 |
| --- | --- |
| GPU | NVIDIA GeForce RTX 5090 D v2，约 24 GB |
| 驱动 | 610.88 |
| Compute Capability | 12.0（Blackwell / `sm_120`） |
| CUDA Toolkit | 13.2 |
| 系统 Python | 3.12 |
| 系统 PyTorch | 2.3.1 CPU-only，不可用于 gsplat CUDA |
| Brush | 0.3.0，已经使用 MCMC 类训练方式 |

gsplat 必须使用隔离运行时，不能修改或依赖系统 Python。

## 3. 上游能力与边界

### 3.1 Speedy-Splat

Speedy-Splat 的加速分为两部分：

- SnugBox / AccuTile：更精确地确定 Gaussian 与瓦片的相交范围，减少无效像素工作；
- Efficient Pruning：在 densification 期间执行 Soft Pruning，在 densification 后执行 Hard Pruning，减少训练中和最终 PLY 中的 Gaussian 数量。

论文报告的是相对 Inria 3DGS 的平均结果，不是相对 Brush：

- 训练速度约 `1.4x`；
- 渲染速度约 `6.71x`；
- primitive 数量约减少 `10.6x`。

限制：官方代码继承 Inria Gaussian Splatting 的非商业研究/评估许可证。当前 OOOSplat 是 Apache-2.0 项目，不能把 Speedy-Splat 源代码直接复制或打包进产品。

### 3.2 gsplat

gsplat 是 Apache-2.0 的 PyTorch/CUDA Gaussian Splatting 库，适合作为可选训练后端。相关能力包括：

- 读取 COLMAP 数据集；
- Default、AbsGrad、MCMC 等 densification 策略；
- CUDA rasterization；
- 标准 Gaussian PLY 导出；
- Windows 构建和部分预编译 wheel；
- 新版主分支已引入 AccuTile、CUDA 13 和 MCMC CUDA 路径优化。

风险：

- 上游基准主要比较 Inria 3DGS，不能证明 gsplat 比 Brush 0.3 更快；
- gsplat 的完整示例 trainer 依赖较多，不适合原样塞进桌面包；
- RTX 50 系 `sm_120` 支持仍在演进；
- Blackwell 上高 Gaussian 数 MCMC 存在上游非法内存访问报告；
- PyTorch、CUDA、gsplat 和 Python ABI 必须形成固定兼容矩阵。

## 4. 改造目标与非目标

### 4.1 目标

1. 在不破坏 Brush 的情况下新增可选 gsplat CUDA 后端。
2. 统一训练输入、输出、进度、取消和错误契约。
3. 用相同 COLMAP 结果公平比较 Brush 与 gsplat。
4. 保证任一实验后端失败时可安全回退到 Brush。
5. 让最终 PLY 继续通过现有发布流程和 Brush 查看器。
6. 精确记录每阶段时间、显存峰值、splat 数和输出大小。
7. 为后续独立实现 Speedy-Splat 式裁剪保留 Strategy 扩展点。

### 4.2 非目标

- 不在第一阶段移除 Brush。
- 不在第一阶段默认启用 gsplat。
- 不直接复制 Speedy-Splat 源码。
- 不把 Nerfstudio 整套框架作为桌面应用依赖。
- 不在第一阶段实现多 GPU。
- 不改变 FFmpeg、图像筛选和 COLMAP 的算法行为。
- 不承诺论文基准可在视频采集数据或 RTX 5090D 上复现。

## 5. 目标架构

```text
                         +----------------------+
视频 -> 帧筛选 -> COLMAP | StandardColmapDataset|
                         +----------+-----------+
                                    |
                      +-------------+-------------+
                      |                           |
              +-------v-------+           +-------v--------+
              | Brush Adapter |           | gsplat Adapter |
              | zip + CLI     |           | Python + CUDA  |
              +-------+-------+           +-------+--------+
                      |                           |
                      +-------------+-------------+
                                    |
                           final.ply.tmp
                                    |
                        PLY 校验 / 原子发布 / 元数据
```

核心原则：流水线只依赖统一的训练请求和统一的临时 PLY，不感知后端内部实现。

## 6. Rust 侧接口设计

### 6.1 后端枚举

建议新增：

```rust
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TrainingBackend {
    #[default]
    Brush,
    Gsplat,
}
```

设置文件 schema 从 3 升到 4，并增加：

```json
{
  "schemaVersion": 4,
  "trainingBackend": "brush"
}
```

旧设置迁移时必须填充 `brush`，不能因为检测到 CUDA 就自动切换。

### 6.2 统一训练请求

```rust
pub struct TrainingRequest {
    pub dataset_root: PathBuf,
    pub output_directory: PathBuf,
    pub total_steps: u32,
    pub max_resolution: u32,
    pub max_splats: u32,
    pub seed: u64,
    pub log_path: PathBuf,
}
```

后端返回：

```rust
pub struct TrainingOutput {
    pub candidate_ply: PathBuf,
    pub backend: TrainingBackend,
    pub elapsed_ms: u64,
    pub peak_vram_mb: Option<u64>,
    pub reported_splats: Option<u64>,
}
```

### 6.3 适配器边界

推荐在 `src-tauri/src/engines/` 下增加：

```text
engines/
├── brush.rs
├── gsplat.rs
├── training.rs
└── mod.rs
```

`training.rs` 负责：

- `TrainingBackend`；
- `TrainingRequest` / `TrainingOutput`；
- 后端分发；
- 统一健康检查结果；
- 不包含 Brush 或 Python 的私有参数。

`brush.rs` 继续负责：

- 从标准目录生成 `dataset.zip`；
- Brush CLI 参数；
- Brush 输出文件名差异兼容。

`gsplat.rs` 负责：

- 找到隔离 Python 解释器和 adapter；
- 生成 JSON 配置文件；
- 通过现有 `ProcessManager` 启动和取消进程；
- 解析 JSONL 进度；
- 找到并返回最终 PLY。

## 7. 数据准备改造

### 7.1 标准数据目录

新增统一目录：

```text
work/training-input/
├── images/
│   └── *.jpg
└── sparse/0/
    ├── cameras.bin
    ├── images.bin
    └── points3D.bin
```

数据准备规则：

1. `images/` 优先使用 Windows 硬链接，失败再复制。
2. `sparse/0/` 文件体积较小，直接复制。
3. 构建完成后校验三份 COLMAP 二进制文件及至少一张 JPEG。
4. 用临时目录构建，完成后原子重命名为 `training-input`。
5. 输入目录构建成功后，Brush 才从它生成 ZIP。
6. gsplat 直接读取该目录，不重复解压 ZIP。

这样可以避免当前数据只为 Brush 服务，也避免 gsplat 每次训练先解压一份大 ZIP。

### 7.2 缓存与恢复

`state.json` 增加：

```json
{
  "trainingInputComplete": true,
  "trainingBackend": "gsplat",
  "trainingComplete": false
}
```

恢复规则：

- COLMAP 模型和训练输入均完成：允许只重跑训练；
- 切换 Brush/gsplat：复用同一 `training-input`；
- 后端或预设改变：不得复用旧训练输出；
- 最终 PLY 已发布：不得被实验重试静默覆盖。

## 8. gsplat 运行时设计

### 8.1 隔离目录

建议布局：

```text
engines/gsplat/
├── python/python.exe
├── Lib/site-packages/
├── adapter/train_adapter.py
├── adapter/version.json
├── LICENSES/
└── runtime-manifest.json
```

不得使用 PATH 中的 `python.exe`，也不得向系统 Python 安装包。

### 8.2 版本策略

首个实验包应锁定：

- Python 小版本；
- PyTorch 精确版本；
- PyTorch CUDA wheel 系列；
- gsplat 精确 commit；
- `TORCH_CUDA_ARCH_LIST=12.0`；
- adapter 版本；
- 所有下载归档和关键文件 SHA-256。

不能直接跟随 gsplat `main` 自动更新。上游升级必须重新跑完整基准和 PLY 兼容测试。

### 8.3 引擎清单

`engines/manifest.json` schema 升级时新增可选引擎条目，至少记录：

- `optional: true`；
- Python/PyTorch/gsplat 版本；
- CUDA runtime 系列；
- 支持的 compute capability；
- 下载地址和 SHA-256；
- 许可证文件；
- 未安装时的 UI 提示；
- 安装后的健康检查命令。

gsplat 必须保持可选下载，不能让默认安装包无条件携带数 GB Python/CUDA 依赖。

## 9. Python Adapter 契约

### 9.1 启动方式

```powershell
engines\gsplat\python\python.exe `
  engines\gsplat\adapter\train_adapter.py `
  --config <project>\work\gsplat\request.json
```

配置示例：

```json
{
  "schemaVersion": 1,
  "dataDir": "A:\\tmp\\project\\work\\training-input",
  "resultDir": "A:\\tmp\\project\\work\\gsplat",
  "outputPly": "A:\\tmp\\project\\work\\gsplat\\final.ply.tmp",
  "strategy": "mcmc",
  "maxSteps": 15000,
  "maxResolution": 1024,
  "maxSplats": 2000000,
  "seed": 42,
  "saveCheckpoints": false
}
```

### 9.2 标准输出

adapter 的 stdout 只输出一行一个 JSON 对象；第三方库的普通日志写 stderr。

```json
{"event":"ready","backend":"gsplat","device":"RTX 5090 D v2","vramMb":24455}
{"event":"progress","step":500,"total":15000,"loss":0.0821,"splats":137200}
{"event":"metric","name":"peakVramMb","value":3812}
{"event":"export","path":"...\\final.ply.tmp","splats":864221}
{"event":"completed","elapsedMs":48721}
```

失败格式：

```json
{"event":"error","code":"cuda_unsupported_arch","message":"..."}
```

约束：

- 路径通过 JSON 传递，避免 PowerShell/命令行转义问题；
- stdout 中无法解析的行不得推进进度；
- 进程退出码非零即失败，即使曾输出 `export`；
- 进程退出码为零但 PLY 缺失仍然失败；
- 取消必须由 Windows Job Object 终止整个 Python/CUDA 子进程树。

## 10. 预设映射

第一轮基准建议使用保守映射：

| OOOSplat 档位 | 步数 | 最大分辨率 | gsplat 策略 | cap |
| --- | ---: | ---: | --- | ---: |
| 快速 | 7,000 | 512 | MCMC | 1,000,000 |
| 均衡 | 15,000 | 1024 | MCMC | 2,000,000 |
| 精细 | 30,000 | 1920 | MCMC | 3,000,000 |

说明：

- 第一轮不使用 5M 以上 cap，降低 Blackwell MCMC 风险；
- 不默认启用 `packed`，因为其主要收益是显存而非速度；
- 不默认启用实验性 `sparse_grad`、`visible_adam` 或 antialiasing；
- 不把 Brush 与 gsplat 的“同名参数”视为完全等价；
- A/B/C 预设后续应改成独立的 `TrainingPreset`，而不是继续命名为 `BrushTrainingPreset`。

## 11. Speedy-Splat 式裁剪扩展

### 11.1 实施前提

必须同时满足：

1. gsplat 基线已经稳定运行；
2. 端到端基准证明训练仍是主要瓶颈；
3. 完成许可证和潜在知识产权检查；
4. 采用论文级 clean-room 实现，不复制 Speedy-Splat 源文件；
5. 能计算保留集质量指标。

### 11.2 策略接口

建议实现为 gsplat Strategy 扩展，不修改流水线协议：

```text
GsplatTrainer
  -> rasterization
  -> loss.backward
  -> SpeedyPruneStrategy.step_post_backward
       -> score accumulation
       -> soft prune
       -> hard prune
```

第一轮参数不得直接照搬论文的激进值。建议：

| 阶段 | 起始策略 |
| --- | --- |
| Soft Pruning | 6k 后每 3k 步裁剪 30% |
| Hard Pruning | densification 结束后每 3k 步裁剪 10% |
| 最小保留数 | 不低于 COLMAP 初始点数的安全倍数 |
| 回退条件 | loss 激增、验证 PSNR/SSIM 超阈值下降或输出出现空洞 |

通过 A/B 基准后再逐步接近论文的 80% / 30%。

## 12. PLY 输出契约

所有训练后端必须生成临时文件：

```text
work/<backend>/final.ply.tmp
```

发布前至少验证：

1. 文件存在且大小大于合理下限；
2. `ply` 魔数；
3. `format binary_little_endian 1.0`；
4. `element vertex N` 且 `N > 0`；
5. 具备标准 3DGS 属性：位置、法线或兼容占位、SH、opacity、scale、rotation；
6. 按 header 计算的单顶点字节数与文件长度一致；
7. Brush 查看器可以打开；
8. 只有全部通过后才原子发布到项目根目录。

当前 `inspect_gaussian_ply()` 只读取顶点数，需要在接入第二后端前增强，避免把普通点云 PLY 误判成 Gaussian PLY。

## 13. 健康检查与自动回退

### 13.1 gsplat 健康检查

健康检查分三级：

1. 文件检查：Python、adapter、manifest、关键包存在且哈希正确；
2. 导入检查：能导入 torch 和 gsplat；
3. CUDA smoke：分配小 Tensor，执行一个最小 rasterization 或官方最小 CUDA op。

UI 只有在三级全部通过后才显示“gsplat 可用”。

### 13.2 回退策略

| 情况 | 行为 |
| --- | --- |
| gsplat 未安装 | 设置中禁用 gsplat，提供可选下载 |
| `sm_120` 不支持 | 不启动训练，提示改用 Brush |
| CUDA OOM | 保留 COLMAP 和训练输入，允许降低预设或切回 Brush |
| CUDA illegal access | 标记此次后端失败，当前任务不得自动无限重试 |
| gsplat 无 PLY | 任务失败，不发布半成品 |
| 用户手动切回 Brush | 复用 `training-input`，不重跑 COLMAP |

自动回退不得静默发生。应在事件日志和最终项目元数据中记录请求后端、实际后端和失败原因。

## 14. 可观测性

### 14.1 阶段计时

项目元数据建议增加：

```json
{
  "timings": {
    "probeMs": 0,
    "extractMs": 0,
    "selectMs": 0,
    "colmapFeaturesMs": 0,
    "colmapMatchingMs": 0,
    "colmapMappingMs": 0,
    "trainingInputMs": 0,
    "trainingMs": 0,
    "plyValidationMs": 0,
    "totalMs": 0
  }
}
```

### 14.2 训练指标

每个后端至少记录：

- backend 名称和版本；
- GPU、驱动、CUDA runtime；
- 步数和有效分辨率；
- 输入图像数和 COLMAP 点数；
- 每秒迭代数；
- 训练峰值显存；
- 最终 splat 数；
- PLY 文件大小；
- 训练和端到端耗时；
- 是否发生回退。

不能把 UI 心跳、进程退出码 0 或 PLY 存在单独当作质量和加速证据。

## 15. UI 与设置

设置页新增“训练后端”：

- `Brush（稳定，推荐）`；
- `gsplat CUDA（实验）`。

行为要求：

- 初次升级仍选择 Brush；
- gsplat 未安装或健康检查失败时选项保持可见但不可选择；
- 显示下载大小、CUDA 要求和实验性提示；
- 任务开始后锁定后端，运行中不可切换；
- 历史任务详情显示实际后端；
- 中英文文案都跟随当前 UI 语言状态；
- 错误文案必须区分未安装、不兼容、OOM、进程崩溃和 PLY 校验失败。

## 16. 安全、分发与许可证

1. 所有运行时下载必须固定 HTTPS URL 和 SHA-256。
2. 下载后先解压到临时目录，校验后再原子安装。
3. 不运行来自项目目录或用户 PATH 的任意 Python。
4. 配置中的输入、输出路径必须在已注册项目目录内。
5. gsplat 运行时要补充 Apache-2.0 声明以及 PyTorch、CUDA runtime、Python 和所有二进制依赖的第三方 notices。
6. Speedy-Splat 源码及其 Inria 依赖不得进入 `engines/`、安装包、源码派生文件或补丁。
7. 生成 PLY 的权利说明继续遵守 `GENERATED_OUTPUTS.md`，但新增后端后应重新审查第三方 notices。

## 17. 实施阶段

### M0：建立可比较基线

- 为现有 Brush 流水线增加精确阶段计时；
- 固定三个本机样本：42 图、64 图和一个 150–300 图样本；
- 记录 Brush 三次运行的中位数；
- 增加训练后端、版本、显存和 PLY 属性元数据；
- 不改变训练行为。

交付条件：可以回答“时间到底花在哪一阶段”。

### M1：训练接口解耦

- 新增 `TrainingBackend` 和统一请求/输出；
- 把 Brush 调用迁到统一接口；
- 生成 `work/training-input`；
- Brush 结果与当前版本保持一致；
- 设置 schema 迁移但 UI 暂不开放 gsplat。

交付条件：只使用 Brush 时功能和输出无回归。

### M2：gsplat 独立实验包

- 在应用外构建隔离 Python/PyTorch/gsplat 环境；
- 完成 `sm_120` CUDA smoke；
- 编写最小 adapter 和 JSONL 协议；
- 在固定 COLMAP 输入上导出 PLY；
- 用 Brush 查看器打开；
- 不修改默认安装包。

交付条件：同一 `training-input` 可分别产出 Brush 和 gsplat PLY。

### M3：桌面集成

- 加入可选引擎下载和健康检查；
- UI 加入实验后端选项；
- 接入进度、取消、日志、恢复和明确回退；
- 完成许可证清单；
- 默认仍为 Brush。

交付条件：普通用户可以显式安装、选择和卸载 gsplat 后端。

### M4：Speedy 裁剪实验

- 完成 clean-room Strategy；
- 从保守裁剪比例开始；
- 加入留出视图质量评估；
- 与 gsplat MCMC 和 Brush 做三方基准；
- 未通过质量门槛时不进入桌面产品。

交付条件：证明裁剪带来端到端收益，而不仅是更小 PLY。

## 18. 测试计划

### 18.1 Rust 单元测试

- 设置 schema 3 -> 4 迁移默认到 Brush；
- 后端枚举序列化；
- 标准 COLMAP 目录构建；
- 硬链接失败后的复制回退；
- JSONL 正常、乱序和非法行解析；
- 非零退出码、零退出码但无 PLY；
- PLY 属性和长度一致性；
- 后端切换不重跑 COLMAP；
- 取消能终止子进程树。

### 18.2 Adapter 测试

- CPU-only 环境返回明确错误；
- 不支持的 compute capability 返回明确错误；
- 1 张图/空模型拒绝训练；
- 最小 COLMAP fixture 能完成 CUDA smoke；
- 固定 seed 的输出元数据稳定；
- OOM 和 CUDA illegal access 映射为稳定错误码。

### 18.3 集成测试

- Brush 默认路径；
- gsplat 成功路径；
- gsplat 失败后用户选择 Brush 重试；
- 应用重启后复用训练输入；
- 最终 PLY 只能发布一次；
- 历史项目显示正确后端和阶段时间。

## 19. 基准与验收标准

每个后端对同一份 `training-input` 连续运行三次，使用中位数。GPU 温度、后台负载、驱动、运行时版本和 seed 必须记录。

### 19.1 性能门槛

gsplat 要进入 UI 实验选项，至少满足：

- 训练耗时比 Brush 降低 `>= 20%`；
- 连续三次完成且没有 CUDA 崩溃；
- 峰值显存不超过 20 GB；
- PLY 能通过增强校验并由 Brush 查看器打开。

gsplat 要成为推荐后端，至少满足：

- 训练耗时比 Brush 降低 `>= 30%`；
- 端到端耗时降低 `>= 15%`；
- 三组真实样本均通过质量门槛；
- 30 次连续任务无后端崩溃；
- 可选运行时安装、升级、卸载和回退全部验证。

### 19.2 质量门槛

需要从输入图像中固定留出验证视图。相对 Brush：

- PSNR 下降不超过 `0.3 dB`；
- SSIM 下降不超过 `0.005`；
- LPIPS 增加不超过 `0.01`；
- 关键前景不得出现明显空洞、漂浮点或透明度崩坏；
- 相机外插查看不得明显劣化。

若没有留出视图和渲染质量指标，只能称为“成功生成 PLY”，不能称为同质量加速。

## 20. 文件级改造清单

预计影响范围：

| 文件/目录 | 改造内容 |
| --- | --- |
| `src-tauri/src/engines/training.rs` | 新增统一训练接口 |
| `src-tauri/src/engines/gsplat.rs` | 新增 gsplat 进程适配 |
| `src-tauri/src/engines/brush.rs` | 改为消费标准训练输入 |
| `src-tauri/src/engines/health.rs` | 增加 gsplat 三级健康检查 |
| `src-tauri/src/pipeline/runner.rs` | 后端分发、阶段计时、恢复 |
| `src-tauri/src/presets/quality.rs` | 泛化 Brush 专用命名和预设映射 |
| `src-tauri/src/project/catalog.rs` | settings schema 6、Brush 默认与后端设置 |
| `src-tauri/src/project/metadata.rs` | 后端、版本、阶段时间和显存字段 |
| `src-tauri/src/reconstruction/ply.rs` | 增强 Gaussian PLY 属性校验 |
| `src/types/pipeline.ts` | 前端后端类型、状态和元数据 |
| `src/lib/backend.ts` | 后端设置和可选引擎下载 IPC |
| `src/stores/appStore.ts` | 保存后端选择和刷新健康状态 |
| `src/app/App.tsx` | 训练后端设置、提示和错误展示 |
| `engines/manifest.json` | schema 升级及可选 gsplat runtime |
| `scripts/` | 下载、校验和许可验证脚本 |
| `licenses/` | 新运行时第三方 notices |

## 21. 实施前检查表

开始改代码前必须完成：

- [ ] 确认本设计文档已评审；
- [ ] 为工作区创建可恢复备份；
- [ ] 记录当前 Brush 三组基线；
- [ ] 确认 gsplat 精确 commit 和运行时版本矩阵；
- [ ] 确认 RTX 5090D `sm_120` CUDA smoke 通过；
- [ ] 完成第三方许可证清单草案；
- [ ] 确认第一阶段不包含 Speedy-Splat 源码；
- [ ] 定义 PLY 质量和性能验收报告格式。

## 22. 最终决策点

完成 M2 基准后，只允许以下三种结论：

1. **保留 Brush**：gsplat 不更快、不稳定或分发成本过高；
2. **gsplat 作为实验后端**：有局部收益，但兼容性或稳定性仍不足；
3. **推荐 gsplat**：端到端、质量、稳定性和分发全部达到门槛。

不得因为 gsplat 使用 CUDA、单次运行更快或生成的 PLY 更小，就跳过质量、稳定性和端到端验收。
