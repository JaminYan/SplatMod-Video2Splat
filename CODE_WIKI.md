# OOOSplat 代码 Wiki

> 源仓库：<https://github.com/ooolabdev/ooosplat>
> 原项目：<https://github.com/ooolabdev/ooosplat>
> 本地升级版：2026-08-21（`ooo-splat` 0.3.0）
> 文档目的：说明当前本地升级版的架构、关键行为、运行方式以及与原项目的差异。

---

## 1. 项目概览

OOOSplat 是一款仅面向 Windows 的桌面应用，用于将录制的 MP4/MOV 视频转换为 3D Gaussian Splatting 场景（`final.ply`）。它把三个本地引擎——**FFmpeg/FFprobe**、可切换的 **COLMAP CPU/CUDA** 与 **Brush**——一并调度。GUI 由 Tauri 2 承载，内嵌一个 React 19 单页应用。

| 项目 | 取值 |
| --- | --- |
| 应用名 | `OOOSplat`（Rust crate / package 标识：`ooo-splat`，Tauri 标识：`studio.ooo.splat`） |
| 版本 | `0.3.0`（项目元数据；GUI 顶栏标注用户面向的发布为 **LOCAL / 0.1.0**） |
| 许可证 | Apache-2.0 |
| 目标平台 | Windows 10/11 x64，WebView2 Runtime |
| 框架 | Tauri 2、React 19、TypeScript、Vite 8、Zustand 5 |
| 原生流水线 | Tokio（异步运行时）、FFmpeg/FFprobe、COLMAP CPU/CUDA、Brush（Apache-2.0） |

### 1.1 相对原项目的升级点

| 领域 | 原项目基线 | 当前本地升级版 |
| --- | --- | --- |
| 候选抽帧 | 按源视频 FPS 比例抽帧 | 固定候选速率：快速 1、均衡 2、精细 4 FPS |
| 画面选择 | 抽帧后直接进入 COLMAP | Rayon 有界并行计算 pHash/Laplacian；顺序提交连续近重复分组与删除，保留更清晰的代表帧 |
| COLMAP | CPU/no-CUDA 固定路径 | CPU/CUDA 可切换；CUDA 实际启用 SIFT 与 SIFT_BRUTEFORCE GPU 参数，CPU 显式禁用 GPU；两条路径输出隔离 |
| Brush 输入 | 历史实现依赖目录式数据集 | 显式生成并校验 `dataset.zip`，包含 `images/` 与 `sparse/0/` COLMAP 布局 |
| 进度体验 | 进程日志可覆盖当前提示，空输出无运行提示 | 单调事件序号；日志/心跳继承阶段进度；全任务总进度与阶段计数分开显示 |
| 项目查看 | 仅在资源管理器定位 PLY | 支持从历史项目直接启动 Brush 3D 查看器 |

上述升级均在本地工作区实现；它们不代表已合并到原项目仓库。

### 1.2 仓库结构

```
ooosplat/
├── assets/                       # 静态 SVG 资源（例如 app-icon.svg）
├── engines/manifest.json         # 锁定的引擎下载地址、哈希、CPU 策略
├── licenses/                     # 第三方声明与各引擎许可证文本
├── scripts/                      # PowerShell 工具：setup / verify
├── src/                          # React + TypeScript 前端
│   ├── app/App.tsx
│   ├── lib/backend.ts
│   ├── stores/appStore.ts
│   ├── types/pipeline.ts
│   ├── main.tsx
│   └── styles.css
├── src-tauri/                    # Rust 后端（库 + 两个二进制）
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   ├── capabilities/default.json
│   └── src/
│       ├── main.rs / lib.rs / error.rs
│       ├── bin/splatstudio.rs    # 诊断用 CLI
│       ├── commands/             # Tauri IPC 处理器
│       ├── engines/              # FFmpeg / FFprobe / COLMAP / Brush 适配层
│       ├── pipeline/             # 流程编排、事件、状态机
│       ├── presets/              # 质量档位
│       ├── process/              # ProcessManager 与 Windows Job Object
│       ├── project/              # 项目布局、元数据、目录索引
│       ├── reconstruction/       # 稀疏重建校验器 + PLY 解析器
│       └── video/                # FFprobe JSON 解析、抽帧规划
├── index.html
├── package.json
├── vite.config.ts
├── tsconfig*.json
├── LICENSE / NOTICE / TRADEMARK_POLICY.md / GENERATED_OUTPUTS.md
└── README.md
```

---

## 2. 整体架构

```
┌─────────────────────────────────────────────────────────────┐
│                   React 19 UI（src/）                       │
│   src/app/App.tsx · src/stores/appStore.ts · src/lib/...    │
│   —— 监听 "pipeline-event"（Tauri 事件通道）                 │
└─────────────────────────────────────────────────────────────┘
                          ▲ invoke / listen
                          ▼
┌─────────────────────────────────────────────────────────────┐
│           Tauri 2 宿主（src-tauri/src/lib.rs）              │
│  • PipelineController（同一时刻仅一条流水线在跑）           │
│  • commands：check_engines、probe_and_plan、                │
│             start_pipeline、cancel_pipeline、               │
│             export_ply、get_project_overview、              │
│             set_projects_root、delete_project              │
└─────────────────────────────────────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────────┐
│              流水线执行器（pipeline/runner.rs）             │
│   probe → plan → extract → pHash/select → feature → match  │
│   → map → validate → ZIP → Brush train → 发布 final.ply    │
└─────────────────────────────────────────────────────────────┘
       │              │              │             │
       ▼              ▼              ▼             ▼
   FFprobe         FFmpeg          COLMAP         Brush
   (engines/      (engines/       (engines/      (engines/
    ffprobe.rs)    ffmpeg.rs)     colmap.rs)     brush.rs)
       └──────────────┴──────────────┴─────────────┘
                          │
                          ▼
        process::ProcessManager（process/mod.rs）
        —— tokio 子进程 + Windows Job Object + 观察者
```

### 2.1 前端 ⇄ 后端契约

`src/lib/backend.ts` 是唯一与 Tauri 对话的模块。它在 `invoke` 与 `listen` 之上封装了一组薄包装，并提供三个对话框辅助函数：

| 函数 | Tauri 命令 / 事件 | 使用位置 |
| --- | --- | --- |
| `selectVideo()` | `dialog:allow-open`（过滤 mp4 / mov） | UI 中“选择 MP4 或 MOV 视频”按钮 |
| `selectProjectsRoot(current)` | `dialog:allow-open`（目录） | UI 项目根目录选择器 |
| `checkEngines()` | `check_engines` | 初始挂载 + 顶栏状态 |
| `probeAndPlan(path, quality)` | `probe_and_plan` | 选择视频或切换档位之后 |
| `getProjectOverview()` | `get_project_overview` | 刷新历史任务列表 |
| `setProjectsRoot(root)` | `set_projects_root` | 保存所选根目录并写入 `%LOCALAPPDATA%\SplatStudio\settings.json` |
| `startPipeline(path, quality, root)` | `start_pipeline` | “开始生成”按钮 |
| `cancelPipeline()` | `cancel_pipeline` | 实时面板里的取消按钮 |
| `onPipelineEvent(handler)` | 监听 `pipeline-event` | 把事件流接入 UI |
| `revealProject(summary)` | `opener:allow-reveal-item-in-dir` | “在资源管理器中显示”按钮 |
| `confirmAndDeleteProject(summary)` | 原生确认对话框 + `delete_project` | 历史行上的“删除”按钮 |
| `exportPly(result)` | 原生保存对话框 + `export_ply` | 导出最终 PLY（UI 菜单） |

