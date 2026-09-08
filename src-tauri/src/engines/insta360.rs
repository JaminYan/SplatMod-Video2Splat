use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use crate::{
    engines::{ffmpeg::convert_equirectangular_video, FfmpegHwAccel},
    error::{Result, SplatError},
    process::{ProcessManager, ProcessSpec},
};

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
    let perspective = work_dir.join("perspective.mp4");
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
    // ponytail: one fixed perspective view keeps the existing SfM contract;
    // add multi-view panorama tracks only after COLMAP evidence justifies it.
    convert_equirectangular_video(
        ffmpeg,
        &stitched,
        &perspective,
        hw_accel,
        ffmpeg_log,
        process_manager,
        None,
    )
    .await?;
    Ok(perspective)
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
