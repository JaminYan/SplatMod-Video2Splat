use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use crate::{
    engines::{ffmpeg::convert_equirectangular_video_views, ffprobe::probe_video, FfmpegHwAccel},
    error::{Result, SplatError},
    process::{ProcessManager, ProcessSpec},
};

pub const PANORAMA_VIEW_COUNT: u32 = 4;

#[derive(Debug, Clone)]
pub struct Insta360RigDataset {
    pub images_dir: PathBuf,
    pub rig_config: PathBuf,
    pub frame_count: u64,
    pub duration: f64,
    pub sample_fps: f64,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Insta360SdkStatus {
    pub configured: bool,
    pub path: Option<PathBuf>,
    pub executable: Option<PathBuf>,
    pub models_present: bool,
    pub ready: bool,
    pub detail: String,
}

pub fn is_insv(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("insv"))
}

fn resolve_media_sdk_test(configured: Option<&Path>) -> Option<PathBuf> {
    let configured = configured
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("OOOSPLAT_INSTA360_SDK_DIR").map(PathBuf::from))?;
    Some(if configured.is_file() {
        configured
    } else {
        configured.join("bin").join("MediaSDKTest.exe")
    })
}

pub fn sdk_status(configured: Option<&Path>) -> Insta360SdkStatus {
    let executable = resolve_media_sdk_test(configured);
    let path = executable
        .as_ref()
        .and_then(|value| value.parent())
        .and_then(|value| value.parent())
        .map(PathBuf::from);
    let models_present = executable
        .as_ref()
        .and_then(|value| value.parent())
        .map(|value| value.join("models").is_dir())
        .unwrap_or(false);
    let ready = executable.as_ref().is_some_and(|value| value.is_file()) && models_present;
    let detail = if ready {
        "MediaSDKTest.exe 与 models 已就绪"
    } else if executable.is_some() {
        "已配置 SDK，但缺少 MediaSDKTest.exe 或 models"
    } else {
        "未配置；选择 INSV 前请设置 MediaSDK 路径"
    };
    Insta360SdkStatus {
        configured: executable.is_some(),
        path,
        executable,
        models_present,
        ready,
        detail: detail.into(),
    }
}

fn media_sdk_test() -> Result<PathBuf> {
    let executable = resolve_media_sdk_test(None).ok_or_else(|| {
        SplatError::UnsupportedEngine(
            "检测到 INSV；请设置 OOOSPLAT_INSTA360_SDK_DIR 指向 Insta360 MediaSDK 根目录".into(),
        )
    })?;
    if executable.is_file() {
        Ok(executable)
    } else {
        Err(SplatError::EngineMissing(format!(
            "Insta360 MediaSDKTest.exe：{}",
            executable.display()
        )))
    }
}

/// M1 deliberately uses the vendor sample executable and a temporary stitched
/// MP4. Upgrade to selected-frame export after source-index mapping is proven.
pub async fn prepare_video(
    input: &Path,
    work_dir: &Path,
    ffprobe: &Path,
    ffmpeg: &Path,
    hw_accel: FfmpegHwAccel,
    process_manager: &ProcessManager,
    sdk_log: Option<PathBuf>,
    ffmpeg_log: Option<PathBuf>,
) -> Result<PathBuf> {
    if !input.is_file() {
        return Err(SplatError::InvalidPath(input.to_path_buf()));
    }
    let sdk_test = media_sdk_test()?;
    tokio::fs::create_dir_all(work_dir).await?;
    let stitched = work_dir.join("stitched-equirect.mp4");
    let perspective = work_dir.join("perspective-4view.mp4");
    if perspective.is_file() {
        return Ok(perspective);
    }
    if stitched.exists() {
        return Err(SplatError::Process(format!(
            "检测到未完成的 Insta360 拼接输出，拒绝混用：{}",
            stitched.display()
        )));
    }
    let result = process_manager
        .run(ProcessSpec {
            executable: sdk_test,
            args: vec![
                OsString::from("-inputs"),
                input.as_os_str().to_owned(),
                OsString::from("-output"),
                stitched.as_os_str().to_owned(),
                OsString::from("-output_size"),
                OsString::from("3840x1920"),
                OsString::from("-stitch_type"),
                OsString::from("optflow"),
                OsString::from("--log_level"),
                OsString::from("info"),
            ],
            working_directory: work_dir.parent().map(Path::to_path_buf),
            log_path: sdk_log,
            observer: None,
        })
        .await?;
    if !result.success {
        return Err(SplatError::Process(format!(
            "Insta360 MediaSDK 拼接退出码 {:?}",
            result.exit_code
        )));
    }
    if !stitched.is_file() {
        return Err(SplatError::Process(
            "Insta360 MediaSDK 未输出拼接视频".into(),
        ));
    }
    let stitched_info = probe_video(ffprobe, &stitched, None).await?;
    let output_fps = (stitched_info.fps.max(1.0) * PANORAMA_VIEW_COUNT as f64)
        .round()
        .clamp(1.0, 240.0);
    convert_equirectangular_video_views(
        ffmpeg,
        &stitched,
        &perspective,
        &[0, 90, 180, -90],
        output_fps,
        hw_accel,
        ffmpeg_log,
        process_manager,
        None,
    )
    .await?;
    Ok(perspective)
}

