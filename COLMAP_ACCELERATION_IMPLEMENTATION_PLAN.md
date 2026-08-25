# OOOSplat 抽帧筛选与 COLMAP 加速详细实施文档

> 文档状态：部分实施（原里程碑 1–4 已完成代码与单元验收；新增自适应 SfM 抽帧里程碑待实施；CASPAR 的真实大样本性能/质量基准仍待完成）
> 制定日期：2026-08-22  
> 最近更新：2026-08-23
> 依赖报告：`COLMAP_PIPELINE_PERFORMANCE_REPORT.md`  
> 范围约束：本期不实施 GLOMAP 自动质量回退，不实施预置 ONNX/LightGlue 恢复模式

## 1. 实施目标

本期在不改变 OOOSplat 输入/输出契约、帧时间顺序和训练输入布局的前提下，完成以下能力：

1. 将候选帧 pHash/Laplacian 指标计算改为确定性的受控 CPU 并行。
2. 让 CPU 与 CUDA COLMAP 后端真正控制 SIFT 特征提取和匹配设备。
3. 将 Windows CUDA COLMAP 固定到经过验证的 4.1.1 发行物及完整依赖清单。
4. 为中大型任务增加增量 Mapper + Caspar，并在能力或结果失败时回退到增量 Ceres。
5. 为每个阶段建立可复现的性能、质量、回退和打包验收证据。
6. 将均衡/精细从固定 2/4 FPS 改为以 2/4 FPS 为软基准的自适应 SfM 关键帧选择；按可靠背景视差、共视、三帧轨迹和清晰度动态决定帧数，并以现有固定策略作为质量回退。

最终流水线保持为：

```text
视频
  -> FFmpeg 低分辨率有界代理扫描
  -> 可靠背景轨迹、场景分段与累计视差规划
  -> 按源帧编号/PTS 精确抽取原始分辨率关键帧
  -> CPU 并行 pHash + Laplacian 几何约束筛选
  -> CUDA SIFT 特征提取（CUDA 可用时）
  -> CUDA SIFT_BRUTEFORCE 顺序匹配（CUDA 可用时）
  -> 小任务：增量 Mapper + Ceres
  -> 中/大任务：增量 Mapper + Caspar
  -> Caspar 失败：增量 Mapper + Ceres
  -> 稀疏模型质量门禁
  -> 弱连接区间定向补帧（最多一次、总量不超过初选 25%）
  -> 仍不合格：隔离运行原固定 2/4 FPS 策略
  -> Brush 或独立训练后端
  -> PLY 校验与导出
```

## 2. 本期不做的内容

以下内容只记录接口预留和触发条件，不编写运行代码、不下载模型、不改变安装包：

- 独立 `global_mapper`/GLOMAP 实验路线。
- GLOMAP 前置 `view_graph_calibrator`、数据库副本和自动质量回退。
- SIFT-LightGlue 或 ALIKED-LightGlue 匹配。
- LightGlue/ALIKED ONNX 模型预置、下载、哈希和许可集成。
- nvJPEG、NPP 或自研 CUDA pHash/Laplacian 后端。
- 用 GLOMAP、LightGlue 或 CUDA 去重的预计倍数替代真实基准。

## 3. 不得破坏的现有契约

### 3.1 帧筛选契约

- 快速档继续使用固定 1 FPS；均衡/精细档分别保留 2/4 FPS 作为软基准与失败回退，不再作为全片固定抽取率。
- 均衡/精细的最终关键帧数量由可靠背景视差、共视率、三帧轨迹、清晰度和场景边界共同决定；源视频 FPS 只限定候选时间精度和局部重扫上限。
- 可变帧率视频必须按源帧编号和实际 PTS 规划与提取，不能仅用 `frame_index / avg_frame_rate` 推算时间。
- 输入 JPEG 按 `frame_%06d.jpg` 排序，源时间顺序不变。
- pHash 汉明距离阈值保持 `8`；自适应路径只有在几何位移低于目标位移一半时才允许把 pHash 近似帧判为冗余，不能删除用于连接视图的桥接帧。
- 连续近重复组只保留 Laplacian 方差最大的一张。
- 清晰度相同时保留前一张，避免结果漂移。
- 继续报告 `candidates`、`retained`、`removed_near_duplicates`。
- 筛选仍属于独立 `SelectingFrames` 阶段。
- 在所有候选指标成功计算前不得删除任何 JPEG。
- 自适应路径失败或质量不足时，原固定 2/4 FPS 路径必须在隔离目录中可用，不能覆盖或混用自适应尝试的数据库与稀疏模型。

### 3.2 COLMAP 输入输出契约

- 相机模型继续使用 `SIMPLE_RADIAL`。
- 同一视频继续使用 `ImageReader.single_camera=1`。
- 默认匹配仍是 `SIFT_BRUTEFORCE + sequential_matcher`。
- 默认顺序重叠继续为 `10`，本期不顺带调整 loop detection 或 quadratic overlap。
- 下游继续消费已验证的最佳稀疏模型，并整理为训练输入的 `sparse/0`。
- CUDA、Caspar 或回退尝试不得共享可能部分写入的输出目录。

### 3.3 产品和运行契约

- CPU-only 机器必须继续可完整生成 PLY。
- 用户取消任务后必须终止当前子进程并清理该次尝试的临时目录。
- UI 显示的设备和后端必须来自实际生效配置，不能只来自用户选择。
- 任何回退必须写入事件、项目状态和日志，不允许静默回退。
- Brush 保持稳定默认训练后端，本期不把 COLMAP 改造与 gsplat 改造耦合。

## 4. 里程碑 0：冻结基线与建立可恢复边界

### 4.0 实施记录（2026-08-22）

- 已建立可恢复备份：`.backup/20260822-142556-colmap-acceleration/`。
- 本机基线：NVIDIA GeForce RTX 5090 D v2、驱动 610.88、24455 MiB 显存；CUDA COLMAP `4.1.1`（commit `a0d785f`）已确认可启动并暴露 GPU 参数。
- 尚未取得计划要求的 45/84/631 帧三组真实样本三次运行中位数；因此本文不宣称任何实际加速倍数。

### 4.1 目标

在改动算法或命令行前，固定代码、引擎和样本证据，确保后续每项收益可以单独归因。

### 4.2 操作

1. 创建带时间戳的 `.backup/<timestamp>-colmap-acceleration/`。
2. 备份计划修改的 Rust 文件、`Cargo.toml`、`Cargo.lock`、`engines/manifest.json` 和相关测试。
3. 记录当前 CPU/CUDA COLMAP 的：
   - `colmap -h` 版本、提交、CUDA 状态；
   - `feature_extractor -h`、`sequential_matcher -h`、`mapper -h` 关键参数；
   - 所有直接依赖 DLL 的文件名、大小和 SHA-256；
   - NVIDIA 驱动、GPU 名称和可用显存。
4. 固定三个代表数据集：
   - 小：约 42 张保留帧；
   - 中：约 64 张保留帧、84 张候选帧；
   - 大：约 631 张保留帧。
5. 将三个数据集的抽帧结果复制为只读基准输入。COLMAP 对比全部从同一帧目录开始，避免把筛选变化混入 COLMAP 比较。
6. 预热一次后，每个配置运行三次，记录中位数和最慢一次。

### 4.3 需要记录的指标

```text
probe_ms
extract_ms
select_ms
colmap_process_start_ms
colmap_features_ms
colmap_matching_ms
colmap_mapping_ms
reconstruction_validation_ms
training_ms
export_ms
total_ms
candidate_frames
retained_frames
registered_images
registered_ratio
model_count
points3d
mean_reprojection_error
peak_ram_mb
peak_vram_mb
effective_colmap_backend
effective_feature_device
effective_matching_device
effective_ba_backend
fallback_reason
```

