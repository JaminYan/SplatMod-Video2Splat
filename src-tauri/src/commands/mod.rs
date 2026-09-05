use crate::{
    engines::{
        self, brush, ffmpeg, ffprobe::probe_video, ColmapBackend, CudaColmapFlavor, EngineKind,
        EnginePaths, EngineStatus, FfmpegHwAccel, GsplatDensificationStrategy, MapperBaMode,
        TrainingBackend,
    },
    error::{Result, SplatError},
    pipeline::runner::{PipelineResult, PipelineRunner},
    presets::{BrushTrainingPreset, GsplatSplatCap, Quality},
    project::{
        catalog::{self, AppSettings, EffectiveSettings, ProjectOverview},
        ProjectStatus,
    },
    video::{
        analyze_proxy_images, AdaptiveFrameProfile, FramePlan, FrameSelectionStrategy,
        SourceFrameTimestamp, UniformRatioFrameSelection, VideoInfo,
    },
};
use chrono::Utc;
use serde::Serialize;
use std::{
    collections::{hash_map::DefaultHasher, HashSet},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tauri::{Emitter, Manager, State};
use tokio::sync::Mutex;
use uuid::Uuid;
const SPLATCAM_PREFLIGHT_TTL: Duration = Duration::from_secs(300);

struct SplatcamPreflightCache {
    source: PathBuf,
    fingerprint: String,
    checked_at: Instant,
    report: crate::splatcam::SplatcamImportReport,
}

#[derive(Default)]
pub struct PipelineController {
    active: Mutex<Option<Arc<PipelineRunner>>>,
    splatcam_preflight: Mutex<Option<SplatcamPreflightCache>>,
}
impl PipelineController {
    /// Used by the native close handler. It deliberately checks the Rust-owned
    /// active runner rather than any possibly stale WebView state.
    pub fn cancel_for_close(&self) -> bool {
        let Ok(active) = self.active.try_lock() else {
            return true;
        };
        let Some(runner) = active.clone() else {
            return false;
        };
        tauri::async_runtime::spawn(async move {
            runner.cancel().await;
        });
        true
    }

    async fn take_splatcam_preflight(
        &self,
        source: &Path,
    ) -> Option<crate::splatcam::SplatcamImportReport> {
        let fingerprint = tokio::task::spawn_blocking({
            let source = source.to_path_buf();
            move || crate::splatcam::source_fingerprint(&source)
        })
        .await
        .ok()
        .and_then(std::result::Result::ok);
        let mut cache = self.splatcam_preflight.lock().await;
        let valid = fingerprint.as_ref().is_some_and(|fingerprint| {
            cache.as_ref().is_some_and(|entry| {
                entry.source == source
                    && entry.fingerprint == *fingerprint
                    && entry.checked_at.elapsed() <= SPLATCAM_PREFLIGHT_TTL
            })
        });
        if valid {
            cache.take().map(|entry| entry.report)
        } else {
            *cache = None;
            None
        }
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeAndPlan {
    video: VideoInfo,
    plan: FramePlan,
}
fn paths_for_app(app: &tauri::AppHandle) -> EnginePaths {
    EnginePaths::discover(app.path().resource_dir().ok().as_deref())
}
/// Probe all engines (mandatory + optional). The UI consumes both columns and shows them
/// in the Settings drawer.
#[tauri::command]
pub async fn check_engines(app: tauri::AppHandle) -> Vec<EngineStatus> {
    paths_for_app(&app).check_all().await
}
#[tauri::command]
pub async fn get_settings(app: tauri::AppHandle) -> Result<EffectiveSettings> {
    let paths = paths_for_app(&app);
    let settings = catalog::load_settings().await?;
    let statuses = paths.check_all().await;
    let cpu = statuses.iter().find(|s| s.path == paths.colmap).cloned();
    let cuda = statuses
        .iter()
        .find(|s| s.path == paths.colmap_cuda)
        .cloned();
    let caspar = statuses
        .iter()
        .find(|s| s.path == paths.colmap_caspar)
        .cloned();
    let _ = statuses; // not used directly; statuses are pulled on demand.
    let selected_backend = settings.colmap_backend;
    let gsplat_available = engines::training::gsplat_runtime_healthy(&paths.root).await;
    Ok(EffectiveSettings {
        projects_root: settings.projects_root.clone(),
        settings,
        // Preserve the persisted choice. Starting a task still performs strict
        // availability validation and gives a clear CUDA-missing error if needed.
        colmap_backend: selected_backend,
        cpu_colmap: cpu,
        cuda_colmap: cuda,
        caspar_colmap: caspar,
        gsplat_available,
    })
}
#[tauri::command]
pub async fn probe_and_plan(
    app: tauri::AppHandle,
    path: String,
    quality: Quality,
) -> std::result::Result<ProbeAndPlan, SplatError> {
    let video = probe_video(&paths_for_app(&app).ffprobe, &PathBuf::from(path), None).await?;
    let plan = UniformRatioFrameSelection.create_plan(&video, &quality.preset());
    Ok(ProbeAndPlan { video, plan })
}
#[tauri::command]
pub async fn get_project_overview(
    state: State<'_, PipelineController>,
) -> std::result::Result<ProjectOverview, SplatError> {
    let mut overview = catalog::get_overview().await?;
    if state.active.lock().await.is_none() {
        for project in &mut overview.projects {
            if project.status == ProjectStatus::Running {
                project.status = ProjectStatus::Interrupted;
            }
        }
    }
    Ok(overview)
}

/// Loads a persisted weak-interval report for display only. Supplemental-media
/// ingestion is deliberately a separate command so opening a project can never
/// restart a completed pipeline attempt.
#[tauri::command]
pub async fn get_supplement_diagnostics(
    project_id: String,
) -> std::result::Result<crate::pipeline::runner::SupplementDiagnostics, SplatError> {
    let id = Uuid::parse_str(&project_id)
        .map_err(|_| SplatError::Process("补充素材项目编号无效".into()))?;
    let overview = catalog::get_overview().await?;
    let project = overview
        .projects
        .into_iter()
        .find(|project| project.id == id)
        .ok_or_else(|| SplatError::Process("项目索引中不存在该补充素材任务".into()))?;
    if project.status != ProjectStatus::NeedsSupplement {
        return Err(SplatError::Process("该项目不处于等待补充素材状态".into()));
    }
    crate::pipeline::runner::read_supplement_diagnostics(&project.project_path).await
}

#[tauri::command]
pub async fn get_supplement_previews(
    project_id: String,
) -> std::result::Result<Vec<crate::pipeline::runner::SupplementPreview>, SplatError> {
    let id = Uuid::parse_str(&project_id)
        .map_err(|_| SplatError::Process("补充素材项目编号无效".into()))?;
    let overview = catalog::get_overview().await?;
    let project = overview
        .projects
        .into_iter()
        .find(|project| project.id == id)
        .ok_or_else(|| SplatError::Process("项目索引中不存在该补充素材任务".into()))?;
    if project.status != ProjectStatus::NeedsSupplement {
        return Err(SplatError::Process("该项目不处于等待补充素材状态".into()));
    }
    crate::pipeline::runner::read_supplement_previews(&project.project_path).await
}

#[tauri::command]
pub async fn get_supplement_original_preview(
    project_id: String,
    output_file: String,
) -> std::result::Result<String, SplatError> {
    let id = Uuid::parse_str(&project_id)
        .map_err(|_| SplatError::Process("补充素材项目编号无效".into()))?;
    let project = catalog::get_overview()
        .await?
        .projects
        .into_iter()
        .find(|project| project.id == id)
        .ok_or_else(|| SplatError::Process("项目索引中不存在该补充素材任务".into()))?;
    if project.status != ProjectStatus::NeedsSupplement {
        return Err(SplatError::Process("该项目不处于等待补充素材状态".into()));
    }
    crate::pipeline::runner::read_supplement_original_preview(&project.project_path, &output_file)
        .await
}

/// Persists a no-process preflight for an isolated `supplemented-<n>` attempt.
/// The later execution command must consume this exact plan rather than infer
/// candidate media again from a mutable UI selection.
#[tauri::command]
pub async fn prepare_supplement_reconstruction(
    project_id: String,
) -> std::result::Result<crate::pipeline::SupplementReconstructionPlan, SplatError> {
    let id = Uuid::parse_str(&project_id)
        .map_err(|_| SplatError::Process("补充素材项目编号无效".into()))?;
    let project = catalog::get_overview()
        .await?
        .projects
        .into_iter()
        .find(|project| project.id == id)
        .ok_or_else(|| SplatError::Process("项目索引中不存在该补充素材任务".into()))?;
    if project.status != ProjectStatus::NeedsSupplement {
        return Err(SplatError::Process("该项目不处于等待补充素材状态".into()));
    }
    let metadata_path = project.project_path.join("project.json");
    let mut metadata: crate::pipeline::ProjectMetadata =
        serde_json::from_slice(&tokio::fs::read(&metadata_path).await?)?;
    let approved_media = metadata
        .supplemental_media
        .iter()
        .filter(|media| {
            media.validation_status == crate::pipeline::SupplementalValidationStatus::Passed
        })
        .cloned()
        .collect::<Vec<_>>();
    if approved_media.is_empty() {
        return Err(SplatError::Process(
            "尚无通过低成本验证的候补素材，不能开始补充重建".into(),
        ));
    }
    for media in &approved_media {
        let file = tokio::fs::metadata(&media.path).await.map_err(|_| {
            SplatError::Process(format!(
                "已通过的候补素材已不存在：{}",
                media.path.display()
            ))
        })?;
        if !file.is_file() || file.len() == 0 {
            return Err(SplatError::Process(format!(
                "已通过的候补素材不可读取：{}",
                media.path.display()
            )));
        }
    }
    let mut frames = tokio::fs::read_dir(project.project_path.join("frames")).await?;
    let mut original_frame_count = 0;
    while let Some(entry) = frames.next_entry().await? {
        if entry
            .path()
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jpg"))
        {
            original_frame_count += 1;
        }
    }
    if original_frame_count == 0 {
        return Err(SplatError::Process(
            "原关键帧目录为空，不能创建补充重建计划".into(),
        ));
    }
    let attempts = project.project_path.join("work").join("colmap-attempts");
    let mut next = 1_u64;
    if let Ok(mut entries) = tokio::fs::read_dir(&attempts).await {
        while let Some(entry) = entries.next_entry().await? {
            if let Some(value) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_prefix("supplemented-"))
                .and_then(|number| number.parse::<u64>().ok())
            {
                next = next.max(value.saturating_add(1));
            }
        }
    }
    let plan = crate::pipeline::SupplementReconstructionPlan {
        attempt_id: format!("supplemented-{next}"),
        created_at: Utc::now(),
        original_frame_count,
        approved_media,
    };
    metadata.supplement_reconstruction_plan = Some(plan.clone());
    crate::project::atomic_write_json(&metadata_path, &metadata).await?;
    let state_path = project.project_path.join("state.json");
    if let Ok(bytes) = tokio::fs::read(&state_path).await {
        let mut state: crate::pipeline::PipelineStateFile = serde_json::from_slice(&bytes)?;
        state.supplement_reconstruction_plan = Some(plan.clone());
        crate::project::atomic_write_json(&state_path, &state).await?;
    }
    crate::project::atomic_write_json(
        &project
            .project_path
            .join("logs")
            .join("supplement-reconstruction-plan.json"),
        &plan,
    )
    .await?;
    Ok(plan)
}

#[tauri::command]
pub async fn start_supplement_reconstruction(
    app: tauri::AppHandle,
    state: State<'_, PipelineController>,
    project_id: String,
) -> std::result::Result<crate::pipeline::SupplementReconstructionResult, SplatError> {
    let id = Uuid::parse_str(&project_id)
        .map_err(|_| SplatError::Process("补充素材项目编号无效".into()))?;
    let project = catalog::get_overview()
        .await?
        .projects
        .into_iter()
        .find(|project| project.id == id)
        .ok_or_else(|| SplatError::Process("项目索引中不存在该补充素材任务".into()))?;
    if project.status != ProjectStatus::NeedsSupplement {
        return Err(SplatError::Process("该项目不处于等待补充素材状态".into()));
    }
    let metadata: crate::pipeline::ProjectMetadata =
        serde_json::from_slice(&tokio::fs::read(project.project_path.join("project.json")).await?)?;
    let plan = metadata
        .supplement_reconstruction_plan
        .ok_or_else(|| SplatError::Process("尚未准备补充重建计划".into()))?;
    let settings = catalog::load_settings().await?;
    let emitter = app.clone();
    let runner = Arc::new(PipelineRunner::new(
        paths_for_app(&app),
        settings.colmap_backend,
        settings.cuda_colmap_flavor,
        settings.mapper_ba_mode,
        settings.ffmpeg_hw_accel,
        settings.brush_training_preset,
        settings.gsplat_splat_cap,
        settings.gsplat_densification_strategy,
        settings.multi_view_densification_gate,
        settings.floater_pruning,
        settings.photometric_mode,
        settings.training_backend,
        false,
        move |event| {
            let _ = emitter.emit("pipeline-event", event);
        },
    ));
    {
        let mut active = state.active.lock().await;
        if active.is_some() {
            return Err(SplatError::Process("已有任务正在运行".into()));
        }
        *active = Some(runner.clone());
    }
    let result = runner
        .run_supplement_reconstruction(&project.project_path, &plan)
        .await;
    if let Err(error) = &result {
        let stage = if matches!(error, SplatError::Cancelled) {
            crate::pipeline::PipelineStage::Cancelled
        } else {
            crate::pipeline::PipelineStage::Failed
        };
        let _ = app.emit(
            "pipeline-event",
            crate::pipeline::PipelineEvent::mapped(stage, 1.0, error.to_string()),
        );
    }
    *state.active.lock().await = None;
    result
}

#[tauri::command]
pub async fn continue_supplement_reconstruction(
    app: tauri::AppHandle,
    state: State<'_, PipelineController>,
    project_id: String,
) -> std::result::Result<PipelineResult, SplatError> {
    let id = Uuid::parse_str(&project_id)
        .map_err(|_| SplatError::Process("补充素材项目编号无效".into()))?;
    let project = catalog::get_overview()
        .await?
        .projects
        .into_iter()
        .find(|project| project.id == id)
        .ok_or_else(|| SplatError::Process("项目索引中不存在该补充素材任务".into()))?;
    if project.status != ProjectStatus::NeedsSupplement {
        return Err(SplatError::Process("该项目不处于等待补充素材状态".into()));
    }
    let metadata: crate::pipeline::ProjectMetadata =
        serde_json::from_slice(&tokio::fs::read(project.project_path.join("project.json")).await?)?;
    if metadata.supplement_reconstruction_plan.is_none() {
        return Err(SplatError::Process("尚未准备补充重建计划".into()));
    }
    let settings = catalog::load_settings().await?;
    let emitter = app.clone();
    let runner = Arc::new(PipelineRunner::new(
        paths_for_app(&app),
        settings.colmap_backend,
        settings.cuda_colmap_flavor,
        settings.mapper_ba_mode,
        settings.ffmpeg_hw_accel,
        metadata.brush_training_preset,
        metadata.gsplat_splat_cap,
        metadata.gsplat_densification_strategy,
        settings.multi_view_densification_gate,
        settings.floater_pruning,
        metadata.photometric_mode,
        metadata.training_backend,
        true,
        move |event| {
            let _ = emitter.emit("pipeline-event", event);
        },
    ));
    {
        let mut active = state.active.lock().await;
        if active.is_some() {
            return Err(SplatError::Process("已有任务正在运行".into()));
        }
        *active = Some(runner.clone());
    }
    let result = runner
        .continue_supplement_reconstruction(&project.project_path)
        .await;
    if let Err(error) = &result {
        let stage = if matches!(error, SplatError::Cancelled) {
            crate::pipeline::PipelineStage::Cancelled
        } else {
            crate::pipeline::PipelineStage::Failed
        };
        let _ = app.emit(
            "pipeline-event",
            crate::pipeline::PipelineEvent::mapped(stage, 1.0, error.to_string()),
        );
    }
    *state.active.lock().await = None;
    result
}

/// Binds one user-selected video or photo to a weak interval. This intentionally
/// only records a validated external path; the next validation stage is the
/// first operation allowed to decode the supplemental media.
#[tauri::command]
pub async fn attach_supplemental_media(
    project_id: String,
    weak_interval_index: u64,
    path: String,
) -> std::result::Result<crate::pipeline::runner::SupplementDiagnostics, SplatError> {
    let id = Uuid::parse_str(&project_id)
        .map_err(|_| SplatError::Process("补充素材项目编号无效".into()))?;
    let overview = catalog::get_overview().await?;
    let project = overview
        .projects
        .into_iter()
        .find(|project| project.id == id)
        .ok_or_else(|| SplatError::Process("项目索引中不存在该补充素材任务".into()))?;
    if project.status != ProjectStatus::NeedsSupplement {
        return Err(SplatError::Process("该项目不处于等待补充素材状态".into()));
    }
    let diagnostics =
        crate::pipeline::runner::read_supplement_diagnostics(&project.project_path).await?;
    if usize::try_from(weak_interval_index)
        .ok()
        .map_or(true, |index| index >= diagnostics.weak_intervals.len())
    {
        return Err(SplatError::Process("补充素材未绑定到有效弱区".into()));
    }
    let input = PathBuf::from(path);
    let metadata = tokio::fs::metadata(&input)
        .await
        .map_err(|_| SplatError::Process("补充素材文件不存在或无法读取".into()))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(SplatError::Process("补充素材必须是非空的普通文件".into()));
    }
    let extension = input
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| SplatError::Process("补充素材缺少受支持的文件扩展名".into()))?;
    let kind = match extension.as_str() {
        "mp4" | "mov" => crate::pipeline::SupplementalMediaKind::Video,
        "jpg" | "jpeg" | "png" => crate::pipeline::SupplementalMediaKind::Photo,
        _ => {
            return Err(SplatError::Process(
                "补充素材仅支持 MP4、MOV、JPG、JPEG 或 PNG".into(),
            ))
        }
    };
    let canonical = tokio::task::spawn_blocking(move || input.canonicalize())
        .await
        .map_err(|error| SplatError::Process(format!("无法解析补充素材路径：{error}")))??;
    let metadata_path = project.project_path.join("project.json");
    let mut project_metadata: crate::pipeline::ProjectMetadata =
        serde_json::from_slice(&tokio::fs::read(&metadata_path).await?)?;
    project_metadata.supplemental_media.retain(|media| {
        !(media.weak_interval_index == weak_interval_index && media.path == canonical)
    });
    project_metadata
        .supplemental_media
        .push(crate::pipeline::SupplementalMedia {
            path: canonical,
            kind,
            weak_interval_index,
            validation_status: crate::pipeline::SupplementalValidationStatus::Pending,
            validation_reason: None,
        });
    project_metadata.supplement_reconstruction_plan = None;
    crate::project::atomic_write_json(&metadata_path, &project_metadata).await?;
    let state_path = project.project_path.join("state.json");
    if let Ok(bytes) = tokio::fs::read(&state_path).await {
        let mut state: crate::pipeline::PipelineStateFile = serde_json::from_slice(&bytes)?;
        state.supplemental_media = project_metadata.supplemental_media.clone();
        state.supplement_reconstruction_plan = None;
        crate::project::atomic_write_json(&state_path, &state).await?;
    }
    crate::pipeline::runner::read_supplement_diagnostics(&project.project_path).await
}

