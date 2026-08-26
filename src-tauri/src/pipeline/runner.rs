use crate::{
    engines::{
        self, brush, colmap,
        colmap::{
            ColmapComputeMode, ColmapFeatureOptions, ColmapMatchingOptions, IncrementalBaBackend,
            IncrementalMapperOptions, MapperBaMode,
        },
        ffmpeg::{
            extract_proxy_frames, extract_selected_frames, extract_selected_proxy_frames,
            extract_uniform_frames,
        },
        ffprobe::probe_video,
        training::{self, TrainingBackend, TrainingRequest},
        ColmapBackend, CudaColmapFlavor, EngineKind, EnginePaths, FfmpegHwAccel,
    },
    error::{Result, SplatError},
    pipeline::{
        progress::stage_progress_range, ColmapExecution, EventKind, EventLevel, InputSource,
        PipelineEngine, PipelineEvent, PipelineStage, SplatcamImportState, SupplementRequirement,
    },
    presets::Quality,
    process::{ProcessManager, ProcessObserver, ProcessUpdate},
    project::{
        atomic_write_json, FrameState, PipelineStateFile, ProjectManager, ProjectMetadata,
        ProjectOutput, ProjectPaths, ProjectStatus,
    },
    reconstruction::{
        ply::inspect_gaussian_ply,
        validator::{ReconstructionQuality, ReconstructionReport, ReconstructionValidator},
    },
    splatcam,
    video::{
        adaptive_plan, analyze_prepared_proxy_images_with_progress, passes_proxy_geometry,
        prepare_proxy_tracking_pyramids_with_progress, proxy_analysis_worker_count,
        select_adaptive_frames, select_useful_frames_parallel_with_progress, AdaptiveFrameProfile,
        FramePlan, FrameSelectionReport, FrameSelectionStrategy, ProxyFrame, SelectedSourceFrame,
        SelectionReason, SourceFrameTimestamp, UniformRatioFrameSelection, VideoInfo,
    },
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

/// CASPAR has a fixed startup/initialization cost. On smaller sequences Ceres
/// is both more predictable and usually faster; use CASPAR only where its GPU
/// bundle adjustment can amortize that cost.
const CASPAR_MINIMUM_IMAGES: u64 = 151;

fn should_use_caspar(
    backend: ColmapBackend,
    caspar_available: bool,
    mode: MapperBaMode,
    retained: u64,
) -> bool {
    backend == ColmapBackend::Cuda
        && caspar_available
        && match mode {
            MapperBaMode::Auto => retained >= CASPAR_MINIMUM_IMAGES,
            MapperBaMode::Ceres => false,
            MapperBaMode::Caspar => true,
        }
}
pub struct PreparedFrames {
    pub video: VideoInfo,
    pub plan: FramePlan,
    pub extracted_frames: u64,
    pub selection: FrameSelectionReport,
    pub probe_ms: u64,
    pub extract_ms: u64,
    pub select_ms: u64,
    pub frame_analysis_ms: u64,
    pub adaptive_planning_ms: u64,
    pub selected_extraction_ms: u64,
    pub adaptive_fallback_reason: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineResult {
    pub project_id: String,
    pub project_path: PathBuf,
    pub final_ply: PathBuf,
    pub file_size: u64,
    pub splat_count: u64,
    pub input_images: u64,
    pub registered_images: u64,
    pub registered_ratio: f64,
    pub points_3d: u64,
    pub duration_ms: u64,
    pub completed_at: chrono::DateTime<Utc>,
    pub warning: Option<String>,
    pub logs_directory: PathBuf,
    pub colmap_backend: ColmapBackend,
}

/// Read-only view model for a project that stopped before training because
/// selected frames contain a weak registration interval. It intentionally
/// contains only persisted diagnostics: requesting it cannot resume FFmpeg,
/// COLMAP, or training.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupplementDiagnostics {
    pub selected_frames: u64,
    pub registered_frames: u64,
    pub weak_intervals: Vec<SupplementWeakInterval>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupplementWeakInterval {
    pub reason: String,
    pub start_pts_seconds: f64,
    pub end_pts_seconds: f64,
    pub unregistered_frames: u64,
    pub first_output_file: String,
    pub last_output_file: String,
    pub before_anchor: Option<SupplementAnchor>,
    pub after_anchor: Option<SupplementAnchor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupplementAnchor {
    pub output_file: String,
    pub pts_seconds: f64,
}

pub async fn read_supplement_diagnostics(project: &Path) -> Result<SupplementDiagnostics> {
    let diagnostics_path = project.join("logs").join("adaptive-registered-frames.json");
    let diagnostics = serde_json::from_slice::<SupplementDiagnostics>(
        &tokio::fs::read(&diagnostics_path).await?,
    )?;
    if diagnostics.weak_intervals.is_empty() {
        return Err(SplatError::Process("项目没有可展示的弱区诊断".into()));
    }
    Ok(diagnostics)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdaptiveSelectedFramesLog {
    frames: Vec<AdaptiveSelectedFrameLog>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdaptiveProxyAnalysisLog {
    frames: Vec<ProxyFrame>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdaptiveSelectedFrameLog {
    output_file: String,
    source_index: u64,
    pts_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct RegisteredFrameTimeline {
    selected_frames: u64,
    registered_frames: u64,
    frames: Vec<RegisteredFrameTimelineEntry>,
    weak_intervals: Vec<WeakFrameInterval>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct RegisteredFrameTimelineEntry {
    output_file: String,
    pts_seconds: f64,
    registered: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WeakFrameInterval {
    reason: &'static str,
    start_pts_seconds: f64,
    end_pts_seconds: f64,
    unregistered_frames: u64,
    first_output_file: String,
    last_output_file: String,
    before_anchor: Option<WeakIntervalAnchor>,
    after_anchor: Option<WeakIntervalAnchor>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WeakIntervalAnchor {
    output_file: String,
    pts_seconds: f64,
}

#[derive(Debug, Clone, Copy)]
struct RegistrationTimelineSummary {
    selected_frames: u64,
    registered_frames: u64,
    weak_intervals: u64,
    planned_bridge_frames: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdaptiveBridgePlan {
    max_additional_frames: u64,
    planned_frames: Vec<AdaptiveBridgeFrame>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdaptiveBridgeFrame {
    source_index: u64,
    pts_seconds: f64,
    weak_interval_index: u64,
    reason: String,
    sharpness: f64,
    inliers: u32,
    grid_coverage: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdaptiveProxyDiagnostics {
    proxy_candidates: u64,
    selected_frames: u64,
    min_textured_cells: u32,
    min_matched_cells: u32,
    min_inliers_floor: u32,
    min_inlier_ratio: f64,
    min_three_view_floor: u32,
    min_three_view_ratio: f64,
    geometry_qualified_frames: u64,
    below_min_textured_cells: u64,
    below_min_matched_cells: u64,
    below_min_inlier_floor: u64,
    below_min_inlier_ratio: u64,
    below_min_three_view_floor: u64,
    below_min_three_view_ratio: u64,
    confirmed_scene_changes: u64,
    median_inliers: f64,
    median_textured_cells: f64,
    median_matched_cells: f64,
    median_grid_coverage: f64,
    median_three_view_tracks: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdaptiveAttemptReport {
    selection_target: Option<u64>,
    final_tier: Option<String>,
    fallback_reason: Option<String>,
    attempts: Vec<AdaptiveAttemptRecord>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdaptiveAttemptRecord {
    name: String,
    input_frames: u64,
    registered_images: Option<u64>,
    registered_ratio: Option<f64>,
    accepted: bool,
    detail: String,
}
#[derive(Clone)]
struct EventSink {
    emit: Arc<dyn Fn(PipelineEvent) + Send + Sync>,
    sequence: Arc<AtomicU64>,
    dispatch: Arc<std::sync::Mutex<()>>,
    started: Instant,
    task_log: Arc<std::sync::Mutex<Option<PathBuf>>>,
}
impl EventSink {
    #[allow(clippy::too_many_arguments)]
    fn send(
        &self,
        stage: PipelineStage,
        engine: Option<PipelineEngine>,
        kind: EventKind,
        level: EventLevel,
        stage_progress: Option<f32>,
        indeterminate: bool,
        message: impl Into<String>,
        current: Option<u64>,
        total: Option<u64>,
        unit: Option<&str>,
    ) {
        let (start, end) = stage_progress_range(stage);
        let progress = stage_progress
            .map(|value| start + (end - start) * value.clamp(0.0, 1.0))
            .unwrap_or(start);
        let _dispatch = self
            .dispatch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let event = PipelineEvent {
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed) + 1,
            timestamp: Utc::now(),
            kind,
            level,
            stage,
            engine,
            progress,
            stage_progress: stage_progress.map(|value| value.clamp(0.0, 1.0) * 100.0),
            indeterminate,
            message: message.into(),
            current,
            total,
            unit: unit.map(str::to_owned),
            elapsed_ms: self.started.elapsed().as_millis() as u64,
        };
        if let Some(path) = self
            .task_log
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
        {
            if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
                let engine = event
                    .engine
                    .map(|value| format!("{value:?}"))
                    .unwrap_or_else(|| "System".into());
                let _ = writeln!(
                    file,
                    "{}\t{:?}\t{}\t{}",
                    event.timestamp.to_rfc3339(),
                    event.level,
                    engine,
                    event.message
                );
            }
        }
        (self.emit)(event);
    }
    fn stage(&self, stage: PipelineStage, progress: f32, message: impl Into<String>) {
        self.send(
            stage,
            Some(PipelineEngine::System),
            EventKind::Stage,
            EventLevel::Info,
            Some(progress),
            false,
            message,
            None,
            None,
            None,
        );
    }
    fn set_task_log(&self, path: PathBuf) {
        *self
            .task_log
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(path);
    }
}
pub struct PipelineRunner {
    engines: EnginePaths,
    colmap_backend: ColmapBackend,
    cuda_colmap_flavor: CudaColmapFlavor,
    mapper_ba_mode: MapperBaMode,
    ffmpeg_hw_accel: FfmpegHwAccel,
    brush_training_preset: crate::presets::BrushTrainingPreset,
    gsplat_splat_cap: crate::presets::GsplatSplatCap,
    gsplat_densification_strategy: crate::engines::training::GsplatDensificationStrategy,
    photometric_mode: crate::engines::training::PhotometricMode,
    training_backend: TrainingBackend,
    auto_bridge_frames: bool,
    process_manager: ProcessManager,
    events: EventSink,
}
impl PipelineRunner {
    pub fn new(
        engines: EnginePaths,
        colmap_backend: ColmapBackend,
        cuda_colmap_flavor: CudaColmapFlavor,
        mapper_ba_mode: MapperBaMode,
        ffmpeg_hw_accel: FfmpegHwAccel,
        brush_training_preset: crate::presets::BrushTrainingPreset,
        gsplat_splat_cap: crate::presets::GsplatSplatCap,
        gsplat_densification_strategy: crate::engines::training::GsplatDensificationStrategy,
        photometric_mode: crate::engines::training::PhotometricMode,
        training_backend: TrainingBackend,
        auto_bridge_frames: bool,
        emit: impl Fn(PipelineEvent) + Send + Sync + 'static,
    ) -> Self {
        Self {
            engines,
            colmap_backend,
            cuda_colmap_flavor,
            mapper_ba_mode,
            ffmpeg_hw_accel,
            brush_training_preset,
            gsplat_splat_cap,
            gsplat_densification_strategy,
            photometric_mode,
            training_backend,
            auto_bridge_frames,
            process_manager: ProcessManager::new(),
            events: EventSink {
                emit: Arc::new(emit),
                sequence: Arc::new(AtomicU64::new(0)),
                dispatch: Arc::new(std::sync::Mutex::new(())),
                started: Instant::now(),
                task_log: Arc::new(std::sync::Mutex::new(None)),
            },
        }
    }
    pub async fn cancel(&self) {
        // Window-close confirmation must not leave a GPU training child behind.
        self.process_manager.kill().await;
    }
    pub fn colmap_backend(&self) -> ColmapBackend {
        self.colmap_backend
    }
    fn colmap_executable(&self) -> &Path {
        self.engines
            .colmap_for(self.colmap_backend, self.cuda_colmap_flavor)
    }
    pub async fn verify_pipeline_engines(&self) -> Result<()> {
        let statuses = self.engines.check_all().await;
        for required in [
            EngineKind::Ffmpeg,
            EngineKind::Ffprobe,
            EngineKind::Colmap,
            EngineKind::Brush,
        ] {
            let status = statuses
                .iter()
                .find(|status| status.kind == required)
                .expect("all engine kinds returned");
            if !status.exists {
                return Err(SplatError::EngineMissing(status.path.display().to_string()));
            }
            if !status.can_start {
                return Err(SplatError::EngineStart {
                    engine: format!("{required:?}"),
                    detail: status.detail.clone(),
                });
            }
        }
        match self.colmap_backend {
            ColmapBackend::Cpu => engines::require_cpu_colmap(&self.engines).await?,
            ColmapBackend::Cuda => {
                engines::require_cuda_colmap(&self.engines, self.cuda_colmap_flavor).await?
            }
        }
        colmap::require_verified_cli(self.colmap_executable())?;
        if self.training_backend == TrainingBackend::Brush {
            brush::require_verified_cli(&self.engines.brush)
        } else if engines::training::gsplat_runtime_healthy(&self.engines.root).await {
            Ok(())
        } else {
            Err(SplatError::UnsupportedEngine(
                "gsplat CUDA 实验运行时尚未安装或未通过健康检查；请改用 Brush。".into(),
            ))
        }
    }
    pub async fn prepare_frames(
        &self,
        input: &Path,
        quality: Quality,
        output: &Path,
        logs: Option<&Path>,
    ) -> Result<PreparedFrames> {
        self.events
            .stage(PipelineStage::ProbingVideo, 0.0, "正在读取视频信息");
        let probe_started = Instant::now();
        let video = probe_video(
            &self.engines.ffprobe,
            input,
            logs.map(|path| path.join("ffprobe.log")),
        )
        .await?;
        let probe_ms = probe_started.elapsed().as_millis() as u64;
        self.events.stage(
            PipelineStage::ProbingVideo,
            1.0,
            format!(
                "视频 {:.1} 秒 · {:.2} FPS · {}×{}",
                video.duration, video.fps, video.width, video.height
            ),
        );
        let mut plan = UniformRatioFrameSelection.create_plan(&video, &quality.preset());
        let mut adaptive_selected = None;
        let mut adaptive_proxy_frames = None;
        let mut adaptive_proxy_samples = None;
        let mut adaptive_profile = None;
        let mut adaptive_selection_tier = None;
        let mut adaptive_selection_target = None;
        let mut adaptive_reason = None;
        let mut adaptive_attempts = Vec::new();
        let mut frame_analysis_ms = 0;
        let mut proxy_analysis_workers = None;
        let mut proxy_jpeg_decode_ms = 0;
        let mut proxy_tracking_prepare_ms = 0;
        let mut proxy_grid_analysis_ms = 0;
        let mut adaptive_planning_ms = 0;
        let mut selected_extraction_ms = 0;
        if let Some(profile) = AdaptiveFrameProfile::for_quality(quality, video.fps) {
            adaptive_profile = Some(profile);
            self.events.stage(
                PipelineStage::PlanningFrames,
                0.0,
                "正在规划自适应 SfM 关键帧",
            );
            let planning_started = Instant::now();
            let estimated_proxy_frames =
                (video.duration * profile.analysis_fps).ceil().max(1.0) as u64;
            let project = output
                .parent()
                .ok_or_else(|| SplatError::Process("无法定位自适应抽帧工作目录".into()))?;
            let proxy_dir = project.join("work").join("adaptive-proxy").join("frames");
            let proxy_work = project.join("work").join("adaptive-proxy");
            self.events.stage(
                PipelineStage::ExtractingFrames,
                0.0,
                format!(
                    "正在以 {:.1} FPS 提取低分辨率代理画面并同步映射 PTS",
                    profile.analysis_fps
                ),
            );
            let proxy_result = extract_proxy_frames(
                &self.engines.ffmpeg,
                input,
                &proxy_dir,
                &proxy_work,
                video.width,
                video.height,
                profile.analysis_fps,
                logs.map(|path| path.join("ffmpeg-adaptive-proxy.log")),
                &self.process_manager,
                Some(self.process_observer(
                    PipelineStage::ExtractingFrames,
                    PipelineEngine::Ffmpeg,
                    Some(estimated_proxy_frames),
                    ObserverMode::Ffmpeg,
                )),
            )
            .await;
            match proxy_result {
                Ok(report) => {
                    let proxy_candidate_samples = report.frames.clone();
                    self.events.stage(PipelineStage::ExtractingFrames, 1.0, format!("高速代理抽帧完成：{} 帧（已同步映射源 PTS，缓存上限 {} 帧 / {:.1} GiB）", report.frames.len(), report.buffered_frame_limit, report.memory_budget_bytes as f64 / 1024.0 / 1024.0 / 1024.0));
                    self.events.send(
                        PipelineStage::SelectingFrames,
                        Some(PipelineEngine::System),
                        EventKind::Progress,
                        EventLevel::Info,
                        Some(0.0),
                        false,
                        format!("正在分析 {} 个代理画面的背景运动", report.frames.len()),
                        Some(0),
                        Some(report.frames.len() as u64),
                        Some("帧"),
                    );
                    let analysis_started = Instant::now();
                    let proxy_directory = proxy_dir.clone();
                    let proxy_source_frames = report.frames.clone();
                    let analysis_events = self.events.clone();
                    let analysis_workers = proxy_analysis_worker_count();
                    proxy_analysis_workers = Some(analysis_workers);
                    self.events.send(
                        PipelineStage::SelectingFrames,
                        Some(PipelineEngine::System),
                        EventKind::Log,
                        EventLevel::Info,
                        Some(0.0),
                        false,
                        format!("CPU 代理网格分析：{analysis_workers} 个工作线程"),
                        Some(analysis_workers as u64),
                        Some(analysis_workers as u64),
                        Some("线程"),
                    );
                    let analysis = tokio::task::spawn_blocking(move || {
                        let proxy_decode_started = Instant::now();
                        let mut paths = std::fs::read_dir(proxy_directory)?.filter_map(|entry| entry.ok().map(|entry| entry.path()))
                            .filter(|path| path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("jpg"))).collect::<Vec<_>>();
                        paths.sort();
                        let images = paths.into_iter().map(|path| image::open(&path).map_err(|error| SplatError::Process(format!("无法读取代理图 {}：{error}", path.display())))).collect::<Result<Vec<_>>>()?;
                        let proxy_jpeg_decode_ms = proxy_decode_started.elapsed().as_millis() as u64;
                        let pool = rayon::ThreadPoolBuilder::new().num_threads(analysis_workers).build()
                            .map_err(|error| SplatError::Process(format!("无法创建 CPU 代理分析线程池：{error}")))?;
                        analysis_events.send(PipelineStage::SelectingFrames, Some(PipelineEngine::System), EventKind::Progress, EventLevel::Info, Some(0.0), false,
                            format!("正在并行准备 {} 张代理灰度图与跟踪金字塔", images.len()), Some(0), Some(images.len() as u64), Some("帧"));
                        let tracking_prepare_started = Instant::now();
                        let tracking_pyramids = pool.install(|| prepare_proxy_tracking_pyramids_with_progress(&images, |current, total| {
                            if current == total || current % 5 == 0 {
                                analysis_events.send(PipelineStage::SelectingFrames, Some(PipelineEngine::System), EventKind::Progress, EventLevel::Info, Some(0.25 * current as f32 / total.max(1) as f32), false,
                                    format!("正在并行准备代理灰度图与跟踪金字塔 {current} / {total}"), Some(current), Some(total), Some("帧"));
                            }
                        }));
                        let proxy_tracking_prepare_ms = tracking_prepare_started.elapsed().as_millis() as u64;
                        analysis_events.send(PipelineStage::SelectingFrames, Some(PipelineEngine::System), EventKind::Progress, EventLevel::Info, Some(0.25), false,
                            format!("代理灰度图与跟踪金字塔准备完成：{} 张", images.len()), Some(images.len() as u64), Some(images.len() as u64), Some("帧"));
                        let grid_analysis_started = Instant::now();
                        let proxy_frames = pool.install(|| analyze_prepared_proxy_images_with_progress(&proxy_source_frames, &images, &tracking_pyramids, |current, total| {
                            if current == total || current % 5 == 0 {
                                analysis_events.send(PipelineStage::SelectingFrames, Some(PipelineEngine::System), EventKind::Progress, EventLevel::Info, Some(0.25 + 0.75 * current as f32 / total.max(1) as f32), false,
                                    format!("正在分析代理画面 {current} / {total}"), Some(current), Some(total), Some("帧"));
                            }
                        }))?;
                        Ok::<_, SplatError>((proxy_frames, proxy_jpeg_decode_ms, proxy_tracking_prepare_ms, grid_analysis_started.elapsed().as_millis() as u64))
                    }).await.map_err(|error| SplatError::Process(format!("代理分析任务异常结束：{error}")))?;
                    frame_analysis_ms = analysis_started.elapsed().as_millis() as u64;
                    match analysis {
                        Ok((
                            proxy_frames,
                            jpeg_decode_ms,
                            tracking_prepare_ms,
                            grid_analysis_ms,
                        )) => {
                            proxy_jpeg_decode_ms = jpeg_decode_ms;
                            proxy_tracking_prepare_ms = tracking_prepare_ms;
                            proxy_grid_analysis_ms = grid_analysis_ms;
                            let proxy_count = proxy_frames.len();
                            let analysis_seconds = proxy_grid_analysis_ms as f64 / 1_000.0;
                            let pair_count = proxy_count.saturating_sub(1) as u64;
                            let pairs_per_second = if analysis_seconds > 0.0 {
                                pair_count as f64 / analysis_seconds
                            } else {
                                0.0
                            };
                            self.events.send(
                                PipelineStage::SelectingFrames,
                                Some(PipelineEngine::System),
                                EventKind::Log,
                                EventLevel::Info,
                                Some(0.0),
                                false,
                                format!(
                                    "CPU 网格匹配基准：{proxy_count} 帧、{pair_count} 对，{analysis_seconds:.2} 秒，{pairs_per_second:.2} 对/秒（{analysis_workers} 线程；JPEG 读取 {proxy_jpeg_decode_ms} ms；金字塔准备 {proxy_tracking_prepare_ms} ms；总阶段 {frame_analysis_ms} ms）"
                                ),
                                Some(pair_count),
                                Some(pair_count),
                                Some("帧对"),
                            );
                            // 精细档以覆盖预算而不是“能否凑够一个初始化对”作为
                            // 自适应成功条件。20% 的锚点预算保留了明显的压缩空间，
                            // 同时避免 20 秒素材只有 9 张图就进入训练。
                            let minimum_selected = match quality {
                                Quality::High => {
                                    ((video.duration * profile.anchor_fps * 0.20).ceil() as usize)
                                        .clamp(12, 32)
                                }
                                _ => 3,
                            };
                            let mut selection_profile = profile;
                            let mut selection_tier = "strict";
                            let mut selected =
                                select_adaptive_frames(&proxy_frames, selection_profile);
                            adaptive_attempts.push(AdaptiveAttemptRecord {
                                name: "strict".into(),
                                input_frames: selected.len() as u64,
                                registered_images: None,
                                registered_ratio: None,
                                accepted: selected.len() >= minimum_selected,
                                detail: format!(
                                    "代理选帧 {} / {}",
                                    selected.len(),
                                    minimum_selected
                                ),
                            });
                            // 精细档的采样密度（8 FPS、80 ms、较小目标位移）不应
                            // 与更严的代理观测门槛绑定。代理只负责筛候选；当 strict
                            // 无法形成可初始化序列时，逐级放宽底线，再由真实 COLMAP
                            // 质量验收裁决，避免把可重建视频直接误退回固定 FPS。
                            if quality == Quality::High && selected.len() < minimum_selected {
                                selection_profile.min_textured_cells = 14;
                                selection_profile.min_matched_cells = 9;
                                selection_profile.min_inliers_floor = 7;
                                selection_profile.min_inlier_ratio = 0.45;
                                selection_profile.min_three_view_floor = 3;
                                selection_profile.min_three_view_ratio = 0.35;
                                selected = select_adaptive_frames(&proxy_frames, selection_profile);
                                selection_tier = "relaxed";
                                adaptive_attempts.push(AdaptiveAttemptRecord {
                                    name: "relaxed".into(),
                                    input_frames: selected.len() as u64,
                                    registered_images: None,
                                    registered_ratio: None,
                                    accepted: selected.len() >= minimum_selected,
                                    detail: format!(
                                        "代理选帧 {} / {}",
                                        selected.len(),
                                        minimum_selected
                                    ),
                                });
                                self.events.send(PipelineStage::SelectingFrames, Some(PipelineEngine::System), EventKind::Log, EventLevel::Warning, Some(0.9), false,
                                    format!("精细代理 strict 未达到覆盖预算，切换 relaxed 最低可观测门：{} / {} 个关键帧", selected.len(), minimum_selected),
                                    Some(selected.len() as u64), Some(minimum_selected as u64), Some("帧"));
                            }
                            if quality == Quality::High && selected.len() < minimum_selected {
                                // 保持精细时间密度，只采用经均衡档验证的可观测底线。
                                selection_profile.min_textured_cells = 12;
                                selection_profile.min_matched_cells = 8;
                                selection_profile.min_inliers_floor = 6;
                                selection_profile.min_inlier_ratio = 0.45;
                                selection_profile.min_three_view_floor = 3;
                                selection_profile.min_three_view_ratio = 0.35;
                                selected = select_adaptive_frames(&proxy_frames, selection_profile);
                                selection_tier = "minimumObservable";
                                adaptive_attempts.push(AdaptiveAttemptRecord {
                                    name: "minimumObservable".into(),
                                    input_frames: selected.len() as u64,
                                    registered_images: None,
                                    registered_ratio: None,
                                    accepted: selected.len() >= minimum_selected,
                                    detail: format!(
                                        "代理选帧 {} / {}",
                                        selected.len(),
                                        minimum_selected
                                    ),
                                });
                                self.events.send(PipelineStage::SelectingFrames, Some(PipelineEngine::System), EventKind::Log, EventLevel::Warning, Some(0.95), false,
                                    format!("精细代理 relaxed 未达到覆盖预算，切换 minimum-observable 门：{} / {} 个关键帧", selected.len(), minimum_selected),
                                    Some(selected.len() as u64), Some(minimum_selected as u64), Some("帧"));
                            }
                            adaptive_profile = Some(selection_profile);
                            adaptive_selection_tier = Some(selection_tier);
                            adaptive_selection_target = Some(minimum_selected);
                            adaptive_proxy_frames = Some(proxy_frames.clone());
                            adaptive_proxy_samples = Some(report.frames.clone());
                            if selected.len() >= minimum_selected {
                                let mut adaptive = adaptive_plan(&video, quality)
                                    .expect("profile exists for adaptive quality");
                                adaptive.proxy_candidates = Some(proxy_count as u64);
                                adaptive.effective_fps =
                                    Some(selected.len() as f64 / video.duration.max(0.001));
                                adaptive.estimated_frames = selected.len() as u64;
                                plan = adaptive;
                                adaptive_selected = Some(selected);
                                self.events.send(PipelineStage::SelectingFrames, Some(PipelineEngine::System), EventKind::Progress, EventLevel::Info, Some(1.0), false,
                                    format!("自适应规划完成（{selection_tier}）：{} 个代理候选 → {} 个关键帧", proxy_count, plan.estimated_frames), Some(proxy_count as u64), Some(proxy_count as u64), Some("帧"));
                            } else if quality == Quality::High
                                && selected.len() >= (minimum_selected * 9 + 9) / 10
                            {
                                // 接近预算时不凭代理分数猜测：在独立目录做一次廉价
                                // Ceres 验证。它既不使用正式 frames，也不会覆盖随后
                                // 的固定回退；只有真实注册质量通过才允许采用这些帧。
                                let validation_root =
                                    project.join("work").join("adaptive-near-budget-validation");
                                let validation_frames = validation_root.join("frames");
                                let validation_database = validation_root.join("database.db");
                                let validation_sparse = validation_root.join("sparse");
                                tokio::fs::create_dir_all(&validation_root).await?;
                                self.events.stage(PipelineStage::SelectingFrames, 0.96,
                                    format!("精细自适应接近覆盖预算（{} / {}），正在执行隔离 COLMAP 验证", selected.len(), minimum_selected));
                                let validation = async {
                                    extract_selected_proxy_frames(
                                        &self.engines.ffmpeg,
                                        input,
                                        &validation_frames,
                                        &validation_root.join("ffmpeg"),
                                        &selected,
                                        &proxy_candidate_samples,
                                        profile.analysis_fps,
                                        self.ffmpeg_hw_accel,
                                        logs.map(|path| {
                                            path.join("ffmpeg-adaptive-near-budget-validation.log")
                                        }),
                                        &self.process_manager,
                                        Some(self.process_observer(
                                            PipelineStage::SelectingFrames,
                                            PipelineEngine::Ffmpeg,
                                            Some(selected.len() as u64),
                                            ObserverMode::FfmpegSelected {
                                                source_duration_seconds: video.duration,
                                            },
                                        )),
                                    )
                                    .await?;
                                    let validation_backend = self.colmap_backend;
                                    let validation_compute = match validation_backend {
                                        ColmapBackend::Cpu => ColmapComputeMode::Cpu,
                                        ColmapBackend::Cuda => {
                                            ColmapComputeMode::Cuda { gpu_index: -1 }
                                        }
                                    };
                                    let validation_exe = self.colmap_executable().to_path_buf();
                                    let validation_log = validation_root.join("colmap.log");
                                    colmap::extract_features(
                                        &validation_exe,
                                        &validation_database,
                                        &validation_frames,
                                        ColmapFeatureOptions {
                                            compute: validation_compute,
                                        },
                                        validation_log.clone(),
                                        &self.process_manager,
                                        Some(self.process_observer(
                                            PipelineStage::SelectingFrames,
                                            PipelineEngine::Colmap,
                                            Some(selected.len() as u64),
                                            ObserverMode::BracketProgress,
                                        )),
                                    )
                                    .await?;
                                    colmap::match_sequential(
                                        &validation_exe,
                                        &validation_database,
                                        ColmapMatchingOptions {
                                            compute: validation_compute,
                                            overlap: 10,
                                        },
                                        validation_log.clone(),
                                        &self.process_manager,
                                        Some(self.process_observer(
                                            PipelineStage::SelectingFrames,
                                            PipelineEngine::Colmap,
                                            Some(selected.len() as u64),
                                            ObserverMode::BracketProgress,
                                        )),
                                    )
                                    .await?;
                                    run_ceres_mapper(
                                        &validation_exe,
                                        &validation_database,
                                        &validation_frames,
                                        &validation_sparse,
                                        validation_log,
                                        &self.process_manager,
                                        Some(self.process_observer(
                                            PipelineStage::SelectingFrames,
                                            PipelineEngine::Colmap,
                                            Some(selected.len() as u64),
                                            ObserverMode::Mapper,
                                        )),
                                    )
                                    .await
                                }
                                .await;
                                match validation {
                                    Ok((_, report))
                                        if report.quality != ReconstructionQuality::Failed
                                            && report.registered_ratio >= 0.80
                                            && report.registered_images * 4
                                                >= selected.len() as u64 * 3 =>
                                    {
                                        adaptive_attempts.push(AdaptiveAttemptRecord {
                                            name: "nearBudgetValidation".into(),
                                            input_frames: selected.len() as u64,
                                            registered_images: Some(report.registered_images),
                                            registered_ratio: Some(report.registered_ratio),
                                            accepted: true,
                                            detail: "隔离 Ceres 验证通过".into(),
                                        });
                                        let mut adaptive = adaptive_plan(&video, quality)
                                            .expect("profile exists for adaptive quality");
                                        adaptive.proxy_candidates = Some(proxy_count as u64);
                                        adaptive.effective_fps =
                                            Some(selected.len() as f64 / video.duration.max(0.001));
                                        adaptive.estimated_frames = selected.len() as u64;
                                        plan = adaptive;
                                        adaptive_selected = Some(selected);
                                        adaptive_selection_tier = Some("nearBudgetValidated");
                                        self.events.send(PipelineStage::SelectingFrames, Some(PipelineEngine::System), EventKind::Log, EventLevel::Info, Some(1.0), false,
                                            format!("隔离 COLMAP 验证通过：注册 {}/{}（{:.1}%），采用接近预算的精细自适应帧",
                                                report.registered_images, report.input_images, report.registered_ratio * 100.0),
                                            Some(report.registered_images), Some(report.input_images), Some("帧"));
                                    }
                                    Ok((validation_model, report)) => {
                                        adaptive_attempts.push(AdaptiveAttemptRecord {
                                            name: "nearBudgetValidation".into(),
                                            input_frames: selected.len() as u64,
                                            registered_images: Some(report.registered_images),
                                            registered_ratio: Some(report.registered_ratio),
                                            accepted: false,
                                            detail: "隔离 Ceres 验证未达到接受门槛".into(),
                                        });
                                        let planned = match logs {
                                            Some(log_directory) => {
                                                match write_near_budget_validation_diagnostics(
                                                    log_directory,
                                                    &validation_model,
                                                    &selected,
                                                    &proxy_frames,
                                                )
                                                .await
                                                {
                                                    Ok(count) => count,
                                                    Err(error) => {
                                                        self.events.send(PipelineStage::SelectingFrames, Some(PipelineEngine::System), EventKind::Log,
                                                        EventLevel::Warning, Some(1.0), false,
                                                        format!("无法写入近预算验证弱区诊断：{error}"), None, None, None);
                                                        0
                                                    }
                                                }
                                            }
                                            None => 0,
                                        };
                                        let repair = if planned > 0 {
                                            let log_directory = logs
                                                .expect("planned bridge diagnostics require logs");
                                            let bridge_plan =
                                                read_near_budget_bridge_plan(log_directory).await?;
                                            let mut repair_selection = selected.clone();
                                            for bridge in bridge_plan.planned_frames {
                                                if !repair_selection.iter().any(|frame| {
                                                    frame.source_index == bridge.source_index
                                                }) {
                                                    repair_selection.push(SelectedSourceFrame {
                                                        source_index: bridge.source_index,
                                                        pts_seconds: bridge.pts_seconds,
                                                        reason: SelectionReason::Bridge,
                                                        motion: 0.0,
                                                        inliers: bridge.inliers,
                                                        grid_coverage: bridge.grid_coverage,
                                                        sharpness: bridge.sharpness,
                                                    });
                                                }
                                            }
                                            repair_selection
                                                .sort_by_key(|frame| frame.source_index);
                                            let repair_root = project
                                                .join("work")
                                                .join("adaptive-near-budget-bridge-repair");
                                            let repair_frames = repair_root.join("frames");
                                            let repair_database = repair_root.join("database.db");
                                            let repair_sparse = repair_root.join("sparse");
                                            tokio::fs::create_dir_all(&repair_root).await?;
                                            self.events.stage(
                                                PipelineStage::SelectingFrames,
                                                0.97,
                                                format!(
                                                    "近预算桥接 repair：正在验证 {} 张关键帧",
                                                    repair_selection.len()
                                                ),
                                            );
                                            let repair_result = async {
                                                extract_selected_proxy_frames(&self.engines.ffmpeg, input, &repair_frames, &repair_root.join("ffmpeg"),
                                                    &repair_selection, &proxy_candidate_samples, profile.analysis_fps, self.ffmpeg_hw_accel,
                                                    Some(log_directory.join("ffmpeg-adaptive-near-budget-bridge-repair.log")), &self.process_manager,
                                                    Some(self.process_observer(PipelineStage::SelectingFrames, PipelineEngine::Ffmpeg,
                                                        Some(repair_selection.len() as u64), ObserverMode::FfmpegSelected {
                                                            source_duration_seconds: video.duration,
                                                        }))).await?;
                                                let compute = match self.colmap_backend {
                                                    ColmapBackend::Cpu => ColmapComputeMode::Cpu,
                                                    ColmapBackend::Cuda => ColmapComputeMode::Cuda { gpu_index: -1 },
                                                };
                                                let executable = self.colmap_executable().to_path_buf();
                                                let repair_log = repair_root.join("colmap.log");
                                                colmap::extract_features(&executable, &repair_database, &repair_frames,
                                                    ColmapFeatureOptions { compute }, repair_log.clone(), &self.process_manager,
                                                    Some(self.process_observer(PipelineStage::SelectingFrames, PipelineEngine::Colmap,
                                                        Some(repair_selection.len() as u64), ObserverMode::BracketProgress))).await?;
                                                colmap::match_sequential(&executable, &repair_database,
                                                    ColmapMatchingOptions { compute, overlap: 10 }, repair_log.clone(), &self.process_manager,
                                                    Some(self.process_observer(PipelineStage::SelectingFrames, PipelineEngine::Colmap,
                                                        Some(repair_selection.len() as u64), ObserverMode::BracketProgress))).await?;
                                                run_ceres_mapper(&executable, &repair_database, &repair_frames, &repair_sparse, repair_log,
                                                    &self.process_manager, Some(self.process_observer(PipelineStage::SelectingFrames,
                                                        PipelineEngine::Colmap, Some(repair_selection.len() as u64), ObserverMode::Mapper))).await
                                            }.await;
                                            Some((repair_selection, repair_result))
                                        } else {
                                            None
                                        };
                                        match repair {
                                            Some((repair_selection, Ok((_, repair_report)))) if repair_report.quality != ReconstructionQuality::Failed
                                                && repair_report.registered_ratio >= 0.80
                                                && repair_report.registered_images * 4 >= repair_selection.len() as u64 * 3 => {
                                                adaptive_attempts.push(AdaptiveAttemptRecord { name: "bridgeRepair".into(), input_frames: repair_selection.len() as u64,
                                                    registered_images: Some(repair_report.registered_images), registered_ratio: Some(repair_report.registered_ratio),
                                                    accepted: true, detail: "桥接 repair 通过".into() });
                                                let mut adaptive = adaptive_plan(&video, quality).expect("profile exists for adaptive quality");
                                                adaptive.proxy_candidates = Some(proxy_count as u64);
                                                adaptive.effective_fps = Some(repair_selection.len() as f64 / video.duration.max(0.001));
                                                adaptive.estimated_frames = repair_selection.len() as u64;
                                                plan = adaptive;
                                                adaptive_selected = Some(repair_selection);
                                                adaptive_selection_tier = Some("nearBudgetBridgeRepaired");
                                                self.events.send(PipelineStage::SelectingFrames, Some(PipelineEngine::System), EventKind::Log, EventLevel::Info, Some(1.0), false,
                                                    format!("近预算桥接 repair 通过：注册 {}/{}（{:.1}%）", repair_report.registered_images,
                                                        repair_report.input_images, repair_report.registered_ratio * 100.0),
                                                    Some(repair_report.registered_images), Some(repair_report.input_images), Some("帧"));
                                            }
                                            Some((repair_selection, Ok((_, repair_report)))) => {
                                                adaptive_attempts.push(AdaptiveAttemptRecord { name: "bridgeRepair".into(), input_frames: repair_selection.len() as u64,
                                                    registered_images: Some(repair_report.registered_images), registered_ratio: Some(repair_report.registered_ratio),
                                                    accepted: false, detail: "桥接 repair 未达到接受门槛".into() });
                                                let dense_selection = densify_with_proxy_anchors(&repair_selection, &proxy_frames, 0.5);
                                                let dense_root = project.join("work").join("adaptive-density-validation");
                                                let dense_frames = dense_root.join("frames");
                                                let dense_database = dense_root.join("database.db");
                                                let dense_sparse = dense_root.join("sparse");
                                                tokio::fs::create_dir_all(&dense_root).await?;
                                                self.events.stage(PipelineStage::SelectingFrames, 0.98,
                                                    format!("桥接 repair 未通过，正在验证 {:.1} FPS 自适应密度升级（{} 张）", 2.0, dense_selection.len()));
                                                let dense_result = async {
                                                    extract_selected_proxy_frames(&self.engines.ffmpeg, input, &dense_frames, &dense_root.join("ffmpeg"),
                                                        &dense_selection, &proxy_candidate_samples, profile.analysis_fps, self.ffmpeg_hw_accel,
                                                        logs.map(|path| path.join("ffmpeg-adaptive-density-validation.log")), &self.process_manager,
                                                        Some(self.process_observer(PipelineStage::SelectingFrames, PipelineEngine::Ffmpeg,
                                                            Some(dense_selection.len() as u64), ObserverMode::FfmpegSelected {
                                                                source_duration_seconds: video.duration,
                                                            }))).await?;
                                                    let compute = match self.colmap_backend {
                                                        ColmapBackend::Cpu => ColmapComputeMode::Cpu,
                                                        ColmapBackend::Cuda => ColmapComputeMode::Cuda { gpu_index: -1 },
                                                    };
                                                    let executable = self.colmap_executable().to_path_buf();
                                                    let dense_log = dense_root.join("colmap.log");
                                                    colmap::extract_features(&executable, &dense_database, &dense_frames,
                                                        ColmapFeatureOptions { compute }, dense_log.clone(), &self.process_manager,
                                                        Some(self.process_observer(PipelineStage::SelectingFrames, PipelineEngine::Colmap,
                                                            Some(dense_selection.len() as u64), ObserverMode::BracketProgress))).await?;
                                                    colmap::match_sequential(&executable, &dense_database,
                                                        ColmapMatchingOptions { compute, overlap: 10 }, dense_log.clone(), &self.process_manager,
                                                        Some(self.process_observer(PipelineStage::SelectingFrames, PipelineEngine::Colmap,
                                                            Some(dense_selection.len() as u64), ObserverMode::BracketProgress))).await?;
                                                    run_ceres_mapper(&executable, &dense_database, &dense_frames, &dense_sparse, dense_log,
                                                        &self.process_manager, Some(self.process_observer(PipelineStage::SelectingFrames,
                                                            PipelineEngine::Colmap, Some(dense_selection.len() as u64), ObserverMode::Mapper))).await
                                                }.await;
                                                match dense_result {
                                                    Ok((_, dense_report)) if dense_report.quality != ReconstructionQuality::Failed
                                                        && dense_report.registered_ratio >= 0.80
                                                        && dense_report.registered_images * 4 >= dense_selection.len() as u64 * 3 => {
                                                        adaptive_attempts.push(AdaptiveAttemptRecord { name: "densityValidation".into(), input_frames: dense_selection.len() as u64,
                                                            registered_images: Some(dense_report.registered_images), registered_ratio: Some(dense_report.registered_ratio),
                                                            accepted: true, detail: "中等密度验证通过".into() });
                                                        let mut adaptive = adaptive_plan(&video, quality).expect("profile exists for adaptive quality");
                                                        adaptive.proxy_candidates = Some(proxy_count as u64);
                                                        adaptive.effective_fps = Some(dense_selection.len() as f64 / video.duration.max(0.001));
                                                        adaptive.estimated_frames = dense_selection.len() as u64;
                                                        plan = adaptive;
                                                        adaptive_selected = Some(dense_selection);
                                                        adaptive_selection_tier = Some("densityValidated");
                                                        self.events.send(PipelineStage::SelectingFrames, Some(PipelineEngine::System), EventKind::Log, EventLevel::Info, Some(1.0), false,
                                                            format!("自适应密度验证通过：注册 {}/{}（{:.1}%）", dense_report.registered_images,
                                                                dense_report.input_images, dense_report.registered_ratio * 100.0),
                                                            Some(dense_report.registered_images), Some(dense_report.input_images), Some("帧"));
                                                    }
                                                    Ok((_, dense_report)) => adaptive_reason = Some(format!(
                                                        "精细自适应密度验证未通过：注册 {}/{}（{:.1}%）；桥接 repair 为 {}/{}（{:.1}%）",
                                                        dense_report.registered_images, dense_report.input_images, dense_report.registered_ratio * 100.0,
                                                        repair_report.registered_images, repair_report.input_images, repair_report.registered_ratio * 100.0
                                                    )),
                                                    Err(SplatError::Cancelled) => return Err(SplatError::Cancelled),
                                                    Err(error) => adaptive_reason = Some(format!("精细自适应密度验证失败：{error}")),
                                                }
                                            }
                                            Some((_, Err(SplatError::Cancelled))) => return Err(SplatError::Cancelled),
                                            Some((_, Err(error))) => adaptive_reason = Some(format!("精细近预算桥接 repair 失败：{error}")),
                                            None => adaptive_reason = Some(format!(
                                                "精细近预算 COLMAP 验证未通过：注册 {}/{}（{:.1}%）；没有可用桥接帧",
                                                report.registered_images, report.input_images, report.registered_ratio * 100.0
                                            )),
                                        }
                                    }
                                    Err(SplatError::Cancelled) => {
                                        return Err(SplatError::Cancelled)
                                    }
                                    Err(error) => {
                                        adaptive_reason =
                                            Some(format!("精细近预算 COLMAP 验证失败：{error}"))
                                    }
                                }
                            } else {
                                adaptive_reason = Some(format!(
                                    "{} 自适应关键帧覆盖不足（{} / {} 张）",
                                    if quality == Quality::High {
                                        "精细"
                                    } else {
                                        "可靠几何"
                                    },
                                    selected.len(),
                                    minimum_selected
                                ));
                            }
                        }
                        Err(error) => adaptive_reason = Some(format!("代理分析失败：{error}")),
                    }
                }
                Err(error) if matches!(error, SplatError::Cancelled) => return Err(error),
                Err(error) => adaptive_reason = Some(format!("代理抽帧或 PTS 映射失败：{error}")),
            }
            adaptive_planning_ms = planning_started.elapsed().as_millis() as u64;
        }
        if let Some(reason) = &adaptive_reason {
            let diagnostics_hint = adaptive_proxy_frames
                .is_some()
                .then_some("；代理门禁诊断将写入 logs/adaptive-proxy-diagnostics.json")
                .unwrap_or_default();
            self.events.send(
                PipelineStage::PlanningFrames,
                Some(PipelineEngine::System),
                EventKind::Log,
                EventLevel::Warning,
                Some(1.0),
                false,
                format!("自适应抽帧回退到固定策略：{reason}{diagnostics_hint}"),
                None,
                None,
                None,
            );
        }
        self.events.stage(
            PipelineStage::PlanningFrames,
            1.0,
            match &adaptive_selected {
                Some(_) => format!("自适应 SfM 计划：{} 个关键帧", plan.estimated_frames),
                None => format!("固定抽帧计划：预计 {} 帧", plan.estimated_frames),
            },
        );
        let extract_started = Instant::now();
        let extracted_frames = if let Some(selected) = adaptive_selected.as_deref() {
            let candidate_total = plan.proxy_candidates.unwrap_or(selected.len() as u64);
            let proxy_samples = adaptive_proxy_samples.as_deref().ok_or_else(|| {
                SplatError::Process("自适应关键帧缺少代理候选映射，无法安全定向抽取原图".into())
            })?;
            self.events.stage(
                PipelineStage::ExtractingFrames,
                0.0,
                format!(
                    "正在以 {:.1} FPS 重跑 {candidate_total} 个原图采样点，并定向编码 {} 张关键帧",
                    plan.analysis_fps.unwrap_or(6.0),
                    selected.len()
                ),
            );
            self.events.send(
                PipelineStage::ExtractingFrames,
                Some(PipelineEngine::System),
                EventKind::Log,
                EventLevel::Info,
                Some(0.0),
                false,
                format!(
                    "原图定向抽帧：跳过 {} 张未入选候选 JPEG 的编码与磁盘写入",
                    candidate_total.saturating_sub(selected.len() as u64)
                ),
                Some(selected.len() as u64),
                Some(candidate_total),
                Some("帧"),
            );
            let count = extract_selected_proxy_frames(
                &self.engines.ffmpeg,
                input,
                output,
                &output
                    .parent()
                    .expect("project frames directory has parent")
                    .join("work")
                    .join("adaptive-selected"),
                selected,
                proxy_samples,
                plan.analysis_fps.unwrap_or(6.0),
                self.ffmpeg_hw_accel,
                logs.map(|path| path.join("ffmpeg-adaptive-selected.log")),
                &self.process_manager,
                Some(self.process_observer(
                    PipelineStage::ExtractingFrames,
                    PipelineEngine::Ffmpeg,
                    Some(selected.len() as u64),
                    ObserverMode::FfmpegSelected {
                        source_duration_seconds: video.duration,
                    },
                )),
            )
            .await?;
            selected_extraction_ms = extract_started.elapsed().as_millis() as u64;
            count
        } else {
            self.events.stage(
                PipelineStage::ExtractingFrames,
                0.0,
                "FFmpeg 开始固定策略抽帧",
            );
            extract_uniform_frames(
                &self.engines.ffmpeg,
                input,
                output,
                &plan,
                self.ffmpeg_hw_accel,
                logs.map(|path| path.join("ffmpeg.log")),
                &self.process_manager,
                Some(self.process_observer(
                    PipelineStage::ExtractingFrames,
                    PipelineEngine::Ffmpeg,
                    Some(plan.estimated_frames),
                    ObserverMode::Ffmpeg,
                )),
            )
            .await?
        };
        let extract_ms = extract_started.elapsed().as_millis() as u64;
        self.events.stage(
            PipelineStage::ExtractingFrames,
            1.0,
            format!("原图抽帧完成：{extracted_frames} 帧"),
        );
        if adaptive_selected.is_some() {
            let candidate_total = plan.proxy_candidates.unwrap_or(extracted_frames);
            let skipped_jpegs = candidate_total.saturating_sub(extracted_frames);
            self.events.send(PipelineStage::ExtractingFrames, Some(PipelineEngine::System), EventKind::Log, EventLevel::Info,
                Some(1.0), false,
                format!("原图定向抽帧基准：扫描 {candidate_total} 个采样点，仅编码 {extracted_frames} 张 JPEG，跳过 {skipped_jpegs} 张；耗时 {selected_extraction_ms} ms"),
                Some(extracted_frames), Some(candidate_total), Some("帧"));
        }
        let select_started = Instant::now();
        let selection = if adaptive_selected.is_some() {
            self.events.stage(
                PipelineStage::SelectingFrames,
                0.0,
                "自适应关键帧已通过几何门禁；正在保护桥接帧",
            );
            let report = FrameSelectionReport {
                candidates: extracted_frames,
                retained: extracted_frames,
                removed_near_duplicates: 0,
            };
            self.events.send(PipelineStage::SelectingFrames, Some(PipelineEngine::System), EventKind::Progress, EventLevel::Info, Some(1.0), false,
                format!("自适应路径保留 {extracted_frames} 张关键帧；未进行可能删除桥接帧的二次 pHash 去重"), Some(extracted_frames), Some(extracted_frames), Some("张"));
            report
        } else {
            self.events.stage(
                PipelineStage::SelectingFrames,
                0.0,
                "正在用 pHash 合并近重复画面，并保留清晰帧",
            );
            let events = self.events.clone();
            let selection_directory = output.to_path_buf();
            tokio::task::spawn_blocking(move || {
                select_useful_frames_parallel_with_progress(
                    &selection_directory,
                    move |current, total| {
                        let stage_progress = current as f32 / total.max(1) as f32;
                        events.send(
                            PipelineStage::SelectingFrames,
                            Some(PipelineEngine::System),
                            EventKind::Progress,
                            EventLevel::Info,
                            Some(stage_progress),
                            false,
                            format!("正在并行筛选画面 {current} / {total}"),
                            Some(current),
                            Some(total),
                            Some("张"),
                        );
                    },
                )
            })
            .await
            .map_err(|error| SplatError::Process(format!("帧筛选任务异常结束：{error}")))??
        };
        let select_ms = select_started.elapsed().as_millis() as u64;
        self.events.stage(
            PipelineStage::SelectingFrames,
            1.0,
            format!(
                "保留 {} / {} 帧（移除 {} 张近重复）",
                selection.retained, selection.candidates, selection.removed_near_duplicates
            ),
        );
        if let Some(log_directory) = logs {
            let strategy = if adaptive_selected.is_some() {
                "adaptiveSfm"
            } else {
                "uniformRatio"
            };
            let fallback = adaptive_reason.as_deref().unwrap_or("none");
            let adaptive_tier = adaptive_selection_tier.unwrap_or("notApplicable");
            let adaptive_target = adaptive_selection_target.unwrap_or(0);
            let analysis_workers = proxy_analysis_workers.unwrap_or(0);
            let proxy_candidates = plan.proxy_candidates.unwrap_or(0);
            let pair_count = proxy_candidates.saturating_sub(1);
            let pairs_per_second = if proxy_grid_analysis_ms > 0 {
                pair_count as f64 / (proxy_grid_analysis_ms as f64 / 1_000.0)
            } else {
                0.0
            };
            tokio::fs::write(log_directory.join("adaptive-frame-selection.log"), format!(
                "strategy={strategy}\nadaptive_selection_tier={adaptive_tier}\nadaptive_selection_target={adaptive_target}\nsource_fps={:.6}\nproxy_candidates={proxy_candidates}\nselected_frames={}\nextracted_frames={extracted_frames}\nfallback_reason={fallback}\nproxy_analysis_workers={analysis_workers}\nproxy_analysis_pairs={pair_count}\nproxy_jpeg_decode_ms={proxy_jpeg_decode_ms}\nproxy_tracking_prepare_ms={proxy_tracking_prepare_ms}\nproxy_grid_analysis_ms={proxy_grid_analysis_ms}\nproxy_analysis_pairs_per_second={pairs_per_second:.3}\nframe_analysis_ms={frame_analysis_ms}\nadaptive_planning_ms={adaptive_planning_ms}\nselected_extraction_ms={selected_extraction_ms}\n",
                video.fps, selection.retained
            )).await?;
            if adaptive_selected.is_some() {
                let encoded_frames = extracted_frames;
                let skipped_candidate_jpegs = proxy_candidates.saturating_sub(encoded_frames);
                let benchmark = serde_json::json!({
                    "strategy": "proxyCandidateIndexBalancedSelect",
                    "proxyCandidateSamplePoints": proxy_candidates,
                    "encodedOriginalJpegs": encoded_frames,
                    "skippedCandidateJpegs": skipped_candidate_jpegs,
                    "selectedExtractionMs": selected_extraction_ms,
                    "framesPerSecond": if selected_extraction_ms > 0 {
                        encoded_frames as f64 / (selected_extraction_ms as f64 / 1_000.0)
                    } else { 0.0 },
                });
                tokio::fs::write(
                    log_directory.join("adaptive-original-extraction-benchmark.json"),
                    serde_json::to_vec_pretty(&benchmark)?,
                )
                .await?;
            }
            let attempt_report = AdaptiveAttemptReport {
                selection_target: adaptive_selection_target.map(|value| value as u64),
                final_tier: adaptive_selection_tier.map(str::to_owned),
                fallback_reason: adaptive_reason.clone(),
                attempts: adaptive_attempts,
            };
            tokio::fs::write(
                log_directory.join("adaptive-attempts.json"),
                serde_json::to_vec_pretty(&attempt_report)?,
            )
            .await?;
            if let Some(selected) = adaptive_selected.as_deref() {
                let entries = selected
                    .iter()
                    .enumerate()
                    .map(|(index, frame)| {
                        serde_json::json!({
                            "outputFile": format!("frame_{:06}.jpg", index + 1),
                            "sourceIndex": frame.source_index,
                            "ptsSeconds": frame.pts_seconds,
                            "reason": frame.reason,
                            "motion": frame.motion,
                            "inliers": frame.inliers,
                            "gridCoverage": frame.grid_coverage,
                            "sharpness": frame.sharpness,
                        })
                    })
                    .collect::<Vec<_>>();
                let manifest = serde_json::json!({
                    "strategy": "adaptiveSfm",
                    "sourceVideo": video.path,
                    "frames": entries,
                });
                tokio::fs::write(
                    log_directory.join("adaptive-selected-frames.json"),
                    serde_json::to_vec_pretty(&manifest).map_err(|error| {
                        SplatError::Process(format!("无法写入自适应帧清单：{error}"))
                    })?,
                )
                .await?;
            }
            if let Some(proxy_frames) = adaptive_proxy_frames.as_deref() {
                let proxy_pairs = proxy_frames.len().saturating_sub(1) as u64;
                let proxy_pairs_per_second = if proxy_grid_analysis_ms > 0 {
                    proxy_pairs as f64 / (proxy_grid_analysis_ms as f64 / 1_000.0)
                } else {
                    0.0
                };
                let benchmark = serde_json::json!({
                    "proxyCandidates": proxy_frames.len(),
                    "matchedFramePairs": proxy_pairs,
                    "gridCellsPerPair": 160,
                    "workers": proxy_analysis_workers,
                    "proxyJpegDecodeMs": proxy_jpeg_decode_ms,
                    "trackingPyramidPrepareMs": proxy_tracking_prepare_ms,
                    "gridAnalysisMs": proxy_grid_analysis_ms,
                    "totalAnalysisStageMs": frame_analysis_ms,
                    "matchedPairsPerSecond": proxy_pairs_per_second,
                });
                tokio::fs::write(
                    log_directory.join("adaptive-analysis-benchmark.json"),
                    serde_json::to_vec_pretty(&benchmark)?,
                )
                .await?;
                let proxy_manifest = serde_json::json!({
                    "strategy": "adaptiveSfm",
                    "frames": proxy_frames,
                });
                tokio::fs::write(
                    log_directory.join("adaptive-proxy-analysis.json"),
                    serde_json::to_vec_pretty(&proxy_manifest).map_err(|error| {
                        SplatError::Process(format!("无法写入自适应代理分析：{error}"))
                    })?,
                )
                .await?;
                if let Some(profile) = adaptive_profile {
                    let diagnostics = summarize_proxy_diagnostics(
                        proxy_frames,
                        profile,
                        adaptive_selected.as_ref().map_or(0, Vec::len),
                    );
                    tokio::fs::write(
                        log_directory.join("adaptive-proxy-diagnostics.json"),
                        serde_json::to_vec_pretty(&diagnostics)?,
                    )
                    .await?;
                }
            }
        }
        Ok(PreparedFrames {
            video,
            plan,
            extracted_frames,
            selection,
            probe_ms,
            extract_ms,
            select_ms,
            frame_analysis_ms,
            adaptive_planning_ms,
            selected_extraction_ms,
            adaptive_fallback_reason: adaptive_reason,
        })
    }
    pub async fn generate(
        &self,
        input: &Path,
        quality: Quality,
        projects_root: &Path,
    ) -> Result<PipelineResult> {
        self.generate_with_manager(
            input,
            quality,
            ProjectManager::with_root(projects_root.to_path_buf()),
        )
        .await
    }
    pub async fn generate_for_diagnostics(
        &self,
        input: &Path,
        quality: Quality,
        projects_root: &Path,
    ) -> Result<PipelineResult> {
        self.generate_with_manager(
            input,
            quality,
            ProjectManager::for_diagnostics(projects_root.to_path_buf()),
        )
        .await
    }
    /// Runs an already reconstructed Splatcam export. This intentionally bypasses all video
    /// probing/extraction, feature extraction, matching and mapper/CASPAR calls.
    pub async fn generate_splatcam(
        &self,
        source: &Path,
        quality: Quality,
        projects_root: &Path,
    ) -> Result<PipelineResult> {
        engines::require_cpu_colmap(&self.engines).await?;
        colmap::require_verified_cli(&self.engines.colmap)?;
        if self.training_backend == TrainingBackend::Brush {
            brush::require_verified_cli(&self.engines.brush)?;
        } else if !engines::training::gsplat_runtime_healthy(&self.engines.root).await {
            return Err(SplatError::UnsupportedEngine(
                "gsplat CUDA 实验运行时尚未安装或未通过健康检查；请改用 Brush。".into(),
            ));
        }
        let project_manager = ProjectManager::with_root(projects_root.to_path_buf());
        let (paths, mut metadata) = project_manager.create(source, quality).await?;
        self.events.set_task_log(paths.logs.join("task.log"));
        metadata.input_source = InputSource::Splatcam;
        let started = Instant::now();
        let result = self
            .run_splatcam_project(&project_manager, &paths, &mut metadata, quality)
            .await;
        if let Err(error) = &result {
            metadata.status = if matches!(error, SplatError::Cancelled) {
                ProjectStatus::Cancelled
            } else {
                ProjectStatus::Failed
            };
            metadata.completed_at = Some(Utc::now());
            metadata.duration_ms = Some(started.elapsed().as_millis() as u64);
            metadata.failure_message = Some(error.to_string());
            let _ = project_manager
                .write_metadata(&paths.metadata, &metadata)
                .await;
        }
        result
    }
    async fn generate_with_manager(
        &self,
        input: &Path,
        quality: Quality,
        project_manager: ProjectManager,
    ) -> Result<PipelineResult> {
        self.verify_pipeline_engines().await?;
        let (paths, mut metadata) = project_manager.create(input, quality).await?;
        self.events.set_task_log(paths.logs.join("task.log"));
        let started = Instant::now();
        let result = self
            .run_project(&project_manager, &paths, &mut metadata, quality)
            .await;
        if let Err(error) = &result {
            let cancelled = matches!(error, SplatError::Cancelled);
            let needs_supplement = matches!(error, SplatError::NeedsSupplement(_));
            metadata.status = if cancelled {
                ProjectStatus::Cancelled
            } else if needs_supplement {
                ProjectStatus::NeedsSupplement
            } else {
                ProjectStatus::Failed
            };
            metadata.completed_at = Some(Utc::now());
            metadata.duration_ms = Some(started.elapsed().as_millis() as u64);
            metadata.failure_message = Some(error.to_string());
            let _ = project_manager
                .write_metadata(&paths.metadata, &metadata)
                .await;
            if !needs_supplement {
                let mut state = PipelineStateFile::created(quality);
                state.stage = if cancelled {
                    PipelineStage::Cancelled
                } else {
                    PipelineStage::Failed
                };
                let _ = project_manager.write_state(&paths.state, &state).await;
            }
        }
        result
    }
    async fn run_project(
        &self,
        project_manager: &ProjectManager,
        paths: &ProjectPaths,
        metadata: &mut ProjectMetadata,
        quality: Quality,
    ) -> Result<PipelineResult> {
        let mut state = PipelineStateFile::created(quality);
        let initial_colmap_execution = ColmapExecution {
            requested_backend: Some(self.colmap_backend),
            requested_ba_mode: Some(self.mapper_ba_mode),
            ..Default::default()
        };
        state.colmap_execution = initial_colmap_execution.clone();
        metadata.colmap_execution = initial_colmap_execution;
        state.training_backend = self.training_backend;
        state.auto_bridge_frames = self.auto_bridge_frames;
        metadata.training_backend = self.training_backend;
        metadata.brush_training_preset = self.brush_training_preset;
        metadata.gsplat_splat_cap = self.gsplat_splat_cap;
        metadata.gsplat_densification_strategy = self.gsplat_densification_strategy;
        metadata.photometric_mode = self.photometric_mode;
        let total_started = Instant::now();
        let prepared = self
            .prepare_frames(
                &metadata.source_path,
                quality,
                &paths.frames,
                Some(&paths.logs),
            )
            .await?;
        metadata.timings.probe_ms = prepared.probe_ms;
        metadata.timings.extract_ms = prepared.extract_ms;
        metadata.timings.select_ms = prepared.select_ms;
        metadata.timings.frame_analysis_ms = prepared.frame_analysis_ms;
        metadata.timings.adaptive_planning_ms = prepared.adaptive_planning_ms;
        metadata.timings.selected_extraction_ms = prepared.selected_extraction_ms;
        state.video = Some(prepared.video);
        let mut frames = FrameState::from(&prepared.plan);
        frames.extracted_frames = Some(prepared.extracted_frames);
        frames.selected_frames = Some(prepared.selection.retained);
        frames.removed_near_duplicates = Some(prepared.selection.removed_near_duplicates);
        frames.adaptive_fallback_used = prepared.adaptive_fallback_reason.is_some();
        frames.adaptive_fallback_reason = prepared.adaptive_fallback_reason;
        frames.auto_bridge_frames = self.auto_bridge_frames;
        state.frames = Some(frames);
        state.stage = PipelineStage::SelectingFrames;
        project_manager.write_state(&paths.state, &state).await?;
        // Each backend owns its database and sparse output. This prevents a
        // failed CUDA feature/matching run from ever contaminating a later CPU
        // retry or a manually selected CPU run.
        let mut effective_backend = self.colmap_backend;
        let attempt_name = match effective_backend {
            ColmapBackend::Cpu => "cpu",
            ColmapBackend::Cuda => "cuda",
        };
        let mut attempt_root = paths
            .project
            .join("work")
            .join("colmap-attempts")
            .join(attempt_name);
        tokio::fs::create_dir_all(&attempt_root).await?;
        let mut database = attempt_root.join("database.db");
        let mut colmap_log = attempt_root.join("colmap.log");
        // COLMAP 4.1.x 的 OptionManager 无法用相对路径（如 ../frames）解析
        // --image_path，`ExistsDir` 校验会直接失败。这里传入绝对路径；
        // COLMAP 4.1.1 的 bitmap loader 已能正确处理 UTF-8/宽字符绝对路径。
        let colmap_images = paths.frames.as_path();
        let mut colmap_exe = self.colmap_executable().to_path_buf();
        let mut backend_label = match effective_backend {
            ColmapBackend::Cpu => "CPU",
            ColmapBackend::Cuda => "CUDA",
        };
        let mut colmap_compute = match effective_backend {
            ColmapBackend::Cpu => ColmapComputeMode::Cpu,
            ColmapBackend::Cuda => ColmapComputeMode::Cuda { gpu_index: -1 },
        };
        self.events.stage(
            PipelineStage::ExtractingFeatures,
            0.0,
            format!("COLMAP 正在使用 {backend_label} 提取特征"),
        );
        let phase_started = Instant::now();
        let feature_result = colmap::extract_features(
            &colmap_exe,
            &database,
            colmap_images,
            ColmapFeatureOptions {
                compute: colmap_compute,
            },
            colmap_log.clone(),
            &self.process_manager,
            Some(self.process_observer(
                PipelineStage::ExtractingFeatures,
                PipelineEngine::Colmap,
                Some(prepared.selection.retained),
                ObserverMode::BracketProgress,
            )),
        )
        .await;
        if let Err(error) = feature_result {
            if effective_backend != ColmapBackend::Cuda || !colmap::is_cuda_runtime_error(&error) {
                return Err(error);
            }
            self.events.stage(
                PipelineStage::ExtractingFeatures,
                0.0,
                "CUDA SIFT 运行时失败，正在使用独立 CPU 数据库重试",
            );
            effective_backend = ColmapBackend::Cpu;
            attempt_root = paths
                .project
                .join("work")
                .join("colmap-attempts")
                .join("cpu-fallback");
            tokio::fs::create_dir_all(&attempt_root).await?;
            database = attempt_root.join("database.db");
            colmap_log = attempt_root.join("colmap.log");
            colmap_exe = self
                .engines
                .colmap_for(effective_backend, self.cuda_colmap_flavor)
                .to_path_buf();
            colmap_compute = ColmapComputeMode::Cpu;
            backend_label = "CPU（CUDA 回退）";
            colmap::extract_features(
                &colmap_exe,
                &database,
                colmap_images,
                ColmapFeatureOptions {
                    compute: colmap_compute,
                },
                colmap_log.clone(),
                &self.process_manager,
                Some(self.process_observer(
                    PipelineStage::ExtractingFeatures,
                    PipelineEngine::Colmap,
                    Some(prepared.selection.retained),
                    ObserverMode::BracketProgress,
                )),
            )
            .await?;
        }
        metadata.timings.colmap_features_ms = phase_started.elapsed().as_millis() as u64;
        state.stage = PipelineStage::ExtractingFeatures;
        state.features_complete = true;
        project_manager.write_state(&paths.state, &state).await?;
        self.events.stage(
            PipelineStage::ExtractingFeatures,
            1.0,
            format!("{backend_label} 特征提取完成"),
        );
        self.events.stage(
            PipelineStage::Matching,
            0.0,
            format!("COLMAP 正在进行 {backend_label} 顺序匹配"),
        );
        let phase_started = Instant::now();
        let matching_result = colmap::match_sequential(
            &colmap_exe,
            &database,
            ColmapMatchingOptions {
                compute: colmap_compute,
                overlap: 10,
            },
            colmap_log.clone(),
            &self.process_manager,
            Some(self.process_observer(
                PipelineStage::Matching,
                PipelineEngine::Colmap,
                Some(prepared.selection.retained),
                ObserverMode::BracketProgress,
            )),
        )
        .await;
        if let Err(error) = matching_result {
            if effective_backend != ColmapBackend::Cuda || !colmap::is_cuda_runtime_error(&error) {
                return Err(error);
            }
            self.events.stage(
                PipelineStage::Matching,
                0.0,
                "CUDA SIFT 匹配运行时失败，正在使用独立 CPU 数据库重试",
            );
            effective_backend = ColmapBackend::Cpu;
            attempt_root = paths
                .project
                .join("work")
                .join("colmap-attempts")
                .join("cpu-fallback");
            tokio::fs::create_dir_all(&attempt_root).await?;
            database = attempt_root.join("database.db");
            colmap_log = attempt_root.join("colmap.log");
            colmap_exe = self
                .engines
                .colmap_for(effective_backend, self.cuda_colmap_flavor)
                .to_path_buf();
            colmap_compute = ColmapComputeMode::Cpu;
            backend_label = "CPU（CUDA 回退）";
            colmap::extract_features(
                &colmap_exe,
                &database,
                colmap_images,
                ColmapFeatureOptions {
                    compute: colmap_compute,
                },
                colmap_log.clone(),
                &self.process_manager,
                Some(self.process_observer(
                    PipelineStage::ExtractingFeatures,
                    PipelineEngine::Colmap,
                    Some(prepared.selection.retained),
                    ObserverMode::BracketProgress,
                )),
            )
            .await?;
            colmap::match_sequential(
                &colmap_exe,
                &database,
                ColmapMatchingOptions {
                    compute: colmap_compute,
                    overlap: 10,
                },
                colmap_log.clone(),
                &self.process_manager,
                Some(self.process_observer(
                    PipelineStage::Matching,
                    PipelineEngine::Colmap,
                    Some(prepared.selection.retained),
                    ObserverMode::BracketProgress,
                )),
            )
            .await?;
        }
        metadata.timings.colmap_matching_ms = phase_started.elapsed().as_millis() as u64;
        let fallback_used =
            self.colmap_backend == ColmapBackend::Cuda && effective_backend == ColmapBackend::Cpu;
        let execution = ColmapExecution {
            requested_backend: Some(self.colmap_backend),
            effective_backend: Some(effective_backend),
            feature_compute_device: Some(
                if effective_backend == ColmapBackend::Cuda {
                    "cuda"
                } else {
                    "cpu"
                }
                .into(),
            ),
            matching_compute_device: Some(
                if effective_backend == ColmapBackend::Cuda {
                    "cuda"
                } else {
                    "cpu"
                }
                .into(),
            ),
            gpu_index: (effective_backend == ColmapBackend::Cuda).then_some(-1),
            cuda_fallback_used: fallback_used,
            cuda_fallback_reason: fallback_used
                .then(|| "CUDA feature extraction or matching runtime failure".into()),
            ..Default::default()
        };
        state.colmap_execution = execution.clone();
        metadata.colmap_execution = execution;
        state.stage = PipelineStage::Matching;
        state.matching_complete = true;
        project_manager.write_state(&paths.state, &state).await?;
        self.events.stage(
            PipelineStage::Matching,
            1.0,
            format!("{backend_label} 顺序匹配完成"),
        );
        let caspar_available = if effective_backend == ColmapBackend::Cuda {
            engines::cuda_colmap_supports_caspar(&self.engines, self.cuda_colmap_flavor).await?
        } else {
            false
        };
        if self.mapper_ba_mode == MapperBaMode::Caspar && !caspar_available {
            return Err(SplatError::UnsupportedEngine(
                "当前 CUDA COLMAP 未以 CASPAR_ENABLED 编译；请切换 Ceres，或安装已验证的 CASPAR 构建。".into(),
            ));
        }
        let use_caspar = should_use_caspar(
            effective_backend,
            caspar_available,
            self.mapper_ba_mode,
            prepared.selection.retained,
        );
        let ceres_sparse = attempt_root.join("incremental-ceres").join("sparse");
        let caspar_sparse = attempt_root.join("incremental-caspar").join("sparse");
        let mut caspar_fallback_reason = None;
        self.events.stage(
            PipelineStage::Reconstructing,
            0.0,
            if use_caspar {
                "正在使用 CASPAR GPU 增量重建相机轨迹"
            } else {
                "正在使用 Ceres 增量重建相机轨迹"
            },
        );
        let phase_started = Instant::now();
        let mapper_observer = || {
            Some(self.process_observer(
                PipelineStage::Reconstructing,
                PipelineEngine::Colmap,
                Some(prepared.selection.retained),
                ObserverMode::Mapper,
            ))
        };
        let (mut model, mut report, mut ba_backend) = if use_caspar {
            let caspar_options = IncrementalMapperOptions {
                ba_backend: IncrementalBaBackend::Caspar { gpu_index: -1 },
            };
            let caspar_result = colmap::map(
                &colmap_exe,
                &database,
                colmap_images,
                &caspar_sparse,
                caspar_options,
                colmap_log.clone(),
                &self.process_manager,
                mapper_observer(),
            )
            .await
            .and_then(|_| best_sparse_model(&paths.frames, &caspar_sparse));
            match caspar_result {
                Ok((model, report)) if report.quality != ReconstructionQuality::Failed => {
                    (model, report, "caspar")
                }
                Ok((_, report)) => {
                    caspar_fallback_reason = Some(format!(
                        "CASPAR 注册率 {:.1}% 低于 50% 阈值",
                        report.registered_ratio * 100.0
                    ));
                    self.events.stage(
                        PipelineStage::Reconstructing,
                        0.0,
                        "CASPAR 重建质量不足，正在回退 Ceres",
                    );
                    let (model, report) = run_ceres_mapper(
                        &colmap_exe,
                        &database,
                        colmap_images,
                        &ceres_sparse,
                        colmap_log.clone(),
                        &self.process_manager,
                        mapper_observer(),
                    )
                    .await?;
                    (model, report, "ceres")
                }
                Err(SplatError::Cancelled) => return Err(SplatError::Cancelled),
                Err(error) => {
                    caspar_fallback_reason = Some(error.to_string());
                    self.events.stage(
                        PipelineStage::Reconstructing,
                        0.0,
                        "CASPAR 不可用，正在回退 Ceres",
                    );
                    let (model, report) = run_ceres_mapper(
                        &colmap_exe,
                        &database,
                        colmap_images,
                        &ceres_sparse,
                        colmap_log.clone(),
                        &self.process_manager,
                        mapper_observer(),
                    )
                    .await?;
                    (model, report, "ceres")
                }
            }
        } else {
            let (model, report) = run_ceres_mapper(
                &colmap_exe,
                &database,
                colmap_images,
                &ceres_sparse,
                colmap_log.clone(),
                &self.process_manager,
                mapper_observer(),
            )
            .await?;
            (model, report, "ceres")
        };
        metadata.timings.colmap_mapping_ms = phase_started.elapsed().as_millis() as u64;
        state.colmap_execution.effective_ba_backend = Some(ba_backend.into());
        state.colmap_execution.caspar_fallback_used = caspar_fallback_reason.is_some();
        state.colmap_execution.caspar_fallback_reason = caspar_fallback_reason.clone();
        metadata.colmap_execution = state.colmap_execution.clone();
        state.stage = PipelineStage::Reconstructing;
        state.reconstruction_complete = true;
        project_manager.write_state(&paths.state, &state).await?;
        self.events.stage(
            PipelineStage::Reconstructing,
            1.0,
            format!(
                "{} 增量重建完成",
                if ba_backend == "caspar" {
                    "CASPAR"
                } else {
                    "Ceres"
                }
            ),
        );
        self.events.stage(
            PipelineStage::ValidatingReconstruction,
            0.0,
            "正在核验注册率和三维点",
        );
        let mut registration_summary =
            match write_registered_frame_timeline(&paths.logs, &model, self.auto_bridge_frames)
                .await
            {
                Ok(Some(summary)) => {
                    self.events.send(
                        PipelineStage::ValidatingReconstruction,
                        Some(PipelineEngine::System),
                        EventKind::Log,
                        EventLevel::Info,
                        Some(0.5),
                        false,
                        format!(
                            "已将 COLMAP 注册结果关联到 {}/{} 个自适应关键帧",
                            summary.registered_frames, summary.selected_frames
                        ),
                        Some(summary.registered_frames),
                        Some(summary.selected_frames),
                        Some("帧"),
                    );
                    if summary.weak_intervals > 0 {
                        self.events.send(
                            PipelineStage::ValidatingReconstruction,
                            Some(PipelineEngine::System),
                            EventKind::Log,
                            EventLevel::Warning,
                            Some(0.5),
                            false,
                            format!(
                                "检测到 {} 个未注册关键帧弱区；已写入补帧诊断",
                                summary.weak_intervals
                            ),
                            Some(summary.weak_intervals),
                            Some(summary.weak_intervals),
                            Some("区间"),
                        );
                        if summary.planned_bridge_frames > 0 {
                            self.events.send(
                                PipelineStage::ValidatingReconstruction,
                                Some(PipelineEngine::System),
                                EventKind::Log,
                                EventLevel::Info,
                                Some(0.5),
                                false,
                                format!(
                                    "已为弱区规划 {} 张原视频桥接帧，等待隔离补帧 attempt 执行",
                                    summary.planned_bridge_frames
                                ),
                                Some(summary.planned_bridge_frames),
                                Some(summary.planned_bridge_frames),
                                Some("帧"),
                            );
                        }
                    }
                    Some(summary)
                }
                Ok(None) => None,
                Err(error) => {
                    self.events.send(
                        PipelineStage::ValidatingReconstruction,
                        Some(PipelineEngine::System),
                        EventKind::Log,
                        EventLevel::Warning,
                        Some(0.5),
                        false,
                        format!("无法写入自适应关键帧注册时间轴：{error}"),
                        None,
                        None,
                        None,
                    );
                    None
                }
            };
        if self.auto_bridge_frames
            && report.registered_ratio < 0.80
            && registration_summary
                .as_ref()
                .is_some_and(|summary| summary.planned_bridge_frames > 0)
        {
            self.events.stage(
                PipelineStage::ValidatingReconstruction,
                0.55,
                "自动补帧 attempt 1：正在准备隔离重建",
            );
            let bridge_plan = read_adaptive_bridge_plan(&paths.logs).await?;
            let combined_selection = combined_bridge_selection(&paths.logs, &bridge_plan).await?;
            let bridge_attempt = paths
                .project
                .join("work")
                .join("colmap-attempts")
                .join("supplemented-1");
            let bridge_frames = bridge_attempt.join("frames");
            let bridge_database = bridge_attempt.join("database.db");
            let bridge_log = bridge_attempt.join("colmap.log");
            let _bridge_sparse = bridge_attempt.join("incremental-ceres").join("sparse");
            tokio::fs::create_dir_all(&bridge_attempt).await?;
            let analysis_fps = state
                .frames
                .as_ref()
                .and_then(|frames| frames.analysis_fps)
                .unwrap_or(quality.preset().target_sampling_fps);
            match read_adaptive_proxy_source_samples(&paths.logs).await {
                Ok(proxy_samples) => {
                    self.events.stage(
                        PipelineStage::ValidatingReconstruction,
                        0.55,
                        format!(
                            "自动补帧 attempt 1：正在定向抽取 {} 张关键帧",
                            combined_selection.len()
                        ),
                    );
                    extract_selected_proxy_frames(
                        &self.engines.ffmpeg,
                        &metadata.source_path,
                        &bridge_frames,
                        &bridge_attempt.join("ffmpeg"),
                        &combined_selection,
                        &proxy_samples,
                        analysis_fps,
                        self.ffmpeg_hw_accel,
                        Some(paths.logs.join("ffmpeg-adaptive-bridge.log")),
                        &self.process_manager,
                        state.video.as_ref().map(|video| {
                            self.process_observer(
                                PipelineStage::ValidatingReconstruction,
                                PipelineEngine::Ffmpeg,
                                Some(combined_selection.len() as u64),
                                ObserverMode::FfmpegSelected {
                                    source_duration_seconds: video.duration,
                                },
                            )
                        }),
                    )
                    .await?;
                }
                Err(error) => {
                    self.events.send(
                        PipelineStage::ValidatingReconstruction,
                        Some(PipelineEngine::System),
                        EventKind::Log,
                        EventLevel::Warning,
                        Some(0.55),
                        false,
                        format!("自动补帧缺少可验证的代理候选映射，回退兼容抽帧：{error}"),
                        None,
                        None,
                        None,
                    );
                    self.events.stage(
                        PipelineStage::ValidatingReconstruction,
                        0.55,
                        "自动补帧 attempt 1：正在兼容抽取关键帧",
                    );
                    extract_selected_frames(
                        &self.engines.ffmpeg,
                        &metadata.source_path,
                        &bridge_frames,
                        &bridge_attempt.join("ffmpeg"),
                        &combined_selection,
                        analysis_fps,
                        self.ffmpeg_hw_accel,
                        Some(paths.logs.join("ffmpeg-adaptive-bridge.log")),
                        &self.process_manager,
                        None,
                    )
                    .await?;
                }
            }
            self.events.stage(
                PipelineStage::ValidatingReconstruction,
                0.65,
                "自动补帧 attempt 1：正在提取独立 COLMAP 特征",
            );
            colmap::extract_features(
                &colmap_exe,
                &bridge_database,
                &bridge_frames,
                ColmapFeatureOptions {
                    compute: colmap_compute,
                },
                bridge_log.clone(),
                &self.process_manager,
                None,
            )
            .await?;
            self.events.stage(
                PipelineStage::ValidatingReconstruction,
                0.75,
                "自动补帧 attempt 1：正在进行独立顺序匹配",
            );
            colmap::match_sequential(
                &colmap_exe,
                &bridge_database,
                ColmapMatchingOptions {
                    compute: colmap_compute,
                    overlap: 10,
                },
                bridge_log.clone(),
                &self.process_manager,
                None,
            )
            .await?;
            self.events.stage(
                PipelineStage::ValidatingReconstruction,
                0.85,
                "自动补帧 attempt 1：正在使用 Ceres 验证重建质量",
            );
            let candidate = run_ceres_mapper(
                &colmap_exe,
                &bridge_database,
                &bridge_frames,
                &_bridge_sparse,
                bridge_log,
                &self.process_manager,
                None,
            )
            .await;
            match candidate {
                Ok((candidate_model, candidate_report))
                    if accepts_bridge_attempt(&report, &candidate_report) =>
                {
                    promote_supplemented_frames(
                        &paths.frames,
                        &bridge_frames,
                        &bridge_attempt.join("original-frames"),
                    )?;
                    write_adaptive_selection_manifest(
                        &paths.logs,
                        &metadata.source_path,
                        &combined_selection,
                    )
                    .await?;
                    database = bridge_database;
                    model = candidate_model;
                    report = candidate_report;
                    ba_backend = "ceres-supplemented";
                    registration_summary =
                        write_registered_frame_timeline(&paths.logs, &model, false).await?;
                    self.events.stage(
                        PipelineStage::ValidatingReconstruction,
                        0.95,
                        "自动补帧 attempt 1 已通过质量验收",
                    );
                }
                Ok((_, candidate_report)) => self.events.stage(
                    PipelineStage::ValidatingReconstruction,
                    0.95,
                    format!(
                        "自动补帧 attempt 1 未采用：注册率 {:.1}% 未达到提升门槛",
                        candidate_report.registered_ratio * 100.0
                    ),
                ),
                Err(SplatError::Cancelled) => return Err(SplatError::Cancelled),
                Err(error) => self.events.stage(
                    PipelineStage::ValidatingReconstruction,
                    0.95,
                    format!("自动补帧 attempt 1 失败，继续使用原 attempt：{error}"),
                ),
            }
        }
        if let Some(summary) = registration_summary.filter(|summary| summary.weak_intervals > 0) {
            if !self.auto_bridge_frames {
                let requirement = SupplementRequirement {
                    reason: "自动补帧已关闭，检测到未注册关键帧弱区".into(),
                    weak_interval_count: summary.weak_intervals,
                    diagnostics_path: paths.logs.join("adaptive-registered-frames.json"),
                };
                state.needs_supplement = Some(requirement.clone());
                state.stage = PipelineStage::NeedsSupplement;
                metadata.needs_supplement = Some(requirement);
                project_manager.write_state(&paths.state, &state).await?;
                self.events.stage(
                    PipelineStage::NeedsSupplement,
                    1.0,
                    format!(
                        "已停止在训练前：{} 个弱区需要补充素材",
                        summary.weak_intervals
                    ),
                );
                return Err(SplatError::NeedsSupplement(format!(
                    "检测到 {} 个弱区；请查看 logs/adaptive-registered-frames.json 并上传补充素材，或重新启用自动补帧。",
                    summary.weak_intervals
                )));
            }
        }
        if report.quality == ReconstructionQuality::Failed {
            return Err(SplatError::Process(format!(
                "素材重建失败：注册 {}/{} 张（{:.1}%），低于 50% 阈值",
                report.registered_images,
                report.input_images,
                report.registered_ratio * 100.0
            )));
        }
        let warning = (report.quality == ReconstructionQuality::Warning).then(|| {
            format!(
                "注册率 {:.1}%：低于 80%，结果质量可能受影响",
                report.registered_ratio * 100.0
            )
        });
        append_final_adaptive_attempt(&paths.logs, &report, ba_backend).await?;
        let model = promote_colmap_attempt(&database, &model, &paths.colmap)?;
        self.events.stage(
            PipelineStage::ValidatingReconstruction,
            1.0,
            format!(
                "注册 {}/{} 张 · 三维点 {}",
                report.registered_images, report.input_images, report.points_3d
            ),
        );
        let phase_started = Instant::now();
        let training_input = match self.training_backend {
            TrainingBackend::Brush => {
                training::prepare_standard_colmap_dataset(
                    &paths.training_input,
                    &paths.frames,
                    &model,
                )
                .await?;
                paths.training_input.clone()
            }
            TrainingBackend::Gsplat => {
                let undistorted = paths.gsplat.join("training-input-undistorted");
                let temporary = paths.gsplat.join(".training-input-undistorted.tmp");
                if temporary.exists() {
                    tokio::fs::remove_dir_all(&temporary).await?;
                }
                colmap::undistort_images(
                    &colmap_exe,
                    &paths.frames,
                    &model,
                    &temporary,
                    paths.logs.join("colmap-undistort.log"),
                    &self.process_manager,
                )
                .await?;
                let sparse_model = normalize_undistorted_sparse_layout(&temporary).await?;
                if !temporary.join("images").is_dir() || !sparse_model.join("cameras.bin").is_file()
                {
                    return Err(SplatError::Process(
                        "COLMAP 去畸变输出不完整，已停止 gsplat 训练。".into(),
                    ));
                }
                if undistorted.exists() {
                    tokio::fs::remove_dir_all(&undistorted).await?;
                }
                tokio::fs::rename(&temporary, &undistorted).await?;
                undistorted
            }
        };
        metadata.timings.training_input_ms = phase_started.elapsed().as_millis() as u64;
        state.training_input_complete = true;
        project_manager.write_state(&paths.state, &state).await?;
        let preset = match self.training_backend {
            TrainingBackend::Brush => self.brush_training_preset.apply(quality.preset()),
            TrainingBackend::Gsplat => quality.preset(),
        };
        let engine = match self.training_backend {
            TrainingBackend::Brush => PipelineEngine::Brush,
            TrainingBackend::Gsplat => PipelineEngine::Gsplat,
        };
        let backend_name = match self.training_backend {
            TrainingBackend::Brush => "Brush",
            TrainingBackend::Gsplat => "gsplat CUDA",
        };
        self.events.send(
            PipelineStage::TrainingSplats,
            Some(engine),
            EventKind::Stage,
            EventLevel::Info,
            Some(0.0),
            false,
            format!("{backend_name} · 0/{}", preset.brush_iterations),
            Some(0),
            Some(preset.brush_iterations as u64),
            Some("iterations"),
        );
        let phase_started = Instant::now();
        let training_output = training::train(
            self.training_backend,
            &self.engines.brush,
            &self.engines.root,
            TrainingRequest {
                dataset_root: training_input,
                output_directory: match self.training_backend {
                    TrainingBackend::Brush => paths.brush.clone(),
                    TrainingBackend::Gsplat => paths.gsplat.clone(),
                },
                total_steps: preset.brush_iterations,
                max_resolution: preset.brush_max_resolution,
                max_splats: match self.training_backend {
                    TrainingBackend::Brush => preset.brush_max_splats,
                    TrainingBackend::Gsplat => self.gsplat_splat_cap.limit(preset.brush_max_splats),
                },
                seed: 42,
                photometric_mode: self.photometric_mode,
                densification_strategy: self.gsplat_densification_strategy,
                log_path: paths.logs.join(match self.training_backend {
                    TrainingBackend::Brush => "brush.log",
                    TrainingBackend::Gsplat => "gsplat.log",
                }),
            },
            &self.process_manager,
            Some(self.process_observer(
                PipelineStage::TrainingSplats,
                engine,
                Some(preset.brush_iterations as u64),
                match self.training_backend {
                    TrainingBackend::Brush => ObserverMode::Brush(paths.brush.clone()),
                    TrainingBackend::Gsplat => ObserverMode::Gsplat,
                },
            )),
        )
        .await?;
        metadata.timings.training_ms = phase_started.elapsed().as_millis() as u64;
        let candidate = training_output.candidate_ply;
        state.stage = PipelineStage::TrainingSplats;
        // Keep the legacy field truthful for resumed projects; the selected
        // backend is recorded separately in `training_backend`.
        state.brush_complete = self.training_backend == TrainingBackend::Brush;
        project_manager.write_state(&paths.state, &state).await?;
        self.events.stage(
            PipelineStage::TrainingSplats,
            1.0,
            format!("{backend_name} 完成"),
        );
        self.events
            .stage(PipelineStage::Exporting, 0.0, "正在校验并发布 PLY");
        let phase_started = Instant::now();
        let ply = inspect_gaussian_ply(&candidate)?;
        metadata.timings.ply_validation_ms = phase_started.elapsed().as_millis() as u64;
        let output_stem = metadata
            .source_path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("splat");
        let final_ply = paths.project.join(format!("{output_stem}.ply"));
        tokio::fs::rename(&candidate, &final_ply).await?;
        state.stage = PipelineStage::Completed;
        project_manager.write_state(&paths.state, &state).await?;
        let completed_at = Utc::now();
        let duration_ms = metadata
            .started_at
            .map(|started| (completed_at - started).num_milliseconds().max(0) as u64)
            .unwrap_or(0);
        metadata.status = ProjectStatus::Completed;
        metadata.completed_at = Some(completed_at);
        metadata.duration_ms = Some(duration_ms);
        metadata.timings.total_ms = total_started.elapsed().as_millis() as u64;
        let output = ProjectOutput {
            final_ply: final_ply.clone(),
            file_size: ply.file_size,
            splat_count: ply.splat_count,
            input_images: report.input_images,
            registered_images: report.registered_images,
            registered_ratio: report.registered_ratio,
            points_3d: report.points_3d,
        };
        metadata.output = Some(output.clone());
        project_manager
            .write_metadata(&paths.metadata, metadata)
            .await?;
        self.events.stage(
            PipelineStage::Exporting,
            1.0,
            format!(
                "{} 已发布",
                final_ply
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("PLY")
            ),
        );
        self.events.stage(PipelineStage::Completed, 1.0, "任务完成");
        Ok(PipelineResult {
            project_id: metadata.id.to_string(),
            project_path: paths.project.clone(),
            final_ply,
            file_size: ply.file_size,
            splat_count: ply.splat_count,
            input_images: report.input_images,
            registered_images: report.registered_images,
            registered_ratio: report.registered_ratio,
            points_3d: report.points_3d,
            duration_ms,
            completed_at,
            warning,
            logs_directory: paths.logs.clone(),
            colmap_backend: self.colmap_backend,
        })
    }
    async fn run_splatcam_project(
        &self,
        project_manager: &ProjectManager,
        paths: &ProjectPaths,
        metadata: &mut ProjectMetadata,
        quality: Quality,
    ) -> Result<PipelineResult> {
        let total_started = Instant::now();
        let source = metadata.source_path.clone();
        let import_root = paths.project.join("work").join("splatcam-import");
        let text_model = import_root.join("normalized-model");
        let binary_model = import_root.join("model-bin");
        let mut state = PipelineStateFile::created(quality);
        state.input_source = InputSource::Splatcam;
        state.training_backend = self.training_backend;
        metadata.training_backend = self.training_backend;
        metadata.photometric_mode = self.photometric_mode;
        metadata.brush_training_preset = self.brush_training_preset;
        metadata.gsplat_splat_cap = self.gsplat_splat_cap;
        metadata.gsplat_densification_strategy = self.gsplat_densification_strategy;
        self.splatcam_import_step(0, 5, "正在验证 Splatcam RGB、相机、位姿与点云");
        tokio::fs::create_dir_all(&import_root).await?;
        let report = tokio::task::spawn_blocking({
            let source = source.clone();
            move || splatcam::inspect_export(&source)
        })
        .await
        .map_err(|error| SplatError::Process(format!("Splatcam 导入检查任务失败：{error}")))??;
        // Keep the full geometry metrics beside the normalized model. `project.json` deliberately
        // stores only the durable summary used by history, while this report is the diagnostic
        // artifact needed to audit an accepted or rejected source export.
        atomic_write_json(&import_root.join("import-report.json"), &report).await?;
        if !report.geometry_gate.passed {
            return Err(SplatError::Process(
                report
                    .geometry_gate
                    .reason
                    .unwrap_or_else(|| "Splatcam 坐标系门禁失败".into()),
            ));
        }
        self.splatcam_import_step(
            1,
            5,
            format!(
                "已校验 {} 张 RGB、{} 个位姿、{} 个初始化点；正在保留来源快照",
                report.image_count, report.pose_count, report.point_count
            ),
        );
        let staged_source = paths.project.join("source").join("splatcam");
        tokio::task::spawn_blocking({
            let source = source.clone();
            let staged_source = staged_source.clone();
            move || splatcam::stage_source_export(&source, &staged_source)
        })
        .await
        .map_err(|error| SplatError::Process(format!("Splatcam 来源快照任务失败：{error}")))??;
        let import_state = SplatcamImportState {
            source_path: source.clone(),
            image_count: report.image_count,
            pose_count: report.pose_count,
            point_count: report.point_count,
            coordinate_convention: report.coordinate_convention.into(),
            has_depth: report.has_depth,
            has_transforms: report.has_transforms,
            geometry_gate_passed: true,
        };
        metadata.splatcam_import = Some(import_state.clone());
        state.splatcam_import = Some(import_state);
        project_manager
            .write_metadata(&paths.metadata, metadata)
            .await?;
        project_manager.write_state(&paths.state, &state).await?;
        self.splatcam_import_step(2, 5, "来源快照已保留；正在标准化 COLMAP 文本模型");
        let source_for_model = staged_source.clone();
        let text_destination = text_model.clone();
        tokio::task::spawn_blocking(move || {
            splatcam::prepare_normalized_text_model(&source_for_model, &text_destination)
        })
        .await
        .map_err(|error| SplatError::Process(format!("Splatcam 文本模型标准化失败：{error}")))??;
        self.splatcam_import_step(3, 5, "文本模型已标准化；正在转换 COLMAP 二进制模型");
        tokio::fs::create_dir(&binary_model).await?;
        colmap::convert_text_model_to_binary(
            &self.engines.colmap,
            &text_model,
            &binary_model,
            paths.logs.join("splatcam-model-converter.log"),
            &self.process_manager,
            None,
        )
        .await?;
        tokio::task::spawn_blocking({
            let binary_model = binary_model.clone();
            move || {
                splatcam::verify_binary_model_counts(
                    &binary_model,
                    report.camera_count,
                    report.pose_count,
                    report.point_count,
                )
            }
        })
        .await
        .map_err(|error| SplatError::Process(format!("Splatcam 二进制模型验证失败：{error}")))??;
        self.splatcam_import_step(4, 5, "二进制模型已校验；正在生成标准训练输入");
        let input_images = staged_source.join("images");
        training::prepare_standard_colmap_dataset(
            &paths.training_input,
            &input_images,
            &binary_model,
        )
        .await?;
        self.splatcam_import_step(5, 5, "训练输入已就绪，正在进入训练");
        metadata.timings.training_input_ms = total_started.elapsed().as_millis() as u64;
        state.training_input_complete = true;
        state.stage = PipelineStage::TrainingSplats;
        project_manager.write_state(&paths.state, &state).await?;
        let preset = match self.training_backend {
            TrainingBackend::Brush => self.brush_training_preset.apply(quality.preset()),
            TrainingBackend::Gsplat => quality.preset(),
        };
        let engine = match self.training_backend {
            TrainingBackend::Brush => PipelineEngine::Brush,
            TrainingBackend::Gsplat => PipelineEngine::Gsplat,
        };
        let backend_name = match self.training_backend {
            TrainingBackend::Brush => "Brush",
            TrainingBackend::Gsplat => "gsplat CUDA",
        };
        self.events.send(
            PipelineStage::TrainingSplats,
            Some(engine),
            EventKind::Stage,
            EventLevel::Info,
            Some(0.0),
            false,
            format!("{backend_name} · 0/{}", preset.brush_iterations),
            Some(0),
            Some(preset.brush_iterations as u64),
            Some("iterations"),
        );
        let train_started = Instant::now();
        let training_output = training::train(
            self.training_backend,
            &self.engines.brush,
            &self.engines.root,
            TrainingRequest {
                dataset_root: paths.training_input.clone(),
                output_directory: match self.training_backend {
                    TrainingBackend::Brush => paths.brush.clone(),
                    TrainingBackend::Gsplat => paths.gsplat.clone(),
                },
                total_steps: preset.brush_iterations,
                max_resolution: preset.brush_max_resolution,
                max_splats: match self.training_backend {
                    TrainingBackend::Brush => preset.brush_max_splats,
                    TrainingBackend::Gsplat => self.gsplat_splat_cap.limit(preset.brush_max_splats),
                },
                seed: 42,
                photometric_mode: self.photometric_mode,
                densification_strategy: self.gsplat_densification_strategy,
                log_path: paths.logs.join(match self.training_backend {
                    TrainingBackend::Brush => "brush.log",
                    TrainingBackend::Gsplat => "gsplat.log",
                }),
            },
            &self.process_manager,
            Some(self.process_observer(
                PipelineStage::TrainingSplats,
                engine,
                Some(preset.brush_iterations as u64),
                match self.training_backend {
                    TrainingBackend::Brush => ObserverMode::Brush(paths.brush.clone()),
                    TrainingBackend::Gsplat => ObserverMode::Gsplat,
                },
            )),
        )
        .await?;
        metadata.timings.training_ms = train_started.elapsed().as_millis() as u64;
        state.brush_complete = self.training_backend == TrainingBackend::Brush;
        self.events.stage(
            PipelineStage::TrainingSplats,
            1.0,
            format!("{backend_name} 完成"),
        );
        self.events
            .stage(PipelineStage::Exporting, 0.0, "正在校验并发布 Gaussian PLY");
        let ply = inspect_gaussian_ply(&training_output.candidate_ply)?;
        let output_stem = source
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("splatcam");
        let final_ply = paths.project.join(format!("{output_stem}.ply"));
        tokio::fs::rename(&training_output.candidate_ply, &final_ply).await?;
        let completed_at = Utc::now();
        let duration_ms = metadata
            .started_at
            .map(|started| (completed_at - started).num_milliseconds().max(0) as u64)
            .unwrap_or(0);
        let output = ProjectOutput {
            final_ply: final_ply.clone(),
            file_size: ply.file_size,
            splat_count: ply.splat_count,
            input_images: report.image_count,
            registered_images: report.pose_count,
            registered_ratio: 1.0,
            points_3d: report.point_count,
        };
        state.stage = PipelineStage::Completed;
        metadata.status = ProjectStatus::Completed;
        metadata.completed_at = Some(completed_at);
        metadata.duration_ms = Some(duration_ms);
        metadata.timings.total_ms = total_started.elapsed().as_millis() as u64;
        metadata.output = Some(output.clone());
        project_manager.write_state(&paths.state, &state).await?;
        project_manager
            .write_metadata(&paths.metadata, metadata)
            .await?;
        self.events.stage(
            PipelineStage::Exporting,
            1.0,
            format!(
                "{} 已发布",
                final_ply
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("PLY")
            ),
        );
        self.events.stage(PipelineStage::Completed, 1.0, "任务完成");
        Ok(PipelineResult {
            project_id: metadata.id.to_string(),
            project_path: paths.project.clone(),
            final_ply,
            file_size: output.file_size,
            splat_count: output.splat_count,
            input_images: output.input_images,
            registered_images: output.registered_images,
            registered_ratio: output.registered_ratio,
            points_3d: output.points_3d,
            duration_ms,
            completed_at,
            warning: None,
            logs_directory: paths.logs.clone(),
            colmap_backend: ColmapBackend::Cpu,
        })
    }
    fn splatcam_import_step(&self, current: u64, total: u64, message: impl Into<String>) {
        self.events.send(
            PipelineStage::ImportingSplatcam,
            Some(PipelineEngine::System),
            EventKind::Progress,
            EventLevel::Info,
            Some(current as f32 / total as f32),
            false,
            message,
            Some(current),
            Some(total),
            Some("步骤"),
        );
    }
    fn process_observer(
        &self,
        stage: PipelineStage,
        engine: PipelineEngine,
        total: Option<u64>,
        mode: ObserverMode,
    ) -> ProcessObserver {
        let events = self.events.clone();
        // A caller that supplies a total has established a determinate stage,
        // even when the external tool has not yet emitted its first counter.
        // Keeping the zero value here prevents the periodic heartbeat from
        // replacing the initial `0 / total` with the UI's "持续运行" fallback.
        let initial_total = match &mode {
            ObserverMode::FfmpegSelected {
                source_duration_seconds,
            } => Some(source_duration_seconds.ceil().max(1.0) as u64),
            _ => total,
        };
        let initial_unit = match &mode {
            ObserverMode::Brush(_) | ObserverMode::Gsplat => Some("iterations".to_string()),
            ObserverMode::FfmpegSelected { .. } => Some("秒".to_string()),
            _ => None,
        };
        let stage_progress = Arc::new(std::sync::Mutex::new((
            0.0_f32,
            initial_total.map(|_| 0_u64),
            initial_total,
            initial_unit,
        )));
        let ffmpeg_state = Arc::new(std::sync::Mutex::new(FfmpegProgressState::default()));
        Arc::new(move |update| match update {
            ProcessUpdate::Line { stream, line } => {
                let mut ffmpeg_state = ffmpeg_state.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(progress) = parse_progress(&line, &mode, total, &mut ffmpeg_state) {
                    *stage_progress.lock().unwrap_or_else(|p| p.into_inner()) =
                        (progress.0, Some(progress.2), progress.3, progress.4.clone());
                    events.send(
                        stage,
                        Some(engine),
                        EventKind::Progress,
                        EventLevel::Info,
                        Some(progress.0),
                        false,
                        progress.1,
                        Some(progress.2),
                        progress.3,
                        progress.4.as_deref(),
                    );
                } else if is_user_visible_diagnostic(&line) {
                    // External tools (especially COLMAP) emit verbose INFO lines
                    // such as focal length and camera parameters. They remain in
                    // the per-project log file, but the live UI is reserved for
                    // meaningful progress and actionable failures.
                    let (progress, current, total, unit) = stage_progress
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .clone();
                    events.send(
                        stage,
                        Some(engine),
                        EventKind::Log,
                        match stream {
                            crate::process::ProcessStream::Stderr => EventLevel::Warning,
                            crate::process::ProcessStream::Stdout => EventLevel::Info,
                        },
                        Some(progress),
                        total.is_none(),
                        line,
                        current,
                        total,
                        unit.as_deref(),
                    );
                }
            }
            ProcessUpdate::Heartbeat { elapsed_ms } => {
                if let ObserverMode::Brush(directory) = &mode {
                    if let Some(step) = brush_checkpoint_step(directory) {
                        let bounded = total.map(|value| step.min(value)).unwrap_or(step);
                        let ratio = total
                            .filter(|value| *value > 0)
                            .map(|value| bounded as f32 / value as f32)
                            .unwrap_or(0.0);
                        *stage_progress.lock().unwrap_or_else(|p| p.into_inner()) =
                            (ratio, Some(bounded), total, Some("iterations".into()));
                    }
                }
                let (progress, current, total, unit) = stage_progress
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone();
                let message = match (engine, current, total) {
                    (PipelineEngine::Brush, Some(current), Some(total)) if current > 0 => {
                        format!("Brush · {current}/{total}")
                    }
                    (PipelineEngine::Brush, _, _) => format!("Brush · {} 秒", elapsed_ms / 1000),
                    _ => format!("运行中 · {} 秒", elapsed_ms / 1000),
                };
                events.send(
                    stage,
                    Some(engine),
                    EventKind::Heartbeat,
                    EventLevel::Info,
                    Some(progress),
                    total.is_none(),
                    message,
                    current,
                    total,
                    unit.as_deref(),
                );
            }
            ProcessUpdate::Started { .. } => {}
        })
    }
}

fn is_user_visible_diagnostic(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("ERROR")
        || line.starts_with("Error")
        || line.starts_with("error:")
        || line.starts_with("fatal:")
        // COLMAP/GLOG uses `Eyyyymmdd ...` for failures and `I...` for
        // informational diagnostics. Never surface the latter in real time.
        || (line.starts_with('E')
            && line.as_bytes().get(1).is_some_and(|byte| byte.is_ascii_digit()))
}
pub fn default_engine_paths(engine_dir: Option<PathBuf>) -> EnginePaths {
    match engine_dir {
        Some(path) => EnginePaths::from_root(path),
        None => EnginePaths::discover(None),
    }
}
#[derive(Clone)]
enum ObserverMode {
    Ffmpeg,
    /// Sparse `select` extraction needs to report scan time, not just the
    /// number of JPEGs already emitted. The latter can reach its total before
    /// FFmpeg has decoded the tail of the source video.
    FfmpegSelected { source_duration_seconds: f64 },
    BracketProgress,
    Mapper,
    Brush(PathBuf),
    Gsplat,
}
type ProgressSample = (f32, String, u64, Option<u64>, Option<String>);

#[derive(Default)]
struct FfmpegProgressState {
    encoded_frames: u64,
}

#[derive(Deserialize)]
struct GsplatProgressEvent {
    event: String,
    step: Option<u64>,
    total: Option<u64>,
    loss: Option<f64>,
    splats: Option<u64>,
}

fn brush_checkpoint_step(directory: &Path) -> Option<u64> {
    std::fs::read_dir(directory)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            name.strip_prefix("checkpoint_")?
                .split('.')
                .next()?
                .parse::<u64>()
                .ok()
        })
        .max()
}
fn parse_progress(
    line: &str,
    mode: &ObserverMode,
    total: Option<u64>,
    ffmpeg_state: &mut FfmpegProgressState,
) -> Option<ProgressSample> {
    match mode {
        ObserverMode::Ffmpeg => {
            // FFmpeg writes `frame=42` lines.
            if let Some(rest) = line.strip_prefix("frame=") {
                if let Some(value) = rest.split_whitespace().next() {
                    if let Ok(parsed) = value.parse::<u64>() {
                        let ratio = total
                            .filter(|t| *t > 0)
                            .map(|t| parsed.min(t) as f32 / t as f32)
                            .unwrap_or(0.0);
                        return Some((
                            ratio,
                            format!("已处理 {parsed} 帧"),
                            parsed,
                            total,
                            Some("张".into()),
                        ));
                    }
                }
            }
            None
        }
        ObserverMode::FfmpegSelected {
            source_duration_seconds,
        } => {
            if let Some(rest) = line.strip_prefix("frame=") {
                if let Some(value) = rest.split_whitespace().next() {
                    ffmpeg_state.encoded_frames = value.parse::<u64>().ok()?;
                }
                return None;
            }
            let source_seconds = line
                .strip_prefix("out_time_us=")?
                .parse::<f64>()
                .ok()?
                / 1_000_000.0;
            let source_total_seconds = source_duration_seconds.max(0.001);
            let source_current_seconds = source_seconds
                .ceil()
                .max(0.0)
                .min(source_total_seconds.ceil()) as u64;
            let source_total = source_total_seconds.ceil().max(1.0) as u64;
            let ratio = (source_seconds / source_total_seconds).clamp(0.0, 1.0) as f32;
            let encoded_total = total.unwrap_or(0);
            Some((
                ratio,
                format!(
                    "正在扫描原视频 {:.1} / {:.1} 秒；已编码 {} / {} 张关键帧",
                    source_seconds.min(source_total_seconds),
                    source_total_seconds,
                    ffmpeg_state.encoded_frames.min(encoded_total),
                    encoded_total
                ),
                source_current_seconds,
                Some(source_total),
                Some("秒".into()),
            ))
        }
        ObserverMode::BracketProgress => {
            // Generic `<X>/<Y>` bracket counter, e.g. `processed 12/100`.
            for token in line.split_whitespace() {
                if let Some((left, right)) = token.split_once('/') {
                    if let (Ok(current), Ok(total)) = (
                        left.trim_start_matches(|c: char| !c.is_ascii_digit())
                            .parse::<u64>(),
                        right
                            .trim_end_matches(|c: char| !c.is_ascii_digit())
                            .parse::<u64>(),
                    ) {
                        let ratio = if total > 0 {
                            current.min(total) as f32 / total as f32
                        } else {
                            0.0
                        };
                        return Some((
                            ratio,
                            line.to_string(),
                            current,
                            Some(total),
                            Some("步".into()),
                        ));
                    }
                }
            }
            None
        }
        ObserverMode::Mapper => {
            // COLMAP/GLOG prefixes each line and reports the meaningful count
            // as `num_reg_frames=N`, e.g. `I... Registering image #61
            // (num_reg_frames=2)`. Do not require the message at column zero.
            if let Some((_, rest)) = line.split_once("num_reg_frames=") {
                if let Some(value) = rest.split(|c: char| !c.is_ascii_digit()).next() {
                    if let Ok(parsed) = value.parse::<u64>() {
                        let ratio = total
                            .filter(|t| *t > 0)
                            .map(|t| parsed.min(t) as f32 / t as f32)
                            .unwrap_or(0.0);
                        return Some((
                            ratio,
                            format!("正在注册 {parsed}"),
                            parsed,
                            total,
                            Some("张".into()),
                        ));
                    }
                }
            }
            None
        }
        ObserverMode::Brush(_) => {
            if let Some(rest) = line.strip_prefix("step=") {
                if let Some(value) = rest.split_whitespace().next() {
                    if let Ok(parsed) = value.parse::<u64>() {
                        let ratio = total
                            .filter(|t| *t > 0)
                            .map(|t| parsed.min(t) as f32 / t as f32)
                            .unwrap_or(0.0);
                        return Some((
                            ratio,
                            format!("Brush · {parsed}"),
                            parsed,
                            total,
                            Some("iterations".into()),
                        ));
                    }
                }
            }
            None
        }
        ObserverMode::Gsplat => {
            let event = serde_json::from_str::<GsplatProgressEvent>(line).ok()?;
            if event.event != "progress" {
                return None;
            }
            let (step, event_total) = (event.step?, event.total?);
            if event_total == 0 || step > event_total {
                return None;
            }
            let loss = event
                .loss
                .map(|value| format!(" · loss {value:.5}"))
                .unwrap_or_default();
            let splats = event
                .splats
                .map(|value| format!(" · {value} splats"))
                .unwrap_or_default();
            Some((
                step as f32 / event_total as f32,
                format!("gsplat 训练 {step}/{event_total}{loss}{splats}"),
                step,
                Some(event_total),
                Some("iterations".into()),
            ))
        }
    }
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    #[test]
    fn parses_gsplat_jsonl_progress_for_live_ui() {
        let line = r#"{"event":"progress","step":13700,"total":15000,"loss":0.01941635087132454,"splats":5920}"#;
        let sample = parse_progress(
            line,
            &ObserverMode::Gsplat,
            None,
            &mut FfmpegProgressState::default(),
        )
        .unwrap();
        assert_eq!(sample.2, 13_700);
        assert_eq!(sample.3, Some(15_000));
        assert!(sample.1.contains("loss 0.01942"));
        assert!(sample.1.contains("5920 splats"));
    }

    #[tokio::test]
    async fn normalizes_direct_undistorter_sparse_layout_for_gsplat() {
        let temporary = tempfile::tempdir().unwrap();
        let sparse = temporary.path().join("sparse");
        std::fs::create_dir_all(&sparse).unwrap();
        for name in [
            "cameras.bin",
            "images.bin",
            "points3D.bin",
            "frames.bin",
            "rigs.bin",
        ] {
            std::fs::write(sparse.join(name), b"colmap").unwrap();
        }

        let model = normalize_undistorted_sparse_layout(temporary.path())
            .await
            .unwrap();

        assert_eq!(model, sparse.join("0"));
        for name in [
            "cameras.bin",
            "images.bin",
            "points3D.bin",
            "frames.bin",
            "rigs.bin",
        ] {
            assert!(model.join(name).is_file());
            assert!(!sparse.join(name).exists());
        }
    }

    #[test]
    fn hides_colmap_info_but_keeps_actionable_errors() {
        assert!(!is_user_visible_diagnostic(
            "I20260822 15:26:11.953427 24288 feature_extraction.cc:289] Focal Length: 2304.00px"
        ));
        assert!(is_user_visible_diagnostic(
            "E20260822 15:26:12.0 mapper.cc:1] reconstruction failed"
        ));
    }

    #[test]
    fn parses_glog_mapper_registration_counter() {
        let sample = parse_progress(
            "I20260823 02:34:08 incremental_pipeline.cc:537] Registering image #61 (num_reg_frames=2)",
            &ObserverMode::Mapper,
            Some(273),
            &mut FfmpegProgressState::default(),
        )
        .unwrap();
        assert_eq!(sample.2, 2);
        assert_eq!(sample.3, Some(273));
    }

    #[test]
    fn parses_ffmpeg_frame_progress_as_a_determinate_counter() {
        let sample = parse_progress(
            "frame=42",
            &ObserverMode::Ffmpeg,
            Some(120),
            &mut FfmpegProgressState::default(),
        )
        .unwrap();
        assert_eq!(sample.0, 0.35);
        assert_eq!(sample.1, "已处理 42 帧");
        assert_eq!(sample.2, 42);
        assert_eq!(sample.3, Some(120));
        assert_eq!(sample.4.as_deref(), Some("张"));
    }

    #[test]
    fn selected_ffmpeg_progress_tracks_source_scan_and_encoded_count() {
        let mode = ObserverMode::FfmpegSelected {
            source_duration_seconds: 21.0,
        };
        let mut state = FfmpegProgressState::default();
        assert!(parse_progress("frame=9", &mode, Some(12), &mut state).is_none());
        let sample = parse_progress("out_time_us=10500000", &mode, Some(12), &mut state).unwrap();
        assert_eq!(sample.0, 0.5);
        assert_eq!(sample.1, "正在扫描原视频 10.5 / 21.0 秒；已编码 9 / 12 张关键帧");
        assert_eq!(sample.2, 11);
        assert_eq!(sample.3, Some(21));
        assert_eq!(sample.4.as_deref(), Some("秒"));
    }

    #[test]
    fn supplement_diagnostics_deserialize_persisted_weak_intervals() {
        let diagnostics: SupplementDiagnostics = serde_json::from_str(
            r#"{
                "selectedFrames": 12,
                "registeredFrames": 9,
                "weakIntervals": [{
                    "reason": "unregisteredSelectedFrames",
                    "startPtsSeconds": 4.0,
                    "endPtsSeconds": 5.5,
                    "unregisteredFrames": 2,
                    "firstOutputFile": "frame_000005.jpg",
                    "lastOutputFile": "frame_000006.jpg",
                    "beforeAnchor": { "outputFile": "frame_000004.jpg", "ptsSeconds": 3.5 },
                    "afterAnchor": null
                }]
            }"#,
        )
        .unwrap();
        assert_eq!(diagnostics.selected_frames, 12);
        assert_eq!(diagnostics.weak_intervals[0].before_anchor.as_ref().unwrap().pts_seconds, 3.5);
        assert!(diagnostics.weak_intervals[0].after_anchor.is_none());
    }

    #[test]
    fn mapper_ba_mode_routes_cuda_and_cpu_deterministically() {
        assert!(!should_use_caspar(
            ColmapBackend::Cuda,
            true,
            MapperBaMode::Auto,
            150
        ));
        assert!(should_use_caspar(
            ColmapBackend::Cuda,
            true,
            MapperBaMode::Auto,
            151
        ));
        assert!(!should_use_caspar(
            ColmapBackend::Cuda,
            true,
            MapperBaMode::Ceres,
            631
        ));
        assert!(should_use_caspar(
            ColmapBackend::Cuda,
            true,
            MapperBaMode::Caspar,
            42
        ));
        assert!(!should_use_caspar(
            ColmapBackend::Cuda,
            false,
            MapperBaMode::Caspar,
            631
        ));
        assert!(!should_use_caspar(
            ColmapBackend::Cpu,
            true,
            MapperBaMode::Caspar,
            631
        ));
    }

    #[test]
    fn groups_adjacent_unregistered_frames_with_registered_anchors() {
        let frames = vec![
            timeline_frame("frame_000001.jpg", 0.0, true),
            timeline_frame("frame_000002.jpg", 0.5, false),
            timeline_frame("frame_000003.jpg", 1.0, false),
            timeline_frame("frame_000004.jpg", 1.5, true),
            timeline_frame("frame_000005.jpg", 2.0, false),
        ];

        let intervals = detect_weak_intervals(&frames);
        assert_eq!(intervals.len(), 2);
        assert_eq!(intervals[0].start_pts_seconds, 0.5);
        assert_eq!(intervals[0].end_pts_seconds, 1.0);
        assert_eq!(intervals[0].unregistered_frames, 2);
        assert_eq!(
            intervals[0]
                .before_anchor
                .as_ref()
                .map(|anchor| anchor.pts_seconds),
            Some(0.0)
        );
        assert_eq!(
            intervals[0]
                .after_anchor
                .as_ref()
                .map(|anchor| anchor.pts_seconds),
            Some(1.5)
        );
        assert!(intervals[1].after_anchor.is_none());
    }

    #[test]
    fn bridge_plan_respects_global_cap_and_excludes_selected_frames() {
        let selected = vec![
            selected_log("frame_000001.jpg", 1, 0.0),
            selected_log("frame_000004.jpg", 4, 3.0),
            selected_log("frame_000007.jpg", 7, 5.0),
            selected_log("frame_000008.jpg", 8, 6.0),
        ];
        let weak = WeakFrameInterval {
            reason: "unregisteredSelectedFrames",
            start_pts_seconds: 1.0,
            end_pts_seconds: 2.0,
            unregistered_frames: 1,
            first_output_file: "frame_000004.jpg".into(),
            last_output_file: "frame_000004.jpg".into(),
            before_anchor: Some(WeakIntervalAnchor {
                output_file: "frame_000001.jpg".into(),
                pts_seconds: 0.0,
            }),
            after_anchor: Some(WeakIntervalAnchor {
                output_file: "frame_000007.jpg".into(),
                pts_seconds: 5.0,
            }),
        };
        let mut lower_sharpness = proxy_frame(2, 1.0, 2.0);
        let mut higher_sharpness = proxy_frame(3, 2.0, 9.0);
        lower_sharpness.inliers = 100;
        higher_sharpness.inliers = 100;

        let plan = plan_adaptive_bridges(&[lower_sharpness, higher_sharpness], &selected, &[weak]);
        assert_eq!(plan.max_additional_frames, 1);
        assert_eq!(plan.planned_frames.len(), 1);
        assert_eq!(plan.planned_frames[0].source_index, 3);
    }

    #[test]
    fn proxy_diagnostics_exposes_the_failing_geometry_gate() {
        let mut qualifying = proxy_frame(2, 0.5, 2.0);
        qualifying.textured_cells = 30;
        qualifying.matched_cells = 20;
        qualifying.inliers = 12;
        qualifying.grid_coverage = 0.4;
        qualifying.three_view_tracks = 6;
        let mut rejected = proxy_frame(1, 0.0, 1.0);
        rejected.grid_coverage = 0.1;
        let diagnostics = summarize_proxy_diagnostics(
            &[rejected, qualifying],
            AdaptiveFrameProfile {
                anchor_fps: 2.0,
                analysis_fps: 6.0,
                local_refine_fps: 12.0,
                target_motion: 0.035,
                max_motion: 0.08,
                min_interval_ms: 120,
                min_textured_cells: 12,
                min_matched_cells: 8,
                min_inliers_floor: 6,
                min_inlier_ratio: 0.45,
                min_three_view_floor: 3,
                min_three_view_ratio: 0.35,
            },
            1,
        );
        assert_eq!(diagnostics.proxy_candidates, 2);
        assert_eq!(diagnostics.geometry_qualified_frames, 1);
        assert_eq!(diagnostics.below_min_textured_cells, 1);
        assert_eq!(diagnostics.below_min_matched_cells, 1);
        assert_eq!(diagnostics.below_min_inlier_floor, 1);
    }

    fn timeline_frame(
        output_file: &str,
        pts_seconds: f64,
        registered: bool,
    ) -> RegisteredFrameTimelineEntry {
        RegisteredFrameTimelineEntry {
            output_file: output_file.into(),
            pts_seconds,
            registered,
        }
    }

    fn selected_log(
        output_file: &str,
        source_index: u64,
        pts_seconds: f64,
    ) -> AdaptiveSelectedFrameLog {
        AdaptiveSelectedFrameLog {
            output_file: output_file.into(),
            source_index,
            pts_seconds,
        }
    }

    fn proxy_frame(source_index: u64, pts_seconds: f64, sharpness: f64) -> ProxyFrame {
        ProxyFrame {
            source_index,
            pts_seconds,
            phash: source_index,
            sharpness,
            textured_cells: 4,
            matched_cells: 4,
            background_motion: 0.02,
            inliers: 0,
            grid_coverage: 0.5,
            three_view_tracks: 20,
            confirmed_scene_change: false,
        }
    }
}
fn summarize_proxy_diagnostics(
    frames: &[ProxyFrame],
    profile: AdaptiveFrameProfile,
    selected_frames: usize,
) -> AdaptiveProxyDiagnostics {
    let geometry_qualified_frames = frames
        .iter()
        .filter(|frame| passes_proxy_geometry(frame, profile))
        .count();
    AdaptiveProxyDiagnostics {
        proxy_candidates: u64::try_from(frames.len()).expect("usize always fits u64"),
        selected_frames: u64::try_from(selected_frames).expect("usize always fits u64"),
        min_textured_cells: profile.min_textured_cells,
        min_matched_cells: profile.min_matched_cells,
        min_inliers_floor: profile.min_inliers_floor,
        min_inlier_ratio: profile.min_inlier_ratio,
        min_three_view_floor: profile.min_three_view_floor,
        min_three_view_ratio: profile.min_three_view_ratio,
        geometry_qualified_frames: u64::try_from(geometry_qualified_frames)
            .expect("usize always fits u64"),
        below_min_textured_cells: u64::try_from(
            frames
                .iter()
                .filter(|frame| frame.textured_cells < profile.min_textured_cells)
                .count(),
        )
        .expect("usize always fits u64"),
        below_min_matched_cells: u64::try_from(
            frames
                .iter()
                .filter(|frame| frame.matched_cells < profile.min_matched_cells)
                .count(),
        )
        .expect("usize always fits u64"),
        below_min_inlier_floor: u64::try_from(
            frames
                .iter()
                .filter(|frame| frame.inliers < profile.min_inliers_floor)
                .count(),
        )
        .expect("usize always fits u64"),
        below_min_inlier_ratio: u64::try_from(
            frames
                .iter()
                .filter(|frame| {
                    frame.matched_cells == 0
                        || frame.inliers as f64 / (frame.matched_cells as f64)
                            < profile.min_inlier_ratio
                })
                .count(),
        )
        .expect("usize always fits u64"),
        below_min_three_view_floor: u64::try_from(
            frames
                .iter()
                .filter(|frame| frame.three_view_tracks < profile.min_three_view_floor)
                .count(),
        )
        .expect("usize always fits u64"),
        below_min_three_view_ratio: u64::try_from(
            frames
                .iter()
                .filter(|frame| {
                    frame.inliers == 0
                        || frame.three_view_tracks as f64 / (frame.inliers as f64)
                            < profile.min_three_view_ratio
                })
                .count(),
        )
        .expect("usize always fits u64"),
        confirmed_scene_changes: u64::try_from(
            frames
                .iter()
                .filter(|frame| frame.confirmed_scene_change)
                .count(),
        )
        .expect("usize always fits u64"),
        median_inliers: median_proxy_metric(frames.iter().map(|frame| frame.inliers as f64)),
        median_textured_cells: median_proxy_metric(
            frames.iter().map(|frame| frame.textured_cells as f64),
        ),
        median_matched_cells: median_proxy_metric(
            frames.iter().map(|frame| frame.matched_cells as f64),
        ),
        median_grid_coverage: median_proxy_metric(frames.iter().map(|frame| frame.grid_coverage)),
        median_three_view_tracks: median_proxy_metric(
            frames.iter().map(|frame| frame.three_view_tracks as f64),
        ),
    }
}

fn median_proxy_metric(values: impl Iterator<Item = f64>) -> f64 {
    let mut values = values.filter(|value| value.is_finite()).collect::<Vec<_>>();
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let middle = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

/// Persist the registration evidence from the isolated near-budget attempt so
/// the following bridge-repair step can be auditable and source-PTS aware.
async fn write_near_budget_validation_diagnostics(
    logs: &Path,
    model: &Path,
    selected: &[SelectedSourceFrame],
    proxy: &[ProxyFrame],
) -> Result<u64> {
    let registered = ReconstructionValidator::registered_images(model)?
        .into_iter()
        .map(|image| image.name.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let selected_log = AdaptiveSelectedFramesLog {
        frames: selected
            .iter()
            .enumerate()
            .map(|(index, frame)| AdaptiveSelectedFrameLog {
                output_file: format!("frame_{:06}.jpg", index + 1),
                source_index: frame.source_index,
                pts_seconds: frame.pts_seconds,
            })
            .collect(),
    };
    let timeline_frames = selected_log
        .frames
        .iter()
        .map(|frame| RegisteredFrameTimelineEntry {
            output_file: frame.output_file.clone(),
            pts_seconds: frame.pts_seconds,
            registered: registered.contains(&frame.output_file.to_ascii_lowercase()),
        })
        .collect::<Vec<_>>();
    let weak_intervals = detect_weak_intervals(&timeline_frames);
    let timeline = RegisteredFrameTimeline {
        selected_frames: timeline_frames.len() as u64,
        registered_frames: timeline_frames
            .iter()
            .filter(|frame| frame.registered)
            .count() as u64,
        frames: timeline_frames,
        weak_intervals: weak_intervals.clone(),
    };
    tokio::fs::write(
        logs.join("adaptive-near-budget-registered-frames.json"),
        serde_json::to_vec_pretty(&timeline)?,
    )
    .await?;
    let plan = plan_adaptive_bridges(proxy, &selected_log.frames, &weak_intervals);
    let planned = plan.planned_frames.len() as u64;
    tokio::fs::write(
        logs.join("adaptive-near-budget-bridge-plan.json"),
        serde_json::to_vec_pretty(&plan)?,
    )
    .await?;
    Ok(planned)
}

/// Adds PTS-preserving medium-density anchors from the already decoded proxy
/// map. This is a bounded escalation after targeted bridges fail; it avoids
/// inventing timestamps from average FPS and remains cheaper than 4 FPS fixed.
fn densify_with_proxy_anchors(
    selected: &[SelectedSourceFrame],
    proxy: &[ProxyFrame],
    spacing_seconds: f64,
) -> Vec<SelectedSourceFrame> {
    let mut result = selected.to_vec();
    let start = proxy.first().map_or(0.0, |frame| frame.pts_seconds);
    let end = proxy.last().map_or(start, |frame| frame.pts_seconds);
    let mut target = start;
    while target <= end + spacing_seconds * 0.25 {
        if let Some(frame) = proxy.iter().min_by(|left, right| {
            (left.pts_seconds - target)
                .abs()
                .partial_cmp(&(right.pts_seconds - target).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        }) {
            if !result
                .iter()
                .any(|existing| existing.source_index == frame.source_index)
            {
                result.push(SelectedSourceFrame {
                    source_index: frame.source_index,
                    pts_seconds: frame.pts_seconds,
                    reason: SelectionReason::Bridge,
                    motion: frame.background_motion,
                    inliers: frame.inliers,
                    grid_coverage: frame.grid_coverage,
                    sharpness: frame.sharpness,
                });
            }
        }
        target += spacing_seconds;
    }
    result.sort_by_key(|frame| frame.source_index);
    result
}

async fn read_adaptive_bridge_plan(logs: &Path) -> Result<AdaptiveBridgePlan> {
    let bytes = tokio::fs::read(logs.join("adaptive-bridge-plan.json")).await?;
    serde_json::from_slice(&bytes).map_err(Into::into)
}

/// The post-Mapper bridge attempt reloads the exact proxy order rather than
/// deriving frames from average FPS, so it can use the same direct extractor
/// and source-index verification as the original adaptive attempt.
async fn read_adaptive_proxy_source_samples(logs: &Path) -> Result<Vec<SourceFrameTimestamp>> {
    let proxy: AdaptiveProxyAnalysisLog =
        serde_json::from_slice(&tokio::fs::read(logs.join("adaptive-proxy-analysis.json")).await?)?;
    if proxy.frames.is_empty() {
        return Err(SplatError::Process("自适应代理候选映射为空".into()));
    }
    Ok(proxy
        .frames
        .into_iter()
        .map(|frame| SourceFrameTimestamp {
            source_index: frame.source_index,
            pts_seconds: frame.pts_seconds,
        })
        .collect())
}

async fn read_near_budget_bridge_plan(logs: &Path) -> Result<AdaptiveBridgePlan> {
    let bytes = tokio::fs::read(logs.join("adaptive-near-budget-bridge-plan.json")).await?;
    serde_json::from_slice(&bytes).map_err(Into::into)
}

/// Adds the production mapper outcome to the report that was created before
/// formal extraction. Missing reports are normal for fixed/FPS-only runs.
async fn append_final_adaptive_attempt(
    logs: &Path,
    report: &ReconstructionReport,
    mapper_backend: &str,
) -> Result<()> {
    let path = logs.join("adaptive-attempts.json");
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut document: serde_json::Value = serde_json::from_slice(&bytes)?;
    let attempt = serde_json::json!({
        "name": "finalMapper",
        "inputFrames": report.input_images,
        "registeredImages": report.registered_images,
        "registeredRatio": report.registered_ratio,
        "accepted": report.quality != ReconstructionQuality::Failed,
        "detail": format!("正式 {mapper_backend} Mapper 结果；三维点 {}", report.points_3d),
    });
    document["attempts"]
        .as_array_mut()
        .map(|items| items.push(attempt));
    tokio::fs::write(path, serde_json::to_vec_pretty(&document)?).await?;
    Ok(())
}

async fn combined_bridge_selection(
    logs: &Path,
    plan: &AdaptiveBridgePlan,
) -> Result<Vec<SelectedSourceFrame>> {
    let selected: AdaptiveSelectedFramesLog = serde_json::from_slice(
        &tokio::fs::read(logs.join("adaptive-selected-frames.json")).await?,
    )?;
    let proxy: AdaptiveProxyAnalysisLog =
        serde_json::from_slice(&tokio::fs::read(logs.join("adaptive-proxy-analysis.json")).await?)?;
    let mut frames = selected
        .frames
        .into_iter()
        .map(|frame| {
            let source = proxy
                .frames
                .iter()
                .find(|candidate| candidate.source_index == frame.source_index);
            SelectedSourceFrame {
                source_index: frame.source_index,
                pts_seconds: frame.pts_seconds,
                reason: SelectionReason::MotionTarget,
                motion: source.map_or(0.0, |value| value.background_motion),
                inliers: source.map_or(0, |value| value.inliers),
                grid_coverage: source.map_or(0.0, |value| value.grid_coverage),
                sharpness: source.map_or(0.0, |value| value.sharpness),
            }
        })
        .collect::<Vec<_>>();
    for bridge in &plan.planned_frames {
        if !frames
            .iter()
            .any(|frame| frame.source_index == bridge.source_index)
        {
            frames.push(SelectedSourceFrame {
                source_index: bridge.source_index,
                pts_seconds: bridge.pts_seconds,
                reason: SelectionReason::Bridge,
                motion: 0.0,
                inliers: bridge.inliers,
                grid_coverage: bridge.grid_coverage,
                sharpness: bridge.sharpness,
            });
        }
    }
    frames.sort_by_key(|frame| frame.source_index);
    Ok(frames)
}

fn accepts_bridge_attempt(
    original: &ReconstructionReport,
    candidate: &ReconstructionReport,
) -> bool {
    candidate.quality != ReconstructionQuality::Failed
        && candidate.registered_ratio >= 0.80
        && candidate.registered_images > original.registered_images
}

fn promote_supplemented_frames(canonical: &Path, supplemented: &Path, backup: &Path) -> Result<()> {
    if backup.exists() {
        return Err(SplatError::Process(format!(
            "补帧备份目录已存在：{}",
            backup.display()
        )));
    }
    std::fs::rename(canonical, backup)
        .map_err(|error| SplatError::Process(format!("无法保留原关键帧：{error}")))?;
    std::fs::rename(supplemented, canonical)
        .map_err(|error| SplatError::Process(format!("无法提升补帧关键帧：{error}")))?;
    Ok(())
}

async fn write_adaptive_selection_manifest(
    logs: &Path,
    source: &Path,
    frames: &[SelectedSourceFrame],
) -> Result<()> {
    let entries = frames.iter().enumerate().map(|(index, frame)| serde_json::json!({
        "outputFile": format!("frame_{:06}.jpg", index + 1), "sourceIndex": frame.source_index,
        "ptsSeconds": frame.pts_seconds, "reason": frame.reason, "motion": frame.motion,
        "inliers": frame.inliers, "gridCoverage": frame.grid_coverage, "sharpness": frame.sharpness,
    })).collect::<Vec<_>>();
    tokio::fs::write(
        logs.join("adaptive-selected-frames.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "strategy": "adaptiveSfmSupplemented", "sourceVideo": source, "frames": entries,
        }))?,
    )
    .await?;
    Ok(())
}

/// Write the durable join between adaptive source PTS and COLMAP registration.
/// A missing manifest is expected for the fixed-FPS fallback and older projects.
async fn write_registered_frame_timeline(
    logs: &Path,
    model: &Path,
    auto_bridge_frames: bool,
) -> Result<Option<RegistrationTimelineSummary>> {
    let manifest_path = logs.join("adaptive-selected-frames.json");
    let manifest_bytes = match tokio::fs::read(&manifest_path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let manifest: AdaptiveSelectedFramesLog = serde_json::from_slice(&manifest_bytes)?;
    let registered_names = ReconstructionValidator::registered_images(model)?
        .into_iter()
        .map(|image| image.name.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let frames = manifest
        .frames
        .iter()
        .map(|frame| {
            let registered = registered_names.contains(&frame.output_file.to_ascii_lowercase());
            RegisteredFrameTimelineEntry {
                output_file: frame.output_file.clone(),
                pts_seconds: frame.pts_seconds,
                registered,
            }
        })
        .collect::<Vec<_>>();
    let selected = u64::try_from(frames.len())
        .map_err(|_| SplatError::Process("自适应关键帧数量超过可记录范围".into()))?;
    let registered = u64::try_from(frames.iter().filter(|frame| frame.registered).count())
        .map_err(|_| SplatError::Process("已注册关键帧数量超过可记录范围".into()))?;
    let weak_intervals = detect_weak_intervals(&frames);
    let timeline = RegisteredFrameTimeline {
        selected_frames: selected,
        registered_frames: registered,
        frames,
        weak_intervals,
    };
    tokio::fs::write(
        logs.join("adaptive-registered-frames.json"),
        serde_json::to_vec_pretty(&timeline)?,
    )
    .await?;
    let weak_intervals = u64::try_from(timeline.weak_intervals.len())
        .map_err(|_| SplatError::Process("弱区数量超过可记录范围".into()))?;
    let planned_bridge_frames = if auto_bridge_frames && !timeline.weak_intervals.is_empty() {
        write_adaptive_bridge_plan(logs, &manifest, &timeline.weak_intervals).await?
    } else {
        0
    };
    Ok(Some(RegistrationTimelineSummary {
        selected_frames: selected,
        registered_frames: registered,
        weak_intervals,
        planned_bridge_frames,
    }))
}

async fn write_adaptive_bridge_plan(
    logs: &Path,
    selected: &AdaptiveSelectedFramesLog,
    weak_intervals: &[WeakFrameInterval],
) -> Result<u64> {
    let path = logs.join("adaptive-proxy-analysis.json");
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    let proxy: AdaptiveProxyAnalysisLog = serde_json::from_slice(&bytes)?;
    let plan = plan_adaptive_bridges(&proxy.frames, &selected.frames, weak_intervals);
    let planned = u64::try_from(plan.planned_frames.len())
        .map_err(|_| SplatError::Process("桥接帧数量超过可记录范围".into()))?;
    tokio::fs::write(
        logs.join("adaptive-bridge-plan.json"),
        serde_json::to_vec_pretty(&plan)?,
    )
    .await?;
    Ok(planned)
}

/// Choose at most two high-quality unselected proxy frames per weak interval,
/// while keeping the total at or below 25% of the original adaptive selection.
fn plan_adaptive_bridges(
    proxy_frames: &[ProxyFrame],
    selected_frames: &[AdaptiveSelectedFrameLog],
    weak_intervals: &[WeakFrameInterval],
) -> AdaptiveBridgePlan {
    let max_additional_frames = u64::try_from(selected_frames.len() / 4)
        .expect("usize always fits u64 on supported platforms");
    let selected_indices = selected_frames
        .iter()
        .map(|frame| frame.source_index)
        .collect::<HashSet<_>>();
    let mut planned_frames = Vec::new();
    for (weak_interval_index, interval) in weak_intervals.iter().enumerate() {
        if planned_frames.len() >= max_additional_frames as usize {
            break;
        }
        let start = interval
            .before_anchor
            .as_ref()
            .map(|anchor| anchor.pts_seconds)
            .unwrap_or(interval.start_pts_seconds);
        let end = interval
            .after_anchor
            .as_ref()
            .map(|anchor| anchor.pts_seconds)
            .unwrap_or(interval.end_pts_seconds);
        let remaining = max_additional_frames as usize - planned_frames.len();
        let mut candidates = proxy_frames
            .iter()
            .filter(|frame| {
                !selected_indices.contains(&frame.source_index)
                    && !frame.confirmed_scene_change
                    && frame.pts_seconds >= start
                    && frame.pts_seconds <= end
                    && frame.sharpness.is_finite()
                    && frame.sharpness > 0.0
                    && frame.inliers > 0
                    && frame.grid_coverage > 0.0
                    && frame.three_view_tracks > 0
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            right
                .sharpness
                .partial_cmp(&left.sharpness)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.source_index.cmp(&right.source_index))
        });
        for candidate in candidates.into_iter().take(remaining.min(2)) {
            planned_frames.push(AdaptiveBridgeFrame {
                source_index: candidate.source_index,
                pts_seconds: candidate.pts_seconds,
                weak_interval_index: u64::try_from(weak_interval_index)
                    .expect("usize always fits u64 on supported platforms"),
                reason: "unregisteredGapBridge".to_string(),
                sharpness: candidate.sharpness,
                inliers: candidate.inliers,
                grid_coverage: candidate.grid_coverage,
            });
        }
    }
    AdaptiveBridgePlan {
        max_additional_frames,
        planned_frames,
    }
}

/// A run of unregistered selected frames is a diagnosable coverage gap. It is
/// intentionally only a diagnostic at this stage: a later quality gate decides
/// whether to insert bridge frames, request supplementary media, or accept the
/// reconstruction as-is.
fn detect_weak_intervals(frames: &[RegisteredFrameTimelineEntry]) -> Vec<WeakFrameInterval> {
    let mut intervals = Vec::new();
    let mut index = 0;
    while index < frames.len() {
        if frames[index].registered {
            index += 1;
            continue;
        }
        let start = index;
        while index < frames.len() && !frames[index].registered {
            index += 1;
        }
        let end = index - 1;
        let before_anchor = start.checked_sub(1).and_then(|anchor_index| {
            let anchor = &frames[anchor_index];
            anchor.registered.then(|| WeakIntervalAnchor {
                output_file: anchor.output_file.clone(),
                pts_seconds: anchor.pts_seconds,
            })
        });
        let after_anchor = frames.get(index).and_then(|anchor| {
            anchor.registered.then(|| WeakIntervalAnchor {
                output_file: anchor.output_file.clone(),
                pts_seconds: anchor.pts_seconds,
            })
        });
        intervals.push(WeakFrameInterval {
            reason: "unregisteredSelectedFrames",
            start_pts_seconds: frames[start].pts_seconds,
            end_pts_seconds: frames[end].pts_seconds,
            unregistered_frames: u64::try_from(end - start + 1)
                .expect("frame slice length always fits u64"),
            first_output_file: frames[start].output_file.clone(),
            last_output_file: frames[end].output_file.clone(),
            before_anchor,
            after_anchor,
        });
    }
    intervals
}

/// Promotes only a validated attempt into the stable layout consumed by
/// training. Failed attempts remain untouched under `colmap-attempts`.
async fn normalize_undistorted_sparse_layout(output: &Path) -> Result<PathBuf> {
    let sparse = output.join("sparse");
    let nested_model = sparse.join("0");
    if nested_model.is_dir() {
        return Ok(nested_model);
    }

    let required = ["cameras.bin", "images.bin", "points3D.bin"];
    if !required.iter().all(|name| sparse.join(name).is_file()) {
        return Err(SplatError::Process(
            "COLMAP 去畸变输出缺少稀疏模型文件，已停止 gsplat 训练。".into(),
        ));
    }

    tokio::fs::create_dir_all(&nested_model).await?;
    for name in [
        "cameras.bin",
        "images.bin",
        "points3D.bin",
        "frames.bin",
        "rigs.bin",
    ] {
        let source = sparse.join(name);
        if source.is_file() {
            tokio::fs::rename(&source, nested_model.join(name)).await?;
        }
    }
    Ok(nested_model)
}

fn promote_colmap_attempt(database: &Path, model: &Path, canonical_root: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(canonical_root)?;
    std::fs::copy(database, canonical_root.join("database.db"))
        .map_err(|error| SplatError::Process(format!("无法提升已验证的 COLMAP 数据库：{error}")))?;
    let target = canonical_root.join("sparse").join("0");
    if target.exists() {
        std::fs::remove_dir_all(&target).map_err(|error| {
            SplatError::Process(format!(
                "无法替换正式稀疏模型 {}：{error}",
                target.display()
            ))
        })?;
    }
    std::fs::create_dir_all(&target)?;
    for entry in std::fs::read_dir(model)? {
        let entry = entry?;
        let source = entry.path();
        if source.is_file() {
            std::fs::copy(&source, target.join(entry.file_name()))?;
        }
    }
    Ok(target)
}

async fn run_ceres_mapper(
    executable: &Path,
    database: &Path,
    images: &Path,
    sparse: &Path,
    log: PathBuf,
    manager: &ProcessManager,
    observer: Option<ProcessObserver>,
) -> Result<(PathBuf, ReconstructionReport)> {
    colmap::map(
        executable,
        database,
        images,
        sparse,
        IncrementalMapperOptions {
            ba_backend: IncrementalBaBackend::Ceres,
        },
        log,
        manager,
        observer,
    )
    .await?;
    best_sparse_model(images, sparse)
}

fn best_sparse_model(frames: &Path, sparse_root: &Path) -> Result<(PathBuf, ReconstructionReport)> {
    let mut best: Option<(PathBuf, ReconstructionReport)> = None;
    let entries = std::fs::read_dir(sparse_root)
        .map_err(|error| SplatError::Process(format!("无法列出稀疏重建目录：{error}")))?;
    for entry in entries.flatten() {
        let candidate = entry.path();
        if !candidate.is_dir() {
            continue;
        }
        match ReconstructionValidator::validate(frames, &candidate) {
            Ok(report) => {
                let replace = match &best {
                    None => true,
                    Some((_, current)) => report.registered_images > current.registered_images,
                };
                if replace {
                    best = Some((candidate, report));
                }
            }
            Err(_) => continue,
        }
    }
    best.ok_or_else(|| SplatError::Process("稀疏重建没有可用子模型".into()))
}
