use serde::{Deserialize, Serialize};

use crate::{presets::QualityPreset, video::VideoInfo};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FrameSelectionStrategyKind {
    UniformRatio,
    AdaptiveSfm,
}

impl Default for FrameSelectionStrategyKind {
    fn default() -> Self {
        Self::UniformRatio
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FramePlan {
    pub sampling_fps: f64,
    pub estimated_frames: u64,
    pub source_fps: f64,
    pub source_duration: f64,
    #[serde(default)]
    pub strategy: FrameSelectionStrategyKind,
    /// Fixed-rate baseline used for capacity estimates and a possible later
    /// isolated fallback. It is not an assertion that adaptive extraction ran.
    #[serde(default)]
    pub anchor_fps: Option<f64>,
    #[serde(default)]
    pub analysis_fps: Option<f64>,
    #[serde(default)]
    pub effective_fps: Option<f64>,
    #[serde(default)]
    pub proxy_candidates: Option<u64>,
}

pub trait FrameSelectionStrategy {
    fn create_plan(&self, video: &VideoInfo, preset: &QualityPreset) -> FramePlan;
}

/// Default strategy: use a bounded candidate FPS, independent of source FPS.
#[derive(Debug, Clone, Copy)]
pub struct UniformRatioFrameSelection;

impl FrameSelectionStrategy for UniformRatioFrameSelection {
    fn create_plan(&self, video: &VideoInfo, preset: &QualityPreset) -> FramePlan {
        let source_fps = if video.fps > 0.0 { video.fps } else { 30.0 };
        let sampling_fps = preset.target_sampling_fps.clamp(0.1, source_fps);
        let estimated = ((video.duration.max(0.0)) * sampling_fps).round() as u64;
        FramePlan {
            sampling_fps,
            estimated_frames: estimated.max(1),
            source_fps,
            source_duration: video.duration,
            strategy: FrameSelectionStrategyKind::UniformRatio,
            anchor_fps: Some(sampling_fps),
            analysis_fps: None,
            effective_fps: Some(sampling_fps),
            proxy_candidates: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn target_fps_does_not_scale_with_source_fps() {
        let video = VideoInfo {
            path: PathBuf::from("input.mp4"),
            duration: 60.0,
            fps: 60.0,
            width: 1920,
            height: 1080,
            frame_count: 3600,
            container: "mp4".into(),
            video_codec: None,
            pixel_format: None,
            rotation: 0,
        };
        let plan = UniformRatioFrameSelection
            .create_plan(&video, &crate::presets::Quality::Standard.preset());
        assert_eq!(plan.sampling_fps, 2.0);
        assert_eq!(plan.estimated_frames, 120);
    }
}
