use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    engines::brush,
    error::{Result, SplatError},
    process::{ProcessManager, ProcessObserver, ProcessSpec},
};

/// The pipeline-facing training selection.  Brush remains the only default and
/// production-ready choice until the isolated gsplat runtime passes its CUDA smoke test.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TrainingBackend {
    #[default]
    Brush,
    Gsplat,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PhotometricMode {
    #[default]
    None,
    Ppisp,
}

#[derive(Debug, Clone)]
pub struct TrainingRequest {
    pub dataset_root: PathBuf,
    pub output_directory: PathBuf,
    pub total_steps: u32,
    pub max_resolution: u32,
    pub max_splats: u32,
    pub seed: u64,
    pub photometric_mode: PhotometricMode,
    pub log_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct TrainingOutput {
    pub candidate_ply: PathBuf,
    pub backend: TrainingBackend,
    pub elapsed_ms: u64,
    pub peak_vram_mb: Option<u64>,
    pub reported_splats: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct AdapterEvent {
    event: String,
    step: Option<u64>,
    total: Option<u64>,
    #[serde(rename = "peakVramMb")]
    peak_vram_mb: Option<u64>,
    splats: Option<u64>,
}

/// Development uses `target/debug/engines` for bundled core binaries, while
/// the optional Python runtime remains in the workspace engine directory.
fn resolve_gsplat_root(engines_root: &Path) -> Option<PathBuf> {
    let direct = engines_root.join("gsplat");
    if direct.is_dir() {
        return Some(direct);
    }
    engines_root
        .ancestors()
        .map(|ancestor| ancestor.join("engines").join("gsplat"))
        .find(|candidate| candidate.is_dir())
}

/// Keeps only monotonic, bounded progress events. Invalid JSONL and late/stale
/// lines are deliberately ignored instead of advancing desktop progress.
fn parse_gsplat_events(stdout: &str) -> (Option<u64>, Option<u64>, Option<u64>) {
    let mut last_step = 0;
    let mut total = None;
    let mut peak_vram_mb = None;
    let mut splats = None;
    for line in stdout.lines() {
        let Ok(event) = serde_json::from_str::<AdapterEvent>(line) else {
            continue;
        };
        match event.event.as_str() {
            "progress" => {
                if let (Some(step), Some(candidate_total)) = (event.step, event.total) {
                    if candidate_total > 0 && step >= last_step && step <= candidate_total {
                        last_step = step;
                        total = Some(candidate_total);
                    }
                }
            }
            "metric" => peak_vram_mb = event.peak_vram_mb.or(peak_vram_mb),
            "export" => splats = event.splats.or(splats),
            _ => {}
        }
    }
    (total.map(|_| last_step), peak_vram_mb, splats)
}

/// Dispatches the selected backend without exposing backend-private CLI shapes to the pipeline.
pub async fn train(
    backend: TrainingBackend,
    brush_executable: &Path,
    engines_root: &Path,
    request: TrainingRequest,
    manager: &ProcessManager,
    observer: Option<ProcessObserver>,
) -> Result<TrainingOutput> {
    let started = std::time::Instant::now();
    match backend {
        TrainingBackend::Brush => {
            let dataset = request.output_directory.join("dataset.zip");
            tokio::task::spawn_blocking({
                let frames = request.dataset_root.join("images");
                let model = request.dataset_root.join("sparse").join("0");
                let dataset = dataset.clone();
                move || brush::package_colmap_dataset(&frames, &model, &dataset)
            })
            .await
            .map_err(|error| SplatError::Process(format!("Brush 数据集打包任务失败：{error}")))??;
            let candidate = brush::train_with_params(
                brush_executable,
                &dataset,
                &request.output_directory,
                request.total_steps,
                request.max_resolution,
                request.max_splats,
                request.log_path,
                manager,
                observer,
            )
            .await?;
            Ok(TrainingOutput {
                candidate_ply: candidate,
                backend,
                elapsed_ms: started.elapsed().as_millis() as u64,
                peak_vram_mb: None,
                reported_splats: None,
            })
        }
        TrainingBackend::Gsplat => train_gsplat(engines_root, request, manager, observer).await,
    }
}

/// Three-stage runtime gate for the experimental UI option: required files,
/// Python import, then a real minimal CUDA rasterization.
pub async fn gsplat_runtime_healthy(engines_root: &Path) -> bool {
    let Some(gsplat_root) = resolve_gsplat_root(engines_root) else {
        return false;
    };
    let python = gsplat_root.join("python/Scripts/python.exe");
    let adapter = gsplat_root.join("adapter/train_adapter.py");
    let manifest = gsplat_root.join("adapter/version.json");
    if !python.is_file() || !adapter.is_file() || !manifest.is_file() {
        return false;
    }
    let code = "import torch; from gsplat import rasterization; d='cuda'; assert torch.cuda.is_available(); m=torch.tensor([[0.,0.,2.]],device=d); q=torch.tensor([[1.,0.,0.,0.]],device=d); s=torch.full((1,3),-2.,device=d); o=torch.ones(1,device=d); c=torch.tensor([[1.,0.,0.]],device=d); v=torch.eye(4,device=d)[None]; k=torch.tensor([[[64.,0.,16.],[0.,64.,16.],[0.,0.,1.]]],device=d); image,alpha,_=rasterization(m,q,s,o,c,v,k,32,32,packed=False); torch.cuda.synchronize(); assert alpha.sum().item()>0 and image.sum().item()>0; print('gsplat-health-ok')";
    let manager = ProcessManager::new();
    match manager
        .run(ProcessSpec {
            executable: python,
            args: vec![OsString::from("-c"), OsString::from(code)],
            working_directory: Some(gsplat_root),
            log_path: None,
            observer: None,
        })
        .await
    {
        Ok(output) => output.success && output.stdout.contains("gsplat-health-ok"),
        Err(_) => false,
    }
}

async fn train_gsplat(
    engines_root: &Path,
    request: TrainingRequest,
    manager: &ProcessManager,
    observer: Option<ProcessObserver>,
) -> Result<TrainingOutput> {
    let gsplat_root = resolve_gsplat_root(engines_root).ok_or_else(|| {
        SplatError::UnsupportedEngine("gsplat 隔离运行时未安装；请改用 Brush。".into())
    })?;
    let python = gsplat_root.join("python/Scripts/python.exe");
    let adapter = gsplat_root.join("adapter/train_adapter.py");
    if !python.is_file() || !adapter.is_file() {
        return Err(SplatError::UnsupportedEngine(
            "gsplat 隔离运行时或 adapter 未安装；请改用 Brush。".into(),
        ));
    }
    tokio::fs::create_dir_all(&request.output_directory).await?;
    let candidate = request.output_directory.join("final.ply.tmp");
    if candidate.is_file() {
        tokio::fs::remove_file(&candidate).await?;
    }
    let config_path = request.output_directory.join("request.json");
    let config = serde_json::json!({
        "schemaVersion": 1,
        "dataDir": request.dataset_root,
        "resultDir": request.output_directory,
        "outputPly": candidate,
        "strategy": "mcmc",
        "maxSteps": request.total_steps,
        "maxResolution": request.max_resolution,
        "maxSplats": request.max_splats,
        // M2: stabilise the coarse static scene before MCMC adds new splats.
        "delayedDensificationRatio": 0.10,
        "batchSize": match request.photometric_mode { PhotometricMode::None => 4, PhotometricMode::Ppisp => 1 },
        // M1 is opt-in until it has passed the documented three-material gate.
        "photometricMode": match request.photometric_mode { PhotometricMode::None => "none", PhotometricMode::Ppisp => "ppisp" },
        "ppispController": true,
        "ppispControllerDistillation": true,
        "canonicalExposure": "median",
        "seed": request.seed,
        "saveCheckpoints": false,
        "diagnosticsDir": request.log_path.parent().unwrap_or(&request.output_directory),
    });
    tokio::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).await?;
    let started = std::time::Instant::now();
    let output = manager
        .run(ProcessSpec {
            executable: python,
            args: vec![
                adapter.into(),
                OsString::from("--config"),
                config_path.into(),
            ],
            working_directory: Some(request.output_directory.clone()),
            log_path: Some(request.log_path),
            observer,
        })
        .await?;
    if !output.success {
        return Err(SplatError::Process(format!(
            "gsplat adapter 退出码 {:?}",
            output.exit_code
        )));
    }
    if !candidate.is_file() {
        return Err(SplatError::Process(format!(
            "gsplat 未生成预期文件：{}",
            candidate.display()
        )));
    }
    let (_, peak_vram_mb, reported_splats) = parse_gsplat_events(&output.stdout);
    Ok(TrainingOutput {
        candidate_ply: candidate,
        backend: TrainingBackend::Gsplat,
        elapsed_ms: started.elapsed().as_millis() as u64,
        peak_vram_mb,
        reported_splats,
    })
}

/// Build the backend-neutral COLMAP layout once.  Images use hard links when
/// possible, which avoids duplicating the selected-frame payload for Brush and gsplat.
pub async fn prepare_standard_colmap_dataset(
    destination: &Path,
    frames: &Path,
    sparse_model: &Path,
) -> Result<()> {
    let destination = destination.to_path_buf();
    let frames = frames.to_path_buf();
    let sparse_model = sparse_model.to_path_buf();
    tokio::task::spawn_blocking(move || {
        prepare_standard_colmap_dataset_sync(&destination, &frames, &sparse_model)
    })
    .await
    .map_err(|error| SplatError::Process(format!("训练输入构建任务失败：{error}")))?
}

fn prepare_standard_colmap_dataset_sync(
    destination: &Path,
    frames: &Path,
    sparse_model: &Path,
) -> Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| SplatError::InvalidPath(destination.to_path_buf()))?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".training-input-{}", uuid::Uuid::new_v4()));
    let image_destination = temporary.join("images");
    let model_destination = temporary.join("sparse").join("0");
    let build = || -> Result<()> {
        std::fs::create_dir_all(&image_destination)?;
        std::fs::create_dir_all(&model_destination)?;
        let mut image_count = 0_u64;
        for entry in std::fs::read_dir(frames)? {
            let source = entry?.path();
            if !source.is_file()
                || !source.extension().is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg")
                })
            {
                continue;
            }
            let target = image_destination.join(
                source
                    .file_name()
                    .ok_or_else(|| SplatError::InvalidPath(source.clone()))?,
            );
            if std::fs::hard_link(&source, &target).is_err() {
                std::fs::copy(&source, &target)?;
            }
            image_count += 1;
        }
        if image_count == 0 {
            return Err(SplatError::Process("训练输入没有 JPEG 图像".into()));
        }
        for name in ["cameras.bin", "images.bin", "points3D.bin"] {
            let source = sparse_model.join(name);
            if !source.is_file() {
                return Err(SplatError::Process(format!("COLMAP 模型缺少 {name}")));
            }
            std::fs::copy(source, model_destination.join(name))?;
        }
        Ok(())
    }();
    if let Err(error) = build {
        let _ = std::fs::remove_dir_all(&temporary);
        return Err(error);
    }
    if destination.exists() {
        std::fs::remove_dir_all(destination)?;
    }
    std::fs::rename(temporary, destination)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[tokio::test]
    async fn builds_standard_colmap_dataset() {
        let temp = tempfile::tempdir().unwrap();
        let frames = temp.path().join("frames");
        let model = temp.path().join("model");
        std::fs::create_dir_all(&frames).unwrap();
        std::fs::create_dir_all(&model).unwrap();
        std::fs::File::create(frames.join("one.jpg"))
            .unwrap()
            .write_all(b"jpeg")
            .unwrap();
        for name in ["cameras.bin", "images.bin", "points3D.bin"] {
            std::fs::File::create(model.join(name))
                .unwrap()
                .write_all(b"colmap")
                .unwrap();
        }
        let output = temp.path().join("training-input");
        prepare_standard_colmap_dataset(&output, &frames, &model)
            .await
            .unwrap();
        assert!(output.join("images/one.jpg").is_file());
        assert!(output.join("sparse/0/cameras.bin").is_file());
    }

    #[test]
    fn ignores_invalid_and_out_of_order_gsplat_jsonl() {
        let input = "bad\n{\"event\":\"progress\",\"step\":20,\"total\":100}\n{\"event\":\"progress\",\"step\":10,\"total\":100}\n{\"event\":\"metric\",\"peakVramMb\":321}\n{\"event\":\"export\",\"splats\":42}";
        assert_eq!(parse_gsplat_events(input), (Some(20), Some(321), Some(42)));
    }

    #[test]
    fn resolves_workspace_gsplat_from_debug_engine_directory() {
        let temp = tempfile::tempdir().unwrap();
        let debug_engines = temp.path().join("target/debug/engines");
        let expected = temp.path().join("engines/gsplat");
        std::fs::create_dir_all(&debug_engines).unwrap();
        std::fs::create_dir_all(&expected).unwrap();
        assert_eq!(resolve_gsplat_root(&debug_engines), Some(expected));
    }
}
