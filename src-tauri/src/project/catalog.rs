use crate::{
    engines::{
        ColmapBackend, CudaColmapFlavor, EngineStatus, FfmpegHwAccel, MapperBaMode,
        GsplatDensificationStrategy, PhotometricMode, TrainingBackend,
    },
    error::{Result, SplatError},
    presets::{BrushTrainingPreset, GsplatSplatCap},
    project::{manager::atomic_write_json, ProjectMetadata, ProjectStatus, PROJECT_APP_ID},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
use uuid::Uuid;
const CURRENT_SETTINGS_SCHEMA: u32 = 11;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub schema_version: u32,
    pub projects_root: PathBuf,
    /// Default backend; the user can flip this from the Settings drawer.
    #[serde(default)]
    pub colmap_backend: ColmapBackend,
    /// CUDA builds are installed side-by-side so switching never overwrites the official engine.
    #[serde(default)]
    pub cuda_colmap_flavor: CudaColmapFlavor,
    #[serde(default)]
    pub mapper_ba_mode: MapperBaMode,
    /// FFmpeg hardware acceleration mode used during uniform frame extraction.
    #[serde(default)]
    pub ffmpeg_hw_accel: FfmpegHwAccel,
    #[serde(default)]
    pub brush_training_preset: BrushTrainingPreset,
    #[serde(default)]
    pub training_backend: TrainingBackend,
    #[serde(default)]
    pub gsplat_splat_cap: GsplatSplatCap,
    #[serde(default)]
    pub gsplat_densification_strategy: GsplatDensificationStrategy,
    #[serde(default)]
    pub multi_view_densification_gate: bool,
    #[serde(default)]
    pub floater_pruning: bool,
    #[serde(default)]
    pub photometric_mode: PhotometricMode,
}
impl Default for AppSettings {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SETTINGS_SCHEMA,
            projects_root: default_projects_root().unwrap_or_else(|_| PathBuf::from(".")),
            colmap_backend: ColmapBackend::Cuda,
            cuda_colmap_flavor: CudaColmapFlavor::Official,
            mapper_ba_mode: MapperBaMode::Auto,
            ffmpeg_hw_accel: FfmpegHwAccel::Off,
            brush_training_preset: BrushTrainingPreset::A,
            training_backend: TrainingBackend::Brush,
            gsplat_splat_cap: GsplatSplatCap::Auto,
            gsplat_densification_strategy: GsplatDensificationStrategy::Mcmc,
            multi_view_densification_gate: false,
            floater_pruning: false,
            photometric_mode: PhotometricMode::None,
        }
    }
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveSettings {
    pub settings: AppSettings,
    pub projects_root: PathBuf,
    pub colmap_backend: ColmapBackend,
    pub cpu_colmap: Option<EngineStatus>,
    pub cuda_colmap: Option<EngineStatus>,
    pub caspar_colmap: Option<EngineStatus>,
    pub gsplat_available: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexedProject {
    id: Uuid,
    path: PathBuf,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectIndex {
    schema_version: u32,
    projects: Vec<IndexedProject>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub id: Uuid,
    pub name: String,
    pub status: ProjectStatus,
    pub project_path: PathBuf,
    pub final_ply: Option<PathBuf>,
    pub file_size: Option<u64>,
    pub splat_count: Option<u64>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u64>,
    pub quality: crate::presets::Quality,
    pub input_source: crate::pipeline::InputSource,
    pub training_backend: TrainingBackend,
    pub brush_training_preset: BrushTrainingPreset,
    pub gsplat_splat_cap: GsplatSplatCap,
    pub gsplat_densification_strategy: GsplatDensificationStrategy,
    pub photometric_mode: PhotometricMode,
    pub source_name: String,
    pub registered_ratio: Option<f64>,
    pub points_3d: Option<u64>,
    pub failure_message: Option<String>,
    pub weak_interval_count: Option<u64>,
    pub supplemental_media_count: u64,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectOverview {
    pub projects_root: PathBuf,
    pub colmap_backend: ColmapBackend,
    pub projects: Vec<ProjectSummary>,
}
fn app_data_root() -> Result<PathBuf> {
    dirs::data_local_dir()
        .map(|v| v.join("SplatStudio"))
        .ok_or_else(|| SplatError::Process("无法定位 LOCALAPPDATA 目录".into()))
}
fn settings_path() -> Result<PathBuf> {
    Ok(app_data_root()?.join("settings.json"))
}
fn index_path() -> Result<PathBuf> {
    Ok(app_data_root()?.join("project-index.json"))
}
pub fn default_projects_root() -> Result<PathBuf> {
    dirs::document_dir()
        .map(|v| v.join("SplatStudio").join("Projects"))
        .ok_or_else(|| SplatError::Process("无法定位 Documents 目录".into()))
}
pub async fn load_settings() -> Result<AppSettings> {
    let path = settings_path()?;
    if path.is_file() {
        if let Ok(bytes) = tokio::fs::read(&path).await {
            if let Ok(mut value) = serde_json::from_slice::<AppSettings>(&bytes) {
                // Deserialize defaults preserve old files, but keep the persisted schema honest
                // so the next settings write records the explicit Brush training default.
                if value.schema_version < 4 {
                    value.schema_version = CURRENT_SETTINGS_SCHEMA;
                    value.training_backend = TrainingBackend::Brush;
                } else if value.schema_version < CURRENT_SETTINGS_SCHEMA {
                    value.schema_version = CURRENT_SETTINGS_SCHEMA;
                    // v0.45 resets the default choice to the stable backend.
                    // A user can still explicitly select gsplat again afterwards.
                    value.training_backend = TrainingBackend::Brush;
                }
                return Ok(value);
            }
            // Older files: schema_version=1 has only projects_root; schema_version=2 has the
            // colmap backend too. Both migrate by filling in defaults so the rest of the
            // pipeline can rely on every field being populated.
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                let root = value
                    .get("projectsRoot")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from);
                let colmap_backend = value
                    .get("colmapBackend")
                    .and_then(|v| v.as_str())
                    .and_then(|raw| match raw {
                        "cuda" => Some(ColmapBackend::Cuda),
                        _ => Some(ColmapBackend::Cpu),
                    })
                    .unwrap_or_default();
                if let Some(root) = root {
                    return Ok(AppSettings {
                        schema_version: CURRENT_SETTINGS_SCHEMA,
                        projects_root: root,
                        colmap_backend,
                        cuda_colmap_flavor: CudaColmapFlavor::Official,
                        mapper_ba_mode: MapperBaMode::Auto,
                        ffmpeg_hw_accel: FfmpegHwAccel::Off,
                        brush_training_preset: BrushTrainingPreset::A,
                        training_backend: TrainingBackend::Brush,
                        gsplat_splat_cap: GsplatSplatCap::Auto,
                        gsplat_densification_strategy: GsplatDensificationStrategy::Mcmc,
                        multi_view_densification_gate: false,
                        floater_pruning: false,
                        photometric_mode: PhotometricMode::None,
                    });
                }
            }
        }
    }
    Ok(AppSettings {
        schema_version: CURRENT_SETTINGS_SCHEMA,
        projects_root: default_projects_root()?,
        colmap_backend: ColmapBackend::Cuda,
        cuda_colmap_flavor: CudaColmapFlavor::Official,
        mapper_ba_mode: MapperBaMode::Auto,
        ffmpeg_hw_accel: FfmpegHwAccel::Off,
        brush_training_preset: BrushTrainingPreset::A,
        training_backend: TrainingBackend::Brush,
        gsplat_splat_cap: GsplatSplatCap::Auto,
        gsplat_densification_strategy: GsplatDensificationStrategy::Mcmc,
        multi_view_densification_gate: false,
        floater_pruning: false,
        photometric_mode: PhotometricMode::None,
    })
}
pub async fn save_projects_root(root: PathBuf) -> Result<AppSettings> {
    crate::project::ProjectManager::validate_root(&root).await?;
    let mut settings = load_settings().await?;
    settings.projects_root = root;
    persist(&settings).await?;
    Ok(settings)
}
pub async fn save_colmap_backend(backend: ColmapBackend) -> Result<AppSettings> {
    let mut settings = load_settings().await?;
    settings.colmap_backend = backend;
    persist(&settings).await?;
    Ok(settings)
}
pub async fn save_cuda_colmap_flavor(flavor: CudaColmapFlavor) -> Result<AppSettings> {
    let mut settings = load_settings().await?;
    settings.cuda_colmap_flavor = flavor;
    persist(&settings).await?;
    Ok(settings)
}
pub async fn save_mapper_ba_mode(mode: MapperBaMode) -> Result<AppSettings> {
    let mut settings = load_settings().await?;
    settings.mapper_ba_mode = mode;
    persist(&settings).await?;
    Ok(settings)
}
pub async fn save_ffmpeg_hw_accel(mode: FfmpegHwAccel) -> Result<AppSettings> {
    let mut settings = load_settings().await?;
    settings.ffmpeg_hw_accel = mode;
    persist(&settings).await?;
    Ok(settings)
}
pub async fn save_brush_training_preset(preset: BrushTrainingPreset) -> Result<AppSettings> {
    let mut settings = load_settings().await?;
    settings.brush_training_preset = preset;
    persist(&settings).await?;
    Ok(settings)
}
pub async fn save_training_backend(backend: TrainingBackend) -> Result<AppSettings> {
    let mut settings = load_settings().await?;
    settings.training_backend = backend;
    persist(&settings).await?;
    Ok(settings)
}
pub async fn save_gsplat_splat_cap(cap: GsplatSplatCap) -> Result<AppSettings> {
    let mut settings = load_settings().await?;
    settings.gsplat_splat_cap = cap;
    persist(&settings).await?;
    Ok(settings)
}
pub async fn save_gsplat_densification_strategy(
    strategy: GsplatDensificationStrategy,
) -> Result<AppSettings> {
    let mut settings = load_settings().await?;
    settings.gsplat_densification_strategy = strategy;
    if strategy != GsplatDensificationStrategy::Mcmc {
        settings.multi_view_densification_gate = false;
        settings.floater_pruning = false;
    }
    persist(&settings).await?;
    Ok(settings)
}
pub async fn save_multi_view_densification_gate(enabled: bool) -> Result<AppSettings> {
    let mut settings = load_settings().await?;
    settings.multi_view_densification_gate = enabled;
    if enabled {
        settings.floater_pruning = false;
    }
    persist(&settings).await?;
    Ok(settings)
}
pub async fn save_floater_pruning(enabled: bool) -> Result<AppSettings> {
    let mut settings = load_settings().await?;
    if enabled && (settings.gsplat_densification_strategy != GsplatDensificationStrategy::Mcmc || settings.multi_view_densification_gate) {
        return Err(SplatError::Process("保守浮点裁剪仅支持关闭新增点门控的 gsplat MCMC。".into()));
    }
    settings.floater_pruning = enabled;
    persist(&settings).await?;
    Ok(settings)
}
pub async fn save_photometric_mode(mode: PhotometricMode) -> Result<AppSettings> {
    let mut settings = load_settings().await?;
    settings.photometric_mode = mode;
    persist(&settings).await?;
    Ok(settings)
}
async fn persist(settings: &AppSettings) -> Result<()> {
    let path = settings_path()?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    atomic_write_json(&path, settings).await
}
async fn load_index() -> Result<ProjectIndex> {
    let path = index_path()?;
    if path.is_file() {
        if let Ok(bytes) = tokio::fs::read(path).await {
            if let Ok(value) = serde_json::from_slice(&bytes) {
                return Ok(value);
            }
        }
    }
    Ok(ProjectIndex {
        schema_version: 1,
        projects: Vec::new(),
    })
}
async fn save_index(index: &ProjectIndex) -> Result<()> {
    let path = index_path()?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    atomic_write_json(&path, index).await
}
pub async fn register_project(id: Uuid, path: &Path) -> Result<()> {
    let mut index = load_index().await?;
    index
        .projects
        .retain(|item| item.id != id && item.path != path);
    index.projects.push(IndexedProject {
        id,
        path: path.to_path_buf(),
    });
    save_index(&index).await
}
pub async fn validate_registered_final_ply(source: &Path) -> Result<PathBuf> {
    let source = std::fs::canonicalize(source)?;
    if source
        .extension()
        .is_none_or(|extension| !extension.eq_ignore_ascii_case("ply"))
    {
        return Err(SplatError::InvalidPath(source));
    }
    let index = load_index().await?;
    for item in index.projects {
        let root = match std::fs::canonicalize(&item.path) {
            Ok(path) => path,
            Err(_) => continue,
        };
        let recorded = tokio::fs::read(root.join("project.json"))
            .await
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ProjectMetadata>(&bytes).ok())
            .and_then(|metadata| metadata.output.map(|output| output.final_ply));
        let direct = root.join("final.ply");
        let legacy = root.join("output").join("final.ply");
        if recorded
            .into_iter()
            .chain([direct, legacy])
            .into_iter()
            .filter_map(|path| std::fs::canonicalize(path).ok())
            .any(|path| path == source)
        {
            return Ok(source);
        }
    }
    Err(SplatError::InvalidPath(source))
}
async fn scan_root(root: &Path, destinations: &mut Vec<PathBuf>) -> Result<()> {
    if !root.is_dir() {
        return Ok(());
    }
    let mut entries = tokio::fs::read_dir(root).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.is_dir() && path.join("project.json").is_file() {
            destinations.push(path);
        }
    }
    Ok(())
}
pub async fn get_overview() -> Result<ProjectOverview> {
    let settings = load_settings().await?;
    let mut index = load_index().await?;
    let mut paths = index
        .projects
        .iter()
        .map(|v| v.path.clone())
        .collect::<Vec<_>>();
    scan_root(&default_projects_root()?, &mut paths).await?;
    if settings.projects_root != default_projects_root()? {
        scan_root(&settings.projects_root, &mut paths).await?;
    }
    let mut seen = HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
    let mut summaries = Vec::new();
    let mut valid = Vec::new();
    for path in paths {
        if let Ok(summary) = summarize_project(&path).await {
            valid.push(IndexedProject {
                id: summary.id,
                path,
            });
            summaries.push(summary);
        }
    }
    summaries.sort_by(|a, b| {
        b.completed_at
            .unwrap_or(b.created_at)
            .cmp(&a.completed_at.unwrap_or(a.created_at))
    });
    index.projects = valid;
    save_index(&index).await?;
    Ok(ProjectOverview {
        projects_root: settings.projects_root.clone(),
        colmap_backend: settings.colmap_backend,
        projects: summaries,
    })
}
async fn summarize_project(project: &Path) -> Result<ProjectSummary> {
    let bytes = tokio::fs::read(project.join("project.json")).await?;
    let mut metadata: ProjectMetadata = serde_json::from_slice(&bytes)?;
    let recorded_ply = metadata
        .output
        .as_ref()
        .map(|output| output.final_ply.clone());
    let new_ply = project.join("final.ply");
    let legacy_ply = project.join("output").join("final.ply");
    let final_ply = if recorded_ply.as_ref().is_some_and(|path| path.is_file()) {
        recorded_ply
    } else if new_ply.is_file() {
        Some(new_ply)
    } else if legacy_ply.is_file() {
        Some(legacy_ply)
    } else {
        None
    };
    if final_ply.is_some() {
        metadata.status = ProjectStatus::Completed;
    }
    let completed_at = metadata.completed_at.or_else(|| {
        final_ply
            .as_ref()
            .and_then(|p| p.metadata().ok()?.modified().ok())
            .map(DateTime::<Utc>::from)
    });
    let source_name = metadata
        .source_path
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("视频")
        .to_string();
    let output = metadata.output.as_ref();
    Ok(ProjectSummary {
        id: metadata.id,
        name: if metadata.name.is_empty() {
            project
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or("项目")
                .into()
        } else {
            metadata.name
        },
        status: metadata.status,
        project_path: project.to_path_buf(),
        final_ply,
        // Overview must remain cheap: project.json already stores these values.
        // Full PLY inspection belongs to the explicit viewer/export path.
        file_size: output.map(|v| v.file_size),
        splat_count: output.map(|v| v.splat_count),
        created_at: metadata.created_at,
        completed_at,
        duration_ms: metadata.duration_ms,
        quality: metadata.quality,
        input_source: metadata.input_source,
        training_backend: metadata.training_backend,
        brush_training_preset: metadata.brush_training_preset,
        gsplat_splat_cap: metadata.gsplat_splat_cap,
        gsplat_densification_strategy: metadata.gsplat_densification_strategy,
        photometric_mode: metadata.photometric_mode,
        source_name,
        registered_ratio: output.map(|v| v.registered_ratio),
        points_3d: output.map(|v| v.points_3d),
        failure_message: metadata.failure_message,
        weak_interval_count: metadata
            .needs_supplement
            .as_ref()
            .map(|requirement| requirement.weak_interval_count),
        supplemental_media_count: u64::try_from(metadata.supplemental_media.len())
            .expect("supplemental media count always fits u64"),
    })
}
pub async fn delete_project(id: Uuid) -> Result<()> {
    let mut index = load_index().await?;
    let item = index
        .projects
        .iter()
        .find(|item| item.id == id)
        .cloned()
        .ok_or_else(|| SplatError::Process("项目索引中不存在该项目".into()))?;
    let bytes = tokio::fs::read(item.path.join("project.json")).await?;
    let metadata: ProjectMetadata = serde_json::from_slice(&bytes)?;
    if metadata.id != id {
        return Err(SplatError::Process("项目身份校验失败，拒绝删除".into()));
    }
    let owned = has_project_ownership(&metadata, &item.path, id);
    if !owned {
        return Err(SplatError::Process(
            "目录没有 OOOSplat 所有权标记，拒绝删除".into(),
        ));
    }
    let mut targets = vec![item.path.clone()];
    if metadata.app_id != PROJECT_APP_ID {
        let legacy_work = app_data_root()?.join("work").join(id.to_string());
        if legacy_work.exists() {
            targets.push(legacy_work);
        }
    }
    let targets_for_trash = targets.clone();
    let trash_result = tokio::task::spawn_blocking(move || trash::delete_all(targets_for_trash))
        .await
        .map_err(|e| SplatError::Process(e.to_string()))?;
    if let Err(trash_error) = trash_result {
        // 回收站不可用（如分区无回收站、文件被临时锁定）时，回退为直接永久删除。
        let mut fully_removed = true;
        for target in &targets {
            let path = target.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                if path.exists() {
                    std::fs::remove_dir_all(&path)
                } else {
                    Ok(())
                }
            })
            .await
            .map_err(|e| SplatError::Process(e.to_string()))?;
            if outcome.is_err() {
                fully_removed = false;
                break;
            }
        }
        if !fully_removed {
            return Err(SplatError::Process(format!(
                "回收站不可用且直接删除失败：{trash_error}"
            )));
        }
    }
    index.projects.retain(|item| item.id != id);
    save_index(&index).await
}
fn has_project_ownership(metadata: &ProjectMetadata, path: &Path, id: Uuid) -> bool {
    let id_string = id.to_string();
    metadata.id == id
        && (metadata.app_id == PROJECT_APP_ID
            || path.file_name().and_then(|value| value.to_str()) == Some(id_string.as_str()))
}
