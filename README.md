# SplatMod-Video2Splat 0.48

Windows 本地视频或已重建数据转 3D Gaussian Splatting 桌面应用。输入视频时，应用在本机完成抽帧、COLMAP 稀疏重建与训练；输入 Splatcam 导出时，可直接复用 RGB、相机、位姿和点云，跳过视频抽帧、画面筛选和 COLMAP 重建，最终生成标准 Gaussian PLY。默认训练后端为 Brush；gsplat CUDA 是通过运行时健康检查后才可选的实验后端。

> MOD By Jamin。项目基于 [ooolabdev/ooosplat](https://github.com/ooolabdev/ooosplat) 的桌面流水线继续演进；不会上传视频或训练数据。代码仓库：[JaminYan/SplatMod-Video2Splat](https://github.com/JaminYan/SplatMod-Video2Splat)。

## 0.48 新增与完善

- **自适应 SfM 抽帧**：均衡/精细档先用低分辨率代理分析背景运动，通过 FFmpeg `-stats_mux_pre` 保留输入帧索引 `ni` 与显示时间 `ti`，再精确抽取原图；索引缺失、时间戳异常或代理失败时自动回退到固定 2/4 FPS。
- **质量闭环与可恢复诊断**：自适应结果会继续经过 COLMAP 注册率、稀疏点和模型解析检查；不足时支持定向补帧或进入 `needsSupplement`，不覆盖原始 attempt，取消后仍保留诊断日志和恢复上下文。
- **CASPAR/Ceres 明确分工**：COLMAP 4.1.1 的局部 BA 使用 Ceres，中大型任务尝试 CASPAR 全局 BA；CASPAR 启动、Mapper、模型解析或质量门禁失败时，在独立目录回退 Ceres并记录实际后端与原因。
- **可切换工作流锁定**：任务开始后锁定 FFmpeg 解码模式、COLMAP CPU/CUDA/CASPAR 后端、Brush/gsplat 训练后端和质量预设，避免运行中改变输入或设备状态。
- **进度与诊断增强**：界面区分总进度、当前阶段和阶段百分比；长时间无外部输出时显示心跳，详细 FFmpeg/COLMAP/训练日志写入项目目录，错误提示包含可执行的回退建议。
- **代码-only 发布边界**：新仓库只提交源码、配置、脚本、许可证和必要文档；COLMAP/Brush/gsplat/CUDA 运行时、安装包、缓存、`node_modules` 和构建产物不进入 Git。
- **Splatcam 导入工作流**：支持 `images/*.jpg` + `sparse/0/cameras.txt` + `images.txt` + RGB `points3D.ply`；导入时跳过 FFmpeg、pHash/Laplacian、自适应抽帧、COLMAP 特征/匹配/Mapper，转换为标准 `training-input` 后直接进入 Brush 或 gsplat。
- **Splatcam 导入质量门禁**：校验 RGB 与位姿一一对应、PINHOLE 内参、COLMAP world-to-camera 位姿、PLY 有限点坐标、投影覆盖和模型记录数；失败时保留原始导出与诊断，不静默改走视频流程。
- **Splatcam 状态与 UI**：新增数据源选择、导入目录统计、仅检查/导入并训练、导入阶段进度、坐标约定和失败原因展示；原始 RGB 点云明确标记为训练初始化点云，不当作最终 Gaussian PLY。
- **Splatcam 后端完整接入**：新增导入命令、项目状态持久化、COLMAP 文本到 binary 模型标准化、导入报告和训练前质量门禁，导入失败不会静默回退到视频工作流。

## 相对原项目的升级

| 能力 | 本地升级内容 |
| --- | --- |
| 有界候选抽帧 | 不再按源视频的每一帧抽取。质量档位固定为快速 **1 FPS**、均衡 **2 FPS**、精细 **4 FPS**，避免 30/60 FPS 视频产生过量候选帧。 |
| 有效视图筛选 | 基于 Rayon 对候选帧在最多 8 个 CPU 线程中并行计算 pHash 与 Laplacian 指标；全部计算成功后，按时间顺序合并连续近重复画面，并以 Laplacian 方差保留更清晰的一帧。 |
| COLMAP 双后端 | 同时提供 CPU/no-CUDA 与可选 CUDA COLMAP。桌面应用默认选择 CUDA；CUDA SIFT 与 SIFT_BRUTEFORCE 匹配会实际传递 GPU 参数，CPU 后端强制关闭 GPU。 |
| CASPAR CUDA 引擎 | 官方 CUDA 与自编译 CASPAR CUDA 并存，不互相覆盖。选择 CASPAR 会自动绑定 CASPAR 全局 BA，适合中大型项目；同一 273 帧素材实测建图 430 秒降至 126 秒，注册率均为 100%。 |
| Brush 训练预设 | A/B/C 三档独立于抽帧质量：A 稳定均衡、B 显存优先、C 质量优先；训练时会传入对应的步数、最大分辨率与 splat 上限。 |
| 双训练后端 | Brush 为默认稳定后端。gsplat 使用隔离 Python/CUDA 运行时，读取同一份 `work/training-input`；只有 Python、CUDA 和 rasterization 健康检查均通过才可选择。 |
| gsplat 显存与模型控制 | 自动识别显存，GPU 图片 LRU 缓存限制为 256–1024 MB；可选自动安全、100 万、200 万、400 万 splat 硬上限。MCMC 采用 opacity/scale 正则、验证损失停滞冻结增殖与透明 splat 导出过滤，cap 不代表质量目标。 |
| Brush 数据集与查看 | 自动创建含 `images/`、`sparse/0/` COLMAP 二进制模型的 `dataset.zip`；历史任务可通过“查看 3D”启动 Brush 内置查看器。 |
| 可观测性 | 总任务进度和阶段进度区分显示；相机重建解析 `num_reg_frames`；Brush 每 10% 写入 checkpoint，据真实 checkpoint 更新训练计数。 |
| 历史任务 | 首屏仅读取项目元数据，不扫描大型 PLY；详情按需展开，保留查看 3D、资源管理器与删除操作。 |
| 任务产物 | 项目目录采用 `YYYYMMDD_视频文件名`；最终 PLY 使用原视频文件名，例如 `20260822_walkthrough/walkthrough.ply`。 |
| Splatcam 数据导入 | 复用已经完成的相机/位姿/点云；通过模型标准化生成 `cameras.bin`、`images.bin`、`points3D.bin`，不重复执行视频和 COLMAP 重建。 |

## 流程

```text
视频输入：
  视频
    → FFprobe 验证视频流
    → FFmpeg 按 1 / 2 / 4 FPS 生成候选帧
    → 并行 pHash / Laplacian 与自适应 SfM 选帧
    → COLMAP CPU / CUDA / CASPAR 稀疏重建与质量门禁
    → 标准 training-input

Splatcam 输入：
  导出目录
    → 校验 RGB + cameras.txt + images.txt + points3D.ply
    → 文本 COLMAP 模型标准化为 binary
    → 生成标准 training-input

共同后续：
  training-input → Brush（默认）或 gsplat CUDA（实验）训练
    → 校验并发布 Gaussian PLY
```

COLMAP 注册率低于 50% 时任务停止；50%–80% 时继续完成，但会提示质量风险。

## 使用

1. 启动应用，确认 FFmpeg、COLMAP 和 Brush 状态正常。默认训练后端为 **Brush**。
2. 打开设置：默认选择 **COLMAP CUDA**。中大型项目可选 **CASPAR CUDA**；它自动使用 CASPAR 全局 BA，小项目可能受初始化开销影响。
3. 选择视频、项目根目录和画质档位（快速 1 FPS / 均衡 2 FPS / 精细 4 FPS）。
4. 在设置中按显存与质量需求选择 Brush 训练预设；需要实验 gsplat 时，先通过其 CUDA 健康检查，再选择后端与 splat 上限。
5. 点击“开始生成”。左侧显示整体进度，任务日志在右侧历史/详情中查看。
6. 完成后在历史任务中打开项目目录、点击“查看 3D”，或直接取得同名 PLY。

### 导入 Splatcam 已重建数据

在数据源中选择 **Splatcam 已重建数据**，选择包含以下结构的目录：

```text
<splatcam-export>/
  images/*.jpg
  sparse/0/cameras.txt
  sparse/0/images.txt
  sparse/0/points3D.ply
```

导入器会检查图像与位姿是否一一对应、相机尺寸和内参是否有效、点云是否可读以及坐标投影是否合理，然后生成：

```text
work/training-input/
  images/
  sparse/0/cameras.bin
  sparse/0/images.bin
  sparse/0/points3D.bin
```

导入模式不会执行 FFprobe、FFmpeg、pHash/Laplacian、SIFT、匹配、Mapper 或 CASPAR。可以先选择“仅检查导入”，确认质量门禁通过后再选择 Brush 或 gsplat 训练。

当前 0.48 支持的是 RGB 点云导出格式；`points3D.ply` 只是训练初始化点云，不是最终 Gaussian PLY。LiDAR 深度和 `transforms.json` 尚未作为本期必需输入，发现它们时会保留为后续扩展数据。

### Brush 训练预设

| 预设 | 适用场景 | 训练参数取舍 |
| --- | --- | --- |
| A（默认） | 大多数设备 | 稳定均衡；按画质使用 7k / 15k / 30k 步，splat 上限为 1.5M / 3M / 5M。 |
| B | 6–8 GB 显存或希望减少中断 | 保持步数，降低最大分辨率与 splat 上限为 1M / 2M / 3M。 |
| C | 建议 12 GB 以上显存、优先质量 | 步数提升至 1.5 倍，splat 上限为 2M / 5M / 8M；耗时与显存需求更高。 |

这些预设只影响 Brush 训练，不改变 1 / 2 / 4 FPS 候选抽帧档位。

### 项目目录

```text
<projects-root>/20260822_<视频文件名>/
  source/splatcam/             # Splatcam 导入时保留的原始输入
    images/
    sparse/0/
    points3D.ply
  frames/                       # 经过 pHash / 清晰度筛选后送入 COLMAP 的 JPEG
  work/colmap-attempts/<cpu|cuda>/ # 后端独立的 database.db、sparse/ 与 colmap.log
  work/splatcam-import/         # 导入报告、模型标准化和质量门禁结果
  work/brush/dataset.zip        # Brush 输入归档
  work/brush/                   # Brush 训练过程及临时输出
  work/training-input/          # 统一训练输入：images/ + sparse/0/
  work/gsplat/                  # gsplat 请求、训练过程及临时输出（选择该后端时）
  logs/                         # ffmpeg / colmap / brush / gsplat 完整日志
  <视频文件名>.ply              # 最终产物
  project.json                  # 项目元数据与最终 PLY 路径
  state.json                    # 可恢复的流水线状态
```

## 安装与开发

环境要求：Windows 10/11 x64、WebView2、Node.js 22+、Rust stable、Visual Studio 2022 Build Tools。

```powershell
npm install
npm run setup:engines
# 需要 COLMAP CUDA 时执行；默认安装只部署 CPU/no-CUDA 版本
npm run setup:engines:optional
npm run tauri -- dev
```

### gsplat CUDA 双包安装

基础 NSIS 安装包默认仅包含 Brush 等核心引擎。gsplat CUDA Python 运行时约 3 GB，超过 NSIS 单安装包的可靠容量，须作为独立运行时包分发，且不应提交到 Git。

发布者在已验证本机 CUDA 运行时后执行：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\package-gsplat-runtime.ps1
```

生成的 ZIP 位于 `.tmp\runtime-packages`，不含 `engines\gsplat\source` 或构建日志。用户先安装基础包，再解压 ZIP 并运行：

```powershell
powershell -ExecutionPolicy Bypass -File .\install-gsplat-runtime.ps1
```

脚本会请求 Windows 管理员权限，识别 OOOSplat 的 `engines` 或 `resources\engines` 布局后安装 `gsplat`，并将既有运行时改名备份而不删除。完成后重新启动应用，设置页的 gsplat CUDA 状态会重新健康检查。

常用校验与构建命令：

```powershell
npm test
cargo test --manifest-path src-tauri\Cargo.toml
npm run verify:engines
npm run verify:licenses
npm run build:bundle             # 打包前完整校验：引擎 + 许可 + 前端构建
npm run tauri -- build
```

`engines/manifest.json` 使用 schemaVersion 3。它将 Windows CPU/CUDA COLMAP 归档固定到官方 `4.1.1` 发行物与完整 SHA-256，并锁定 CUDA `colmap.exe` 哈希。`npm run verify:engines` 会检查 CPU/no-CUDA、CUDA 支持、特征/匹配 GPU 参数及 Mapper BA 参数。请不要手工修改归档 SHA-256。

Splatcam 导入的详细数据契约、模型转换、坐标系门禁和测试矩阵见 [SPLATCAM_IMPORT_IMPLEMENTATION_PLAN.md](SPLATCAM_IMPORT_IMPLEMENTATION_PLAN.md)。

## 当前进度

详细的已完成项、验证状态、性能证据边界和待办事项见 [PROJECT_PROGRESS.md](PROJECT_PROGRESS.md)。

## 限制与许可

- CUDA 是默认偏好，不是必需条件。实际的 COLMAP 加速效果取决于显卡、驱动与 COLMAP 参数；CPU/no-CUDA 可作为回退。
- Brush 是默认训练后端；gsplat CUDA 为实验功能。相同画质档不承诺生成相同数量的 splat，也不能凭单次训练宣称速度或质量更优。
- Splatcam 导入当前要求标准 COLMAP 文本相机/位姿和 RGB 点云；如果导出缺少 `cameras.txt`、`images.txt` 或 `points3D.ply`，不会自动猜测坐标系或回退到视频流程。
- 当前 Splatcam PLY 只有 XYZ/RGB 属性，不能直接作为最终 Gaussian PLY；最终输出仍必须由 Brush/gsplat 训练并通过 Gaussian 属性校验。
- `images.txt` 的位姿按 COLMAP world-to-camera 解释，不直接套用 NeRF/OpenGL 的 camera-to-world 转换。真实深度单位、`transforms.json` 轴向转换和深度监督训练属于后续扩展。
- CUDA 与 CPU 的 COLMAP 尝试目录彼此隔离。仅 CUDA/驱动/显存等运行时故障才会自动切换到独立 CPU 尝试；素材质量问题不会被误判为回退条件。CUDA 且筛选后保留至少 151 帧时，Mapper 会先尝试 CASPAR GPU BA；启动、Mapper、模型解析或注册率低于 50% 时，会在独立目录回退 Ceres，并把实际 BA 后端与原因写入项目记录。CASPAR 仍是实验路径，尚未完成 631 帧真实样本的性能/质量验收。
- CASPAR 由“COLMAP 引擎版本”统一控制：选择 CASPAR CUDA 自动使用 CASPAR 全局 BA；官方 CUDA 使用自动 Ceres 路径。COLMAP 4.1.1 不支持 CASPAR local BA，局部 BA 始终使用 Ceres。
- FFmpeg 硬件加速可在设置中选择关闭、自动、D3D11VA 或 CUDA。若驱动或运行时不支持，应切换为“自动”或“关闭”。
- 重建质量受拍摄覆盖、纹理、运动模糊、显卡、驱动及 COLMAP 注册率影响。建议围绕目标物体缓慢移动，并保持充足视角重叠。
- 项目代码采用 Apache-2.0；FFmpeg、COLMAP、Brush 分别适用其自身许可证。详见 [NOTICE](NOTICE)、[licenses/THIRD_PARTY_NOTICES.txt](licenses/THIRD_PARTY_NOTICES.txt) 与 [GENERATED_OUTPUTS.md](GENERATED_OUTPUTS.md)。
