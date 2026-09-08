# Insta360 全景视频接入调研（2026-09-08）

## 结论

OOOSplat 当前只能把普通 `mp4` / `mov` 经 FFprobe/FFmpeg 抽成 JPEG，再以
`SIMPLE_RADIAL` 相机模型送入 COLMAP。原始 Insta360 `insv` 不是可直接替换的
容器输入：它需要由官方 MediaSDK 完成镜头拼接，并且拼接结果为 `2:1`
等距柱状全景，不能直接作为普通透视照片送入现有 COLMAP 步骤。

最小且正确的接入边界是：**官方 SDK sidecar 只负责 `insv -> 已拼接全景帧`；
项目已有 FFmpeg `v360` 负责 `equirect -> perspective`；其余 COLMAP/训练流程
复用不动。** 不应试图让 FFmpeg 直接处理原始 `insv`，也不应把全景 JPEG 直接
交给当前 `SIMPLE_RADIAL` 的 COLMAP。

## 官方 SDK 事实

| 项目 | 已确认行为 | 对接影响 |
| --- | --- | --- |
| 原始视频输入 | 仅 `insv`；输入数组通常为 1 或 2 个文件 | 选择一个 `_00_` 文件时必须自动发现并校验同组 `_10_` 文件；X4 是单一原始文件，即使是高分辨率 |
| 输出 | 视频仅 `.mp4`，图像仅 `.jpg` | 优先使用图像序列，避免额外编码/解码和索引漂移 |
| 全景尺寸 | `SetOutputSize(w,h)` 强制 `w:h = 2:1` | 这是 equirect 中间产物，不是 COLMAP 最终输入 |
| 精确帧导出 | `SetExportFrameSequence(vector<uint64_t>)` + `SetImageSequenceInfo(...)`；索引从 0 开始 | 能与未来的抽帧计划对齐，但需用真实素材验证 SDK 帧号与代理 MP4 的一一对应 |
| 拼接 | 设置完成后调用 `StartStitch()`；有进度、错误、取消回调/API | sidecar 必须把进度/错误写成结构化 stdout；取消可先由现有 Job Object 终止 sidecar，后续再接 `CancelStitch()` |
| GPU | 3.x 要求 GPU；当前在线文档称运行时只要求 NVIDIA 驱动 >=470 | 本机 RTX 5090 D v2、驱动 616.64 满足该最低驱动门槛；仍须用 SDK 样例和真实 `insv` 验证兼容性 |
| 分发 | SDK 需向 Insta360 申请；Windows 包含 DLL、模型和运行时依赖 | 未获得 SDK 包与许可前，不能承诺可把它随 NSIS 安装器再分发 |