/// Binds a selection of videos/photos to the same weak interval. Every file is
/// validated before project metadata is changed, so a bad selection cannot
/// leave a partial set of candidate media behind.
#[tauri::command]
pub async fn attach_supplemental_media_batch(
    project_id: String,
    weak_interval_index: u64,
    paths: Vec<String>,
) -> std::result::Result<crate::pipeline::runner::SupplementDiagnostics, SplatError> {
    if paths.is_empty() {
        return Err(SplatError::Process("未选择补充素材文件".into()));
    }
    let id = Uuid::parse_str(&project_id)
        .map_err(|_| SplatError::Process("补充素材项目编号无效".into()))?;
    let overview = catalog::get_overview().await?;
    let project = overview
        .projects
        .into_iter()
        .find(|project| project.id == id)
        .ok_or_else(|| SplatError::Process("项目索引中不存在该补充素材任务".into()))?;
    if project.status != ProjectStatus::NeedsSupplement {
        return Err(SplatError::Process("该项目不处于等待补充素材状态".into()));
    }
    let diagnostics =
        crate::pipeline::runner::read_supplement_diagnostics(&project.project_path).await?;
    if usize::try_from(weak_interval_index)
        .ok()
        .map_or(true, |index| index >= diagnostics.weak_intervals.len())
    {
        return Err(SplatError::Process("补充素材未绑定到有效弱区".into()));
    }

    let mut incoming = Vec::with_capacity(paths.len());
    let mut seen_paths = HashSet::new();
    for path in paths {
        let input = PathBuf::from(path);
        let file_metadata = tokio::fs::metadata(&input).await.map_err(|_| {
            SplatError::Process(format!("补充素材文件不存在或无法读取：{}", input.display()))
        })?;
        if !file_metadata.is_file() || file_metadata.len() == 0 {
            return Err(SplatError::Process(format!(
                "补充素材必须是非空的普通文件：{}",
                input.display()
            )));
        }
        let extension = input
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .ok_or_else(|| {
                SplatError::Process(format!(
                    "补充素材缺少受支持的文件扩展名：{}",
                    input.display()
                ))
            })?;
        let kind = match extension.as_str() {
            "mp4" | "mov" => crate::pipeline::SupplementalMediaKind::Video,
            "jpg" | "jpeg" | "png" => crate::pipeline::SupplementalMediaKind::Photo,
            _ => {
                return Err(SplatError::Process(format!(
                    "补充素材仅支持 MP4、MOV、JPG、JPEG 或 PNG：{}",
                    input.display()
                )))
            }
        };
        let canonical = tokio::task::spawn_blocking(move || input.canonicalize())
            .await
            .map_err(|error| SplatError::Process(format!("无法解析补充素材路径：{error}")))??;
        if seen_paths.insert(canonical.clone()) {
            incoming.push(crate::pipeline::SupplementalMedia {
                path: canonical,
                kind,
                weak_interval_index,
                validation_status: crate::pipeline::SupplementalValidationStatus::Pending,
                validation_reason: None,
            });
        }
    }
    if incoming.is_empty() {
        return Err(SplatError::Process("未选择有效的补充素材文件".into()));
    }

    let metadata_path = project.project_path.join("project.json");
    let mut project_metadata: crate::pipeline::ProjectMetadata =
        serde_json::from_slice(&tokio::fs::read(&metadata_path).await?)?;
    for media in &incoming {
        project_metadata.supplemental_media.retain(|existing| {
            !(existing.weak_interval_index == weak_interval_index && existing.path == media.path)
        });
    }
    project_metadata.supplemental_media.extend(incoming);
    project_metadata.supplement_reconstruction_plan = None;
    crate::project::atomic_write_json(&metadata_path, &project_metadata).await?;
    let state_path = project.project_path.join("state.json");
    if let Ok(bytes) = tokio::fs::read(&state_path).await {
        let mut state: crate::pipeline::PipelineStateFile = serde_json::from_slice(&bytes)?;
        state.supplemental_media = project_metadata.supplemental_media.clone();
        state.supplement_reconstruction_plan = None;
        crate::project::atomic_write_json(&state_path, &state).await?;
    }
    crate::pipeline::runner::read_supplement_diagnostics(&project.project_path).await
}

