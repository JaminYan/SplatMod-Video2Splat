use std::path::{Path, PathBuf};

use crate::{
    error::{Result, SplatError},
    process::{ProcessManager, ProcessObserver, ProcessSpec},
    video::FramePlan,
};

/// Extract frames uniformly using FFmpeg. The output JPEG files are named
/// `frame_%06d.jpg` and reside in `output_directory`.
pub async fn extract_uniform_frames(
    executable: &Path,
    input: &Path,
    output_directory: &Path,
    plan: &FramePlan,
    log_path: Option<PathBuf>,
    process_manager: &ProcessManager,
    observer: Option<ProcessObserver>,
) -> Result<u64> {
    if !input.is_file() {
        return Err(SplatError::InvalidPath(input.to_path_buf()));
    }
    tokio::fs::create_dir_all(output_directory).await?;
    let output_pattern = output_directory.join("frame_%06d.jpg");
    let filter = format!("fps={:.8}", plan.sampling_fps,);
    let args = vec![
        std::ffi::OsString::from("-hide_banner"),
        std::ffi::OsString::from("-y"),
        std::ffi::OsString::from("-i"),
        input.as_os_str().to_owned(),
        std::ffi::OsString::from("-vf"),
        std::ffi::OsString::from(filter),
        std::ffi::OsString::from("-q:v"),
        std::ffi::OsString::from("2"),
        std::ffi::OsString::from("-start_number"),
        std::ffi::OsString::from("1"),
        output_pattern.as_os_str().to_owned(),
    ];
    let output = process_manager
        .run(ProcessSpec {
            executable: executable.to_path_buf(),
            args,
            working_directory: output_directory.parent().map(Path::to_path_buf),
            log_path,
            observer,
        })
        .await?;
    if output.cancelled {
        return Err(SplatError::Cancelled);
    }
    if !output.success {
        return Err(SplatError::Process(format!(
            "FFmpeg 退出码 {:?}",
            output.exit_code
        )));
    }
    let mut count = 0;
    let mut entries = tokio::fs::read_dir(output_directory).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry
            .path()
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jpg"))
        {
            count += 1;
        }
    }
    Ok(count)
}