Tauri 能力声明（`src-tauri/capabilities/default.json`）仅授予恰好所需的权限：`core:default`、`dialog:allow-open`、`dialog:allow-save`、`dialog:allow-confirm`、`opener:allow-reveal-item-in-dir`。

### 2.2 Pipeline 控制器生命周期

`commands::PipelineController`（`src-tauri/src/commands/mod.rs`）是一个 `tokio::sync::Mutex<Option<Arc<PipelineRunner>>>`，保证同一时刻最多只有一条流水线在运行。启动时，`get_project_overview` 会在互斥锁为空（即进程不再活跃）的前提下，把仍处于 `Running` 状态的项目改写为 `Interrupted`。

---

## 3. 后端（Rust）模块

### 3.1 `src-tauri/src/lib.rs` — Tauri 入口

```rust
tauri::Builder::default()
    .plugin(tauri_plugin_dialog::init())
    .plugin(tauri_plugin_opener::init())
    .manage(commands::PipelineController::default())
    .invoke_handler(tauri::generate_handler![...])
    .run(tauri::generate_context!())
```

已注册命令（定义于 `commands/mod.rs`）：

| 命令 | 作用 |
| --- | --- |
| `check_engines` | 调用 `EnginePaths::check_all()` 并返回 `Vec<EngineStatus>` |
| `probe_and_plan(path, quality)` | 调用 `ffprobe::probe_video` + `UniformRatioFrameSelection::create_plan` |
| `start_pipeline(path, quality, projects_root)` | 构造一个会向 `pipeline-event` 通道发送事件的 `PipelineRunner`，注册到控制器上，然后执行 `runner.generate()` |
| `cancel_pipeline` | 触发 `runner.cancel()` → `ProcessManager::cancel()` → 取消令牌 + Windows Job Object 终止 |
| `export_ply(source, destination)` | 校验源文件是已注册的 `final.ply`，通过 Tokio 复制 |
| `get_project_overview` | 返回 `ProjectOverview`（设置 + 摘要） |
| `set_projects_root(root)` | 校验可写性，持久化 `%LOCALAPPDATA%\SplatStudio\settings.json` |
| `delete_project(id)` | 在流水线活跃时拒绝，解析 UUID 后委派给 `catalog::delete_project`（使用 `trash` crate） |

### 3.2 `src-tauri/src/main.rs`

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
fn main() { ooo_splat::run_app(); }
```

`#[bin]` `splatstudio`（位于 `bin/splatstudio.rs`）是一个独立的诊断 CLI，复用同一套库，便于无界面测试。

### 3.3 `error.rs` — 类型化错误

`SplatError` 各变体（每个都附带中文提示）：

| 变体 | 触发条件 |
| --- | --- |
| `EngineMissing(String)` | 引擎路径缺失 |
| `EngineStart { engine, detail }` | 子进程启动失败 |
| `InvalidVideo(String)` | FFprobe 拒绝 / 不支持的扩展名 / 元数据无效 |
| `InvalidPath(PathBuf)` | 路径不存在或扩展名错误 |
| `Cancelled` | 由 `ProcessManager` 传递的取消 |
| `Process(String)` | 外部命令执行失败（非 0 退出码） |
| `UnsupportedEngine(String)` | 当前选择的 COLMAP CPU/CUDA 后端不满足运行时校验 |
| `Io(#[from] std::io::Error)` | 文件系统错误 |
| `Json(#[from] serde_json::Error)` | 解析错误 |

实现 `serde::Serialize`（序列化为可读字符串），保证前端拿到可操作的提示信息。

### 3.4 `commands/mod.rs` — IPC 层

除命令清单外的要点：

* `paths_for_app(app)` 调用 `EnginePaths::discover(app.path().resource_dir()...)`。
* `start_pipeline` 在执行器返回错误时会发出一个合成的 `PipelineEvent::mapped(Failed | Cancelled, 1.0, error.to_string())`，以便 UI 清理实时状态。无论成功失败，控制器互斥锁都会被清空。
* `delete_project` 在有活跃流水线时被拒绝，避免竞态。

### 3.5 `engines/` — 原生引擎适配层

```
engines/
├── mod.rs            # 从 health 重导出 EngineKind/EnginePaths/EngineStatus
├── health.rs         # EnginePaths + 各引擎健康检查 + COLMAP CPU/CUDA 策略
├── ffprobe.rs        # probe_video → VideoInfo
├── ffmpeg.rs         # extract_uniform_frames（使用 fps 滤镜抽帧）
├── colmap.rs         # feature_extractor / sequential_matcher / mapper
└── brush.rs          # train（Gaussian Splatting 训练）
```

#### `engines/health.rs`

* `EngineKind { Ffmpeg, Ffprobe, Colmap, Brush }`。
* `EngineStatus { kind, path, exists, can_start, version, cpu_only, detail }`。
* `EnginePaths::from_root(root)` 与 `EnginePaths::discover(resource_dir)` 解析引擎目录。发现顺序：`OOOSPLAT_ENGINE_DIR` 环境变量 → Tauri 资源目录 + `engines` → `cwd/engines` → `cwd/../engines` → 默认 `cwd/engines`。
* `check_all()` 并发执行：FFmpeg/FFprobe/Brush 的 `check_basic`（`-version` / `--help`），以及更重的 `check_colmap`（依次运行 `feature_extractor -h`、`sequential_matcher -h`、`mapper -h` 并检查帮助输出）。
* CPU 后端要求帮助输出声明 no-CUDA 且运行目录不含 CUDA 运行时；CUDA 后端要求帮助输出或运行目录证明 CUDA 可用。新设置默认选择 CUDA，已保存的用户选择不会被覆盖。
* `require_cpu_colmap` 或 `require_cuda_colmap` 由 `PipelineRunner::verify_pipeline_engines` 根据当前设置调用，否则拒绝启动。

#### `engines/ffprobe.rs`

`probe_video(exe, input, log_path)` → `VideoInfo`：
* 校验输入文件是否存在。
* 通过 `ProcessManager` 执行 `ffprobe -v error -select_streams v:0 -show_entries ... -of json <input>`。
* 使用 `video::parse_ffprobe_json` 解析结果。

#### `engines/ffmpeg.rs`

`extract_uniform_frames(exe, input, output_directory, plan, log_path, manager, observer)`：
* 目标目录中已存在任何 `.jpg` 时直接拒绝（避免与残缺结果混用）。
* 构造 `fps=<sampling>,scale='min(1920,iw)':'min(1920,ih)':force_original_aspect_ratio=decrease`，加 `-q:v 2`、`-start_number 1`、`-progress pipe:1`，输出模板为 `frame_%06d.jpg`。
* 返回实际产出的 JPEG 帧数。

#### `engines/colmap.rs`

