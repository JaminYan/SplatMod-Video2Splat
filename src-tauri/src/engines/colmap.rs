use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use crate::{
    error::{Result, SplatError},
    process::{ProcessManager, ProcessObserver, ProcessSpec},
};
use serde::{Deserialize, Serialize};

/// Explicitly controls the device passed to COLMAP. The executable path only
/// chooses the bundled distribution; it must never be used as a proxy for the
/// effective SIFT device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColmapComputeMode {
    Cpu,
    Cuda { gpu_index: i32 },
}

#[derive(Debug, Clone, Copy)]
pub struct ColmapFeatureOptions {
    pub compute: ColmapComputeMode,
    pub rig: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct ColmapMatchingOptions {
    pub compute: ColmapComputeMode,
    pub overlap: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncrementalBaBackend {
    Ceres,
    Caspar { gpu_index: i32 },
}

#[derive(Debug, Clone, Copy)]
pub struct IncrementalMapperOptions {
    pub ba_backend: IncrementalBaBackend,
    pub rig: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MapperBaMode {
    #[default]
    Auto,
    Ceres,
    Caspar,
}

impl ColmapComputeMode {
    fn use_gpu(self) -> &'static str {
        match self {
            Self::Cpu => "0",
            Self::Cuda { .. } => "1",
        }
    }

    fn gpu_index(self) -> Option<i32> {
        match self {
            Self::Cpu => None,
            Self::Cuda { gpu_index } => Some(gpu_index),
        }
    }
}

pub fn require_verified_cli(executable: &Path) -> Result<()> {
    if executable.is_file() {
        Ok(())
    } else {
        Err(SplatError::EngineMissing(executable.display().to_string()))
    }
}

async fn run_colmap(
    executable: &Path,
    args: Vec<OsString>,
    working_directory: &Path,
    log_path: PathBuf,
    manager: &ProcessManager,
    observer: Option<ProcessObserver>,
) -> Result<()> {
    let output = manager
        .run(ProcessSpec {
            executable: executable.to_path_buf(),
            args,
            working_directory: Some(working_directory.to_path_buf()),
            log_path: Some(log_path),
            observer,
        })
        .await?;
    if output.success {
        Ok(())
    } else {
        Err(SplatError::Process(format!(
            "COLMAP 退出码 {:?}: {}{}",
            output.exit_code, output.stderr, output.stdout,
        )))
    }
}

pub async fn extract_features(
    executable: &Path,
    database: &Path,
    images: &Path,
    options: ColmapFeatureOptions,
    log: PathBuf,
    manager: &ProcessManager,
    observer: Option<ProcessObserver>,
) -> Result<()> {
    run_colmap(
        executable,
        feature_extractor_args(database, images, options),
        database.parent().unwrap_or(images),
        log,
        manager,
        observer,
    )
    .await
}

/// Converts a validated COLMAP text model into the binary layout consumed by Brush and gsplat.
/// This deliberately has no feature/matching/mapper flags, so it is safe for already-reconstructed
/// Splatcam exports.
pub async fn convert_text_model_to_binary(
    executable: &Path,
    input: &Path,
    output: &Path,
    log: PathBuf,
    manager: &ProcessManager,
    observer: Option<ProcessObserver>,
) -> Result<()> {
    run_colmap(
        executable,
        vec![
            "model_converter".into(),
            "--input_path".into(),
            input.as_os_str().to_os_string(),
            "--output_path".into(),
            output.as_os_str().to_os_string(),
            "--output_type".into(),
            "BIN".into(),
        ],
        input,
        log,
        manager,
        observer,
    )
    .await
}

/// Only device/driver/runtime failures are eligible for a CPU retry. Dataset
/// quality failures (for example insufficient matches) must remain visible and
/// never be disguised as a successful CPU fallback.
pub fn is_cuda_runtime_error(error: &SplatError) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    [
        "cuda",
        "cudart",
        "cublas",
        "cudnn",
        "gpu is not available",
        "no compatible gpu",
        "out of memory",
        "outofmemory",
        "driver",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}

fn feature_extractor_args(
    database: &Path,
    images: &Path,
    options: ColmapFeatureOptions,
) -> Vec<OsString> {
    let mut args = vec![
        "feature_extractor".into(),
        "--database_path".into(),
        database.into(),
        "--image_path".into(),
        images.into(),
        "--FeatureExtraction.type".into(),
        "SIFT".into(),
        "--FeatureExtraction.use_gpu".into(),
        options.compute.use_gpu().into(),
    ];
    if options.rig {
        args.extend([
            "--ImageReader.single_camera_per_folder".into(),
            "1".into(),
            "--ImageReader.camera_model".into(),
            "OPENCV_FISHEYE".into(),
            // Insta360's 3840px circular fisheye has an ~1200-1300px focal
            // prior. COLMAP's generic 1.2*width prior is far too narrow and
            // prevents the bootstrap mapper from registering the sequence.
            "--ImageReader.default_focal_length_factor".into(),
            "0.34".into(),
        ]);
    } else {
        args.extend([
            "--ImageReader.camera_model".into(),
            "SIMPLE_RADIAL".into(),
            "--ImageReader.single_camera".into(),
            "1".into(),
        ]);
    }
    if let Some(index) = options.compute.gpu_index() {
        args.extend([
            "--FeatureExtraction.gpu_index".into(),
            index.to_string().into(),
        ]);
    }
    args
}

pub async fn match_sequential(
    executable: &Path,
    database: &Path,
    options: ColmapMatchingOptions,
    log: PathBuf,
    manager: &ProcessManager,
    observer: Option<ProcessObserver>,
) -> Result<()> {
    run_colmap(
        executable,
        sequential_matcher_args(database, options),
        database.parent().unwrap_or(Path::new(".")),
        log,
        manager,
        observer,
    )
    .await
}

pub async fn match_exhaustive(
    executable: &Path,
    database: &Path,
    options: ColmapMatchingOptions,
    log: PathBuf,
    manager: &ProcessManager,
    observer: Option<ProcessObserver>,
) -> Result<()> {
    run_colmap(
        executable,
        exhaustive_matcher_args(database, options),
        database.parent().unwrap_or(Path::new(".")),
        log,
        manager,
        observer,
    )
    .await
}

pub async fn configure_rig(
    executable: &Path,
    database: &Path,
    rig_config: &Path,
    input_model: Option<&Path>,
    output_model: Option<&Path>,
    log: PathBuf,
    manager: &ProcessManager,
    observer: Option<ProcessObserver>,
) -> Result<()> {
    if let Some(output_model) = output_model {
        tokio::fs::create_dir_all(output_model).await?;
    }
    run_colmap(
        executable,
        vec![
            "rig_configurator".into(),
            "--database_path".into(),
            database.into(),
            "--rig_config_path".into(),
            rig_config.into(),
        ]
        .into_iter()
        .chain(input_model.into_iter().flat_map(|path| [
            OsString::from("--input_path"),
            path.as_os_str().to_owned(),
        ]))
        .chain(output_model.into_iter().flat_map(|path| [
            OsString::from("--output_path"),
            path.as_os_str().to_owned(),
        ]))
        .collect(),
        database.parent().unwrap_or(Path::new(".")),
        log,
        manager,
        observer,
    )
    .await
}

fn sequential_matcher_args(database: &Path, options: ColmapMatchingOptions) -> Vec<OsString> {
    let mut args = vec![
        "sequential_matcher".into(),
        "--database_path".into(),
        database.into(),
        "--FeatureMatching.type".into(),
        "SIFT_BRUTEFORCE".into(),
        "--FeatureMatching.use_gpu".into(),
        options.compute.use_gpu().into(),
        "--SequentialMatching.overlap".into(),
        options.overlap.to_string().into(),
    ];
    if let Some(index) = options.compute.gpu_index() {
        args.extend([
            "--FeatureMatching.gpu_index".into(),
            index.to_string().into(),
        ]);
    }
    args
}

fn exhaustive_matcher_args(database: &Path, options: ColmapMatchingOptions) -> Vec<OsString> {
    let mut args = vec![
        "exhaustive_matcher".into(),
        "--database_path".into(),
        database.into(),
        "--FeatureMatching.type".into(),
        "SIFT_BRUTEFORCE".into(),
        "--FeatureMatching.use_gpu".into(),
        options.compute.use_gpu().into(),
    ];
    if let Some(index) = options.compute.gpu_index() {
        args.extend([
            "--FeatureMatching.gpu_index".into(),
            index.to_string().into(),
        ]);
    }
    args
}

pub async fn map(
    executable: &Path,
    database: &Path,
    images: &Path,
    output: &Path,
    options: IncrementalMapperOptions,
    log: PathBuf,
    manager: &ProcessManager,
    observer: Option<ProcessObserver>,
) -> Result<()> {
    tokio::fs::create_dir_all(output).await?;
    run_colmap(
        executable,
        mapper_args(database, images, output, options),
        database.parent().unwrap_or(output),
        log,
        manager,
        observer,
    )
    .await
}

pub async fn bundle_adjust_rig(
    executable: &Path,
    input: &Path,
    output: &Path,
    log: PathBuf,
    manager: &ProcessManager,
    observer: Option<ProcessObserver>,
) -> Result<()> {
    tokio::fs::create_dir_all(output).await?;
    run_colmap(
        executable,
        vec![
            "bundle_adjuster".into(),
            "--input_path".into(),
            input.into(),
            "--output_path".into(),
            output.into(),
            "--BundleAdjustment.refine_sensor_from_rig".into(),
            "0".into(),
        ],
        input,
        log,
        manager,
        observer,
    )
    .await
}

/// Produces a pinhole COLMAP training layout whose images and cameras share
/// the same undistorted projection contract. The caller owns promotion of the
/// completed directory so original frames and sparse reconstruction stay intact.
pub async fn undistort_images(
    executable: &Path,
    images: &Path,
    model: &Path,
    output: &Path,
    max_image_size: Option<u32>,
    log: PathBuf,
    manager: &ProcessManager,
) -> Result<()> {
    let mut args = vec![
            "image_undistorter".into(),
            "--image_path".into(),
            images.into(),
            "--input_path".into(),
            model.into(),
            "--output_path".into(),
            output.into(),
            "--output_type".into(),
            "COLMAP".into(),
        ];
    if let Some(max_image_size) = max_image_size {
        args.extend(["--max_image_size".into(), max_image_size.to_string().into()]);
    }
    run_colmap(
        executable,
        args,
        images,
        log,
        manager,
        None,
    )
    .await
}

/// Limits COLMAP's automatically selected source views for each PatchMatch
/// reference view. `image_undistorter` writes this value into the workspace
/// config rather than exposing it as a `patch_match_stereo` CLI option.
pub async fn limit_patch_match_sources(workspace: &Path, max_sources: u32) -> Result<usize> {
    let config = workspace.join("stereo").join("patch-match.cfg");
    let contents = tokio::fs::read_to_string(&config).await?;
    let needle = "__auto__, 20";
    let replacement = format!("__auto__, {max_sources}");
    let replaced = contents.matches(needle).count();
    if replaced > 0 {
        tokio::fs::write(&config, contents.replace(needle, &replacement)).await?;
    }
    Ok(replaced)
}

pub async fn patch_match_stereo(
    executable: &Path,
    workspace: &Path,
    num_iterations: u32,
    log: PathBuf,
    manager: &ProcessManager,
    observer: Option<ProcessObserver>,
) -> Result<()> {
    run_colmap(
        executable,
        vec![
            "patch_match_stereo".into(),
            "--workspace_path".into(),
            workspace.into(),
            "--workspace_format".into(),
            "COLMAP".into(),
            "--PatchMatchStereo.geom_consistency".into(),
            "true".into(),
            "--PatchMatchStereo.num_iterations".into(),
            num_iterations.to_string().into(),
        ],
        workspace,
        log,
        manager,
        observer,
    )
    .await
}

pub async fn stereo_fusion(
    executable: &Path,
    workspace: &Path,
    output: &Path,
    log: PathBuf,
    manager: &ProcessManager,
    observer: Option<ProcessObserver>,
) -> Result<()> {
    run_colmap(
        executable,
        vec![
            "stereo_fusion".into(),
            "--workspace_path".into(),
            workspace.into(),
            "--workspace_format".into(),
            "COLMAP".into(),
            "--input_type".into(),
            "geometric".into(),
            "--output_path".into(),
            output.into(),
        ],
        workspace,
        log,
        manager,
        observer,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_arguments_disable_gpu_for_both_stages() {
        let feature = feature_extractor_args(
            Path::new("db"),
            Path::new("images"),
            ColmapFeatureOptions {
                compute: ColmapComputeMode::Cpu,
                rig: false,
            },
        );
        let matching = sequential_matcher_args(
            Path::new("db"),
            ColmapMatchingOptions {
                compute: ColmapComputeMode::Cpu,
                overlap: 10,
            },
        );
        let feature = feature
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>();
        let matching = matching
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>();
        assert!(feature
            .windows(2)
            .any(|pair| pair == ["--FeatureExtraction.use_gpu", "0"]));
        assert!(matching
            .windows(2)
            .any(|pair| pair == ["--FeatureMatching.use_gpu", "0"]));
        assert!(!feature
            .iter()
            .any(|value| value == "--FeatureExtraction.gpu_index"));
        assert!(!matching
            .iter()
            .any(|value| value == "--FeatureMatching.gpu_index"));
    }

    #[test]
    fn cuda_arguments_enable_sift_and_pass_the_verified_gpu_index() {
        let compute = ColmapComputeMode::Cuda { gpu_index: -1 };
        let feature = feature_extractor_args(
            Path::new("db"),
            Path::new("images"),
            ColmapFeatureOptions { compute, rig: false },
        );
        let matching = sequential_matcher_args(
            Path::new("db"),
            ColmapMatchingOptions {
                compute,
                overlap: 10,
            },
        );
        let feature = feature
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>();
        let matching = matching
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>();
        assert!(feature
            .windows(2)
            .any(|pair| pair == ["--FeatureExtraction.type", "SIFT"]));
        assert!(feature
            .windows(2)
            .any(|pair| pair == ["--FeatureExtraction.use_gpu", "1"]));
        assert!(feature
            .windows(2)
            .any(|pair| pair == ["--FeatureExtraction.gpu_index", "-1"]));
        assert!(matching
            .windows(2)
            .any(|pair| pair == ["--FeatureMatching.type", "SIFT_BRUTEFORCE"]));
        assert!(matching
            .windows(2)
            .any(|pair| pair == ["--FeatureMatching.use_gpu", "1"]));
        assert!(matching
            .windows(2)
            .any(|pair| pair == ["--FeatureMatching.gpu_index", "-1"]));
    }

    #[test]
    fn rig_arguments_use_per_folder_fisheye_cameras_and_fixed_sensor_pose() {
        let feature = feature_extractor_args(
            Path::new("db"),
            Path::new("images"),
            ColmapFeatureOptions {
                compute: ColmapComputeMode::Cpu,
                rig: true,
            },
        );
        let mapper = mapper_args(
            Path::new("db"),
            Path::new("images"),
            Path::new("output"),
            IncrementalMapperOptions {
                ba_backend: IncrementalBaBackend::Ceres,
                rig: true,
            },
        );
        let feature = feature
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>();
        let mapper = mapper
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>();
        assert!(feature
            .windows(2)
            .any(|pair| pair == ["--ImageReader.single_camera_per_folder", "1"]));
        assert!(feature
            .windows(2)
            .any(|pair| pair == ["--ImageReader.camera_model", "OPENCV_FISHEYE"]));
        assert!(feature
            .windows(2)
            .any(|pair| pair == ["--ImageReader.default_focal_length_factor", "0.34"]));
        assert!(mapper
            .windows(2)
            .any(|pair| pair == ["--Mapper.ba_refine_sensor_from_rig", "0"]));
    }

    #[test]
    fn mapper_arguments_select_the_requested_bundle_adjustment_backend() {
        let ceres = mapper_args(
            Path::new("db"),
            Path::new("images"),
            Path::new("output"),
            IncrementalMapperOptions {
                ba_backend: IncrementalBaBackend::Ceres,
                rig: false,
            },
        );
        let caspar = mapper_args(
            Path::new("db"),
            Path::new("images"),
            Path::new("output"),
            IncrementalMapperOptions {
                ba_backend: IncrementalBaBackend::Caspar { gpu_index: -1 },
                rig: false,
            },
        );
        let ceres = ceres
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>();
        let caspar = caspar
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>();
        assert!(ceres
            .windows(2)
            .any(|pair| pair == ["--Mapper.ba_local_backend", "CERES"]));
        assert!(caspar
            .windows(2)
            .any(|pair| pair == ["--Mapper.ba_local_backend", "CERES"]));
        assert!(caspar
            .windows(2)
            .any(|pair| pair == ["--Mapper.ba_global_backend", "CASPAR"]));
        assert!(caspar
            .windows(2)
            .any(|pair| pair == ["--Mapper.ba_gpu_index", "-1"]));
    }
}

fn mapper_args(
    database: &Path,
    images: &Path,
    output: &Path,
    options: IncrementalMapperOptions,
) -> Vec<OsString> {
    let mut args = vec![
        "mapper".into(),
        "--database_path".into(),
        database.into(),
        "--image_path".into(),
        images.into(),
        "--output_path".into(),
        output.into(),
    ];
    match options.ba_backend {
        IncrementalBaBackend::Ceres => args.extend([
            "--Mapper.ba_local_backend".into(),
            "CERES".into(),
            "--Mapper.ba_global_backend".into(),
            "CERES".into(),
        ]),
        IncrementalBaBackend::Caspar { gpu_index } => args.extend([
            "--Mapper.ba_local_backend".into(),
            // COLMAP 4.1.1 rejects CASPAR for the frequent local BA pass.
            // CASPAR is supported for the global BA pass only.
            "CERES".into(),
            "--Mapper.ba_global_backend".into(),
            "CASPAR".into(),
            "--Mapper.ba_gpu_index".into(),
            gpu_index.to_string().into(),
        ]),
    }
    if options.rig {
        args.extend([
            "--Mapper.ba_refine_sensor_from_rig".into(),
            "0".into(),
        ]);
    }
    args
}
