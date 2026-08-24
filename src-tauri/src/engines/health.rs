use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    error::Result,
    process::{ProcessManager, ProcessSpec},
};
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColmapBackend {
    Cpu,
    #[default]
    Cuda,
}

/// Selects which CUDA COLMAP distribution is used when the CUDA backend is active.
/// The CASPAR build lives beside, never replaces, the official CUDA build.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CudaColmapFlavor {
    #[default]
    Official,
    Caspar,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FfmpegHwAccel {
    #[default]
    Off,
    Auto,
    D3d11va,
    Cuda,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EngineKind {
    Ffmpeg,
    Ffprobe,
    Colmap,
    Brush,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    pub kind: EngineKind,
    pub path: PathBuf,
    pub exists: bool,
    pub can_start: bool,
    pub version: Option<String>,
    pub cpu_only: Option<bool>,
    pub cuda_available: Option<bool>,
    pub caspar_available: Option<bool>,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct EnginePaths {
    pub root: PathBuf,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub colmap: PathBuf,
    pub colmap_cuda: PathBuf,
    pub colmap_caspar: PathBuf,
    pub brush: PathBuf,
}

impl EnginePaths {
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            ffmpeg: root.join("ffmpeg").join("ffmpeg.exe"),
            ffprobe: root.join("ffmpeg").join("ffprobe.exe"),
            colmap: root.join("colmap").join("bin").join("colmap.exe"),
            colmap_cuda: root.join("colmap-cuda").join("bin").join("colmap.exe"),
            colmap_caspar: root.join("colmap-caspar").join("bin").join("colmap.exe"),
            brush: root.join("brush").join("brush_app.exe"),
            root,
        }
    }

    pub fn discover(resource_dir: Option<&Path>) -> Self {
        if let Some(value) = std::env::var_os("OOOSPLAT_ENGINE_DIR") {
            return Self::from_root(value);
        }

        let current = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        // Tauri runs from `src-tauri/target/{profile}` in development, while
        // bundled engines live at the workspace root. Walk ancestors so both
        // development and packaged layouts resolve the same executables.
        let mut candidates = resource_dir
            .map(|path| path.join("engines"))
            .into_iter()
            .chain(current.ancestors().map(|path| path.join("engines")))
            .chain(std::iter::once(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("..")
                    .join("engines"),
            ));
        let root = candidates
            .find(|path| path.is_dir())
            .unwrap_or_else(|| current.join("engines"));
        Self::from_root(root)
    }

    pub async fn check_all(&self) -> Vec<EngineStatus> {
        let (ffmpeg, ffprobe, colmap, cuda, caspar, brush) = tokio::join!(
            check_basic(EngineKind::Ffmpeg, &self.ffmpeg, &["-version"]),
            check_basic(EngineKind::Ffprobe, &self.ffprobe, &["-version"]),
            check_colmap(&self.colmap),
            check_colmap(&self.colmap_cuda),
            check_colmap(&self.colmap_caspar),
            check_basic(EngineKind::Brush, &self.brush, &["--help"]),
        );
        vec![ffmpeg, ffprobe, colmap, cuda, caspar, brush]
    }
    pub fn colmap_for(&self, backend: ColmapBackend, cuda_flavor: CudaColmapFlavor) -> &Path {
        match backend {
            ColmapBackend::Cpu => &self.colmap,
            ColmapBackend::Cuda if cuda_flavor == CudaColmapFlavor::Caspar => &self.colmap_caspar,
            ColmapBackend::Cuda => &self.colmap_cuda,
        }
    }
}

fn missing(kind: EngineKind, path: &Path) -> EngineStatus {
    EngineStatus {
        kind,
        path: path.to_path_buf(),
        exists: false,
        can_start: false,
        version: None,
        cpu_only: None,
        cuda_available: None,
        caspar_available: None,
        detail: format!("未找到 {}", path.display()),
    }
}

pub async fn check_basic(kind: EngineKind, path: &Path, args: &[&str]) -> EngineStatus {
    if !path.is_file() {
        return missing(kind, path);
    }
    let manager = ProcessManager::new();
    let result = manager
        .run(ProcessSpec {
            executable: path.to_path_buf(),
            args: args.iter().map(OsString::from).collect(),
            working_directory: path.parent().map(Path::to_path_buf),
            log_path: None,
            observer: None,
        })
        .await;

    match result {
        Ok(output) => {
            let combined = format!("{}\n{}", output.stdout, output.stderr);
            let first_line = combined
                .lines()
                .find(|line| !line.trim().is_empty())
                .map(|line| line.trim().to_owned());
            EngineStatus {
                kind,
                path: path.to_path_buf(),
                exists: true,
                can_start: output.success,
                version: first_line,
                cpu_only: None,
                cuda_available: None,
                caspar_available: None,
                detail: if output.success {
                    "引擎可启动".into()
                } else {
                    format!("帮助命令退出码：{:?}", output.exit_code)
                },
            }
        }
        Err(error) => EngineStatus {
            kind,
            path: path.to_path_buf(),
            exists: true,
            can_start: false,
            version: None,
            cpu_only: None,
            cuda_available: None,
            caspar_available: None,
            detail: error.to_string(),
        },
    }
}

