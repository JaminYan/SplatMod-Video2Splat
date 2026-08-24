# OOOSplat 0.47 相对原版项目的修改说明

本文用于说明当前本地升级版与上游 [ooolabdev/ooosplat](https://github.com/ooolabdev/ooosplat) 的主要差异。上游公开版本说明为 0.1.0，核心链路是“视频 → FFmpeg 抽帧 → COLMAP → Brush → final.ply”，并且不包含内置 3D Viewer；当前版本为本地升级版 0.47。

> 文档更新时间：2026-08-24。本文区分“已实施”“已通过局部验证”和“仍待真实素材验收”，不把计划、静态检查或单次诊断运行写成完整发布结论。

## 当前进度快照

| 模块 | 当前状态 | 证据与边界 |
|---|---|---|
| 帧筛选与候选帧控制 | 已实施 | Rayon 并行 pHash/Laplacian、近重复合并、清晰度保留；自适应 SfM 抽帧已接入运行时，真实素材质量闭环仍待验收 |
| COLMAP 4.1.1 | 已接入 | CPU/no-CUDA、官方 CUDA 与 CASPAR CUDA 引擎可切换；manifest/参数校验已具备，仍需在目标机器现场复核 DLL、驱动和 CUDA SIFT |
| CASPAR CUDA | 已接入实验路径 | 中大型任务可使用 CASPAR 全局 BA，失败时保留独立 attempt 并回退 Ceres；当前不宣称默认后端或所有素材均有同等收益 |
| Brush | 稳定默认后端 | 保持原有兼容路径、数据集 ZIP 和查看器工作流 |
| gsplat CUDA | 可选实验后端 | 独立 Python/CUDA 运行时、三级健康检查、显存 LRU 和 splat 上限已接入；完整 Brush/gsplat 质量与端到端速度基准仍待完成 |
| UI 与可观测性 | 已实施 | 设置抽屉、后端状态、阶段进度、日志、健康检查和历史任务查看入口已接入 |
| 测试 | 部分通过 | 权限/锁阻塞已解除；已记录 Rust 18/18，前端曾有 3 通过、1 个进度状态断言失败，需在清理后重新跑完整套件 |
| 项目清理与备份 | 已完成 | 外部备份位于 `A:\project\backup\Splat Back`；本地已移除临时 Cargo、vcpkg 缓存、Debug 中间产物和 Python/Node 缓存，保留引擎、发行构建产物、Git 与 `.backup` |

## 1. 为什么需要修改

原版已经能完成视频到 Gaussian Splatting PLY 的完整闭环，但在长视频、30/60 FPS 素材和不同显卡环境下存在几个实际问题：

- 抽帧数量直接随源视频帧率增长，重复画面会增加磁盘、解码、特征提取和匹配成本。
- 原流程没有独立的清晰度/重复帧筛选，运动模糊帧可能进入 COLMAP，影响注册质量。
- 原版主要依赖自动判断 CUDA/CPU，用户不能清楚地选择或验证实际使用的 COLMAP 后端。
- 原版采用 COLMAP 4.0.4 CUDA 构建，当前版本需要固定到 4.1.1 作为可复现、可校验的基线。
- 训练阶段只有 Brush 一条路径，显存较小、显存较大或希望实验其他 CUDA 训练栈时缺少选择。
- 原版没有直接的 3D 查看入口、独立的训练预设和较完整的阶段/状态提示。

因此，本版本的目标不是简单替换一个二进制，而是把流水线拆成可控的“抽帧—筛选—重建—训练—发布”阶段，并为不同硬件提供可切换、可诊断的工作流。

## 2. 主要改动总览

| 领域 | 原版 | 当前 0.47 |
|---|---|---|
| 抽帧 | 快速/均衡/精细按源 FPS 比例抽取 | 固定为快速 1 FPS、均衡 2 FPS、精细 4 FPS 候选帧 |
| 帧筛选 | 均匀抽帧后直接进入 COLMAP | Rayon 并行计算 pHash 与 Laplacian，连续近重复合并并保留更清晰帧 |
| COLMAP | 4.0.4 CUDA 构建，自动选择 | 固定 4.1.1 分发物；CPU/no-CUDA 与 CUDA 可切换，参数明确传递 GPU/CPU 模式 |
| 训练 | Brush 单一后端 | Brush 稳定后端 + gsplat CUDA 实验后端 |
| Brush | 基本质量档位 | A/B/C 训练预设，分别偏向均衡、显存和质量 |
| gsplat | 无 | 独立 Python/CUDA 运行时、健康检查、显存缓存和 splat 上限控制 |
| FFmpeg | 基础抽帧 | 关闭、自动、D3D11VA、CUDA/NVDEC 四种硬件解码选择 |
| 产物 | `final.ply` | 使用原视频名生成 PLY，并保留统一 `training-input`、日志和独立 COLMAP attempt 目录 |
| 查看与诊断 | 外部工具查看 PLY | 历史任务提供“查看 3D”入口、阶段进度、心跳、项目日志和引擎健康状态 |

## 3. 性能优化点位

### 3.1 有界候选抽帧

当前质量档位首先限制候选帧密度：1 FPS、2 FPS、4 FPS。这样可以避免 30/60 FPS 视频把大量相邻、几乎相同的画面送入后续流程。收益主要来自：

1. 少写入和读取 JPEG；
2. 少执行 pHash/Laplacian；
3. 少执行 COLMAP 特征提取、匹配和 Mapper；
4. 降低 Brush/gsplat 的训练输入规模。

这属于输入规模控制，不代表固定比例的端到端加速；真实收益取决于视频时长、运动速度和画面变化。

### 3.2 pHash 去除连续近重复帧

`src-tauri/src/video/select.rs` 对候选图像计算低频 DCT pHash，并比较相邻帧的汉明距离。连续近重复画面只保留代表帧，减少冗余视图，同时保持时间顺序，避免把不连续的相似画面错误合并。

### 3.3 Laplacian 方差保留清晰帧

对于被判断为近重复的一组画面，使用 Laplacian 方差估计局部清晰度，在相邻画面内容基本相同的情况下保留纹理更清楚的一帧。这样减少运动模糊帧进入 COLMAP，优化的是输入质量和重建稳定性，而不仅是数量。

### 3.4 Rayon 有界并行与确定性合并

帧的图像解码、缩放、pHash 和 Laplacian 指标相互独立，因此当前实现使用有界 Rayon 线程池并行计算，线程数限制为不超过可用并行度和 8 个线程。之后仍按原始时间顺序合并结果，并在全部指标计算成功后再删除淘汰文件。

此外，DCT 余弦表通过 `OnceLock` 缓存，避免每张图重复构造。筛选逻辑由 `tokio::task::spawn_blocking` 调度，避免阻塞异步流水线。

### 3.5 COLMAP CPU/CUDA 参数真正分流

当前版本不再只根据可执行文件名称显示“CUDA”，而是根据所选后端明确传递参数：

- CPU：`FeatureExtraction.use_gpu=0`，`FeatureMatching.use_gpu=0`；
- CUDA：`FeatureExtraction.use_gpu=1`，`FeatureMatching.use_gpu=1`，`gpu_index=-1`；
- 匹配器使用 `SIFT_BRUTEFORCE`，顺序匹配使用 overlap 10；
- CPU 与 CUDA 的数据库、稀疏模型和日志写入独立的 `work/colmap-attempts/cpu|cuda/` 目录。

这修复了“选择 CUDA 可执行文件但命令行仍强制 CPU”的问题，也方便对两种后端分别留存结果和日志。

### 3.6 FFmpeg 硬件解码选择

设置中提供关闭、自动、D3D11VA 和 CUDA/NVDEC。当前 CUDA 路径主要优化视频解码；JPEG 写入仍是 CPU 软编码，因此应准确描述为“硬件解码 + CPU/系统内存后处理”，不是完整 GPU 图像管线。

### 3.7 训练阶段的显存与规模控制

Brush 增加 A/B/C 预设：

- A：稳定均衡；
- B：降低最大分辨率和 splat 上限，适合 6–8 GB 显存；
- C：提高训练步数和 splat 上限，适合显存较大的质量优先场景。

gsplat 实验后端增加独立运行时健康检查、Python/CUDA/rasterization 检查、GPU 图片 LRU 缓存限制，以及自动安全、100 万、200 万、400 万 splat 上限。splat 上限是硬约束，不是质量目标。

## 4. 升级和新增模块

### 4.1 COLMAP 4.1.1

`engines/manifest.json` 将 Windows CPU/CUDA COLMAP 固定到官方 4.1.1 发行物，并记录归档 SHA-256 及 CUDA `colmap.exe` 校验信息。`scripts/verify-engines.ps1` 会检查 CPU/no-CUDA、CUDA 支持、SIFT GPU 参数、Mapper BA 参数和可选可执行文件哈希。

需要注意：固定 manifest 和归档哈希说明“分发物按 4.1.1 管理”，不能替代对当前机器实际 `colmap.exe --version`、DLL、驱动和 CUDA SIFT 的现场验证。

### 4.2 gsplat CUDA 实验训练后端

新增 `engines/gsplat/adapter/train_adapter.py` 及版本、构建和恢复脚本。它读取与 Brush 相同的 `work/training-input`，并在选择前执行 Python、CUDA 和 rasterization 健康检查。Brush 仍是默认稳定后端，gsplat 不是默认必需依赖。

### 4.3 项目与产物管理

新增统一的 `training-input`、`project.json`、`state.json`、阶段日志和引擎状态；COLMAP CPU/CUDA 尝试互相隔离；训练完成后校验 PLY，并以原视频文件名发布结果。

### 4.4 可观测性和诊断 CLI

流水线区分总进度与阶段进度，外部进程没有输出时仍保留心跳。新增/强化 `splatstudio` CLI，可执行 health、probe、plan、extract 和 generate，便于在不依赖 UI 的情况下验证引擎和流水线。

## 5. 可切换的工作流

当前可以按硬件和目标选择以下组合：

```text
视频
 └─ FFmpeg 解码：关闭 / 自动 / D3D11VA / CUDA
     └─ 候选帧：1 FPS / 2 FPS / 4 FPS
         └─ pHash 去重 + Laplacian 清晰帧
             └─ COLMAP：CPU/no-CUDA 或 CUDA 4.1.1
                 └─ Brush：A / B / C
                 └─ gsplat CUDA：实验后端 + splat 上限
```

典型选择：

- 无 NVIDIA GPU：FFmpeg 自动或关闭 + COLMAP CPU + Brush A/B；
- NVIDIA GPU：FFmpeg CUDA/NVDEC + COLMAP CUDA 4.1.1 + Brush A；
- 显存有限：COLMAP CUDA + Brush B，或降低输入质量档位；
- 质量优先：4 FPS + Brush C；
- 实验训练：先通过 gsplat 健康检查，再切换 gsplat 和 splat 上限。

任务运行期间锁定后端和预设，避免中途改变输入、设备或输出状态。

## 6. UI 改动

- 新增设置抽屉，集中管理 COLMAP 后端、FFmpeg 硬件加速、训练后端、Brush 预设和 gsplat splat 上限。
- CPU/CUDA 选项显示“就绪、异常、未找到、未下载”等实际状态，不能在后端未通过健康检查时误启用。
- 质量档位改为 1/2/4 FPS，并明确显示档位说明。
- 新增训练后端切换，gsplat 在健康检查未通过时禁用。
- 历史任务新增项目目录、PLY 结果和“查看 3D”入口。
- 实时区域区分总进度、当前阶段和阶段百分比；错误提示更偏向可操作建议，详细 COLMAP 信息保留在项目日志。
- UI 使用图标、状态徽标、紧凑卡片和响应式布局；设置项说明以悬停提示/辅助文本为主，避免界面过度拥挤。

## 7. 如何测试

### 已有验证

历史实现阶段已经通过：

- Rust 库测试：18/18；
- `npm run verify:engines`；
- `npm run verify:licenses`；
- `npm run build:bundle`；
- 帧筛选、CPU/CUDA 参数、manifest/哈希和产物目录的静态检查。

### 本次复核

权限/文件锁问题修正后，已在受控外部环境重新执行：

- `npm run verify:engines`：通过；
- `npm run verify:licenses`：通过；
- `cargo test --manifest-path src-tauri\\Cargo.toml --lib`：通过，22/22；
- `npm test`：测试已正常启动，3 个通过、1 个失败。失败测试为 `src/stores/appStore.test.ts` 的“保持进度单调并忽略过期事件”，实际值为 `100`，断言期望为 `42`。

因此，权限/锁阻塞已经排除，Rust 22/22 与引擎/许可证校验已有通过记录；前端仍存在一个真实的进度状态断言问题，不能把整套 `npm test` 写成通过。项目清理后尚未重新执行完整回归；后续应使用备用 Cargo target 目录，避免重新生成大型本地中间产物。

### 性能测试要求

当前历史样本可用于定位瓶颈，但不能作为严格 A/B 速度提升结论。正式性能测试应固定：同一帧目录、同一质量档位、同一 COLMAP 后端、同一训练预设和同一显卡；每组先预热一次，再正式运行三次，报告中位数。

至少记录：候选帧数、保留帧数、筛选耗时、特征提取、匹配、Mapper、训练和总耗时，注册比例、三维点数、重投影误差、峰值 RAM/显存、是否发生回退和失败原因。

## 8. 当前边界

以下内容不能在当前版本中宣传为已完成：

- CUDA 失败后的自动 CPU 重试；当前只有独立 attempt 目录和手动后端切换；
- Caspar 已成为默认 Mapper；它仍属于后续评估/路由工作；
- GLOMAP 自动质量回退；
- 预置 ONNX 模型的 LightGlue 恢复模式；
- 完整 CUDA/nvJPEG 去重管线；
- 在同素材、同设备、同预设下证明端到端固定倍数加速。

这些边界是为了保证发布说明与真实功能、测试证据保持一致。