围绕 COLMAP CLI 的特征、匹配和 mapper 包装；实际使用的可执行文件与显式的 `ColmapComputeMode` 共同决定行为：
* CPU：`feature_extractor ... --FeatureExtraction.type SIFT --FeatureExtraction.use_gpu 0`；`sequential_matcher ... --FeatureMatching.type SIFT_BRUTEFORCE --FeatureMatching.use_gpu 0 --SequentialMatching.overlap 10`。
* CUDA：同一组 SIFT/SIFT_BRUTEFORCE 参数，另传 `use_gpu 1` 与 `gpu_index -1`。

每次重建按后端写入 `work/colmap-attempts/cuda/` 或 `work/colmap-attempts/cpu/`，其中包含独立的 `database.db` 和 `colmap.log`；CUDA 运行时故障只会切换到 `cpu-fallback/` 的独立数据库。素材质量失败不会触发 CPU fallback。
* `map` 使用隔离的 `incremental-ceres/sparse/` 或 `incremental-caspar/sparse/` 输出。CUDA 且保留帧数至少 151 时先传入 `--Mapper.ba_local_backend CERES --Mapper.ba_global_backend CASPAR --Mapper.ba_gpu_index -1`：COLMAP 4.1.1 不支持 CASPAR local BA，只在 global BA 使用 CASPAR。CASPAR 退出失败、无可解析模型或注册率低于 50% 时，取消任务除外，会回退到 Ceres。项目的 `colmapExecution` 会保存有效设备、BA 后端和回退原因。
* 设置页以“COLMAP 引擎版本”统一控制 BA：选择 CASPAR CUDA 时保存 `caspar`，选择官方 CUDA 时恢复 `auto`，避免双重选择冲突。CASPAR 面向中大型序列；local BA 始终为 Ceres，global BA 才使用 CASPAR GPU。
* 单元测试固定并验证 CPU 的 `--use_gpu 0`、CUDA 的 `--use_gpu 1`/GPU 索引，以及 Ceres/CASPAR 的 Mapper 参数。

`runner.rs` 向 COLMAP 传递帧目录的绝对路径。COLMAP 4.1.1 的 bitmap loader 已能正确处理 UTF-8/宽字符绝对路径；相对路径会在 OptionManager 的目录校验阶段被拒绝。

#### `engines/brush.rs`

`train(exe, dataset, output_dir, preset, log_path, manager, observer)`：
* 执行 `brush_app --total-steps <iterations> --max-resolution <res> --export-every <iterations> --export-path <out> --export-name final.ply.tmp <dataset>`。
* 同时接受 `final.ply.tmp` 与 `final.ply.tmp.ply` 作为生成文件。

### 3.6 `process/mod.rs` — 进程管理

`ProcessManager` 持有一个 `tokio_util::sync::CancellationToken`；按需可生成子令牌（`child_token()`），但当前实现把同一令牌传给 `tokio::select!`。`run(ProcessSpec)` 的流程：

1. 校验可执行文件路径。
2. 通过 Tokio `Command` 派生子进程，stdout/stderr 接管，`kill_on_drop=true`，Windows 上加 `CREATE_NO_WINDOW`。
3. 在 Windows 上，把子进程纳入 `WindowsJob`（见 `windows_job` 模块），使用 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`。`WindowsJob::assign(process_id)` 完成归属；若失败则杀掉子进程并返回错误。
4. 如指定 `log_path`，打开日志文件，写入可执行文件与参数列表，包成 `Arc<Mutex<File>>`。
5. 启动两个 `pump_stream` 任务（stdout/stderr 各一），分别：
   * 把每一行写入日志文件；
   * 通过 `ProcessObserver` 派发 `ProcessUpdate::Line { stream, line }`。
6. 可选地启动心跳任务，每秒上报一次 `elapsed_ms`。
7. 等待 `child.wait()` 或取消。取消时：Windows 端 `job.terminate()` + `child.kill()`；Linux/macOS 仅 `child.kill()`。返回 `SplatError::Cancelled`。
8. 完成时把退出码与耗时写入日志。

### 3.7 `pipeline/` — 流程编排

```
pipeline/
├── mod.rs            # 重导出事件类型与 PipelineStage
├── event.rs          # PipelineEvent（序号、时间戳、级别、引擎等）
├── state.rs          # PipelineStage 枚举 + can_transition_to() 守卫
├── progress.rs       # stage_progress_range()：把阶段映射到全局进度条的 [start,end] %
└── runner.rs         # PipelineRunner、EventSink、PreparedFrames、PipelineResult
```

* `PipelineStage` 是严格的状态机：`Created → ProbingVideo → PlanningFrames → ExtractingFrames → SelectingFrames → ExtractingFeatures → Matching → Reconstructing → ValidatingReconstruction → TrainingSplats → Exporting → Completed`。任意非终态都可以转向 `Failed` 或 `Cancelled`；`Completed` 不能回退。
* `stage_progress_range()` 固定各阶段在全局进度条上的区间，例如 `ExtractingFrames = 7..20`、`TrainingSplats = 60..98`、`Exporting = 98..100`。
* `EventSink` 为包括子进程观察器在内的全部事件分配单调递增的序号，并以派发互斥锁保证顺序；日志与心跳继承该阶段最近一次有效进度，避免覆盖 UI 的当前计数或显示为 0%。`EventSink::send()` 会结合阶段区间和阶段内进度算出全局进度。
* `PipelineRunner::new(paths, emit)` 暴露：
  * `cancel()` → `process_manager.cancel()`。
  * `verify_pipeline_engines()` —— 调用 `EnginePaths::check_all()` 与 `require_cpu_colmap`。
  * `prepare_frames(...)` —— 执行 FFprobe + 抽帧。
  * `generate(input, quality, projects_root)` → 使用普通的 `ProjectManager`（写入目录索引）。
  * `generate_for_diagnostics(...)` → 使用 `ProjectManager::for_diagnostics`（不写入目录索引；CLI 用）。
* Brush 训练输入为 `work/brush/dataset.zip`，而不是 JSON：归档根目录包含 `images/<frame>.jpg` 和 `sparse/0/{cameras,images,points3D}.bin`（以及同层其他 COLMAP 模型文件）。训练前会原子生成并校验该 ZIP，适配随应用分发的 Brush CLI。
* 整条流水线为每个阶段分配一种 `ObserverMode`：
  * `Ffmpeg` —— 解析 `frame=…` 行；
  * `BracketProgress` —— 通用方括号风格阶段；
  * `Mapper` —— 解析 COLMAP mapper 输出；
  * `Brush` —— 解析 `step=…` 行。
* 重建质量门槛：
  * `>= 80%` → `Good`；
  * `>= 50%` → `Warning`（继续执行，但会提示警告）；
  * `< 50%` → `Failed`（返回 `SplatError::Process`）。
* Brush 的输出（`final.ply.tmp`）由 `ply::inspect_gaussian_ply` 校验后，原子地改名为项目根目录下的 `final.ply`。

### 3.8 `presets/quality.rs` — 质量档位

```rust
Quality::Fast     → 保留 0.30, brush_iterations 8_000,  brush_max_resolution 1_200
Quality::Balanced → 保留 0.50, brush_iterations 15_000, brush_max_resolution 1_600  （默认）
Quality::High     → 保留 1.00, brush_iterations 30_000, brush_max_resolution 2_000
```

实现了 `clap::ValueEnum`、`FromStr`、`Display`，默认值为 `Balanced`。

### 3.9 `video/` — 探测解析与抽帧规划

* `video/probe.rs` —— `parse_ffprobe_json` 解析出 `VideoInfo { duration, width, height, fps, total_frames, codec, rotation }`。优先使用 `nb_frames`，缺失时回退到 `duration * fps`。拒绝时长 `<0.25 s` 的视频和缺失视频流的情况。同时处理 `side_data_list[].rotation` 与 `tags.rotate`。
* `video/frame_plan.rs` —— `UniformRatioFrameSelection::create_plan(video, preset)` 返回 `FramePlan`。画质档位使用明确的候选 FPS（快速 1、均衡 2、精细 4），而不是源 FPS 倍数，避免对 30/60 FPS 视频近乎逐帧导出。
* `video/select.rs` —— 抽帧后先以最多 8 个 Rayon 工作线程并行计算 32×32 灰度 DCT pHash 与缩小灰度图 Laplacian 方差；全部指标成功后才按帧名顺序生成选择计划并删除近重复帧。清晰度相等时保留前一帧，指标失败时零删除。筛选统计写入 `state.json`，最终 `frames/` 目录只保留送入 COLMAP 的图像。

### 3.10 `project/` — 项目布局、元数据、目录索引

* `manager.rs` —— `ProjectManager` 创建 `<root>/<yyyyMMdd-HHmmss>_<sanitized_video_stem>/`，子目录包含 `source/`、`work/frames/`、`work/colmap/`、`work/brush/`、`logs/`。文件名清洗规则：剥离 `<>:"/\|?*` 与控制字符，去掉末尾的 `.`/` `，为空则回退为 `"project"`，最长 64 字符。冲突时附加 `-2`、`-3` … 直到 `-9999`。`validate_video_path` 仅接受 `.mp4` 或 `.mov`。`atomic_write_json` 先写 `<file>.json.tmp` 并 fsync，再通过 Windows 的 `MoveFileExW(MOVEFILE_REPLACE_EXISTING|MOVEFILE_WRITE_THROUGH)` 或 Unix 的 `std::fs::rename` 原子替换。
* `metadata.rs` —— `ProjectMetadata`、`ProjectStatus { Running, Completed, Failed, Cancelled, Interrupted }`、`ProjectOutput`、`PipelineStateFile`（流水线快照）、`FrameState`。`PROJECT_APP_ID = "studio.ooo.splat"`。
* `catalog.rs` —— `AppSettings`（projects_root、schema_version=1）、`ProjectIndex`（id+路径）、`ProjectSummary`、`ProjectOverview`。数据存放在 `%LOCALAPPDATA%\SplatStudio\settings.json` 与 `%LOCALAPPDATA%\SplatStudio\project-index.json`。`default_projects_root()` → `Documents/SplatStudio/Projects`。
  * `get_overview()` 把索引里的路径与默认根、用户根的目录扫描结果合并去重，再按 `completed_at` 排序。
  * `validate_registered_final_ply()` 校验导出的 PLY 来自已注册的项目根。
  * `delete_project()` 强制所有权校验（`app_id == PROJECT_APP_ID` 或目录名等于 UUID），在 `spawn_blocking` 中调用 `trash::delete_all`。

