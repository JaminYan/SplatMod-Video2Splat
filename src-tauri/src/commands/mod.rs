use crate::{
    engines::{
        self, brush, ffprobe::probe_video, ColmapBackend, CudaColmapFlavor, EngineKind,
        EnginePaths, EngineStatus, FfmpegHwAccel, MapperBaMode, TrainingBackend,
    },
    error::{Result, SplatError},
    pipeline::runner::{PipelineResult, PipelineRunner},
    presets::{BrushTrainingPreset, GsplatSplatCap, Quality},
    project::{
        catalog::{self, AppSettings, EffectiveSettings, ProjectOverview},
        ProjectStatus,
    },
    video::{FramePlan, FrameSelectionStrategy, UniformRatioFrameSelection, VideoInfo},
};
use serde::Serialize;
use std::{path::PathBuf, sync::Arc};
use tauri::{Emitter, Manager, State};
use tokio::sync::Mutex;
use uuid::Uuid;
#[derive(Default)]
pub struct PipelineController {
    active: Mutex<Option<Arc<PipelineRunner>>>,
}
impl PipelineController {
    /// Used by the native close handler. It deliberately checks the Rust-owned
    /// active runner rather than any possibly stale WebView state.
    pub fn cancel_for_close(&self) -> bool {
        let Ok(active) = self.active.try_lock() else { return true; };
        let Some(runner) = active.clone() else { return false; };
        tauri::async_runtime::spawn(async move { runner.cancel().await; });
        true
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
    path: String,
) -> std::result::Result<crate::splatcam::SplatcamImportReport, SplatError> {
    let source = PathBuf::from(path);
    tokio::task::spawn_blocking(move || crate::splatcam::inspect_export(&source))
        .await
        .map_err(|error| SplatError::Process(format!("Splatcam 导入检查任务失败：{error}")))?
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
pub async fn set_photometric_mode(mode: crate::engines::training::PhotometricMode) -> Result<AppSettings> {
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
