use std::path::PathBuf;

use serde::Deserialize;

use crate::{
    error::{Result, SplatError},
    video::VideoInfo,
};

#[derive(Debug, Deserialize)]
struct ProbeRoot {
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}

#[derive(Debug, Deserialize)]
struct FrameProbeRoot {
    #[serde(default)]
    frames: Vec<FrameProbeEntry>,
}

#[derive(Debug, Deserialize)]
struct FrameProbeEntry {
    media_type: Option<String>,
    best_effort_timestamp_time: Option<String>,
}

/// A decoded source-frame index reported by FFmpeg's FPS mapping diagnostics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceFrameTimestamp {
    pub source_index: u64,
    pub pts_seconds: f64,
}

#[derive(Debug, Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
    format_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProbeStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    nb_frames: Option<String>,
    pix_fmt: Option<String>,
    #[serde(default)]
    tags: Option<ProbeTags>,
}

#[derive(Debug, Deserialize, Default)]
struct ProbeTags {
    rotate: Option<String>,
}

/// Parse the JSON payload printed by `ffprobe -print_format json -show_streams -show_format`.
/// 解析逻辑只关心我们需要的字段，对新版本 ffprobe 引入的新字段保持宽容。
pub fn parse_ffprobe(payload: &str) -> Result<VideoInfo> {
    let root: ProbeRoot = serde_json::from_str(payload)
        .map_err(|error| SplatError::InvalidVideo(format!("无法解析 ffprobe 输出：{error}")))?;
    let video = root
        .streams
        .into_iter()
        .find(|stream| stream.codec_type.as_deref() == Some("video"))
        .ok_or_else(|| SplatError::InvalidVideo("未发现视频流".into()))?;
    let duration = root
        .format
        .as_ref()
        .and_then(|f| f.duration.as_ref())
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0);
    let fps = video
        .avg_frame_rate
        .as_deref()
        .and_then(parse_fraction)
        .or_else(|| video.r_frame_rate.as_deref().and_then(parse_fraction))
        .unwrap_or(30.0);
    let frame_count = video
        .nb_frames
        .as_deref()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or_else(|| (duration * fps).round() as u64);
    let width = video.width.unwrap_or(0);
    let height = video.height.unwrap_or(0);
    let container = root
        .format
        .as_ref()
        .and_then(|f| f.format_name.clone())
        .unwrap_or_default();
    let rotation = video
        .tags
        .as_ref()
        .and_then(|tags| tags.rotate.as_ref())
        .and_then(|value| value.parse::<f64>().ok())
        .map(|value| value.round() as i32)
        .unwrap_or(0);
    Ok(VideoInfo {
        path: PathBuf::new(),
        duration,
        fps,
        width,
        height,
        frame_count,
        container,
        video_codec: video.codec_name,
        pixel_format: video.pix_fmt,
        rotation,
    })
}