### 4.4 完成标准

- 三个样本均有可复现的基线记录。
- 能从日志区分进程启动、特征、匹配、映射和训练时间。
- 基线没有使用新的并行筛选或 Caspar。
- 备份目录可以恢复所有准备修改的源文件和 manifest。

## 5. 里程碑 1：确定性 CPU 并行帧筛选

### 5.1 设计原则

每张候选图片的 `hash` 和 `sharpness` 可独立计算；近重复分组与“保留更清晰帧”的决策必须串行按源顺序执行。实现分为两个阶段：

```text
阶段 A：并行只读计算
paths.par_iter() -> Result<CandidateMetrics>

阶段 B：顺序决策与提交
sorted metrics -> duplicate grouping -> deletion plan -> delete discarded files
```

这样既能使用多核，又保留当前相邻分组语义。

### 5.2 数据结构

建议新增：

```rust
struct CandidateMetrics {
    path: PathBuf,
    hash: u64,
    sharpness: f64,
}

struct FrameSelectionPlan {
    candidates: u64,
    keep: Vec<PathBuf>,
    discard: Vec<PathBuf>,
}
```

`compute_candidate_metrics` 只能读取文件，不得删除。`build_selection_plan` 是纯顺序逻辑。`commit_selection_plan` 最后执行删除并生成报告。

### 5.3 并行度

- 使用 Rayon 有界线程池或等价实现。
- 默认线程数为 `min(物理核心数, 8)`，以后再根据内存证据调整。
- 不直接使用无限制的全局线程数，因为同时解码 1920 像素 JPEG 会放大内存峰值。
- 在已有 Tokio 异步管线中通过 `spawn_blocking` 运行整个 CPU 筛选任务，避免阻塞异步运行时工作线程。
- 并行任务完成数使用原子计数器生成单调进度；不能假设候选会按文件顺序完成。

### 5.4 数学优化分两步实施

第一步只做低风险变更：

- 将 32×8 的余弦表放入 `OnceLock` 或等价静态缓存。
- 复用每张图已解码的 `DynamicImage`。
- 并行计算当前完全相同的 pHash 和 Laplacian 实现。

第二步只有在黄金测试证明 hash 和选择结果不变时才启用：

- 将二维直接 DCT 改为两遍可分离 DCT。
- 或使用固定版本的成熟 DCT 库。
- 浮点实现若导致边界 hash 漂移，保留旧 DCT，只使用并行与静态缓存。

本期不把 32×32 pHash 输入改为从 256×256 灰度图二次缩放，因为这会改变像素插值路径，可能要求重新标定汉明距离阈值。

### 5.5 错误与删除策略

- 任一图片解码或指标计算失败：整个筛选失败，不删除任何候选文件。
- 全部指标成功后才生成 discard 列表。
- 删除时若失败：报告具体文件，任务失败，并保留其余未删除文件；项目目录仍可诊断。
- 不使用通配符删除，只删除计划中经过规范化、确认位于当前项目 `frames` 目录的明确路径。

### 5.6 测试

- 固定图片集的每张 pHash 值黄金测试。
- 固定图片集的 Laplacian 排序测试。
- 连续相似组三张图只保留最清晰帧。
- 清晰度相等时保留前一张。
- 并行完成顺序变化时输出文件集合不变。
- 中途解码失败时零删除。
- 删除失败时错误包含目标路径且不越出项目目录。
- 候选为空、单张、全部相似、全部不同。
- 完整 Rust 测试套件，而不只是筛选模块定向测试。

### 5.7 性能门禁

- 45 张和 84 张样本的保留文件名必须与基线完全一致。
- 84 张目标中位数 5–15 秒；即使未达到目标，也必须比基线 58 秒至少改善 2 倍，才允许默认启用。
- 峰值 RAM 不超过串行基线增加 1.5 GB；超过时降低默认线程数。

### 5.8 实施状态（已完成）

- 已引入 Rayon，并使用 `min(available_parallelism, 8)` 的受控线程池。
- `CandidateMetrics` 的解码、pHash 与 Laplacian 计算并行执行；选择计划与删除提交串行执行。
- DCT 余弦表使用 `OnceLock` 缓存；清晰度相等时仍保留前一帧。
- 失败时先终止指标计算，未进入提交阶段的 JPEG 不会被删除；已有单元测试覆盖。
- `PipelineRunner` 通过 `tokio::task::spawn_blocking` 调度该 CPU 密集步骤。

## 5A. 新增里程碑：视差驱动的自适应 SfM 抽帧

### 5A.1 状态与决策

**状态：运行时接入已完成，待真实素材验收（2026-08-25）。均衡/精细档不再单独读取全视频 PTS：FFmpeg `fps` 高速抽取低分辨率代理的同一次解码，通过 `-stats_mux_pre` 为每张代理记录输入帧索引 `ni` 与输入显示时间 `ti`，作为唯一 PTS 映射来源。这样避免一整次 FFprobe 读取/解码和逐帧 JSON，亦避免超长 `select=eq(n,…)` 表达式及其内存失败。代理映射的索引缺失、时间戳无效或非显示顺序时，会记录原因并回退固定 2/4 FPS。代理抽帧不再强制单解码或单滤镜线程，保留 FFmpeg 按机器 CPU 自动分配的默认多线程；滤镜图排队上限按照源分辨率、三倍帧副本估计与当前可用物理内存动态计算，最多使用可用内存的 80%，同时始终为 Windows 与后续阶段保留 2 GiB；这只是队列许可上限而非预分配。代理 JPEG 解码后全部常驻内存；320px 灰度图与三层跟踪金字塔只会并行预计算一次并被相邻帧对复用，消除中间帧的重复建塔。每对相邻帧会在有界 Rayon 线程池中并行匹配 160 个独立网格（保留一核给 UI，最多 12 线程），但帧对和三视图轨迹状态仍按时间顺序归约，确保选择结果确定；主界面和 `logs/adaptive-analysis-benchmark.json` 会分别记录 JPEG 读取、金字塔准备、网格匹配的工作线程、耗时和吞吐基准。第二遍原图仍重跑同一 `fps` 采样以保持 VFR 映射，但使用任务自有 filter script 在缩放/编码前按代理候选序号只选择最终关键帧，且以 `-fps_mode:v passthrough` 禁止补帧；候选序号会压缩连续区间并组合为平衡表达式树，避免 FFmpeg 在长线性 `select` 表达式初始化时误报内存分配失败。正式抽帧、近预算验证、桥接 repair 与密度验证均使用这套定向抽帧；各 attempt 的 COLMAP 数据库和稀疏模型仍保持隔离。随后再次核验写出的 `ni` 是否逐项等于代理映射。这样不再写出并删除未入选候选 JPEG。分析背景运动后按源帧索引精确抽取原图；主界面会收到每一步的阶段、日志和确定性 `current / total` 进度，并持续显示本地总耗时。几何关键帧不足或代理步骤失败时会记录明确原因并回退固定 2/4 FPS；取消标记会跨 FFmpeg 阶段保持有效。COLMAP 质量闭环、定向补帧与真实素材标定仍待完成。**

均衡和精细不应按照源视频 FPS 做简单比例缩放。30 FPS 快速绕拍可能需要局部高于 4 FPS，而 120 FPS 慢速移动或静止素材仍可能只需要 1–2 FPS。源 FPS 只决定可分析的时间粒度；最终帧数由相机运动形成的可靠视角变化决定。