### 3.11 `reconstruction/` — 重建校验与 PLY 解析

* `validator.rs` —— `ReconstructionValidator::validate(frames, sparse_model)` 读取 `cameras.bin`、`images.bin`、`points3D.bin`，要求每个都大于 8 字节，统计 `.jpg/.jpeg` 帧数，读取 `images.bin` 与 `points3D.bin` 中的 u64 计数，按照 0.80 / 0.50 的阈值判定 `Good/Warning/Failed`。
* `ply.rs` —— `inspect_gaussian_ply(path)` 读取前 256 KiB，要求头部包含 `ply\n`（或 `ply\r\n`）、`end_header`、`element vertex <N>`，并具备 Gaussian 必备属性（`x`、`y`、`z`、`f_dc_0`、`opacity`、`scale_0`、`rot_0`）。返回 `PlyInfo { file_size, splat_count }`。

### 3.12 CLI 二进制 `bin/splatstudio.rs`

通过 `cargo run --manifest-path src-tauri/Cargo.toml --bin splatstudio` 运行，子命令：

| 子命令 | 行为 |
| --- | --- |
| `health` | 打印 `EnginePaths::check_all()` 的 JSON |
| `probe <input>` | `probe_video` → JSON |
| `plan <input> --quality <q>` | 构建并打印 `FramePlan`，不抽帧 |
| `extract <input> <output> --quality <q>` | 直接执行 FFmpeg 抽帧 |
| `generate <input> --projects-root <root> --quality <q>` | 运行完整流水线（调用 `generate_for_diagnostics`） |

全局参数 `--engine-dir <path>`（或环境变量 `OOOSPLAT_ENGINE_DIR`）会覆盖 `EnginePaths::discover`。

---

## 4. 前端（TypeScript / React）

### 4.1 `src/main.tsx`

标准的 React 19 入口：`createRoot(...).render(<StrictMode><App/></StrictMode>)`，并导入 `./styles.css`。

### 4.2 `src/types/pipeline.ts`

Rust 类型的 TypeScript 镜像：

| 类型 | 说明 |
| --- | --- |
| `Quality = "fast" \| "balanced" \| "high"` | |
| `EngineKind = "ffmpeg" \| "ffprobe" \| "colmap" \| "brush"` | |
| `RunPhase = "idle" \| "analyzing" \| "running" \| "completed" \| "failed" \| "cancelled"` | 仅 UI 使用 |
| `ProjectStatus` | 镜像 Rust 枚举 |
| `EngineStatus` | 镜像 Rust 结构体（camelCase） |
| `VideoInfo` | FFprobe 结果 |
| `FramePlan` | 由档位推出的 UI 计划 |
| `PipelineEvent` | 通过 `listen("pipeline-event", ...)` 订阅 |
| `PipelineResult` | `start_pipeline` 的返回值 |
| `ProjectSummary`、`ProjectOverview` | `get_project_overview` 的返回值 |

### 4.3 `src/stores/appStore.ts`（Zustand）

核心状态字段：

```ts
videoPath, projectsRoot, projects, quality, video, plan, engines,
phase, progress, progressMessage, latestEvent, events[], result, error
```

关键行为：

* `setVideoPath(null)` 清空 video/plan/result/error/progress/phase。
* `setQuality(q)` 清空 plan/result/error（切换档位即作废已有分析）。
* `beginRun()` 把 phase 重置为 `running`，并清空 events、latestEvent、result、error。
* `receiveEvent(event)`：
  * 丢弃 `sequence <= latest.sequence` 的事件，避免乱序抖动。
   * 后端进度为 `[0,1]`，前端先转换为百分比，再以 `Math.max(state.progress, event.progress * 100)` 保持总进度单调递增。
  * 追加到 `events` 末尾，但最多保留 **最近 500 条**（与界面策略一致）。

`appStore.test.ts`（Vitest + jsdom）覆盖默认档位、档位切换时清空 plan、进度单调性、以及 500 条上限。

### 4.4 `src/app/App.tsx` — UI 组成

单页应用渲染于 `main` 与内部 `interface-frame`，使用以下 CSS 变量：

* `--ui-scale`（0.80–1.40，持久化到 `localStorage` 的 `ooo-splat-ui-scale`）
* `--ui-size`（`10000 / uiScale %` 反向缩放）
* `--left-pane-width`（32–68 %，持久化为 `ooo-splat-left-pane`）