/// Removes only the project's supplemental-media reference. The user's source
/// video/photo is never deleted or moved.
#[tauri::command]
pub async fn detach_supplemental_media(
    project_id: String,
    weak_interval_index: u64,
    path: String,
) -> std::result::Result<crate::pipeline::runner::SupplementDiagnostics, SplatError> {
    let id = Uuid::parse_str(&project_id)
        .map_err(|_| SplatError::Process("补充素材项目编号无效".into()))?;
    let overview = catalog::get_overview().await?;
    let project = overview
        .projects
        .into_iter()
        .find(|project| project.id == id)
        .ok_or_else(|| SplatError::Process("项目索引中不存在该补充素材任务".into()))?;
    if project.status != ProjectStatus::NeedsSupplement {
        return Err(SplatError::Process("该项目不处于等待补充素材状态".into()));
    }
    let metadata_path = project.project_path.join("project.json");
    let mut project_metadata: crate::pipeline::ProjectMetadata =
        serde_json::from_slice(&tokio::fs::read(&metadata_path).await?)?;
    let requested_path = PathBuf::from(path);
    let legacy_ordinal = project_metadata
        .supplemental_media
        .iter()
        .filter(|media| media.weak_interval_index == weak_interval_index)
        .position(|media| media.path == requested_path);
    let count_before = project_metadata.supplemental_media.len();
    project_metadata.supplemental_media.retain(|media| {
        !(media.weak_interval_index == weak_interval_index && media.path == requested_path)
    });
    if project_metadata.supplemental_media.len() == count_before {
        return Err(SplatError::Process("该候补素材绑定不存在或已被移除".into()));
    }
    project_metadata.supplement_reconstruction_plan = None;
    clear_validation_cache(&project.project_path, &requested_path, legacy_ordinal).await?;
    crate::project::atomic_write_json(&metadata_path, &project_metadata).await?;
    let state_path = project.project_path.join("state.json");
    if let Ok(bytes) = tokio::fs::read(&state_path).await {
        let mut state: crate::pipeline::PipelineStateFile = serde_json::from_slice(&bytes)?;
        state.supplemental_media = project_metadata.supplemental_media.clone();
        state.supplement_reconstruction_plan = None;
        crate::project::atomic_write_json(&state_path, &state).await?;
    }
    crate::pipeline::runner::read_supplement_diagnostics(&project.project_path).await
}