Mapper 后的自动补帧 attempt 也会从持久化 `adaptive-proxy-analysis.json` 恢复候选顺序与 VFR PTS 映射，采用相同的平衡定向抽帧；该文件缺失或损坏时记录原因，并安全回退兼容抽帧。

本里程碑采用“两遍分析 + 质量闭环”：

```text
FFmpeg 单次代理解码（`fps` + `-stats_mux_pre`）
  -> 低分辨率代理画面 + 采样帧实际 PTS
  -> 背景特征跟踪 + 场景切换检测
  -> 累计视差预算 + 共视/三帧轨迹门禁
  -> 清晰度窗口择优
  -> 生成源帧编号/PTS 清单
  -> 精确抽取原始分辨率 JPEG
  -> COLMAP
  -> 弱连接区间定向补帧
  -> 仍不合格：固定 2/4 FPS 隔离回退
```

该方案不引入 GLOMAP、LightGlue、ONNX 或 CUDA/nvJPEG 去重，与本文备选路线保持解耦。

### 5A.2 科学依据与设计边界

- COLMAP 要求同一物体具有高视觉重叠、不同视角且至少被三张图片观察；官方同时指出更多图片不一定更好，视频应下采样。
- 视频 SfM 关键帧研究将可靠特征生命周期、充分基线、数据冗余和退化规避作为联合目标，而不是只按时间等距采样。
- DSO 类方法使用光流、去旋转运动和亮度变化判断是否生成关键帧。
- Gaussian Splatting SLAM 使用共视关系保留非冗余且具有足够基线的关键帧。

前置筛选无法对所有纯旋转、低纹理、反光、严重模糊或大面积动态场景作数学质量保证。因此，本设计的质量保证来自“前置几何门禁 + COLMAP 结果检查 + 定向补帧 + 固定策略回退”，不能只依赖一个动态 FPS 公式。

资料：

