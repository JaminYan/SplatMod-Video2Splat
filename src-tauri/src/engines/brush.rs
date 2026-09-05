use std::{
    ffi::OsString,
    fs::File,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{
    error::{Result, SplatError},
    presets::QualityPreset,
    process::{ProcessManager, ProcessObserver, ProcessSpec},
};

pub fn require_verified_cli(executable: &Path) -> Result<()> {
    if executable.is_file() {
        Ok(())
    } else {
        Err(SplatError::EngineMissing(executable.display().to_string()))
    }
}

pub fn open_viewer(executable: &Path, source: &Path) -> Result<()> {
    require_verified_cli(executable)?;
    if source
        .extension()
        .is_none_or(|ext| !ext.eq_ignore_ascii_case("ply"))
    {
        return Err(SplatError::InvalidPath(source.to_path_buf()));
    }
    // Loading a PLY alone may run the CLI without opening a window. Brush's
    // explicit flag guarantees that the native viewer is spawned.
    let mut command = Command::new(executable);
    command.arg("--with-viewer").arg(source);
    if let Some(parent) = source.parent() {
        command.current_dir(parent);
    }
    command
        .spawn()
        .map_err(|e| SplatError::Process(format!("无法启动 Brush 查看器：{e}")))?;
    Ok(())
}

pub fn package_colmap_dataset(frames: &Path, model: &Path, output: &Path) -> Result<()> {
    for name in ["cameras.bin", "images.bin", "points3D.bin"] {
        if !model.join(name).is_file() {
            return Err(SplatError::Process(format!("COLMAP 模型缺少 {name}")));
        }
    }
    let file = File::create(output)?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for entry in std::fs::read_dir(frames)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| {
            e.eq_ignore_ascii_case("jpg")
                || e.eq_ignore_ascii_case("jpeg")
                || e.eq_ignore_ascii_case("png")
        }) {
            let name = path.file_name().unwrap().to_string_lossy();
            zip.start_file(format!("images/{name}"), options)
                .map_err(|e| SplatError::Process(e.to_string()))?;
            let mut input = File::open(path)?;
            std::io::copy(&mut input, &mut zip)?;
        }
    }
    for entry in std::fs::read_dir(model)? {
        let path = entry?.path();
        if path.is_file() {
            let name = path.file_name().unwrap().to_string_lossy();
            zip.start_file(format!("sparse/0/{name}"), options)
                .map_err(|e| SplatError::Process(e.to_string()))?;
            let mut input = File::open(path)?;
            std::io::copy(&mut input, &mut zip)?;
        }
    }
    zip.finish()
        .map_err(|e| SplatError::Process(e.to_string()))?;
    Ok(())
}

pub async fn train(
    executable: &Path,
    dataset: &Path,
    output_directory: &Path,
    preset: QualityPreset,
    log_path: PathBuf,
    manager: &ProcessManager,
    observer: Option<ProcessObserver>,
) -> Result<PathBuf> {
    train_with_params(
        executable,
        dataset,
        output_directory,
        preset.brush_iterations,
        preset.brush_max_resolution,
        preset.brush_max_splats,
        log_path,
        manager,
        observer,
    )
    .await
}

pub async fn train_with_params(
    executable: &Path,
    dataset: &Path,
    output_directory: &Path,
    total_steps: u32,
    max_resolution: u32,
    max_splats: u32,
    log_path: PathBuf,
    manager: &ProcessManager,
    observer: Option<ProcessObserver>,
) -> Result<PathBuf> {
    tokio::fs::create_dir_all(output_directory).await?;
    let checkpoint_every = (total_steps / 10).max(1_000);
    let candidate = output_directory.join(format!("checkpoint_{total_steps}.ply"));
    if candidate.exists() {
        tokio::fs::remove_file(&candidate).await?;
    }
    let output = manager
        .run(ProcessSpec {
            executable: executable.to_path_buf(),
            args: vec![
                OsString::from("--total-steps"),
                total_steps.to_string().into(),
                OsString::from("--max-resolution"),
                max_resolution.to_string().into(),
                OsString::from("--max-splats"),
                max_splats.to_string().into(),
                OsString::from("--export-every"),
                checkpoint_every.to_string().into(),
                OsString::from("--export-path"),
                output_directory.into(),
                OsString::from("--export-name"),
                OsString::from("checkpoint_{iter}.ply"),
                dataset.into(),
            ],
            working_directory: Some(output_directory.to_path_buf()),
            log_path: Some(log_path),
            observer,
        })
        .await?;
    if !output.success {
        return Err(SplatError::Process(format!(
            "Brush 退出码 {:?}",
            output.exit_code
        )));
    }
    let candidate = if candidate.is_file() {
        candidate
    } else {
        let alternate = output_directory.join(format!("checkpoint_{total_steps}.ply.ply"));
        if alternate.is_file() {
            alternate
        } else {
            candidate
        }
    };
    if !candidate.is_file() {
        return Err(SplatError::Process(format!(
            "Brush 未生成预期文件：{}",
            candidate.display()
        )));
    }
    Ok(candidate)
}