/// Performs only a bounded proxy/geometry check. It never starts COLMAP and
/// never changes the original reconstruction attempt.
#[tauri::command]
pub async fn validate_supplemental_media(
    app: tauri::AppHandle,
    project_id: String,
    weak_interval_index: u64,
) -> std::result::Result<crate::pipeline::runner::SupplementDiagnostics, SplatError> {
    let id = Uuid::parse_str(&project_id)
        .map_err(|_| SplatError::Process("补充素材项目编号无效".into()))?;
    let overview = catalog::get_overview().await?;
    let project = overview
        .projects
        .into_iter()
        .find(|item| item.id == id)
        .ok_or_else(|| SplatError::Process("项目索引中不存在该补充素材任务".into()))?;
    if project.status != ProjectStatus::NeedsSupplement {
        return Err(SplatError::Process("该项目不处于等待补充素材状态".into()));
    }
    let diagnostics =
        crate::pipeline::runner::read_supplement_diagnostics(&project.project_path).await?;
    let interval = diagnostics
        .weak_intervals
        .get(
            usize::try_from(weak_interval_index)
                .map_err(|_| SplatError::Process("补充素材弱区编号无效".into()))?,
        )
        .ok_or_else(|| SplatError::Process("补充素材未绑定到有效弱区".into()))?;
    let metadata_path = project.project_path.join("project.json");
    let mut metadata: crate::pipeline::ProjectMetadata =
        serde_json::from_slice(&tokio::fs::read(&metadata_path).await?)?;
    let targets = metadata
        .supplemental_media
        .iter()
        .enumerate()
        .filter(|(_, media)| media.weak_interval_index == weak_interval_index)
        .map(|(index, media)| (index, media.path.clone(), media.kind))
        .collect::<Vec<_>>();
    if targets.is_empty() {
        return Err(SplatError::Process("该弱区尚未绑定候补素材".into()));
    }
    let before = interval
        .before_anchor
        .as_ref()
        .ok_or_else(|| SplatError::Process("该弱区位于视频开头，缺少可用于验证的前锚点".into()))?;
    let reference_before = read_validation_image(
        &project
            .project_path
            .join("frames")
            .join(&before.output_file),
    )
    .await?;
    let reference_after_path = interval
        .after_anchor
        .as_ref()
        .map(|anchor| &anchor.output_file)
        .unwrap_or(&interval.first_output_file);
    let reference_after = read_validation_image(
        &project
            .project_path
            .join("frames")
            .join(reference_after_path),
    )
    .await?;
    let profile = AdaptiveFrameProfile::for_quality(metadata.quality, 30.0)
        .ok_or_else(|| SplatError::Process("快速档不支持 SfM 补充素材几何验证".into()))?;
    let engines = paths_for_app(&app);
    let manager = crate::process::ProcessManager::new();
    let mut reports = Vec::with_capacity(targets.len());
    for (ordinal, (metadata_index, path, kind)) in targets.into_iter().enumerate() {
        let images = match validation_candidate_images(
            &engines,
            &manager,
            &project.project_path,
            ordinal,
            &path,
            kind,
        )
        .await
        {
            Ok(images) => images,
            Err(error) => {
                metadata.supplemental_media[metadata_index].validation_status =
                    crate::pipeline::SupplementalValidationStatus::Failed;
                metadata.supplemental_media[metadata_index].validation_reason =
                    Some(format!("无法读取候补素材：{error}"));
                reports.push(serde_json::json!({"path": path, "status": "failed", "reason": metadata.supplemental_media[metadata_index].validation_reason}));
                continue;
            }
        };
        let mut best: Option<(u32, f64, u32, String)> = None;
        for image in images {
            let samples = [
                SourceFrameTimestamp {
                    source_index: 0,
                    pts_seconds: 0.0,
                },
                SourceFrameTimestamp {
                    source_index: 1,
                    pts_seconds: 1.0,
                },
                SourceFrameTimestamp {
                    source_index: 2,
                    pts_seconds: 2.0,
                },
            ];
            let analysis = analyze_proxy_images(
                &samples,
                &[reference_before.clone(), image, reference_after.clone()],
            )?;
            let left = &analysis[1];
            let right = &analysis[2];
            let inliers = left.inliers.min(right.inliers);
            let coverage = left.grid_coverage.min(right.grid_coverage);
            let tracks = right.three_view_tracks;
            let reason = if left.textured_cells < profile.min_textured_cells
                || right.textured_cells < profile.min_textured_cells
            {
                "候补画面清晰度或纹理不足".to_string()
            } else if left.matched_cells < profile.min_matched_cells
                || right.matched_cells < profile.min_matched_cells
                || inliers < profile.min_inliers_floor
            {
                "与原场景重叠不足".to_string()
            } else if tracks < profile.min_three_view_floor {
                "视差不足，未形成稳定三视图连通".to_string()
            } else {
                "已通过低成本重叠与视差验证".to_string()
            };
            let candidate = (inliers, coverage, tracks, reason);
            if best.as_ref().map_or(true, |current| {
                (candidate.0, candidate.2, candidate.1) > (current.0, current.2, current.1)
            }) {
                best = Some(candidate);
            }
        }
        let (inliers, coverage, tracks, reason) =
            best.ok_or_else(|| SplatError::Process("候补视频未能生成代理画面".into()))?;
        let passed = reason == "已通过低成本重叠与视差验证";
        metadata.supplemental_media[metadata_index].validation_status = if passed {
            crate::pipeline::SupplementalValidationStatus::Passed
        } else {
            crate::pipeline::SupplementalValidationStatus::Failed
        };
        metadata.supplemental_media[metadata_index].validation_reason = Some(reason.clone());
        reports.push(serde_json::json!({"path": path, "status": if passed { "passed" } else { "failed" }, "reason": reason, "inliers": inliers, "gridCoverage": coverage, "threeViewTracks": tracks}));
    }
    metadata.supplement_reconstruction_plan = None;
    crate::project::atomic_write_json(&metadata_path, &metadata).await?;
    let state_path = project.project_path.join("state.json");
    if let Ok(bytes) = tokio::fs::read(&state_path).await {
        let mut state: crate::pipeline::PipelineStateFile = serde_json::from_slice(&bytes)?;
        state.supplemental_media = metadata.supplemental_media.clone();
        state.supplement_reconstruction_plan = None;
        crate::project::atomic_write_json(&state_path, &state).await?;
    }
    let log_path = project.project_path.join("logs").join(format!(
        "supplement-validation-weak-{}.json",
        weak_interval_index + 1
    ));
    crate::project::atomic_write_json(&log_path, &serde_json::json!({"weakIntervalIndex": weak_interval_index, "afterAnchorAvailable": interval.after_anchor.is_some(), "results": reports})).await?;
    crate::pipeline::runner::read_supplement_diagnostics(&project.project_path).await
}