- [COLMAP Tutorial](https://colmap.github.io/tutorial.html)
- [Optimal key-frame selection for video-based structure-from-motion](https://doi.org/10.1049/el.2011.2674)
- [Key-Frame Selection and an LMedS-Based Approach](https://www.jstage.jst.go.jp/article/transinf/E91.D/1/E91.D_1_114/_article)
- [Stereo DSO](https://arxiv.org/abs/1708.07878)
- [Gaussian Splatting SLAM](https://openaccess.thecvf.com/content/CVPR2024/papers/Matsuki_Gaussian_Splatting_SLAM_CVPR_2024_paper.pdf)
- [FFmpeg Filters: scdet, blurdetect, mestimate](https://www.ffmpeg.org/ffmpeg-filters.html)

### 5A.3 档位配置

当前 2/4 FPS 改为软基准 `anchor_fps`，用于容量估算、UI 解释和最终固定策略回退。初始工程配置如下，必须通过本地代表数据集标定后才能视为稳定默认值：

| 参数 | 均衡 | 精细 |
|---|---:|---:|
| `anchor_fps` | 2 | 4 |
| 全片代理扫描上限 | `min(source_fps, 6)` | `min(source_fps, 8)` |
| 快速区间局部重扫上限 | `min(source_fps, 12)` | `min(source_fps, 16)` |
| 代理图长边 | 480–640 px | 640 px |
| 目标归一化视差 `mu` | 0.035 × 对角线 | 0.025 × 对角线 |
| 最大相邻视差 | 0.08 × 对角线 | 0.06 × 对角线 |
| 最短关键帧间隔 | 120 ms | 80 ms |
| 动态有效 FPS 上限 | 6 | 10 |
| 最少几何内点 | 80 | 120 |
| 内点网格覆盖率 | 35% | 45% |
| 跨三帧稳定轨迹 | 50 | 80 |
| 自动修复注册率门禁 | 90% | 95% |

快速档本里程碑保持固定 1 FPS，不引入自适应行为。

### 5A.4 代理扫描与时间基准

第一遍只读取低分辨率灰度代理帧，不输出大量 1920 像素 JPEG。每个代理样本至少记录：

```rust
struct ProxyFrame {
    source_index: u64,
    pts_seconds: f64,
    phash: u64,
    sharpness: f64,
}
```

实施要求：

- `VideoInfo.fps` 继续作为显示值和扫描上限参考，但不能作为唯一时间基准。
- 代理解码阶段必须保留每张代理的 `ni` / `ti`（或等价的输入帧编号与实际显示 PTS）；无需为未采样帧单独建立 PTS 清单。
- 原图提取必须按代理映射得到的 `source_index` 选择同一源帧；不得把最终清单重新换算成平均 FPS。
- 选帧表达式较长时写入项目自有 filter script/清单文件，避免 Windows 命令行长度限制。
- 代理文件、分析日志和最终 JPEG 使用独立目录，失败时不能把代理图误送入 COLMAP。

### 5A.5 可靠背景运动指标

在代理帧上使用网格化稀疏特征和跨帧跟踪。只保留至少存活三帧且属于 RANSAC 主运动模型的轨迹，以降低人物、车辆、树叶和反光区域对运动估计的影响。

定义画面对角线：

```text
D = sqrt(width^2 + height^2)
```

定义相邻代理帧的可靠背景位移：

```text
m_i = median(||p_i' - p_i|| for reliable background inliers) / D
```

每个一秒窗口累计：

```text
M_window = sum(m_i)
```

这样高帧率只会提供更密的观测点，不会让同一条慢速相机轨迹因为 120 FPS 而自动抽取四倍图片。

除位移外还必须记录：

- `inlier_count`：可靠主运动内点数。
- `inlier_ratio`：内点占可跟踪特征的比例。
- `grid_coverage`：内点覆盖的图像网格比例。
- `three_view_tracks`：至少跨三帧的可靠轨迹数。
- `dynamic_outlier_ratio`：不服从主背景模型的运动轨迹比例。
- `homography_dominance` 或等价退化指标：用于识别纯旋转/近平面退化风险。

动态离群比例超过约 40% 时，不使用离群运动提高抽帧率，记录“动态场景风险”。稳定轨迹低于最低门禁时，多抽帧通常不能制造纹理，应标记“不可充分观测”，而不是无限加密。

### 5A.6 动态 FPS 变量

保留当前固定值作为软基准：

```text
r0_balanced = 2
r0_high = 4
```

对每个一秒窗口定义场景运动因子：

```text
k_window = clamp(M_window / (r0 * mu), 0.25, 2.5)
```

用于估算和 UI 显示的动态有效率：

```text
r_window = min(source_fps, profile_max_fps, r0 * k_window)
```

真正的选帧触发条件不是“每秒均匀输出 `r_window` 张”，而是从上一关键帧累计可靠背景位移，达到当前 `mu` 后在邻域内选择最清晰且仍满足共视门禁的源帧。

静止窗口覆盖规则：

```text
if M_window < 0.25 * mu
   and phash_change is small
   and scene_change is small:
    r_window = 0
```

静止区间只保留开始/结束附近必要且最清晰的帧，不用最低 FPS 填充重复图片。

纹理修正：

- 稀疏但仍达到最低稳定轨迹门禁时，将 `mu` 乘以约 `0.8`，用更小视角跨度提高匹配连续性。
- 特征丰富、覆盖均匀时可将 `mu` 提高到约 `1.1 × mu`，减少冗余。
- 低于最低可观测门禁时不继续降低 `mu`，直接记录素材风险。

### 5A.7 关键帧、桥接帧与清晰度窗口

候选帧只有同时满足以下条件才能成为普通关键帧：

1. 达到最短时间间隔。
2. 累计可靠背景位移达到目标 `mu`。
3. 与上一关键帧的几何内点、内点率和网格覆盖达到门禁。
4. 有足够轨迹能跨到第三个视图。
5. 未被确认属于场景切换后的另一段视频。
6. 清晰度不处于局部低位。

桥接保护：如果继续等待会让位移超过最大相邻视差、内点迅速下降或轨迹即将断裂，则回选上一个仍可连接且最清晰的候选，即使它尚未达到目标 `mu`。

清晰度选择不固定在首次越过阈值的帧：

- 均衡在目标点前后约 150 ms 内选择。
- 精细在目标点前后约 100 ms 内选择。
- 只在仍满足几何连接的候选中比较 Laplacian 方差。

pHash 仅作为冗余辅助：

```text
phash_distance <= 8
and geometry_motion < 0.5 * mu
```

两项同时成立才允许删除。只因外观相似而删除桥接帧是不允许的。

### 5A.8 场景切换与多场景输入

使用 FFmpeg `scdet` 或等价指标生成场景变化分数。FFmpeg 官方给出的常用阈值范围是 8–14，初始使用 `10`，并通过轨迹存活率二次确认：

```text
scene_score >= 10
and background_track_survival < 10%
```

确认切换后：

- 结束当前连续场景，启动新分段。
- 不通过大量补帧尝试连接完全不同的场景。
- 默认选择最长且满足可重建门禁的连续场景，或在 UI 中提示用户拆分视频。
- 多场景不能静默合并为一个 PLY。

曝光突变但背景轨迹仍稳定时，不应仅凭 scene score 切段；使用亮度归一化后继续几何判断。

### 5A.9 COLMAP 质量闭环与固定策略回退

前置选择完成后正常运行当前 CUDA/CPU SIFT、顺序匹配和增量 Mapper。自适应结果出现以下任一情况时进入定向修复：

- 均衡注册率低于 90%，或精细低于 95%。
- 连续关键帧间的几何内点/网格覆盖低于门禁。
- 某个内部关键帧只与一侧邻居可靠连接。
- 产生多个明显碎片模型。
- 时间轨迹存在无法跨越的弱连接区间。

定向修复规则：

- 只在弱连接时间区间选择最清晰的中间代理候选。
- 每个弱区间最多增加 1–2 张。
- 新增总数不超过初选数量的 25%。
- 最多自动修复一次，避免无限重建。
- 补帧后的 COLMAP 使用新的 attempt 目录；不能原地污染已完成尝试。

修复后仍不合格，执行安全回退：

```text
balanced -> 原固定 2 FPS + 当前 pHash/Laplacian
high     -> 原固定 4 FPS + 当前 pHash/Laplacian
```

固定回退使用独立帧目录、数据库和稀疏输出。最终只提升通过现有重建校验的尝试，并在项目状态中记录 `adaptiveFallbackUsed` 和原因。该回退是抽帧策略回退，不得触发暂缓的 GLOMAP 或 LightGlue 路线。

### 5A.10 数据结构与状态字段

建议扩展：

```rust
enum FrameSelectionStrategyKind {
    UniformRatio,
    AdaptiveSfm,
}

struct AdaptiveFrameProfile {
    anchor_fps: f64,
    analysis_fps: f64,
    local_refine_fps: f64,
    target_motion: f64,
    max_motion: f64,
    min_interval_ms: u64,
    min_inliers: u32,
    min_grid_coverage: f64,
    min_three_view_tracks: u32,
}

struct SelectedSourceFrame {
    source_index: u64,
    pts_seconds: f64,
    reason: SelectionReason,
    motion: f64,
    inliers: u32,
    grid_coverage: f64,
    sharpness: f64,
}
```

`FramePlan`/`FrameState` 至少新增并使用 serde defaults 保持旧历史可读：

```text
strategy
anchorFps
analysisFps
effectiveFps
proxyCandidates
selectedSourceFrames
sceneSegments
motionFactorP50
motionFactorP95
bridgeFrames
weakGapRepairs
adaptiveFallbackUsed
adaptiveFallbackReason
```

阶段计时新增：

```text
frameAnalysisMs
adaptivePlanningMs
selectedExtractionMs
adaptiveRepairMs
```

### 5A.11 测试矩阵

单元测试：

- 30/60/120 FPS、相同 PTS 轨迹产生近似相同关键帧数量。
- VFR 输入按 PTS 排序，选中的源帧编号不重复且单调递增。
- 静止窗口不会因 `k_window` 下限持续填充图片。
- 快速运动在轨迹断裂前插入桥接帧。
- pHash 相似但几何位移足够时不得删除。
- 低几何位移且 pHash 相似时只保留更清晰帧。
- 动态离群运动不会提高背景抽帧率。
- 场景分数高但背景轨迹稳定时不误切段。
- 真场景切换不会跨段补帧。
- 清晰度择优不能选择不满足共视门禁的帧。

集成测试：

- 同一视频以固定 2/4 FPS 和自适应路径运行，attempt 目录互不污染。
- 定向补帧只发生在弱区间，新增不超过 25%，且最多一次。
- 自适应失败后能进入固定回退，并留下明确事件、状态和日志。
- 用户取消发生后不得自动开始修复或固定回退。
- CPU-only 机器能完成代理分析、自适应选择和固定回退。
- 完整 Rust 测试、前端状态兼容测试和打包校验均通过。

真实素材至少覆盖：

- 高 FPS 慢速绕拍。
- 低 FPS 快速移动。
- 长时间静止后继续移动。
- 低纹理墙面与正常纹理混合。
- 明显运动模糊。
- 有人/车辆经过的动态干扰。
- 单镜头环拍和包含剪辑切换的多场景视频。

### 5A.12 验收门禁

质量：

- 均衡自适应结果注册率不低于 90%，精细不低于 95%；否则必须修复或回退。
- 与同素材固定基线相比，注册率下降不超过 2 个百分点。
- 三维点数不低于固定基线 90%。
- 平均重投影误差不高于固定基线 10%。
- 不增加未报告的碎片模型。
- 相同训练后端能够消费模型并生成合法 PLY。

去冗余与性能：

- 高 FPS 慢速/静止素材的最终帧数必须显著低于简单按源 FPS 比例抽取的结果。
- 正常绕拍的帧数应围绕原 2/4 FPS 软预算，不因代理扫描率 6/8 FPS 而全部保留。
- 快速运动只在必要区间局部超过原 2/4 FPS。
- 报告代理扫描、规划、原图抽取、补帧和回退的完整时间；不能只报告减少的 JPEG 数量。
- 固定回退发生率和原因必须纳入验收。若大量正常素材都触发回退，应先重新标定阈值，不得发布为默认。

### 5A.13 弱区补充素材与用户引导

当自动补帧关闭，或一次受限的自动补帧仍不能让自适应重建达到质量门禁时，项目进入持久化的 `needsSupplement` 状态。它不是运行中的进程暂停：所有 FFmpeg、COLMAP 和训练子进程已经结束并释放资源；项目保留诊断和可恢复上下文，等待用户作出下一步选择。

当前实现状态（2026-08-24）：关闭自动补帧且诊断到弱区时，系统会在训练前写入 `state.json` / `project.json` 的 `needsSupplement`，将项目状态显示为“等待补充素材”，并停止正常流水线；已释放的任务不会以“暂停中”形式占用 FFmpeg、COLMAP 或训练资源。开启自动补帧时，系统会写入完整代理分析和 `logs/adaptive-bridge-plan.json`：每个弱区最多两张，合计不超过初选帧的 25%，并排除已选帧、确认场景切换和缺少基本几何证据的候选。若首轮注册率低于 80%，会最多执行一次 `colmap-attempts/supplemented-1`：从原视频重新提取“原关键帧加桥接帧”的完整时间序列，以独立 frames、数据库和稀疏模型做特征、顺序匹配和 Ceres Mapper。只有新 attempt 注册率至少 80%、未失败且注册张数超过原 attempt 时才提升它；否则保留原帧、原数据库和原模型，日志明确记录未采用原因。补充素材上传与低成本验证仍是后续步骤。

无论自适应是否最终回退，代理扫描成功后都必须保留 `logs/adaptive-proxy-analysis.json` 和 `logs/adaptive-proxy-diagnostics.json`。后者记录实际候选/入选数量、各门禁阈值、满足完整几何门禁的帧数、分别低于内点/网格覆盖/三视图轨迹阈值的计数、确认场景切换数和三项指标中位数。回退日志必须指向该文件，便于基于真实素材标定阈值，不能仅显示“可靠关键帧不足”。

代理 JPEG 保持最长边 640，避免无谓增加解码、磁盘与 JPEG 成本；几何分析工作图提高到 320×240。门禁不得继续以固定 160 格的绝对内点和覆盖率作硬否决，而是记录并使用纹理格、成功匹配格、内点相对匹配率和三视图相对内点率：均衡档初始下限为 `12 / 8 / max(6, 45%) / max(3, 35%)`，精细档为 `20 / 12 / max(10, 55%) / max(5, 45%)`。这里的百分比分别以成功匹配格和内点为分母；COLMAP 的真实特征/RANSAC/Mapper 质量门禁仍是最终裁决。快速运动的金字塔或粗到细追踪是后续独立步骤，不能靠无限提高代理 JPEG 分辨率替代。

代理跟踪采用三层粗到细 patch 匹配：最粗层 ±6 px，两个细化层各 ±2 px，折算到 320×240 工作图约 ±30 px 可跟踪位移；网格边界同步扩大以保证候选 patch 不会越界。它替换单尺度 ±6 px 搜索，优先改善快速绕拍时的匹配格和三视图轨迹；JPEG 代理尺寸仍不变。

精细档采用分层候选策略，而不是把更高的采样密度与更严的硬拒绝门绑定：先使用 `strict`（`20 / 12 / max(10, 55%) / max(5, 45%)`）；少于三个时间有序关键帧时，保持 `8 FPS`、`80 ms` 与较小运动目标，转为 `relaxed`（`14 / 9 / max(7, 45%) / max(3, 35%)`）；仍不足时转为 `minimumObservable`（均衡档的 `12 / 8 / max(6, 45%) / max(3, 35%)`）。每次降级必须写入任务日志和 `adaptive-frame-selection.log`；它只放宽代理前端，真实 COLMAP 注册率、点数和重投影质量仍是接受与训练的最终门槛。

精细档不得因为凑够三个初始化候选就视为成功。它还必须达到覆盖预算：`ceil(视频秒数 × 4 FPS × 20%)`，限制在 `12–32` 张。三层都按该预算判断；不足时写入 `adaptive_selection_target`、最终层级和 `selected / target`，并回退到固定高密度抽帧。该预算是防误训练的前端下限，不替代 COLMAP 的注册率与点质量验收。

若精细 `minimumObservable` 达到预算的 90% 但仍差少量帧，系统先在 `work/adaptive-near-budget-validation` 重抽该完整序列，并以独立数据库、特征、顺序匹配和 Ceres Mapper 做一次仅 SfM 验证。只有注册率至少 80%、注册数至少 75% 且模型未失败时才采用该自适应序列；验证失败、取消或出错都保留 attempt 证据并回退固定高密度路径。验证 attempt 不进入训练，也不能覆盖正式 frames、数据库或稀疏模型。

近预算验证未通过时，也必须把验证模型的已注册 JPEG 名映射回源 PTS，写入 `adaptive-near-budget-registered-frames.json`，并写入受限的 `adaptive-near-budget-bridge-plan.json`。计划沿用每弱区最多两张、总数不超过初选的 25% 的限制；它为后续独立桥接 repair attempt 提供可审计输入，不能与正式 attempt 混用。

当前实现会立刻执行一次 `work/adaptive-near-budget-bridge-repair`：将原近预算关键帧与该计划中的桥接帧按源时间重新抽取，在独立数据库和 Ceres Mapper 中验证。仅当 repair 的注册率至少 80%、注册数至少 75% 且模型未失败时，才重新抽取该序列进入正式 COLMAP；失败、取消和没有桥接候选均保留证据并回退固定高密度路径。整个 repair 最多一次。

若桥接 repair 仍未通过，执行一次有界的自适应密度升级：从已读取的 PTS 代理映射中每约 `0.5 s` 选择最近源帧，与已有关键帧合并，写入 `work/adaptive-density-validation` 并以独立 Ceres Mapper 验证。该序列保持源 PTS、不会由平均 FPS 推算，且通常远少于固定 `4 FPS` 输入；只有同样达到 80% 注册率和 75% 注册数门槛才采用。失败后才最终回退固定高密度路径。

每个自适应任务还必须写入 `logs/adaptive-attempts.json`：包含 strict、relaxed、minimum-observable 的选帧数/预算，以及近预算验证、桥接 repair、密度验证的输入数、注册数、注册率、是否采用和简短结论。该报告与主界面事件日志互补，是阈值标定与问题复现的持久事实来源。

在正式 Mapper 完成后，报告必须再追加 `finalMapper` 项：记录实际进入训练的输入数、注册数、注册率、三维点数和有效 BA 后端。验证 attempt 不能被当作正式结果；只有该项才能证明验证质量在生产 CASPAR/Ceres 重建中兑现。

主界面在主视频输入旁提供 `自动从原视频补帧` 勾选项，默认开启：

- 开启时，系统先仅从原视频弱区补 1–2 张最清晰的桥接帧，且新增总数不超过初选的 25%。
- 关闭时，不从原视频追加抽帧；一旦检测到不满足门禁的弱区，直接进入 `needsSupplement`。
- 自动补帧后仍不达标，也转入 `needsSupplement`，不得无限增加原视频帧。

`needsSupplement` 卡片必须显示：

- 弱区在主视频中的开始与结束时间、问题类型（视差/重叠不足、模糊、低纹理、动态干扰或场景切换）。
- 弱区前后锚点图和一张或多张低分辨率弱区预览，供用户直接核对。
- 相对补拍指引：以前后锚点的相机运动为参考给出左/右/前/后/绕行方向，并用“让主体在画面中移动约 15–25% 宽度”表示建议视差。
- 不得把单目 SfM 的任意尺度直接说成厘米或米；只有存在用户提供的真实尺度来源时才能显示绝对移动距离。
- `上传补充视频或照片`、`改用固定 FPS 完成` 与 `保留诊断并结束` 三个明确动作。

补充媒体不直接恢复旧 COLMAP 进程，也不原地修改已完成 attempt 的数据库。上传后先做低成本验证：提取视频代理或读取照片缩略图，与弱区前后锚点检查清晰度、特征内点、网格覆盖和跨三视图连通性。验证失败必须说明“与原场景重叠不足”“视差不足”或其他具体原因，且不得启动完整重建。验证通过后，将补充素材绑定到指定弱区，创建独立的 `supplemented-<n>` 帧目录、数据库和稀疏模型 attempt，再运行完整特征/匹配/Mapper。

首版保留一个主视频输入；补充视频/照片只能从 `needsSupplement` 卡片上传，并显式绑定弱区。不要把主输入立即改成无语义的普通多选，以免丢失“哪个素材服务于哪个弱区”和拍摄顺序。后续“项目素材集”能力若支持初始多视频/照片导入，必须加入排序、分组和场景边界标注，不能静默合并。

建议持久化字段：

```text
autoBridgeFrames
needsSupplement
weakGaps[] { startPts, endPts, reason, anchorBefore, anchorAfter, previewPaths, captureHint }
supplementalMedia[] { path, kind, weakGapId, validationStatus, validationReason }
supplementAttemptId
```

自适应首选帧还必须单独写入 `logs/adaptive-selected-frames.json`。每条记录包含输出 JPEG 名、源帧索引、PTS、选择原因、运动量、内点、覆盖率和清晰度；后续弱区诊断通过它把 COLMAP 的图像文件名还原到视频时间轴。完成 Mapper 后还必须写入 `logs/adaptive-registered-frames.json`：逐帧记录是否注册，并将连续未注册的首选帧聚合为弱区，保留弱区时间范围、前后注册锚点和文件名。该文件只是诊断事实，不能单凭一个未注册帧就自动重建或把项目改为失败；质量门禁负责决定后续的自动补帧或 `needsSupplement`。

验收：关闭自动补帧时不会暗中追加原视频帧；进入 `needsSupplement` 后关闭/重开应用仍能恢复诊断；不合格素材不会启动 COLMAP；补充重建不会修改原 attempt；取消补充流程不会删除诊断或原结果。

## 6. 里程碑 2：真正启用 CUDA SIFT 和 CUDA 匹配

### 6.1 配置模型

不要继续根据 executable 路径推断行为。建议引入明确配置：

```rust
enum ColmapComputeMode {
    Cpu,
    Cuda { gpu_index: i32 },
}

struct ColmapFeatureOptions {
    compute: ColmapComputeMode,
    feature_type: Sift,
}

struct ColmapMatchingOptions {
    compute: ColmapComputeMode,
    matching_type: SiftBruteforce,
    overlap: u32,
}
```

本期枚举中只实现 SIFT 和 SIFT_BRUTEFORCE，避免为暂不实施的 LightGlue 暴露无效选项。

### 6.2 命令行映射

CPU：

```text
--FeatureExtraction.type SIFT
--FeatureExtraction.use_gpu 0
--FeatureMatching.type SIFT_BRUTEFORCE
--FeatureMatching.use_gpu 0
```

CUDA：

```text
--FeatureExtraction.type SIFT
--FeatureExtraction.use_gpu 1
--FeatureExtraction.gpu_index -1
--FeatureMatching.type SIFT_BRUTEFORCE
--FeatureMatching.use_gpu 1
--FeatureMatching.gpu_index -1
```

参数名必须以本地固定的 COLMAP 4.1.1 `-h` 输出为准；实施时不得根据记忆填写未验证参数。

### 6.3 尝试目录与回退边界

CUDA 运行可能在数据库中部分写入特征或匹配，因此不能在同一数据库上原地切换 CPU。建议目录：

```text
work/colmap-attempts/cuda/
  database.db
  sparse/
  logs/

work/colmap-attempts/cpu-fallback/
  database.db
  sparse/
  logs/

work/colmap/
  database.db      # 验收后提升或复制
  sparse/0/        # 验收后规范化输出
```

回退只在以下能力/运行错误发生：

- 无兼容 CUDA 设备或驱动。
- CUDA DLL/运行时加载失败。
- COLMAP 报告 CUDA 不可用。
- 显存不足。
- CUDA 子进程非零退出且错误被分类为设备/运行时错误。

图片不足、匹配图差、注册率低等数据质量问题不能伪装成 CUDA 能力问题。此类失败应进入正常重建诊断，不自动声称 CPU 可以修复。

### 6.4 生效状态与事件

项目状态新增或补齐：

```text
requested_colmap_backend
effective_colmap_backend
feature_compute_device
matching_compute_device
feature_gpu_index
matching_gpu_index
cuda_fallback_used
cuda_fallback_reason
colmap_version
```

UI 只有在进程参数确定且 CUDA 健康检查通过后才能显示“CUDA 特征提取/匹配”。回退时发送一次清晰事件，例如：“CUDA SIFT 因显存不足失败，正在使用独立 CPU 数据库重试”。

### 6.5 测试

- CPU 后端参数固定为 `use_gpu=0`。
- CUDA 后端参数固定为 `use_gpu=1`，并传递验证过的 GPU index。
- 事件显示值与实际 ProcessSpec 一致。
- CUDA 尝试失败不会污染 CPU fallback 数据库。
- 非 CUDA 数据质量失败不触发错误的 CPU 回退。
- 取消发生在特征或匹配阶段时，当前子进程和尝试目录正确收口。
- 真实机器日志必须包含 GPU SIFT/匹配证据；仅 `exitCode=0` 不算 CUDA 功能验收。

### 6.6 验收

- 与相同帧输入的 CPU 基线相比，注册率下降不超过 2 个百分点。
- 平均重投影误差不高于基线 10%。
- 三维点数不低于基线 90%，除非有明确证据表明基线含大量离群点。
- 特征和匹配阶段中位数必须改善；若小任务受进程启动影响没有改善，仍需保证大任务明显改善。

### 6.7 实施状态（部分完成）

- 已实现 `ColmapComputeMode`、`ColmapFeatureOptions` 与 `ColmapMatchingOptions`。
- CPU 命令明确传递 `SIFT` / `SIFT_BRUTEFORCE` 与 `use_gpu=0`；CUDA 明确传递 `use_gpu=1`、`gpu_index=-1`。参数来自本机 4.1.1 帮助输出。
- CPU/CUDA 输出分别写入 `work/colmap-attempts/cpu/` 与 `work/colmap-attempts/cuda/`，不共享数据库或稀疏输出。
- 已实现仅针对 CUDA 运行时错误的 CPU fallback，并写入 `requested/effective` 设备字段与原因；数据质量失败不会触发该回退。CUDA 真实设备日志与端到端质量基准仍需单独验收。

## 7. 里程碑 3：固定 COLMAP 4.1.1 发行物和 manifest

### 7.1 目的

确保开发目录中验证的 CUDA 行为与安装包一致，避免 manifest 声明、缓存归档和实际 executable 漂移。

### 7.2 固定内容

- 官方 release tag：`4.1.1`。
- `colmap -h` 实际版本、提交和 `with CUDA` 状态。
- CPU/no-CUDA 与 CUDA 包分别固定，不能共享错误的 `actualVersion`。
- 下载归档 URL 和完整 64 位 SHA-256。
- 所有直接打包的 executable/DLL SHA-256。
- COLMAP BSD-3-Clause 许可文件。
- CUDA、ONNX Runtime、Caspar/cuDSS 等随包组件的许可证和再分发说明。
- 可选 CUDA 包的安装策略和磁盘体积。

### 7.3 健康检查

安装后执行只读检查：

1. executable 存在且哈希匹配。
2. `colmap -h` 返回预期版本和 CUDA 状态。
3. `feature_extractor -h` 暴露 CUDA 参数。
4. `sequential_matcher -h` 暴露 SIFT GPU 参数。
5. `mapper -h` 暴露 `CASPAR` 可选后端参数。
6. 使用小型自带测试集完成 CUDA SIFT smoke test。
7. 使用已知稀疏问题完成 Caspar BA smoke test；只检查帮助文本不算运行验收。

### 7.4 打包门禁

- `verify-engines`、license verifier、schema 校验全部通过。
- 安装包解包后的文件哈希与 manifest 一致。
- 在没有开发目录和系统 Python/CUDA Toolkit 的干净机器上运行 smoke test。
- CPU-only 安装不包含意外 CUDA/cuDNN DLL。
- CUDA 包缺失时 UI 明确显示可安装/不可用，而不是静默使用 CPU executable 并显示 CUDA。

### 7.5 实施状态（部分完成）

- `manifest.json` 已固定到官方 `4.1.1` Windows CPU/CUDA URL 与 64 位 SHA-256。
- CUDA 可执行文件 SHA-256 已列入 `optionalFiles[]`；校验脚本检查 CUDA 宣告、特征/匹配 GPU 参数、Mapper BA 参数与主程序哈希。
- CPU 归档的实际帮助文本仍报告 `4.1.0.dev0 ... without CUDA`，manifest 以实际输出记录该差异，不能把 release tag 当作 binary version 伪报。
- CUDA SIFT smoke test、Caspar smoke test、所有 DLL 哈希与干净机器打包验收仍待后续里程碑。

## 8. 里程碑 4：中大型任务启用增量 Mapper + Caspar

### 8.1 路由策略

初始策略按保留帧数路由，阈值在基准后可调整：

| 保留帧数 | 默认 Mapper | BA 后端 |
|---:|---|---|
| 1–150 | incremental `mapper` | Ceres |
| 151 及以上，Caspar 健康 | incremental `mapper` | Ceres local + Caspar global |
| 151 及以上，Caspar 不健康 | incremental `mapper` | Ceres |

不根据候选帧数路由，因为真正影响 COLMAP 的是筛选后保留帧数。

**实施状态：** 已按该阈值路由。CASPAR 和 Ceres 分别输出至 `incremental-caspar/sparse/`、`incremental-ceres/sparse/`；CASPAR 的进程、模型解析或低于 50% 注册率失败均会回退 Ceres，取消不会触发回退。该路径已通过参数和单元测试，但还没有 151+ 帧真实重建的 CASPAR 成功证据，故继续标记为实验。

设置已提供 `Mapper BA = 自动 / 强制 Ceres / 强制 CASPAR`。公平基准应在两次运行中均选择 CUDA，仅切换此设置；CPU 下即使选择强制 CASPAR，实际也会使用 Ceres，项目记录会保留请求值和实际值。

### 8.2 配置结构

建议增加：

```rust
enum IncrementalBaBackend {
    Ceres { use_gpu: bool, gpu_index: i32 },
    Caspar { gpu_index: i32 },
}

struct IncrementalMapperOptions {
    ba_backend: IncrementalBaBackend,
}
```

Caspar 路径使用本地 4.1.1 已验证的：

```text
--Mapper.ba_local_backend CERES
--Mapper.ba_global_backend CASPAR
--Mapper.ba_gpu_index -1
```

COLMAP 4.1.1 会拒绝 `ba_local_backend=CASPAR`；CASPAR 仅用于 global BA。不要把 `Mapper.ba_use_gpu=1` 当作 Caspar 开关；该参数属于 Ceres GPU 路径。最终参数组合以固定二进制帮助文本和 smoke test 为准。

### 8.3 独立尝试目录

```text
work/colmap-attempts/incremental-caspar/sparse/
work/colmap-attempts/incremental-ceres/sparse/
```

两条路线可以读取同一份已经完成并只读使用的 `database.db`，但不能共享 `sparse` 输出目录。验收后只提升被选中的模型。

### 8.4 Caspar 自动回退条件

本期允许的自动回退仅限增量 Caspar -> 增量 Ceres：

- Caspar 未编译或运行时依赖缺失。
- GPU/驱动/cuDSS 初始化失败。
- 显存不足。
- Mapper 非零退出。
- 没有生成可解析模型。
- 最佳模型 `registered_images == 0` 或 `points3d == 0`。
- 注册比例低于现有硬失败门禁 50%。

注册率 50%–90% 不立即双跑 Ceres，因为每次都双跑会抵消 Caspar 加速。该区间沿用现有质量报告并记录警告；如果后续真实样本证明 Caspar 在此区间存在系统性退化，再调整门禁。

### 8.5 结果提升

1. 对尝试目录运行现有 `best_sparse_model`。
2. 验证模型文件、图片注册数、三维点数和注册比例。
3. 将通过门禁的最佳模型规范化到正式 `work/colmap/sparse/0`。
4. 写入 `effective_ba_backend`、是否回退、尝试耗时和回退原因。
5. 训练后端只能读取正式规范化目录，不能扫描 attempt 目录猜测结果。

### 8.6 测试与验收

- 150/151 张边界路由测试。
- Caspar 参数和 Ceres 参数互斥测试。
- Caspar 运行失败触发 Ceres，且两个 sparse 目录互不污染。
- Caspar 输出空模型触发 Ceres。
- Caspar 成功且通过门禁时不额外运行 Ceres。
- 取消 Caspar 后不应未经用户请求自动开始 Ceres。
- 631 张样本比较 Caspar 与原增量 Ceres：注册率差不超过 2 个百分点、重投影误差不高于 10%、三维点数不低于 90%。
- 只有在 Mapper 中位数明确改善且未增加异常失败率时，才把 151+ 默认路由设为 Caspar。

## 9. 里程碑 5：综合基准、发布和回滚

### 9.1 基准矩阵

| 配置 | 抽帧规划 | 串行筛选 | 并行筛选 | CPU SIFT | CUDA SIFT | Ceres | Caspar |
|---|---|---:|---:|---:|---:|---:|---:|
| 原始基线 | 固定 2/4 FPS | 是 | 否 | 是 | 否 | 是 | 否 |
| 仅筛选优化 | 固定 2/4 FPS | 否 | 是 | 是 | 否 | 是 | 否 |
| CUDA COLMAP | 固定 2/4 FPS | 否 | 是 | 否 | 是 | 是 | 否 |
| 自适应抽帧对照 | AdaptiveSfM | 否 | 是 | 与基线相同 | 与基线相同 | 是 | 否 |
| 本期最终 | AdaptiveSfM；失败时固定回退 | 否 | 是 | 否 | 是 | 小任务 | 中/大任务 |

每个配置对固定小、中、大帧目录执行一次预热和三次正式运行。COLMAP 微基准不运行 Brush；端到端基准另行运行 Brush，避免训练噪声掩盖 SfM 变化。

### 9.2 统一质量门禁

- 输入帧文件名和文件内容一致。
- 固定回退路径的筛选结果必须与原基线一致；自适应路径允许帧列表变化，但必须记录源帧编号、PTS 和选择原因。
- 均衡自适应注册率达到 90%、精细达到 95%；否则执行定向补帧或固定回退。
- 注册比例相对基线下降不超过 2 个百分点。
- 三维点数不低于基线 90%。
- 平均重投影误差不高于基线 10%。
- 输出至少包含一个可解析稀疏模型；不能产生额外碎片模型而不报告。
- 相同训练后端能够消费规范化 `sparse/0` 并生成合法 PLY。
- PLY header、vertex count、文件大小和查看器加载均通过现有校验。

### 9.3 性能门禁

- 84 张筛选中位数至少提升 2 倍，目标 5–15 秒。
- 高 FPS 慢速/静止素材必须减少冗余帧；快速运动只在必要区间局部加密。
- 自适应代理扫描和规划开销必须计入端到端时间，不能只比较 COLMAP 输入张数。
- 定向补帧与固定回退的失败成本必须单独报告。
- CUDA 特征/匹配在 631 张样本上必须明显快于 CPU 基线。
- Caspar Mapper 在 631 张样本上必须明显快于 Ceres，同时通过质量门禁。
- 报告每项的绝对秒数和端到端占比，不只报告倍数。
- CUDA、Caspar 初始化和进程启动时间计入阶段时间。
- 回退运行的总时间单独报告，不能只报告失败前或回退后的局部时间。

### 9.4 功能开关与回滚

建议保留内部配置：

```text
frameSelectionParallel = true/false
frameSelectionStrategy = uniform/adaptive
adaptiveAnchorFps = 2/4
adaptiveRepair = true/false
adaptiveFixedFallback = true/false
colmapGpuFeatures = auto/off
incrementalBaBackend = auto/ceres/caspar
casparMinImages = 151
```

发布策略：

1. 并行筛选在黄金测试完全一致后默认开启，可一键回串行。
2. 自适应抽帧先作为均衡/精细实验开关；完成多类型真实素材标定且固定回退率可接受后再默认开启。
3. CUDA 特征/匹配在健康检查通过时默认开启，设备失败自动使用独立 CPU 尝试。
4. Caspar 首个版本保留实验标识；收集代表样本后再默认用于 151+。
5. 任一阶段发现质量回归，可以单独关闭对应开关，不回退其他已验证优化。

## 10. 文件级改造清单

预计涉及文件如下，实施前以实际调用关系复核：

| 文件 | 计划变更 |
|---|---|
| `src-tauri/Cargo.toml` | 增加固定版本的并行依赖（若选择 Rayon） |
| `src-tauri/Cargo.lock` | 锁定依赖 |
| `src-tauri/src/video/select.rs` | 拆分并行指标、顺序决策、删除提交；缓存 DCT 表 |
| `src-tauri/src/video/frame_plan.rs` | 新增 `AdaptiveSfm`、档位配置、源帧编号/PTS 计划和旧 `UniformRatio` 回退 |
| `src-tauri/src/video/adaptive.rs`（新增） | 代理分析、可靠背景轨迹、累计视差、桥接、场景分段和定向补帧规划 |
| `src-tauri/src/engines/ffprobe.rs` / `video/probe.rs` | 暴露 VFR 判断和实际帧时间戳所需信息 |
| `src-tauri/src/engines/ffmpeg.rs` | 代理扫描和按源帧编号/PTS 精确提取，不再只接受单一 `sampling_fps` |
| `src-tauri/src/engines/colmap.rs` | 接收显式 feature/matching/mapper 配置，移除硬编码 CPU |
| `src-tauri/src/engines/health.rs` | CUDA、版本和 Caspar smoke 状态 |
| `src-tauri/src/pipeline/runner.rs` | 后端路由、attempt 目录、回退、事件、计时和结果提升 |
| `src-tauri/src/pipeline/mod.rs` | 状态和计时字段 |
| `src-tauri/src/project/*` | 新字段序列化默认值和旧项目兼容 |
| `engines/manifest.json` | 固定真实 4.1.1 包、哈希、运行时和许可 |
| `README.md` / `CODE_WIKI.md` | 用户可见能力、限制、回退和诊断说明 |
| Rust 集成/单元测试 | 参数、确定性、错误分类、回退、质量门禁 |

如果工作区不是可靠 Git 仓库，备份目录是恢复依据；不得使用 `git reset --hard` 或删除现有未归属改动。

## 11. 备选方案：独立 GLOMAP 路线与自动质量回退

**状态：本期暂不实施。**

未来只有在以下前置条件满足后才启动：

- 500+ 张任务仍由增量 Mapper 主导耗时。
- 已有可靠的环拍闭环匹配图。
- 可以从标定信息提供焦距，或验证 `view_graph_calibrator` 的稳定性。
- 可以在数据库副本上运行校准，绝不原地破坏增量路线数据库。
- 本地固定 COLMAP 版本明确支持所需 global mapper 参数。

预备设计：

```text
已完成的 feature/matches 数据库
  -> copy database for global route
  -> optional view_graph_calibrator on copy
  -> global_mapper into isolated sparse-global/
  -> validate global result
  -> pass: promote global result
  -> fail: run or select incremental Caspar/Ceres result
```

未来质量门禁应至少包括：

- `global_mapper` 进程成功。
- 模型可解析且非空。
- 注册比例达到绝对门禁，并与增量基准相差不超过允许值。
- 三维点数、平均重投影误差和碎片模型数合格。
- 环拍首尾相机关系没有明显断裂。
- 下游训练能够消费该模型并生成合法 PLY。

由于当前 4.1.1 `global_mapper` 未暴露 Caspar BA 后端，本备选不能把“GLOMAP + Caspar”当作本地已具备的组合。未来若升级版本，必须重新做参数、依赖和许可证审计。

## 12. 备选方案：预置 ONNX 的 LightGlue 恢复模式

**状态：本期暂不实施，且排在 GLOMAP 评估之后。**

未来触发条件：

- CUDA SIFT brute-force 在困难素材上注册率持续不足。
- 失败可归因于匹配质量，而不是抽帧过疏、运动模糊、内参错误或闭环配对缺失。
- 产品允许增加 ONNX 模型和 CUDA Runtime 的安装体积。

优先顺序：

1. 保持 SIFT 特征，仅把匹配器切换为 `SIFT_LIGHTGLUE`。
2. 只有 SIFT-LightGlue 仍失败时，实验 `ALIKED + ALIKED_LIGHTGLUE`。
3. 不允许 SIFT 描述子配 ALIKED-LightGlue，反之亦然。

未来需要完成：

- 固定 ONNX 模型 URL、版本、SHA-256 和许可证。
- 将模型预置在引擎目录，禁止任务中途联网下载。
- 健康检查 ONNX Runtime CUDA provider 和模型可加载性。
- 每种 matcher 使用独立数据库，或在重匹配前明确清理旧 matches。
- 注册率不足时才触发恢复模式，并在日志中说明恢复原因。
- 对速度、显存、匹配数、inlier ratio、注册率和重投影误差做独立评测。

LightGlue 不能替代闭环候选对生成；即使未来启用，仍需单独解决环拍首尾应该匹配哪些图片的问题。

## 13. 备选方案：完整 CUDA 帧筛选

**状态：本期暂不实施。**

只有在 CPU 并行完成后，候选帧常态超过 500–1000 张且筛选仍占主要时间，才评估批量 nvJPEG 解码、CUDA 灰度/缩放、Laplacian 和 DCT 融合。仅把 32×32 DCT kernel 搬到 CUDA、而 JPEG 仍在 CPU 解码并逐帧传输，通常不会形成合理的端到端收益。

## 14. 最终完成定义

本期只有同时满足以下条件才算完成：

- 两份设计文档与实际实现保持一致。
- 修改前备份可恢复。
- 45/84 张筛选结果与基线完全一致。
- 固定回退路径继续保持 1/2/4 FPS 兼容；均衡/精细自适应路径按源帧编号/PTS 可复现。
- 30/60/120 FPS 的同轨迹素材不会仅因源 FPS 提高而成比例增加最终帧数。
- 静止、快速运动、低纹理、动态干扰和场景切换素材均通过自适应门禁、定向补帧或固定策略回退。
- 自适应补帧不超过初选 25%、最多一次；仍失败时明确回退原固定 2/4 FPS。
- CPU-only 路线完整通过。
- 真实 NVIDIA 机器证明 SIFT 特征和匹配实际使用 CUDA。
- 631 张任务证明 Caspar 实际运行并改善 Mapper 时间。
- CUDA 与 Caspar 回退均使用隔离目录并留下明确证据。
- 相同输入、设备和质量预设下完成三次运行中位数比较。
- 注册率、三维点、重投影误差和 PLY 输出通过质量门禁。
- 完整 Rust 测试、引擎/许可证校验和桌面打包通过。
- GLOMAP 和 LightGlue 没有被误接入本期运行路径，只保留本文备选设计。