fn parse_fraction(text: &str) -> Option<f64> {
    let (num, den) = text.split_once('/')?;
    let num: f64 = num.parse().ok()?;
    let den: f64 = den.parse().ok()?;
    if den == 0.0 {
        None
    } else {
        Some(num / den)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_probe_payload() {
        let payload = r#"{
            "streams": [
                {
                    "codec_type": "video",
                    "codec_name": "h264",
                    "width": 1920,
                    "height": 1080,
                    "avg_frame_rate": "30/1",
                    "nb_frames": "900",
                    "pix_fmt": "yuv420p"
                }
            ],
            "format": {
                "duration": "30.000000",
                "format_name": "mov,mp4,m4a,3gp,3g2,mj2"
            }
        }"#;
        let info = parse_ffprobe(payload).unwrap();
        assert_eq!(info.width, 1920);
        assert_eq!(info.height, 1080);
        assert!((info.fps - 30.0).abs() < 1e-6);
        assert_eq!(info.frame_count, 900);
    }

    #[test]
    fn rejects_payload_without_video_stream() {
        let payload = r#"{"streams":[{"codec_type":"audio"}],"format":{}}"#;
        let err = parse_ffprobe(payload).unwrap_err();
        assert!(matches!(err, SplatError::InvalidVideo(_)));
    }

    #[test]
    fn parses_the_field_subset_requested_by_the_engine_probe() {
        let payload = r#"{"streams":[{"codec_type":"video","codec_name":"hevc","width":3840,"height":2160,"avg_frame_rate":"60000/1001","pix_fmt":"yuv420p"}],"format":{"duration":"1.0","format_name":"mov"}}"#;
        let info = parse_ffprobe(payload).unwrap();
        assert_eq!(info.video_codec.as_deref(), Some("hevc"));
        assert_eq!(info.pixel_format.as_deref(), Some("yuv420p"));
    }

    #[test]
    fn parses_vfr_timestamps_without_using_average_fps() {
        let frames = parse_frame_timestamps(r#"{"frames":[
            {"media_type":"video","best_effort_timestamp_time":"0.000000"},
            {"media_type":"video","best_effort_timestamp_time":"0.033367"},
            {"media_type":"video","best_effort_timestamp_time":"0.101000"}
        ]}"#).unwrap();
        assert_eq!(frames[2], SourceFrameTimestamp { source_index: 2, pts_seconds: 0.101 });
    }

    #[test]
    fn rejects_non_monotonic_frame_pts() {
        let error = parse_frame_timestamps(r#"{"frames":[
            {"media_type":"video","best_effort_timestamp_time":"1.0"},
            {"media_type":"video","best_effort_timestamp_time":"0.9"}
        ]}"#).unwrap_err();
        assert!(error.to_string().contains("单调递增"));
    }

    #[test]
    fn parses_compact_csv_timestamps_without_json_overhead() {
        let frames = parse_frame_timestamp_lines("0.000000\n0.033367\n0.101000\n").unwrap();
        assert_eq!(frames[2], SourceFrameTimestamp { source_index: 2, pts_seconds: 0.101 });
    }
}

/// Parse `ffprobe -show_frames` output into presentation-ordered PTS values.
/// This deliberately does not derive time from average FPS, so VFR input can
/// later be extracted through its exact decoded source indices.
pub fn parse_frame_timestamps(payload: &str) -> Result<Vec<SourceFrameTimestamp>> {
    let root: FrameProbeRoot = serde_json::from_str(payload)
        .map_err(|error| SplatError::InvalidVideo(format!("无法解析帧级 FFprobe 输出：{error}")))?;
    let mut timestamps = Vec::new();
    let mut last_pts = f64::NEG_INFINITY;
    for frame in root.frames {
        if frame.media_type.as_deref() != Some("video") {
            continue;
        }
        let Some(pts_seconds) = frame
            .best_effort_timestamp_time
            .as_deref()
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite())
        else {
            continue;
        };
        if pts_seconds + f64::EPSILON < last_pts {
            return Err(SplatError::InvalidVideo(
                "FFprobe 返回的显示时间戳不是单调递增，无法安全执行自适应抽帧".into(),
            ));
        }
        timestamps.push(SourceFrameTimestamp {
            source_index: timestamps.len() as u64,
            pts_seconds,
        });
        last_pts = pts_seconds;
    }
    if timestamps.is_empty() {
        return Err(SplatError::InvalidVideo(
            "视频没有可用的帧级显示时间戳，无法执行自适应抽帧".into(),
        ));
    }
    Ok(timestamps)
}

/// Parse the single-column CSV emitted by the adaptive ffprobe path. It avoids
/// retaining a JSON object per decoded frame while preserving the decoder's
/// presentation-order, best-effort timestamps.
pub fn parse_frame_timestamp_lines(payload: &str) -> Result<Vec<SourceFrameTimestamp>> {
    let mut timestamps = Vec::new();
    let mut last_pts = f64::NEG_INFINITY;
    for (line_number, line) in payload.lines().enumerate() {
        let value = line.trim().split(',').next().unwrap_or_default();
        let pts_seconds = value.parse::<f64>().ok().filter(|value| value.is_finite())
            .ok_or_else(|| SplatError::InvalidVideo(format!(
                "FFprobe 第 {} 个帧时间戳无效：{line}", line_number + 1
            )))?;
        if pts_seconds + f64::EPSILON < last_pts {
            return Err(SplatError::InvalidVideo(
                "FFprobe 返回的显示时间戳不是单调递增，无法安全执行自适应抽帧".into(),
            ));
        }
        timestamps.push(SourceFrameTimestamp {
            source_index: timestamps.len() as u64,
            pts_seconds,
        });
        last_pts = pts_seconds;
    }
    if timestamps.is_empty() {
        return Err(SplatError::InvalidVideo("FFprobe 未返回任何视频帧时间戳".into()));
    }
    Ok(timestamps)
}