async fn read_validation_image(
    path: &Path,
) -> std::result::Result<image::DynamicImage, SplatError> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        image::ImageReader::open(&path)
            .map_err(|error| {
                SplatError::Process(format!("无法打开验证图像 {}：{error}", path.display()))
            })?
            .decode()
            .map_err(|error| {
                SplatError::Process(format!("无法解码验证图像 {}：{error}", path.display()))
            })
    })
    .await
    .map_err(|error| SplatError::Process(format!("读取验证图像任务异常结束：{error}")))?
}

async fn validation_candidate_images(
    engines: &EnginePaths,
    manager: &crate::process::ProcessManager,
    project: &Path,
    ordinal: usize,
    path: &Path,
    kind: crate::pipeline::SupplementalMediaKind,
) -> std::result::Result<Vec<image::DynamicImage>, SplatError> {
    if kind == crate::pipeline::SupplementalMediaKind::Photo {
        return Ok(vec![read_validation_image(path).await?]);
    }
    let video = probe_video(
        &engines.ffprobe,
        path,
        Some(project.join("logs").join("supplement-validation.log")),
    )
    .await?;
    let sample_fps = (3.0 / video.duration.max(3.0)).clamp(0.1, 1.0);
    let output = validation_cache_path(project, path);
    clear_validation_cache(project, path, Some(ordinal)).await?;
    let work = output.join("work");
    let report = ffmpeg::extract_proxy_frames(
        &engines.ffmpeg,
        path,
        &output,
        &work,
        video.width,
        video.height,
        sample_fps,
        Some(project.join("logs").join("supplement-validation.log")),
        manager,
        None,
    )
    .await?;
    let mut images = Vec::with_capacity(report.frames.len());
    for index in 1..=report.frames.len() {
        images.push(read_validation_image(&output.join(format!("proxy_{index:06}.jpg"))).await?);
    }
    Ok(images)
}