/// Extract the two native fisheye streams as a COLMAP rig dataset. The files
/// are deliberately kept in separate camera folders with identical frame
/// names so COLMAP can group them into synchronized rig frames.
pub async fn prepare_rig_dataset(
    input: &Path,
    output_dir: &Path,
    ffprobe: &Path,
    ffmpeg: &Path,
    sample_fps: f64,
    process_manager: &ProcessManager,
    log_path: Option<PathBuf>,
    observer: Option<crate::process::ProcessObserver>,
) -> Result<Option<Insta360RigDataset>> {
    if !input.is_file() {
        return Err(SplatError::InvalidPath(input.to_path_buf()));
    }
    if !sample_fps.is_finite() || sample_fps <= 0.0 {
        return Err(SplatError::Process("Insta360 rig 抽帧 FPS 无效".into()));
    }
    let probe = process_manager
        .run(ProcessSpec {
            executable: ffprobe.to_path_buf(),
            args: vec![
                OsString::from("-v"),
                OsString::from("error"),
                OsString::from("-show_entries"),
                OsString::from("stream=index,codec_type,width,height:format=duration"),
                OsString::from("-of"),
                OsString::from("json"),
                input.as_os_str().to_owned(),
            ],
            working_directory: input.parent().map(Path::to_path_buf),
            log_path: log_path.clone(),
            observer: None,
        })
        .await?;
    if probe.cancelled {
        return Err(SplatError::Cancelled);
    }
    if !probe.success {
        return Err(SplatError::InvalidVideo("FFprobe 无法读取 INSV 双流信息".into()));
    }
    let value: serde_json::Value = serde_json::from_str(&probe.stdout)
        .map_err(|error| SplatError::InvalidVideo(format!("INSV 流信息无效：{error}")))?;
    let video_streams = value
        .get("streams")
        .and_then(serde_json::Value::as_array)
        .map(|streams| {
            streams
                .iter()
                .filter(|stream| stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("video"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if video_streams.len() < 2 {
        return Ok(None);
    }
    let width = video_streams[0]
        .get("width")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32;
    let height = video_streams[0]
        .get("height")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32;
    if width == 0 || height == 0 || width != height {
        return Ok(None);
    }
    let duration = value
        .get("format")
        .and_then(|format| format.get("duration"))
        .and_then(serde_json::Value::as_str)
        .and_then(|duration| duration.parse::<f64>().ok())
        .unwrap_or(0.0);
    let camera_1 = output_dir.join("rig1").join("camera1");
    let camera_2 = output_dir.join("rig1").join("camera2");
    let rig_config = output_dir
        .parent()
        .unwrap_or(output_dir)
        .join("rig_config.json");
    let legacy_rig_config = output_dir.join("rig_config.json");
    if output_dir.is_dir() && !rig_config.is_file() && jpeg_count(output_dir).await? > 0 {
        return Err(SplatError::Process(
            "Insta360 帧目录已包含旧的平面图像，拒绝与双鱼眼 rig 混用".into(),
        ));
    }
    if legacy_rig_config.is_file() {
        tokio::fs::remove_file(&legacy_rig_config).await?;
    }
    let existing_first = jpeg_count(&camera_1).await.unwrap_or(0);
    let existing_second = jpeg_count(&camera_2).await.unwrap_or(0);
    if !rig_config.is_file() || existing_first == 0 || existing_first != existing_second {
        if camera_1.is_dir() {
            tokio::fs::remove_dir_all(&camera_1).await?;
        }
        if camera_2.is_dir() {
            tokio::fs::remove_dir_all(&camera_2).await?;
        }
        if rig_config.is_file() {
            tokio::fs::remove_file(&rig_config).await?;
        }
        tokio::fs::create_dir_all(&camera_1).await?;
        tokio::fs::create_dir_all(&camera_2).await?;
        extract_rig_stream(
            ffmpeg,
            input,
            &camera_1,
            0,
            sample_fps,
            process_manager,
            log_path.clone(),
            observer.clone(),
        )
        .await?;
        extract_rig_stream(
            ffmpeg,
            input,
            &camera_2,
            1,
            sample_fps,
            process_manager,
            log_path,
            observer,
        )
        .await?;
        let first = jpeg_count(&camera_1).await?;
        let second = jpeg_count(&camera_2).await?;
        if first == 0 || first != second {
            return Err(SplatError::Process(format!(
                "Insta360 双镜头抽帧数量不一致：camera1={first}, camera2={second}"
            )));
        }
        let config = serde_json::json!([{
            "cameras": [
                {"image_prefix": "rig1/camera1/", "ref_sensor": true},
                {"image_prefix": "rig1/camera2/"}
            ]
        }]);
        tokio::fs::write(&rig_config, serde_json::to_vec_pretty(&config)?).await?;
    }
    let frame_count = jpeg_count(&camera_1).await?;
    Ok(Some(Insta360RigDataset {
        images_dir: output_dir.to_path_buf(),
        rig_config,
        frame_count,
        duration,
        sample_fps,
        width,
        height,
    }))
}

async fn extract_rig_stream(
    executable: &Path,
    input: &Path,
    output_dir: &Path,
    stream_index: u32,
    sample_fps: f64,
    process_manager: &ProcessManager,
    log_path: Option<PathBuf>,
    observer: Option<crate::process::ProcessObserver>,
) -> Result<()> {
    let output = process_manager
        .run(ProcessSpec {
            executable: executable.to_path_buf(),
            args: vec![
                OsString::from("-hide_banner"),
                OsString::from("-nostdin"),
                OsString::from("-y"),
                OsString::from("-i"),
                input.as_os_str().to_owned(),
                OsString::from("-map"),
                OsString::from(format!("0:{stream_index}")),
                OsString::from("-vf"),
                OsString::from(format!("fps={sample_fps:.6}:round=down")),
                OsString::from("-q:v"),
                OsString::from("2"),
                OsString::from("-start_number"),
                OsString::from("1"),
                output_dir.join("frame_%06d.jpg").as_os_str().to_owned(),
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
        return Err(SplatError::Process(format!(
            "Insta360 原始鱼眼流 {stream_index} 抽帧退出码 {:?}",
            output.exit_code
        )));
    }
    Ok(())
}

async fn jpeg_count(directory: &Path) -> Result<u64> {
    let mut entries = tokio::fs::read_dir(directory).await?;
    let mut count = 0;
    while let Some(entry) = entries.next_entry().await? {
        if entry.path().is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("jpg"))
        {
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::is_insv;
    use std::path::Path;

    #[test]
    fn recognizes_insv_case_insensitively() {
        assert!(is_insv(Path::new("clip.INSV")));
        assert!(!is_insv(Path::new("clip.mp4")));
    }
}