界面分区：

1. **顶栏** —— 品牌标识（`Aperture` 图标 + “OOO Splat” + `LOCAL / 0.1.0`）、引擎状态指示灯（缺失引擎时为黄色 `status-light warning`，否则为绿色）。
2. **控制面板（`01 创建新任务`）**：
   * 视频路径选择器（`.mp4`/`.mov`）。
   * 项目根目录选择器。
   * 质量档位单选组（快速 / 均衡 / 精细），运行中禁用。
   * 源素材指标（时长、分辨率、预计帧数）。
   * “开始生成”/“正在分析视频” 主按钮（运行中或引擎缺失时禁用）。
3. **实时进程面板**（仅当有运行中任务或 `events.length > 0` 时显示）：
   * 明确标示的全任务总进度百分比 + 状态点 + 当前消息；阶段计数单独显示，避免与总进度混淆。
   * 阶段时间线（视频分析、画面提取、特征提取、顺序匹配、相机重建、Splat 训练、结果发布）。
   * 实时日志（最近 N/500 行）；日志刷新不改变左侧控制面板的滚动位置。
   * “取消任务并终止所有进程”按钮。
4. **历史任务面板（`02 历史任务`）**：
   * 首屏仅读取 `project.json` 元数据并展示名称与操作；PLY/Splat 等详情按需展开，避免历史项目逐个扫描大型 PLY。
5. **内联错误条**，可关闭。
6. **面板分隔条** —— 控制面板与历史面板之间的可拖动竖向分隔条（32–68 %）。
7. **缩放控件** —— 右下角的 `−` / 重置 / `+`（80–140 %），点击百分比按钮可呼出工具条。

生命周期 `useEffect`：

* 挂载时执行 `Promise.all([checkEngines, getProjectOverview])`。
* 订阅一次 `pipeline-event`，清理时取消订阅。
* 新事件到来时保留用户当前滚动位置。
* 持久化 UI 偏好。

### 4.5 `src/lib/backend.ts` — IPC 封装层

唯一导入 `@tauri-apps/api/core` 和 dialog/opener 插件的文件。把每个 Tauri 命令包装成 TypeScript 友好的签名，并加入 `inTauri()` 守卫，便于在非桌面环境下优雅降级。

### 4.6 构建配置（`vite.config.ts`）

* 使用 React 插件，固定端口 `1420`，`strictPort=true` 以满足 Tauri 的预期。
* 文件监听忽略 `src-tauri/**`。
* `target: "chrome105"`（跟随 WebView2 Chromium 版本）。
* 根据 `TAURI_ENV_DEBUG` 决定是否压缩与是否生成 sourcemap。

---

## 5. 项目与目录约定

### 5.1 生成的项目布局（每次运行）

```
<项目根目录>/
  <yyyyMMdd-HHmmss>_<清洗后的视频名>/
    final.ply                  # 最终 Gaussian Splatting 产物
    project.json               # ProjectMetadata（成功后写入 ProjectOutput）
    state.json                 # PipelineStateFile（当前阶段 + 标记位）
    source/
      input.<ext>              # 源视频副本
    work/
      frames/                  # FFmpeg 抽出的 JPEG 帧（frame_000001.jpg …）
      colmap/                  # database.db + sparse/0/{cameras,images,points3D}.bin
      brush/                   # Brush 数据集与中间文件
    logs/
      ffprobe.log
      ffmpeg.log
      colmap.log
      brush.log
```

### 5.2 应用设置

```
%LOCALAPPDATA%\SplatStudio\settings.json
%LOCALAPPDATA%\SplatStudio\project-index.json
```

两份文件读取时具备容错（解析失败时回退默认值），写入时通过 `atomic_write_json` 实现原子落盘。

---

## 6. 引擎清单与许可证策略

`engines/manifest.json` 是引擎版本、哈希、CPU/CUDA 策略的唯一事实来源。

| 引擎 | 版本 / 资源 | 许可证 | 已记录哈希 |
| --- | --- | --- | --- |
| FFmpeg / FFprobe | BtbN `ffmpeg-n8.1-latest-win64-lgpl-shared-8.1.zip`（实测 `n8.1.2-34-g9b6c8969e0`，2026-08-21） | LGPL-2.1-or-later（已禁用 GPL/nonfree 编码器） | `archiveSha256 = 14A87B3C…A7287` + `ffmpeg.exe = E41DBF05…F7EB0` + `ffprobe.exe = 08AE7637…7221` |
| COLMAP (CPU/no-CUDA) | `colmap-x64-windows-nocuda.zip`（`4.1.1`，官方 Windows release，without CUDA） | BSD-3-Clause | `archiveSha256 = FAF1247D…0895A` + `colmap.exe = 4358DF3C…15ADE` |
| COLMAP (CUDA) · *可选* | `colmap-x64-windows-cuda.zip`（`4.1.1`，官方 Windows release，with CUDA） | BSD-3-Clause | `archiveSha256 = B06064E7…32B9B` + `colmap.exe = 31D80701…15C7A`；安装包不预置，按需下载 |
| Brush | `brush-app-x86_64-pc-windows-msvc.zip`（`v0.3.0`） | Apache-2.0 | `archiveSha256 = B68E3E9C…CFCD6` + `brush_app.exe = 37E46CBF…5B34A` |

`requiredFiles[]` 在构建时强制校验可执行文件的 SHA-256；CUDA 版 COLMAP 列入 `optionalFiles[]`，下载后同样校验其主程序哈希。CPU 版的运行时策略为“仅以 CLI 调用；CPU 版 SIFT 与匹配；不允许 CUDA/cuDNN 运行时文件名”——`health::require_cpu_colmap` 在启动时再次校验，`scripts/verify-engines.ps1` 还会断言 `feature_extractor --h` 声明 "without CUDA"。CUDA 校验额外要求特征/匹配 GPU 参数与 Mapper BA 参数出现在本机帮助输出。

许可证策略：

* 顶层项目使用 **Apache-2.0**（`LICENSE`、`Cargo.toml`、`package.json`、`tauri.conf.json` 完全一致）。
* `NOTICE`、`TRADEMARK_POLICY.md`、`GENERATED_OUTPUTS.md` 与 `licenses/THIRD_PARTY_NOTICES.txt` 及各引擎许可证文本（FFmpeg-LGPL-2.1.txt、COLMAP-LICENSE.txt、Brush-LICENSE.txt）一起打包。
* `scripts/verify-licenses.ps1` 会检查：Apache-2.0 文本的精确 SHA-256、package 与 Cargo 元数据中的许可证/作者/仓库信息、Tauri bundle 资源列表、引擎清单映射（4 个 direct engine 条目，含 COLMAP(CPU) 与 COLMAP(CUDA)）、许可证文件存在性，以及必要的商标/生成物声明措辞。

---

## 6.1 COLMAP 后端（CPU / CUDA）可选项

`engines/manifest.json` 的 `schemaVersion` 已升至 `3`，引入 `optional: true` 与 `optionalFiles[]`，用于标记不进入安装包、按需下载的可选引擎：