fn validation_cache_path(project: &Path, path: &Path) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    project
        .join("work")
        .join("supplement-validation")
        .join(format!("media-{:016x}", hasher.finish()))
}

/// Deletes only disposable, project-owned validation proxies. The legacy
/// ordinal directory is also removed while migrating projects created before
/// validation cache directories had a stable media key.
async fn clear_validation_cache(
    project: &Path,
    path: &Path,
    legacy_ordinal: Option<usize>,
) -> std::result::Result<(), SplatError> {
    let root = project.join("work").join("supplement-validation");
    let stable = validation_cache_path(project, path);
    let mut directories = vec![stable];
    if let Some(ordinal) = legacy_ordinal {
        directories.push(root.join(format!("media-{ordinal:03}")));
    }
    for directory in directories {
        if tokio::fs::try_exists(&directory).await? {
            tokio::fs::remove_dir_all(&directory)
                .await
                .map_err(|error| {
                    SplatError::Process(format!(
                        "无法清理候补素材验证缓存 {}：{error}",
                        directory.display()
                    ))
                })?;
        }
    }
    Ok(())
}
#[tauri::command]
pub async fn set_projects_root(
    projects_root: String,
) -> std::result::Result<AppSettings, SplatError> {
    catalog::save_projects_root(PathBuf::from(projects_root)).await
}
#[tauri::command]
pub async fn set_colmap_backend(
    app: tauri::AppHandle,
    backend: ColmapBackend,
) -> std::result::Result<AppSettings, SplatError> {
    let paths = paths_for_app(&app);
    let settings = catalog::load_settings().await?;
    match backend {
        ColmapBackend::Cpu => engines::require_cpu_colmap(&paths).await?,
        ColmapBackend::Cuda => {
            engines::require_cuda_colmap(&paths, settings.cuda_colmap_flavor).await?
        }
    }
    catalog::save_colmap_backend(backend).await
}
#[tauri::command]
pub async fn set_cuda_colmap_flavor(
    app: tauri::AppHandle,
    flavor: CudaColmapFlavor,
) -> std::result::Result<AppSettings, SplatError> {
    let paths = paths_for_app(&app);
    engines::require_cuda_colmap(&paths, flavor).await?;
    if flavor == CudaColmapFlavor::Caspar
        && !engines::cuda_colmap_supports_caspar(&paths, flavor).await?
    {
        return Err(SplatError::UnsupportedEngine(
            "CASPAR COLMAP 未通过健康检查；请确认 colmap-caspar\\bin\\colmap.exe 可运行。".into(),
        ));
    }
    catalog::save_cuda_colmap_flavor(flavor).await?;
    // CASPAR is an engine-level choice, not a second conflicting toggle.
    // Selecting it intentionally forces its compatible global-BA route.
    catalog::save_mapper_ba_mode(match flavor {
        CudaColmapFlavor::Caspar => MapperBaMode::Caspar,
        CudaColmapFlavor::Official => MapperBaMode::Auto,
    })
    .await
}
#[tauri::command]
pub async fn set_mapper_ba_mode(
    mode: MapperBaMode,
) -> std::result::Result<AppSettings, SplatError> {
    catalog::save_mapper_ba_mode(mode).await
}
#[tauri::command]
pub async fn set_ffmpeg_hw_accel(
    app: tauri::AppHandle,
    mode: FfmpegHwAccel,
) -> std::result::Result<AppSettings, SplatError> {
    // Settings can be changed even if no pipeline is running, but we still
    // verify the bundled FFmpeg is present so a broken install doesn't
    // silently accept settings the user can't actually use.
    let paths = paths_for_app(&app);
    let ffmpeg_status =
        engines::check_basic(EngineKind::Ffmpeg, &paths.ffmpeg, &["-version"]).await;
    if !ffmpeg_status.exists {
        return Err(SplatError::EngineMissing(
            ffmpeg_status.path.display().to_string(),
        ));
    }
    catalog::save_ffmpeg_hw_accel(mode).await
}
/// Trigger the on-demand download of the CUDA COLMAP archive. UI calls this when the user
/// toggles the switch and no CUDA binary exists yet.
#[tauri::command]
pub async fn download_colmap_cuda(app: tauri::AppHandle) -> Result<EngineStatus> {
    let paths = paths_for_app(&app);
    let statuses = paths.check_all().await;
    let current = statuses
        .into_iter()
        .find(|s| s.path == paths.colmap_cuda)
        .ok_or_else(|| SplatError::Process("CUDA COLMAP 状态查询失败".into()))?;
    if current.exists && current.cuda_available == Some(true) && current.can_start {
        return Ok(current);
    }
    Err(SplatError::Process(
        "CUDA COLMAP 尚未下载。请在终端运行 `npm run download:colmap-cuda`，或在设置抽屉中点击“下载 CUDA 版 COLMAP”。".into(),
    ))
}