来源：[官方 Media SDK API 文档](https://insta360develop.github.io/Insta360-Developer_Docs/en/x/desktop/media/)、[官方旧版 C++ README](https://github.com/Insta360Develop/Desktop-MediaSDK-Cpp)。后者已明确说明内容将迁移至在线文档，因此以在线文档为准。

## M0 实测结果（本机 SDK 与素材）

| 检查项 | 结果 |
| --- | --- |
| SDK 包 | `A:\SDK\Insta\MediaSDK`，`MediaSDKTest.exe` 报告版本 `3.1.5`；models、`MediaSDK.dll`、CUDA/OpenCV 运行时均在包内 |
| 测试素材 | `A:\tmp\insta\VID_20260830_124955_00_057.insv`（1.65 GB）；SDK 元数据识别为 Insta360 X6 |
| 原始轨道 | 虽然文件系统中只有一个 `.insv`，SDK 识别它内含 2 路 3840×3840 / 29.97 FPS HEVC 视频流和 1 路音频。因此配对规则必须由 SDK/素材元数据确认，不能仅依据文件数量推断。 |
| 指定帧导出 | 运行 `MediaSDKTest -image_sequence_dir ... -export_frame_index 0-30-60 -output_size 1920x960` 成功，生成 `0.jpg`、`30.jpg`、`60.jpg`，进度为 33%/66%/100%，总耗时 1.918 秒。输出确为 1920×960、`yuvj420p` 的 2:1 equirect JPEG。 |
| GPU 运行 | SDK 实际选中 NVIDIA RTX 5090 D v2 的 CUDA HEVC 硬解；Vulkan 初始化成功。 |
| 透视投影 | 项目自带 FFmpeg 8.1 对导出的 `0.jpg` 执行 `v360=input=equirect:output=flat:w=1280:h=960:h_fov=100:v_fov=75:yaw=0:pitch=0` 成功，生成正常的 1280×960 rectilinear JPEG。`output=perspective` 在此版本会得到圆形投影，因此实现必须使用 `output=flat`。 |

证据保存在 `A:\project\splat\.tmp\insta-m0-20260908-01`（三个全景样帧、一个正常透视样帧、一个用于排错的圆形投影样帧及 SDK 日志）。这是 SDK/投影可用性证据，不是 COLMAP 或训练质量通过的证据。

## M1 当前实现

已加入最小导入桥接：

- 文件选择器现在接受 `.insv`；
- Rust 通过 `OOOSPLAT_INSTA360_SDK_DIR` 定位 SDK 根目录（也接受直接指向 `MediaSDKTest.exe`）；
- 官方 `MediaSDKTest.exe` 输出临时 3840×1920 equirect MP4；
- 项目自带 FFmpeg 用 `v360 ... output=flat` 转成固定 1920×1440、100°×75° 单视角视频，然后复用现有 FFprobe、抽帧、COLMAP、训练链路；
- 所有中间文件和日志放在项目 `work/insta360` 与 `logs/`，已存在的残留输出不会被覆盖。

运行前设置（PowerShell）：

```powershell
$env:OOOSPLAT_INSTA360_SDK_DIR = 'A:\SDK\Insta\MediaSDK'
```

M1 有意保留两个边界：它会先生成完整临时 MP4（存储/耗时较高），并只生成一个固定视角；只有完成真实 COLMAP 注册率、稀疏点、重投影和训练对比后，才升级为 SDK 指定帧导出和多视角投影。

本机完整桥接实测也已通过：该素材输出 104.14 秒、3121 帧、29.97 FPS 的 3840×1920 equirect H.264；FFmpeg 以约 11.4× 实时速度转为 104.14 秒、3121 帧、1920×1440 的透视 MPEG-4。该结果只证明预处理链路和时间轴完整，不代表 SfM 质量已通过。

## M2 实测结果（单视角 COLMAP）

使用上述 104.14 秒透视视频，以快速档抽取 104 张 JPEG，运行 CUDA SIFT、10 帧顺序匹配和增量 Mapper：

| 指标 | 结果 |
| --- | ---: |
| 输入图像 | 104 |
| 注册图像 | 102（98.08%） |
| 稀疏点 | 26,960 |
| 平均轨迹长度 | 4.21 |
| 平均每图观测数 | 1,112 |
| 平均重投影误差 | 1.146 px |

证据目录为 `A:\project\splat\.tmp\insta-m2-colmap-20260908-01`，包含抽取帧、COLMAP 数据库、feature/matching/mapper 日志、二进制模型和文本模型。该结果超过当前 80% 注册门槛，说明固定单视角已足以进入后续产品化验证；多视角全景轨道暂不增加。

## M3 当前产品化状态

M3 已将 SDK 路径接入应用设置，但仍保持 sidecar/SDK 的最小边界：

- `AppSettings` 新增持久化的 `insta360SdkDir`，设置 schema 从 11 升至 12；
- `get_settings` 返回 `insta360` 健康状态：是否配置、可执行文件、models 是否存在以及可读提示；
- 设置面板新增“选择 SDK 目录”，选择 `A:\SDK\Insta\MediaSDK` 后无需每次手动设置环境变量；
- `OOOSPLAT_INSTA360_SDK_DIR` 仍作为兼容入口；普通 `mp4` / `mov` 不依赖 SDK，只有选择 `.insv` 时才会严格要求 SDK 可用；
- SDK、DLL、models 没有复制进仓库或安装器。当前实现调用 SDK 包自带的 `MediaSDKTest.exe`，后续只有在需要精确选帧或更细粒度取消/进度时才拆出专用 sidecar。

M3 校验：Rust library tests `68 passed`，TypeScript 增量构建通过，`git diff --check` 通过。该校验覆盖编译与设置链路，不等同于重新运行完整 Insta360/COLMAP 性能基准。

### M3.1 运行后修复（2026-09-08）

一次真实任务中，gsplat 训练、验证和 `final.ply` 导出均已完成，但自动预筛脚本最后读取中文 `review-manifest.json` 时由 Windows PowerShell 5.1 按 ANSI 解码 UTF-8 无 BOM 文件，`ConvertFrom-Json` 因此失败并将整个发布阶段报告为退出码 1。`scripts/run-gsplat-autoselect.ps1` 的 `Read-Json` 已改为 `[IO.File]::ReadAllText(..., [Text.Encoding]::UTF8)`；用同一份 manifest 在 Windows PowerShell 5.1 下解析通过，脚本语法检查也通过。该问题与 Insta360 拼接、COLMAP 或 gsplat CUDA 训练本身无关。

## 与现有项目的实际边界

| 当前位置 | 当前假设 | Insta360 接入所需变化 |
| --- | --- | --- |
| `src/lib/backend.ts` | 文件对话框只接受 `mp4,mov` | 加入 `insv`，但只改变选择层，不把其当普通视频处理 |
| `src-tauri/src/pipeline/runner.rs` 的 `prepare_frames` | 首步就是 `probe_video(ffprobe, input)`，随后 FFmpeg 代理分析/抽帧 | 在此之前增加“原始 Insta360 预处理”：拼接代理全景、规划、按选中索引导出全分辨率全景帧、投影透视帧 |
| `src-tauri/src/engines/ffmpeg.rs` | 从常规视频直接输出 JPEG | 不修改普通视频路径；新增针对已拼接 equirect JPEG 的投影调用即可 |
| `src-tauri/src/engines/colmap.rs` | 固定 `SIMPLE_RADIAL` + `single_camera=1` | 仅接收同一透视 FOV/分辨率的帧；不能接收全景图 |
| `src-tauri/tauri.conf.json` / `engines/manifest.json` | 只打包并核验 FFmpeg、COLMAP、Brush | 只有许可确认后再加入 MediaSDK DLL、模型、sidecar 与第三方许可；SDK 不能混进“源码发布” allowlist |

现有 FFmpeg 8.1 已实测包含 `v360` 过滤器，并支持 `equirect` 输入与 `perspective` 输出，因此投影步骤无需新增依赖。

## 推荐实施顺序

### M0：SDK 可行性门（先做，不改主流程）

取得 Insta360 批准的 Windows x64 MediaSDK 包，并在本机运行其 `MediaSDKTest.exe` 或官方
`example/main.cc`。用一组真实素材记录：相机型号、一个/两个原始文件、SDK 版本、GPU/驱动、
输出尺寸、导出耗时、进度回调、取消结果和 SDK 日志。必须验证：

1. 单文件（X4/X5 等）与双文件（如旧 5.7K `_00_`/`_10_`）各一例；
2. `SetExportFrameSequence({0,10,...})` 的文件名、数量与视觉内容；
3. 低分辨率拼接 MP4 的解码帧号是否和原始 SDK 帧号一致；
4. 路径含中文时能否工作（SDK 要求传入 UTF-8）；
5. 失败、取消、无 NVIDIA GPU 与缺少 models 的错误是否可读。

若 M0 不通过，停在“提示用户先用 Insta360 Studio 导出 equirect MP4”的兼容路径；不要伪造 `.insv` 支持。

### M1：最小原始文件桥接

写一个极小的 C++ `insta360-stitcher.exe` sidecar，而不是把 C++ API 直接 FFI 进 Rust：MediaSDK 是 C++ DLL，sidecar 让 Rust/Tauri 只需启动子进程并解析 JSON Lines，且 DLL/模型搜索路径、SDK 崩溃和取消与主应用隔离。输入参数只包含：

- `--input` 一或两个绝对 UTF-8 `.insv` 路径；
- `--output-dir`（必须为空的新目录）；
- `--frames` 以 0 起始帧号列出；
- `--width/--height`（验证为 2:1）；
- 一个明确的拼接预设（初始只用 SDK 默认/模板拼接）。

输出为按 SDK 帧号命名的 equirect JPEG 与 JSONL 进度/错误。不要在第一版加入 AI stitch、降噪、调色、镜头保护壳、实时预览或自定义编码；这些是可选质量功能，不是导入的前置条件。

### M2：生成对 COLMAP 合法的透视帧

对于每一张全景 JPEG，调用已有 FFmpeg：`v360=input=equirect:output=perspective`，固定输出尺寸、FOV 和方向。首个实验应使用固定的 4 个水平视角（例如 0/90/180/270 度），且相邻视角留重叠；每个项目固定相同参数，以维持 `single_camera=1` 的内参假设。

这一步需要真实素材的 COLMAP 验证，不能仅以 JPEG 数量判定成功。验收应至少记录注册率、稀疏点数、重投影误差、每视角覆盖和最终 PLY/训练结果，并与“从 Insta360 Studio 导出的同一段透视视频”基线比较。若四视角的时序匹配不稳定，最小回退是只用一个固定透视视角；它功能较窄，但不会把等距柱状图错误送进 SfM。

### M3：产品化（仅在 M0–M2 证据通过后）

加入 UI 型号/拼接预设、SDK 健康检查、许可证展示、安装器资源与清晰的“原始素材不会移动或删除”提示。SDK 输出、代理与投影帧都应置于每次独立尝试目录；失败/取消保留日志并拒绝混用残缺 JPEG，沿用当前管线的可恢复原则。

## 不应做的事

- 不只把文件过滤器扩为 `insv`：`ffprobe`/FFmpeg 不能替代官方双鱼眼拼接与陀螺/镜头元数据处理。
- 不先导出完整高码率 MP4 再重复抽帧：SDK 已支持指定帧的图像序列导出，完整视频会增加存储、耗时与时间轴风险。
- 不直接将 2:1 equirect JPEG 输入当前 COLMAP `SIMPLE_RADIAL`。
- 不在未经许可时将 SDK、models 或 DLL 纳入仓库、发布物或源码 push。

## 需要用户提供后才能进入实现

1. 获批下载的 Windows MediaSDK 包、版本和许可/再分发条款；
2. 至少一组可测试的 `.insv`（以及配对文件，如有）；
3. 目标相机型号与是否有镜头保护壳/潜水壳；
4. 可接受的优先级：先要“可重建的固定透视视图”，还是要投入更大范围的多视角全景 SfM 优化。

## 本次检查边界

本次使用了用户提供的本机 SDK 与样本完成 M0–M2 实测，并修改了最小导入、设置和状态报告代码；没有复制 SDK、models 或 DLL。工作区原有的 `scripts/run-gsplat-autoselect.ps1` 改动保持未触碰。
