use std::{ffi::OsString, path::Path};

use crate::{
    error::{Result, SplatError},
    process::{ProcessManager, ProcessObserver, ProcessSpec},
    video::{parse_ffprobe, parse_frame_timestamp_lines, SourceFrameTimestamp, VideoInfo},
};

pub async fn probe_video(
    executable: &Path,
    input: &Path,
    log_path: Option<std::path::PathBuf>,
) -> Result<VideoInfo> {
    if !input.is_file() {
        return Err(SplatError::InvalidPath(input.to_path_buf()));
    }
    let output = ProcessManager::new().run(ProcessSpec {
        executable: executable.to_path_buf(),
        args: vec![
            OsString::from("-v"), OsString::from("error"),
            OsString::from("-select_streams"), OsString::from("v:0"),
            OsString::from("-show_entries"),
            // `parse_ffprobe` identifies the selected stream by `codec_type`; keep it
            // in the response even though `-select_streams v:0` already narrowed it.
            OsString::from("stream=codec_type,codec_name,width,height,avg_frame_rate,r_frame_rate,nb_frames,pix_fmt:stream_tags=rotate:stream_side_data=rotation:format=duration,format_name"),
            OsString::from("-of"), OsString::from("json"),
            input.as_os_str().to_owned(),
        ],
        working_directory: input.parent().map(Path::to_path_buf),
        log_path,
        observer: None,
    }).await?;
    if !output.success {
        return Err(SplatError::InvalidVideo("FFprobe 无法解码这个文件".into()));
    }
    parse_ffprobe(&output.stdout)
}

/// Read presentation-order timestamps only for adaptive planning. The fixed
/// path retains its existing lightweight stream probe.
pub async fn probe_frame_timestamps(
    executable: &Path,
    input: &Path,
    log_path: Option<std::path::PathBuf>,
    process_manager: &ProcessManager,
    observer: Option<ProcessObserver>,
) -> Result<Vec<SourceFrameTimestamp>> {
    if !input.is_file() {
        return Err(SplatError::InvalidPath(input.to_path_buf()));
    }
    let output = process_manager
        .run(ProcessSpec {
            executable: executable.to_path_buf(),
            args: vec![
                OsString::from("-v"),
                OsString::from("error"),
                OsString::from("-select_streams"),
                OsString::from("v:0"),
                OsString::from("-show_frames"),
                OsString::from("-show_entries"),
                OsString::from("frame=best_effort_timestamp_time"),
                OsString::from("-of"),
                OsString::from("csv=p=0"),
                input.as_os_str().to_owned(),
            ],
            working_directory: input.parent().map(Path::to_path_buf),
            log_path,
            observer,
        })
        .await?;
    if output.cancelled {
        return Err(SplatError::Cancelled);
    }
    if !output.success {
        return Err(SplatError::InvalidVideo(
            "FFprobe 无法读取帧级时间戳".into(),
        ));
    }
    parse_frame_timestamp_lines(&output.stdout)
}