/// Performs the read-only Splatcam preflight. It deliberately does not create a project,
/// convert a COLMAP model, or start any video/COLMAP process.
#[tauri::command]
pub async fn inspect_splatcam_import(
    state: State<'_, PipelineController>,
    path: String,
) -> std::result::Result<crate::splatcam::SplatcamImportReport, SplatError> {
    let source = PathBuf::from(path);
    let (fingerprint, report) = tokio::task::spawn_blocking({
        let source = source.clone();
        move || {
            let fingerprint = crate::splatcam::source_fingerprint(&source)?;
            let report = crate::splatcam::inspect_export(&source)?;
            Ok::<_, SplatError>((fingerprint, report))
        }
    })
    .await
    .map_err(|error| SplatError::Process(format!("Splatcam 导入检查任务失败：{error}")))??;
    *state.splatcam_preflight.lock().await = Some(SplatcamPreflightCache {
        source,
        fingerprint,
        checked_at: Instant::now(),
        report: report.clone(),
    });
    Ok(report)
}

#[tauri::command]
pub async fn start_pipeline(
    app: tauri::AppHandle,
    state: State<'_, PipelineController>,
    path: String,
    quality: Quality,
    projects_root: String,
    auto_bridge_frames: bool,
) -> std::result::Result<PipelineResult, SplatError> {
    let settings = catalog::load_settings().await?;
    let paths = paths_for_app(&app);
    // Refuse to start if the chosen backend isn't actually available.
    match settings.colmap_backend {
        ColmapBackend::Cpu => engines::require_cpu_colmap(&paths).await?,
        ColmapBackend::Cuda => {
            engines::require_cuda_colmap(&paths, settings.cuda_colmap_flavor).await?
        }
    }
    let emitter = app.clone();
    let runner_paths = paths.clone();
    let runner = Arc::new(PipelineRunner::new(
        runner_paths,
        settings.colmap_backend,
        settings.cuda_colmap_flavor,
        settings.mapper_ba_mode,
        settings.ffmpeg_hw_accel,
        settings.brush_training_preset,
        settings.gsplat_splat_cap,
        settings.gsplat_densification_strategy,
        settings.multi_view_densification_gate,
        settings.floater_pruning,
        settings.photometric_mode,
        settings.training_backend,
        auto_bridge_frames,
        move |event| {
            let _ = emitter.emit("pipeline-event", event);
        },
    ));
    {
        let mut active = state.active.lock().await;
        if active.is_some() {
            return Err(SplatError::Process("已有任务正在运行".into()));
        }
        *active = Some(runner.clone());
    }
    let result = runner
        .generate(
            PathBuf::from(path).as_path(),
            quality,
            PathBuf::from(projects_root).as_path(),
        )
        .await;
    if let Err(error) = &result {
        let stage = if matches!(error, SplatError::Cancelled) {
            crate::pipeline::PipelineStage::Cancelled
        } else {
            crate::pipeline::PipelineStage::Failed
        };
        let _ = app.emit(
            "pipeline-event",
            crate::pipeline::PipelineEvent::mapped(stage, 1.0, error.to_string()),
        );
    }
    *state.active.lock().await = None;
    result
}