async fn check_colmap(path: &Path) -> EngineStatus {
    if !path.is_file() {
        return missing(EngineKind::Colmap, path);
    }
    let manager = ProcessManager::new();
    let mut help = String::new();
    let mut successful = true;
    for args in [
        vec!["feature_extractor", "-h"],
        vec!["sequential_matcher", "-h"],
        vec!["mapper", "-h"],
        // CASPAR tuning options are registered by the bundle-adjuster command,
        // not by the mapper help text.
        vec!["bundle_adjuster", "-h"],
    ] {
        match manager
            .run(ProcessSpec {
                executable: path.to_path_buf(),
                args: args.into_iter().map(OsString::from).collect(),
                working_directory: path.parent().map(Path::to_path_buf),
                log_path: None,
                observer: None,
            })
            .await
        {
            Ok(output) => {
                successful &= output.success;
                help.push_str(&output.stdout);
                help.push_str(&output.stderr);
            }
            Err(error) => {
                return EngineStatus {
                    kind: EngineKind::Colmap,
                    path: path.to_path_buf(),
                    exists: true,
                    can_start: false,
                    version: None,
                    cpu_only: None,
                    cuda_available: None,
                    caspar_available: None,
                    detail: error.to_string(),
                }
            }
        }
    }

    let lower = help.to_ascii_lowercase();
    let explicit_cpu = [
        "cuda: no",
        "cuda support: no",
        "without cuda",
        "no cuda support",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    // Some self-built CUDA distributions link the CUDA runtime through the
    // system driver and therefore ship no cudart/cublas DLL beside colmap.exe.
    // COLMAP itself still advertises this accurately in the command banner.
    let advertised_cuda = lower.contains("with cuda") || lower.contains("cuda support: yes");
    let bundled_cuda = path.parent().is_some_and(runtime_contains_cuda) || advertised_cuda;
    // Generic Mapper backend flags are present in CUDA-only binaries too.
    // This tuning group is registered only by CASPAR_ENABLED builds.
    let caspar_available =
        bundled_cuda.then(|| lower.contains("--bundleadjustmentcaspar.solver_iter_max"));
    let cpu_only = if bundled_cuda {
        Some(false)
    } else if explicit_cpu {
        Some(true)
    } else {
        None
    };
    let first_line = help
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_owned());
    let detail = match cpu_only {
        Some(true) => "三个必需命令可启动，帮助输出明确报告无 CUDA".into(),
        Some(false) => "运行目录中发现 CUDA 运行时，拒绝将其标记为 CPU 版本".into(),
        None => "命令可启动，但帮助输出未明确证明这是 CPU/no-CUDA 构建".into(),
    };
    EngineStatus {
        kind: EngineKind::Colmap,
        path: path.to_path_buf(),
        exists: true,
        can_start: successful,
        version: first_line,
        cpu_only,
        cuda_available: Some(bundled_cuda),
        caspar_available,
        detail,
    }
}

pub async fn cuda_colmap_supports_caspar(
    paths: &EnginePaths,
    flavor: CudaColmapFlavor,
) -> Result<bool> {
    let status = check_colmap(paths.colmap_for(ColmapBackend::Cuda, flavor)).await;
    Ok(status.exists
        && status.can_start
        && status.cuda_available == Some(true)
        && status.caspar_available == Some(true))
}
pub async fn require_cuda_colmap(paths: &EnginePaths, flavor: CudaColmapFlavor) -> Result<()> {
    let status = check_colmap(paths.colmap_for(ColmapBackend::Cuda, flavor)).await;
    if status.cuda_available == Some(true) && status.can_start {
        Ok(())
    } else {
        Err(crate::error::SplatError::UnsupportedEngine(status.detail))
    }
}

fn runtime_contains_cuda(directory: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            return runtime_contains_cuda(&path);
        }
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        ["cudart", "cublas", "cudnn", "cuda.dll"]
            .iter()
            .any(|needle| name.contains(needle))
    })
}

pub async fn require_cpu_colmap(paths: &EnginePaths) -> Result<()> {
    let status = check_colmap(&paths.colmap).await;
    if status.cpu_only == Some(true) && status.can_start {
        Ok(())
    } else {
        Err(crate::error::SplatError::UnsupportedEngine(status.detail))
    }
}
