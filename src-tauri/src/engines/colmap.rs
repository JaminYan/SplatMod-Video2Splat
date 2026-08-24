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
        "--ImageReader.camera_model".into(),
        "SIMPLE_RADIAL".into(),
        "--ImageReader.single_camera".into(),
        "1".into(),
        "--FeatureExtraction.type".into(),
        "SIFT".into(),
        "--FeatureExtraction.use_gpu".into(),
        options.compute.use_gpu().into(),
    ];
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
            ColmapFeatureOptions { compute },
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
    fn mapper_arguments_select_the_requested_bundle_adjustment_backend() {
        let ceres = mapper_args(
            Path::new("db"),
            Path::new("images"),
            Path::new("output"),
            IncrementalMapperOptions {
                ba_backend: IncrementalBaBackend::Ceres,
            },
        );
        let caspar = mapper_args(
            Path::new("db"),
            Path::new("images"),
            Path::new("output"),
            IncrementalMapperOptions {
                ba_backend: IncrementalBaBackend::Caspar { gpu_index: -1 },
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
    args
}