* **默认**：CUDA；新安装与缺少后端字段的旧设置都会优先选择 CUDA 后端。
* **CPU/no-CUDA 回退**：随安装包分发，CUDA 未下载或不可用时可在设置中切换，并继续执行 CPU 校验。
* **CUDA 版**：来自 [colmap/colmap](https://github.com/colmap/colmap) `4.1.1` 的 `colmap-x64-windows-cuda.zip`（SHA-256 `B06064E7…32B9B`）。落到 `engines/colmap-cuda/`，不参与 `verify:engines` 的强制链路。
* **运行时切换**：通过新命令 `set_colmap_backend(backend)` 写入 `%LOCALAPPDATA%\SplatStudio\settings.json`，Rust 端 `PipelineRunner` 据此选择 `engines.colmap` 或 `engines.colmap-cuda`，并对 COLMAP 三段流水线在事件消息中标记 `CPU` / `CUDA`。
* **下载方式**：Tauri 不在主线程内拉取 411 MB，而是由脚本 `npm run download:colmap-cuda`（即 `scripts/download-colmap-cuda.ps1`）下载并校验；UI 在设置抽屉中提供“检查 CUDA COLMAP 是否可下载”按钮以刷新状态。
* **设置抽屉（顶栏右上角）**：`src/app/App.tsx` 中的 `SettingsDrawer` 新增 `ColmapBackendSwitch` 区块，展示两个后端的就绪状态、选择切换、以及下载入口。控制面板的“生成质量”下方同时插入了一个胶囊+链接，使用户不必打开抽屉就能看到当前后端。
* **顶栏文案**：状态行 `FFmpeg · COLMAP ${currentBackendLabel} · Brush 就绪` 会随当前后端显示 `CPU` 或 `CUDA`。

* **引擎发现策略（viewer 合并新增）**：`engines/health.rs::select_engine_root` 优先选择“完整”的引擎根（同时存在 `ffmpeg/ffmpeg.exe`、`ffmpeg/ffprobe.exe`、`colmap/bin/colmap.exe`、`brush/brush_app.exe`），仅在没有完整根时退回“目录存在”启发式。这避免 dev 模式下错误使用打包资源中的陈旧引擎副本。单元测试覆盖“工作区完整引擎优先于陈旧资源”与“打包资源完整时仍最高优先”两个分支。

---

## 6.2 内置 Brush 3D 查看器

由 `sunshinegdy/ooosplat` 合并而来：在历史任务面板中可“一键查看”最终 `final.ply`。

* **入口**：历史任务行新增主操作按钮 **查看 3D**（图标 `Eye`）。点击后按钮变为旋转的加载图标 + “正在打开”，避免重复触发。
* **后端命令**：`commands::open_project_viewer(app, source_path)`。它先调用 `catalog::validate_registered_final_ply(source_path)` 校验 PLY 属于已注册的项目目录，再读取 `app.path().resource_dir()` 找到的 Brush 可执行文件，最后通过 `tokio::task::spawn_blocking` 把启动任务派发到阻塞线程池。
* **引擎适配**：`engines/brush.rs` 新增 `open_viewer(executable, ply)`：
  * 调用 `require_verified_cli` 确认 `brush_app.exe` 存在；
  * 调用 `inspect_gaussian_ply` 校验 PLY 头部（防御性二次校验，规避伪造 PLY）；
  * 构造 `Command::new(brush_app).arg("--with-viewer").arg(ply)`，禁用 stdio（避免占用 Tauri 控制台），并把 `current_dir` 设为可执行文件所在目录，确保 DLL 解析正常；
  * 在 Windows 上设置 `creation_flags(CREATE_NO_WINDOW)`，避免弹出 cmd 窗口。
  * 失败时返回 `SplatError::EngineStart { engine: "Brush 3D 查看器", detail }`。
* **前端调用**：`src/lib/backend.ts` 暴露 `openProjectViewer(project)`：
  * 缺少 `finalPly` 时直接抛错；
  * 非 Tauri 环境下抛错并保留原有 `inTauri()` 守卫；
  * 否则调用 `invoke("open_project_viewer", { sourcePath: project.finalPly })`。
* **样式**：`src/styles.css` 新增 `.project-actions .viewer-action` 主色高亮样式，与 `.danger-link` 形成对比层级。
* **安全前提**：必须同时通过两层校验——目录索引注册 + PLY 头部格式——才允许启动查看器，从而杜绝被替换为任意 PLY 的攻击面。
* **限制**：查看器与 COLMAP 后端无关，始终使用 `engines.brush`（与训练共用一个二进制）；它不进入流水线状态机，可以在前一条任务收尾后立刻打开。

---

## 7. 脚本说明

| 脚本 | 用途 |
| --- | --- |
| `scripts/setup-engines.ps1` | 从 `engines/manifest.json` 下载归档，校验归档 SHA-256，解压到 `<workspace>/engines/<engine>/`，再重跑 `verify-engines.ps1`。支持 `-Force` 忽略已有文件、`-CacheDirectory` 覆盖缓存目录，以及 `-IncludeOptional` 下载 COLMAP (CUDA)。默认情况下可选引擎会被跳过。脚本运行前先调用 `Assert-Sha256` 校验 manifest 中所有哈希值格式（必须为 64 位十六进制），哈希计算改为 `Get-Sha256Hex`（直接读取文件流并以 `SHA256` 实例计算，规避 `Get-FileHash` 大文件读取的开销）。解压时跳过归档自带的 `README.md`。 |
| `scripts/download-colmap-cuda.ps1` | 便捷封装，等价于 `setup-engines.ps1 -IncludeOptional -Force`。对应 `npm run download:colmap-cuda`。 |
| `scripts/verify-engines.ps1` | 校验 `requiredFiles[]` 中每一项存在且哈希匹配；拒绝 `engines/colmap` 下的任何 CUDA 运行时；断言 `colmap feature_extractor -h` 声明 "without CUDA"；断言 `brush_app --help` 暴露 `--total-steps`、`--max-resolution`、`--export-path`、`--export-name`。若已下载 COLMAP (CUDA)，还会校验其 `feature_extractor -h` 输出与二进制目录中包含 CUDA 运行时。脚本同样先做 SHA-256 格式断言。 |
| `scripts/verify-licenses.ps1` | 强制许可证、NOTICE、商标策略的一致性，并检查 `engines/manifest.json` 中 4 个 direct engine 条目、`download:colmap-cuda` 脚本与 3 个 PowerShell 脚本的存在性。 |

---

## 8. 依赖

### 8.1 Rust（`src-tauri/Cargo.toml`）

| Crate | 版本 | 用途 |
| --- | --- | --- |
| `tauri` | 2 | 桌面宿主 |
| `tauri-plugin-dialog` | 2 | 文件/目录/确认对话框 |
| `tauri-plugin-opener` | 2 | “在资源管理器中显示” |
| `tokio` | 1（fs、io-util、macros、process、rt-multi-thread、sync、time） | 异步运行时 + 进程 I/O |
| `tokio-util` | 0.7 | `CancellationToken` |
| `serde` / `serde_json` | 1 | 序列化 |
| `chrono` | 0.4（serde） | 时间戳 |
| `clap` | 4.5（derive） | `splatstudio` 的 CLI 解析 |
| `tracing` / `tracing-subscriber` | 0.1 / 0.3 | 结构化日志 |
| `thiserror` | 2 | 类型化的 `SplatError` |
| `trash` | 5.2 | 回收站删除 |
| `dirs` | 6 | `LOCALAPPDATA` / `Documents` |
| `image` | 0.25（仅 JPEG） | 抽帧候选的 pHash 与 Laplacian 清晰度测量 |
| `uuid` | 1（serde、v4） | 项目 ID |
| `windows-sys`（仅 Windows） | 0.61（`Win32_Foundation`、`Win32_Security`、`Win32_Storage_FileSystem`、`Win32_System_JobObjects`、`Win32_System_Threading`） | Windows Job Object + `MoveFileExW` |
| `tauri-build`（build） | 2 | Tauri 构建钩子 |
| `tempfile`（dev） | 3 | 测试 |

`crate-type = ["lib", "cdylib", "staticlib"]`，同时支持两个二进制。

### 8.2 前端（`package.json`）

| 包 | 版本 | 用途 |
| --- | --- | --- |
| `@tauri-apps/api` | ^2.11.1 | `invoke`、`listen` |
| `@tauri-apps/plugin-dialog` | ^2.4.0 | 文件/目录对话框 |
| `@tauri-apps/plugin-opener` | ^2.5.0 | “在资源管理器中显示” |
| `lucide-react` | ^0.539.0 | 图标库 |
| `react` / `react-dom` | ^19.2.0 | UI |
| `zustand` | ^5.0.8 | 应用状态 |
| `@tauri-apps/cli`（dev） | ^2.8.4 | `npm run tauri ...` |
| `@vitejs/plugin-react`（dev） | ^5.0.2 | React 热更新 |
| `vite`（dev） | ^8.1.0 | 打包器 |
| `typescript`（dev） | ~5.9.2 | tsc -b |
| `vitest`（dev） | ^3.2.4 | 前端测试 |
| `jsdom`（dev） | ^26.1.0 | vitest 使用的 DOM 环境 |
| `@types/node` / `@types/react` / `@types/react-dom`（dev） | 最新 | 类型定义 |

### 8.3 Tauri 配置（`src-tauri/tauri.conf.json`）

* 产品名：`OOOSplat`，标识符：`studio.ooo.splat`，许可证：Apache-2.0。
* 窗口：1180×780，最小 820×620，背景色 `#eef1f5`。
* `beforeDevCommand: npm run dev`、`devUrl: http://localhost:1420`、`beforeBuildCommand: npm run build:bundle`、`frontendDist: ../dist`。
* CSP 锁定了图片/样式/连接的来源（`asset:`、`http://asset.localhost`、`blob:`、`data:`、`ipc: http://ipc.localhost`）。
* 打包目标：`nsis`，安装模式 `perMachine`，压缩算法 bzip2。资源：`../engines/` 与所有许可证/策略文件。

---

## 9. 运行方式

### 9.1 环境要求

* Windows 10/11 x64，需安装 WebView2 Runtime。
* Node.js ≥ 22.12，Rust stable `x86_64-pc-windows-msvc`，Visual Studio 2022 Build Tools（含“使用 C++ 的桌面开发”）。

### 9.2 本地开发

```powershell
npm install
npm run setup:engines     # 下载并校验三个引擎
npm run tauri -- dev      # 启动 Tauri 开发壳（Vite 在 :1420 + Rust 后端）
```

### 9.3 测试与静态检查

```powershell
npm test                                   # Vitest（appStore.test.ts）
npm run build                              # tsc -b && vite build（前端生产构建）
cargo test --manifest-path src-tauri\Cargo.toml
cargo clippy --manifest-path src-tauri\Cargo.toml --all-targets -- -D warnings
npm run verify:engines
```

### 9.4 发布构建

`beforeBuildCommand` 会执行 `npm run build:bundle`，进而自动串行执行 `verify:engines`、`verify:licenses`、`vite build`，再调用 Tauri 的 NSIS 打包器。

```powershell
npm run tauri -- build
```

输出位置：`src-tauri\target\release\bundle\nsis\OOOSplat_0.1.0_x64-setup.exe`。

### 9.5 CLI

```powershell
cargo run --manifest-path src-tauri\Cargo.toml --bin splatstudio -- health
cargo run --manifest-path src-tauri\Cargo.toml --bin splatstudio -- probe "D:\Videos\orbit.mp4"
cargo run --manifest-path src-tauri\Cargo.toml --bin splatstudio -- plan "D:\Videos\orbit.mp4" --quality balanced
cargo run --manifest-path src-tauri\Cargo.toml --bin splatstudio -- extract "D:\Videos\orbit.mp4" "D:\Frames" --quality fast
cargo run --manifest-path src-tauri\Cargo.toml --bin splatstudio -- generate "D:\Videos\orbit.mp4" --projects-root "D:\Splat Projects" --quality balanced
```

全局覆盖：

```powershell
cargo run --manifest-path src-tauri\Cargo.toml --bin splatstudio -- --engine-dir D:\Engines health
$env:OOOSPLAT_ENGINE_DIR = "D:\Engines"
```

---

## 10. 失败与边界行为

* **不内置 3D 查看器** —— 应用自身不渲染 PLY，只负责生成、项目管理与“在资源管理器中显示”。
* **取消** —— `cancel_pipeline` 会翻转 `ProcessManager` 的取消令牌，Windows 端还会调用 `WindowsJob::terminate()`，整个子进程树都会被终止。流水线会发出最终事件 `PipelineEvent::mapped(Cancelled, 1.0, "任务已取消")`。
* **取消路径会发出合成事件**，确保即便执行器返回 `SplatError::Cancelled` 或 `Process`，UI 也能清除运行状态。
* **中断识别** —— 启动时，若当前没有活跃流水线，`get_project_overview` 会把所有仍处于 `Running` 状态的项目改为 `Interrupted`；仍活跃的项目保持 `Running`。
* **重建门槛** —— 注册率 `<50%` 时任务失败；`50–80%` 时继续并给出警告；`>=80%` 视为正常。
* **Unicode / UNC 路径** —— 所有项目数据都保存在项目目录内，并在调用 COLMAP 位图加载器时使用 ASCII 相对路径（`../frames`），从而保留 Unicode/UNC 项目根。
* **回收站语义** —— `delete_project` 使用 `trash::delete_all`，不会降级为永久删除。如果回收站拒收，将返回错误，项目目录保留在磁盘上。
* **项目名安全** —— 清洗 Windows 保留字符，去掉末尾的 `.`/` `，截断至 64 字符，空名回退为 `"project"`，冲突时依次追加 `-2`、`-3` … 直到 `-9999`。
* **引擎发现** —— 优先使用 `OOOSPLAT_ENGINE_DIR`，否则按 Tauri 资源目录 → 当前工作目录 → 父目录的顺序搜索，默认 `cwd/engines`。CLI 接受 `--engine-dir` 覆盖。
* **构建期把关** —— `build:bundle` 会执行引擎哈希、COLMAP CPU 策略、Brush CLI 参数以及许可证/策略校验；任何缺失或变动都会阻断构建。

---

## 11. 测试范围

| 层级 | 测试内容 | 文件 |
| --- | --- | --- |
| Rust 单元 | COLMAP `--use_gpu 0` 不变量 | `src-tauri/src/engines/colmap.rs` |
| Rust 单元 | 流水线状态转移 | `src-tauri/src/pipeline/state.rs` |
| Rust 单元 | 阶段 → 进度映射 | `src-tauri/src/pipeline/progress.rs` |
| Rust 单元 | 质量档位（集中数值） | `src-tauri/src/presets/quality.rs` |
| Rust 单元 | `parse_ffprobe_json`（优先 nb_frames、回退、缺失流） | `src-tauri/src/video/probe.rs` |
| Rust 单元 | `UniformRatioFrameSelection`（目标 FPS 不随源帧率放大） | `src-tauri/src/video/frame_plan.rs` |
| Rust 单元 | pHash 汉明距离和 Laplacian 方差清晰度排序 | `src-tauri/src/video/select.rs` |
| Rust 单元 | `PipelineStateFile` JSON 结构（`"preset":"balanced"`、不含 `targetFrames`） | `src-tauri/src/project/metadata.rs` |
| Rust 单元 | `ProjectManager` 清洗/冲突、原子 JSON、Unicode 项目布局 | `src-tauri/src/project/manager.rs` |
| Rust 单元 | 目录索引所有权校验 | `src-tauri/src/project/catalog.rs` |
| Rust 单元 | 重建门槛（0.8 / 0.5） | `src-tauri/src/reconstruction/validator.rs` |
| Rust 单元 | PLY Gaussian 头解析（合法 + 拒绝普通点云） | `src-tauri/src/reconstruction/ply.rs` |
| Rust 异步 | ProcessManager stdout/stderr 交错输出与日志落盘 | `src-tauri/src/process/mod.rs` |
| 前端 | 默认档位、档位切换清空 plan、进度单调性、500 条日志上限 | `src/stores/appStore.test.ts`（Vitest + jsdom） |

---

## 12. 术语表

* **COLMAP** —— Structure-from-Motion / 多视图立体重建工具链；本项目支持 CPU/no-CUDA 与 CUDA 后端，默认选择 CUDA。
* **Brush** —— 由 ArthurBrussee 维护的 Apache-2.0 Gaussian Splatting 训练器，用于从 COLMAP 相机位姿拟合 splats。
* **Gaussian Splatting** —— 用一组 3D 高斯表示场景的实时新视角合成技术。
* **PLY** —— Stanford 多边形文件格式；本项目用于最终 Gaussian Splatting 产物（包含 `x`、`y`、`z`、`f_dc_0`、`opacity`、`scale_0`、`rot_0`）。
* **Job Object** —— Windows 内核对象，关闭句柄或调用 `TerminateJobObject` 时可以杀掉整个子进程树。
* **OOOSplat** —— 将三个原生引擎随包发布的流水线，不依赖用户配置 `PATH`；项目标识为 `studio.ooo.splat`。

---

## 13. 快速参考 — 文件索引

### 后端（Rust）

* [lib.rs](file:///a:/project/splat/src-tauri/src/lib.rs)
* [main.rs](file:///a:/project/splat/src-tauri/src/main.rs)
* [error.rs](file:///a:/project/splat/src-tauri/src/error.rs)
* [bin/splatstudio.rs](file:///a:/project/splat/src-tauri/src/bin/splatstudio.rs)
* [commands/mod.rs](file:///a:/project/splat/src-tauri/src/commands/mod.rs)
* [engines/mod.rs](file:///a:/project/splat/src-tauri/src/engines/mod.rs)
* [engines/health.rs](file:///a:/project/splat/src-tauri/src/engines/health.rs)
* [engines/ffprobe.rs](file:///a:/project/splat/src-tauri/src/engines/ffprobe.rs)
* [engines/ffmpeg.rs](file:///a:/project/splat/src-tauri/src/engines/ffmpeg.rs)
* [engines/colmap.rs](file:///a:/project/splat/src-tauri/src/engines/colmap.rs)
* [engines/brush.rs](file:///a:/project/splat/src-tauri/src/engines/brush.rs)
* [pipeline/mod.rs](file:///a:/project/splat/src-tauri/src/pipeline/mod.rs)
* [pipeline/event.rs](file:///a:/project/splat/src-tauri/src/pipeline/event.rs)
* [pipeline/state.rs](file:///a:/project/splat/src-tauri/src/pipeline/state.rs)
* [pipeline/progress.rs](file:///a:/project/splat/src-tauri/src/pipeline/progress.rs)
* [pipeline/runner.rs](file:///a:/project/splat/src-tauri/src/pipeline/runner.rs)
* [process/mod.rs](file:///a:/project/splat/src-tauri/src/process/mod.rs)
* [presets/quality.rs](file:///a:/project/splat/src-tauri/src/presets/quality.rs)
* [video/probe.rs](file:///a:/project/splat/src-tauri/src/video/probe.rs)
* [video/frame_plan.rs](file:///a:/project/splat/src-tauri/src/video/frame_plan.rs)
* [video/extract.rs](file:///a:/project/splat/src-tauri/src/video/extract.rs)
* [project/manager.rs](file:///a:/project/splat/src-tauri/src/project/manager.rs)
* [project/metadata.rs](file:///a:/project/splat/src-tauri/src/project/metadata.rs)
* [project/catalog.rs](file:///a:/project/splat/src-tauri/src/project/catalog.rs)
* [reconstruction/validator.rs](file:///a:/project/splat/src-tauri/src/reconstruction/validator.rs)
* [reconstruction/ply.rs](file:///a:/project/splat/src-tauri/src/reconstruction/ply.rs)
* [Cargo.toml](file:///a:/project/splat/src-tauri/Cargo.toml)
* [tauri.conf.json](file:///a:/project/splat/src-tauri/tauri.conf.json)
* [capabilities/default.json](file:///a:/project/splat/src-tauri/capabilities/default.json)

### 前端（TypeScript）

* [main.tsx](file:///a:/project/splat/src/main.tsx)
* [app/App.tsx](file:///a:/project/splat/src/app/App.tsx)
* [lib/backend.ts](file:///a:/project/splat/src/lib/backend.ts)
* [stores/appStore.ts](file:///a:/project/splat/src/stores/appStore.ts)
* [stores/appStore.test.ts](file:///a:/project/splat/src/stores/appStore.test.ts)
* [types/pipeline.ts](file:///a:/project/splat/src/types/pipeline.ts)
* [vite.config.ts](file:///a:/project/splat/vite.config.ts)
* [package.json](file:///a:/project/splat/package.json)

### 构建、脚本、清单与策略

* [engines/manifest.json](file:///a:/project/splat/engines/manifest.json)
* [scripts/setup-engines.ps1](file:///a:/project/splat/scripts/setup-engines.ps1)
* [scripts/verify-engines.ps1](file:///a:/project/splat/scripts/verify-engines.ps1)
* [scripts/verify-licenses.ps1](file:///a:/project/splat/scripts/verify-licenses.ps1)
* [LICENSE](file:///a:/project/splat/LICENSE)
* [NOTICE](file:///a:/project/splat/NOTICE)
* [TRADEMARK_POLICY.md](file:///a:/project/splat/TRADEMARK_POLICY.md)
* [GENERATED_OUTPUTS.md](file:///a:/project/splat/GENERATED_OUTPUTS.md)
* [licenses/THIRD_PARTY_NOTICES.txt](file:///a:/project/splat/licenses/THIRD_PARTY_NOTICES.txt)

> **链接说明**：以上链接全部指向本地仓库镜像 `a:\project\splat`；上游 GitHub 仓库对应地址为 <https://github.com/ooolabdev/ooosplat>。
