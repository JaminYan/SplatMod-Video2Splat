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

## M4 大空间全景训练参数调研（2026-09-08）

### 外部资料给出的边界

- Nerfstudio 的官方 360 数据流程建议：每个等距柱状全景默认生成 **8 个**透视视图；细节不足或 COLMAP 对齐困难时提高到 **14 个**。视频的全景帧数建议为 `3 × 视频秒数`，并明确说明这只是经验值，不是硬规则。见[官方 360 数据文档](https://github.com/nerfstudio-project/nerfstudio/blob/main/docs/quickstart/custom_dataset.md)。
- 原始 3DGS 官方实现默认使用全部训练图像；只有显式启用 `--eval` 才划出测试集。官方建议的训练图像目标宽度约为 **1–1.6K**，默认 30,000 次迭代。见[官方实现参数说明](https://github.com/graphdeco-inria/gaussian-splatting)。
- COLMAP 官方文档建议：视频序列使用 Sequential Matching；达到数千图像时改用 Vocabulary Tree，不应把全量图像做 Exhaustive Matching。见[COLMAP 官方教程](https://colmap.github.io/tutorial.html)。

### 对本次 104.14 秒素材的换算

当前任务只有 112 张全景时间帧、110 张注册帧；按官方 `3 × 秒数` 经验值，时间帧目标约为 **312 张**。当前每个全景只投影一个固定前视角，因此训练视图仍只有 110 张，且训练划分进一步只使用 55 张。

这两个维度不能混为一个数字（下表是 M4-A 前的单视角基线）：

| 维度 | 当前实现 | 大空间首轮建议 | 高细节建议 |
| --- | ---: | ---: | ---: |
| 等距柱状时间帧 | 112 | 约 200–300（约 2–3 fps） | 约 312（约 3 fps） |
| 每个全景透视视图 | 1 | 4（环绕水平面，项目内推断的最低覆盖） | 8；只有对齐/细节仍不足再试 14 |
| 透视训练视图总量 | 110 | 约 800–1,200 | 约 2,000–2,500 |
| 训练/验证划分 | 55 / 11，另排除 44 | 80–90% / 10–20%，按时间块留验证 | 同左；同一全景的多个投影必须一起划分 |

表中 4/8/14 视图是从官方 360 流程映射到本项目固定透视投影的工程建议，不是论文保证值。先用 2 fps × 4 视图验证 COLMAP 图和训练覆盖，再扩到 3 fps × 8 视图，比一次生成 4,000+ 图更可控。

### 分辨率与精细档位

当前 `balanced` 的 1024 长边是项目自定义的普通视频折中值，不是全景大空间的合理上限。官方 3DGS 的 1–1.6K 目标说明了为什么当前 1024 会损失细节；本项目的透视代理为 1920×1440，因此 1024 实际约为 1024×768。

建议把全景大空间单独看成训练 profile，而不是直接沿用普通视频档位：

| profile | 长边 | 迭代 | gsplat cap | 用途 |
| --- | ---: | ---: | ---: | --- |
| panorama-balanced | 1536 | 20,000–30,000 | 2M 起步 | 2 fps × 4 视图的首轮质量/耗时验证 |
| panorama-fine | 1920 | 30,000 | Auto；24GB 卡预计受安全上限约 3M | 3 fps × 8 视图，接受更长训练 |

本机 RTX 5090 D v2 的本次任务峰值只有约 2.5GB，说明 1024/55 张是当前策略限制而不是本次显存耗尽；但把视图数提高到千级、分辨率提高到 1536/1920 后仍应做一次真实显存与质量 A/B，不能从这次峰值直接承诺性能。

### 对代码的直接结论

1. 全景输入不应继续使用当前 `temporal_validation_split` 的 55/110 训练比例；保护帧可以保留用于评估隔离，但不应全部从训练中永久丢弃。
2. 全景路径需要先增加每个 equirect 帧的多方位透视投影，再谈增加 gsplat splat cap；单纯把 1M 改成 4M 不能弥补视角覆盖不足。
3. “精细”档位应至少保持 1920/30k，并配合更密的时间帧与多视角投影；普通视频的 1024/15k 不能作为全景大空间基线。

### M4-A 已落地：先恢复全景训练覆盖

本阶段只改训练入口，不改变全景帧数量和投影数量：

- `.insv` 的 `balanced` 使用 1536 长边、至少 20,000 次迭代；`high` 使用 1920
  长边、至少 30,000 次迭代；普通视频档位保持不变。
- gsplat 请求显式携带 `panorama=true`。单一等距柱状时间流不再为验证帧额外排除
  保护带，因此 110 张注册帧会按 99 张训练 + 11 张验证参与训练，而不是原来的
  55 张训练 + 44 张保护帧 + 11 张验证。
- M4-A 当时仍是每个全景一个透视视图；M4-B 已加入固定四视角和分组验证，
  8/14 视角与显式源帧清单仍留待后续。

### M4-B 已落地：4 视角固定投影与分组验证

- MediaSDK 拼接后，FFmpeg `v360` 固定输出 `yaw=0/90/180/-90` 四个水平视角，
  按同一时间点轮询排列，保持现有透视图的 `1920×1440 / FOV 100×75` 内参契约。
- 全景固定抽帧按视角数放大采样率：`balanced` 为 12 fps（源时间密度约 3 fps），
  `high` 为 16 fps（源时间密度约 4 fps）；全景路径暂不走普通视频的自适应代理门。
- gsplat 请求记录 `panoramaViewCount=4`，验证索引以四视角为一组选择相同源时间，
  不会把同一时间点的不同方向分别放进训练和验证。

这一步已经把视图覆盖从单一前视角提升到四方向，但尚未实现 8/14 视角、按源帧
ID 的显式清单，也没有用真实大场景重新跑 COLMAP/训练质量 A/B；这些是下一轮验收门。

### M4-C 已落地：大场景 cap 下限

真实任务 `A:\tmp\Splatcam\20260908VID` 的 `request.json` 为 1M cap，训练逻辑
曾达到 `logicalSplats=1,000,000`，最终导出 844,700 个 splat。因此 `.insv` 的
`balanced/high` 现在把 1M 配置提升为 2M 最低上限；`fast`、普通视频以及用户
选择的 2M/4M/Auto 不变。需要重新运行任务才能测量 2M 对最终模型和耗时的影响。

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

### M4-C 实测回归：2M cap 已生效，但导出仍约 1M

任务目录：`A:\tmp\Splatcam\20260908VID20260830`。

- `request.json`：`panorama=true`、`panoramaViewCount=4`、`maxSplats=2,000,000`；
- gsplat `ready`：`configuredMaxSplats=2,000,000`、`maxSplats=2,000,000`，24GB 显存预算未再把它降回 1M；
- 训练内部达到 `logicalSplats=2,000,000`，但 PLY 导出为 `999,883` 个有效 splat；
- 原因不是 cap 仍为 1M，而是导出按 `sigmoid(opacity) > 0.005` 过滤透明/死亡点；MCMC 在训练期间保留逻辑容量，最终仍有约一半点低于导出阈值；
- 最终验证：PSNR `19.2781`、SSIM `0.7239`、峰值显存 `6154MB`，训练约 `19.3min`。

因此下一轮不应继续盲目提高 cap；应做固定素材的单变量 A/B：保持 2M cap、帧和步数不变，只比较 MCMC `min_opacity=0.005` 与较低阈值，检查有效点数、PSNR/SSIM、浮点噪声和文件大小，再决定是否为全景单独降低阈值。当前没有擅自改变该阈值。

### M4-C 临时 adapter A/B：`mcmcMinOpacity=0.0025`

在同一任务的独立目录 `A:\tmp\Splatcam\20260908VID20260830\work\gsplat-mcmc0025` 中完成了一次真实 CUDA 训练。除 `mcmcMinOpacity` 外，帧、seed、2M cap、1536 分辨率、20k steps 和 4 视角均保持不变。

- 有效 splat：`999,883 -> 1,194,472`（`+19.5%`）；
- PLY：约 `236.5MB -> 282.5MB`（`+19.5%`）；
- PSNR：`19.2781 -> 19.4098`（`+0.1317dB`）；
- SSIM：`0.7239 -> 0.7301`（`+0.0062`）；
- 峰值显存：`6154 -> 6107MB`；训练时间：约 `+7.7%`。

这次 A/B 通过：没有出现质量下降，且有效模型规模明显增加。当前建议将全景 `balanced/high` 的 adapter 临时默认阈值设为 `0.0025`，但仍保留普通视频和 `fast` 的 `0.005`；正式固化前还应检查一组不同光照/运动素材的浮点噪声。

### M4-D 已固化：全景 opacity 阈值透传

根据上述 A/B，正式训练请求现在会自动写入：全景 `balanced/high` 为
`mcmcMinOpacity=0.0025`，普通视频和 `fast` 为 `0.005`。本次只透传已验证参数，未改变
分辨率、训练步数或普通视频路径。

### M4-E 实测：`VID_20260914_115515_00_060.insv`

使用正式 `PipelineRunner` 路径完成一次独立全流程，输出目录为
`A:\\tmp\\Splatcam\\20260914\\diag-ff88d09d-0d98-42c0-b812-70068ab14c9f`。

- 素材：35.395 秒、200 FPS、1920x1440，MediaSDK 拼接后按四视角透视投影；
- 采样：283 帧，COLMAP 注册 `283/283`，注册率 `100%`，重建点 `93,280`；
- 训练请求：全景 balanced，`maxSplats=2,000,000`、`1536` 输入、`20,000` steps、`mcmcMinOpacity=0.0025`；
- 训练内部逻辑点 `1,742,263`，最终导出有效 splat `1,095,442`，因此本片没有撞到 2M cap；
- 质量：PSNR `20.3059`、SSIM `0.7900`；峰值显存 `5,702MB`；训练约 `20.2min`，全流程约 `35.0min`；
- 结论：MediaSDK → 四视角投影 → COLMAP → gsplat 链路稳定。有效点数低于逻辑点数仍主要来自 opacity 导出过滤，不是总量上限。视觉复核能识别场景，但高光和边缘有明显径向拖影/过度平滑；源帧本身清晰，因此下一轮优先验证更高采样率与运动片段，而不是继续提高 cap。再用低光/快速运动素材确认 `0.0025` 是否会引入浮点噪声。与上一条素材的 PSNR/SSIM 不宜直接视为同场景增益。

### M4-F 实测：12 FPS 手动运行

任务目录：`A:\\tmp\\Splatcam\\20260917_VID_20260914_115515_00_060`。

- 计划采样 `12 FPS`，候选 `425` 帧；pHash 去重后保留 `300` 帧，移除 `125` 张近重复；
- COLMAP 注册 `295/300`，注册率 `98.33%`，重建点 `87,474`；本次实际 BA 后端为 Caspar，上一条 8 FPS 实测为 Ceres，因此不能把所有变化归因于采样率；
- 训练内部达到 `logicalSplats=2,000,000`，说明当前全景最低 2M cap 已成为限制；最终导出 `1,348,476` 个有效 splat，PLY 约 `318.9MiB`；
- 质量：PSNR `21.5189`、SSIM `0.80545`、峰值显存 `6,104MB`，训练约 `20.1min`，全流程约 `26.2min`；相对同素材 8 FPS 实测约为 `+1.213dB`、`+0.0154 SSIM`、有效 splat `+23.1%`；
- 视觉复核仍有明显径向拖影和几何拉伸。12 FPS 确实改善了指标和有效点数，但没有解决位姿/运动造成的模型变形；下一步应在固定 BA 后端的前提下单变量测试 16 FPS，或单独把 cap 提到 4M，不能同时改变两项。

### M4-G 对照：MipMap Desktop 同素材 70 张输入

对照目录：`A:\\mipmap-desktop\\2d2e7e2d-a9fc-434a-b4a4-2ce16ca52141`，项目为
`360\\360-20260918`，输入与 M4-E/F 为同一段 `VID_20260914_115515_00_060.insv`。

- MipMap 实际使用 `stream_0`、`stream_1` 两路原始鱼眼图，各 `35` 张，合计 `70` 张；它们是 `35` 个同步时刻的双相机观测，不是单相机视频只抽了 70 帧。
- 每张图保留 `3840x3840` 原始鱼眼像素；`photos.json`/`frame_meta.json` 保留 `projection_model=1`、每路 `camera_id`、内参畸变参数、`rig_pose`、时间戳、IMU orientation/accel。MipMap 报告确认 `input_camera_count=2`、`input_image_count=70`。
- MipMap 参数为 `resolution_level=1`、`machine_learning=false`、`max_pnt_count_mode=auto`、`remove_moving_object=true`；不是把当前普通视频训练参数缩小后运行。
- MipMap 结果 `gs.ply` 有 `1,183,467` 个 splat，另有独立 `sky.ply`；输出不是靠更大的点数取胜。报告记录场景面积 `30.049562`、GSD `0.002671`、tie points `6543`、重投影 RMSE `1.083716`。
- 与当前链路的根本差异：本项目先经 MediaSDK 变成 2:1 equirectangular，再经 FFmpeg `v360` 生成 4 个 `1920x1440` 透视视角，并把它们交错成单一序列交给普通 COLMAP；过程中丢失了双镜头相机 ID、刚性基线、鱼眼内参和 IMU 先验，还增加了一次拼接/重投影采样。
- 因此 MipMap 的质量优势主要来自“原始双相机 + 标定/rig/IMU 约束 + 原生鱼眼细节 + 动态物体移除”，不是来自 70 张这个数字，也不是简单的 splat cap 差异。当前四视角路径应视为兼容性 fallback，而不是与厂商 rig-aware 重建等价的质量路径。

结论：继续把全景采样从 12/16 FPS 往上加，或单纯提高 splat cap，不能消除当前径向拖影和几何拉伸。要接近 MipMap，需要新增“原始双鱼眼帧及其 metadata 的 rig-aware 重建边界”；最小实验应直接使用这 70 张原始图和 `photos.json`/`frame_meta.json` 做隔离 adapter A/B，不能再把它们先拼成 equirectangular 后当普通单目视频处理。

### M5 第一阶段实现：原始双流 + COLMAP rig

当前代码已加入隔离的双流入口：

- FFprobe 检测 INSV 是否包含两路视频；对双流文件直接用 FFmpeg 分别导出
  `frames/rig1/camera1/frame_*.jpg` 和 `frames/rig1/camera2/frame_*.jpg`，不再先经过
  MediaSDK/equirectangular/v360；普通单流素材仍回退旧路径。
- 抽帧密度暂定 Draft `0.5 FPS`、Standard `1 FPS`、High `2 FPS`，与 MipMap 的“同步双相机时刻”模型一致；两路文件名完全相同，保证同一时刻能组成一个 rig frame。
- COLMAP 特征提取切换为 `single_camera_per_folder=1` + `OPENCV_FISHEYE`；双流使用 exhaustive matching，先做无 rig 初始重建，再由 `rig_configurator` 从初始结果估计镜头相对位姿，最后以 `ba_refine_sensor_from_rig=0` 进行 rig 约束重建。
- 标准训练输入现在递归复制嵌套相机目录，保留 COLMAP 的多相机文件名；gsplat 已能读取每张图的 camera ID 和最终组合位姿。

本阶段尚未接入 Insta360 私有 IMU/标定元数据，也未把 rig 分支接入 CASPAR；先用 COLMAP 从真实双流匹配估计基线，验证几何质量，再决定是否解析/导入厂商 metadata。MediaSDK 拼接路径仍保留为兼容回退。

### M5 第二阶段：MVS 深度初始化与原生 FishEye 接口

- 双鱼眼 + gsplat 训练前新增 `image_undistorter -> patch_match_stereo -> stereo_fusion`；MVS 去畸变上限固定为 `2560px`，融合结果写入 `gsplat/mvs-fused.ply`，作为 Gaussian 初始化点云，而不是只使用 `points3D.bin`。
- gsplat adapter 新增 COLMAP fused PLY（二进制/ASCII）读取，并支持读取 `OPENCV_FISHEYE` 的四个径向畸变参数；启用 `nativeFisheye` 时走 `camera_model="fisheye"` + `with_ut=true`，避免把原始鱼眼误当针孔。
- 当前隔离运行时 `gsplat.has_3dgut()` 为 `false`，且本机重编译因 nvcc 找不到可执行 host compiler 未完成。因此正式流水线暂时使用 MVS 的针孔工作区 + 融合点云；原生 FishEye 代码路径保留，待 `GSPLAT_BUILD_3DGUT=1` 的 csrc.pyd 就位后再打开。