/// Starts training from an already reconstructed Splatcam export. The runner owns the
/// source boundary and never falls back to the video/SfM pipeline.
#[tauri::command]
pub async fn start_splatcam_pipeline(
    app: tauri::AppHandle,
    state: State<'_, PipelineController>,
    path: String,
    quality: Quality,
    projects_root: String,
) -> std::result::Result<PipelineResult, SplatError> {
    let settings = catalog::load_settings().await?;
    let emitter = app.clone();
    let source = PathBuf::from(path);
    let runner = Arc::new(PipelineRunner::new(
        paths_for_app(&app),
        settings.colmap_backend,
        settings.cuda_colmap_flavor,
        settings.mapper_ba_mode,
        settings.ffmpeg_hw_accel,
        settings.brush_training_preset,
        settings.gsplat_splat_cap,
        settings.gsplat_densification_strategy,
        settings.multi_view_densification_gate,
        settings.floater_pruning,
        settings.photometric_mode,
        settings.training_backend,
        false,
        move |event| {
            let _ = emitter.emit("pipeline-event", event);
        },
    ));
    {
        let mut active = state.active.lock().await;
        if active.is_some() {
            return Err(SplatError::Process("已有任务正在运行".into()));
        }
        *active = Some(runner.clone());
    }
    let preflight = state.take_splatcam_preflight(&source).await;
    let result = runner
        .generate_splatcam(
            &source,
            quality,
            PathBuf::from(projects_root).as_path(),
            preflight,
        )
        .await;
    if let Err(error) = &result {
        let stage = if matches!(error, SplatError::Cancelled) {
            crate::pipeline::PipelineStage::Cancelled
        } else {
            crate::pipeline::PipelineStage::Failed
        };
        let _ = app.emit(
            "pipeline-event",
            crate::pipeline::PipelineEvent::mapped(stage, 1.0, error.to_string()),
        );
    }
    *state.active.lock().await = None;
    result
}

#[tauri::command]
pub async fn cancel_pipeline(state: State<'_, PipelineController>) -> Result<()> {
    let runner = state.active.lock().await.clone();
    if let Some(runner) = runner {
        runner.cancel().await;
    }
    Ok(())
}
#[tauri::command]
pub async fn delete_project(
    state: State<'_, PipelineController>,
    project_id: String,
) -> Result<()> {
    if state.active.lock().await.is_some() {
        return Err(SplatError::Process("任务运行期间不能删除项目".into()));
    }
    let id =
        Uuid::parse_str(&project_id).map_err(|_| SplatError::Process("项目 ID 无效".into()))?;
    catalog::delete_project(id).await
}
#[tauri::command]
pub async fn export_ply(source_path: String, destination_path: String) -> Result<u64> {
    let source = catalog::validate_registered_final_ply(&PathBuf::from(source_path)).await?;
    let destination = PathBuf::from(destination_path);
    if destination
        .extension()
        .is_none_or(|extension| !extension.eq_ignore_ascii_case("ply"))
    {
        return Err(SplatError::InvalidPath(destination));
    }
    Ok(tokio::fs::copy(source, destination).await?)
}

/// 启动 Brush 内置 3D 查看器加载已发布的 final.ply。
///
/// 安全前提：仅在 `catalog::validate_registered_final_ply` 通过后才允许启动；
/// `brush::open_viewer` 会再次独立校验 PLY 头，避免被伪造的 PLY 注入。
#[tauri::command]
pub async fn open_project_viewer(app: tauri::AppHandle, source_path: String) -> Result<()> {
    let source = catalog::validate_registered_final_ply(&PathBuf::from(source_path)).await?;
    let brush_exe = paths_for_app(&app).brush;
    tokio::task::spawn_blocking(move || brush::open_viewer(&brush_exe, &source))
        .await
        .map_err(|error| SplatError::Process(format!("启动 Brush 3D 查看器失败：{error}")))?
}
#[tauri::command]
pub async fn set_brush_training_preset(preset: BrushTrainingPreset) -> Result<AppSettings> {
    catalog::save_brush_training_preset(preset).await
}
#[tauri::command]
pub async fn set_gsplat_splat_cap(cap: GsplatSplatCap) -> Result<AppSettings> {
    catalog::save_gsplat_splat_cap(cap).await
}
#[tauri::command]
pub async fn set_gsplat_densification_strategy(
    strategy: GsplatDensificationStrategy,
) -> Result<AppSettings> {
    catalog::save_gsplat_densification_strategy(strategy).await
}
#[tauri::command]
pub async fn set_multi_view_densification_gate(enabled: bool) -> Result<AppSettings> {
    catalog::save_multi_view_densification_gate(enabled).await
}
#[tauri::command]
pub async fn set_floater_pruning(enabled: bool) -> Result<AppSettings> {
    catalog::save_floater_pruning(enabled).await
}
#[tauri::command]
pub async fn set_photometric_mode(
    mode: crate::engines::training::PhotometricMode,
) -> Result<AppSettings> {
    catalog::save_photometric_mode(mode).await
}
#[tauri::command]
pub async fn set_training_backend(
    app: tauri::AppHandle,
    backend: TrainingBackend,
) -> Result<AppSettings> {
    if backend == TrainingBackend::Gsplat
        && !engines::training::gsplat_runtime_healthy(&paths_for_app(&app).root).await
    {
        return Err(SplatError::UnsupportedEngine(
            "gsplat CUDA 实验运行时尚未安装或未通过健康检查；请改用 Brush。".into(),
        ));
    }
    catalog::save_training_backend(backend).await
}
