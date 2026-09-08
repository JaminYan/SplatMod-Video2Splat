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

本次未安装、未复制、未调用任何 Insta360 SDK，也未修改现有源代码。已只读确认本机 GPU/驱动、现有 FFmpeg `v360` 能力和当前导入管线；工作区原有的 `scripts/run-gsplat-autoselect.ps1`、`src-tauri/src/pipeline/runner.rs`、`src-tauri/src/video/mod.rs` 改动保持未触碰。
