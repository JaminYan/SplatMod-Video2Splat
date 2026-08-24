use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    pipeline::PipelineStage,
    presets::Quality,
    video::{FramePlan, VideoInfo},
};

pub const PROJECT_APP_ID: &str = "studio.ooo.splat";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectStatus {
    #[default]
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectOutput {
    pub final_ply: PathBuf,
    pub file_size: u64,
    pub splat_count: u64,
    pub input_images: u64,
    pub registered_images: u64,
    pub registered_ratio: f64,
    pub points_3d: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectMetadata {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub app_id: String,
    pub id: Uuid,
    #[serde(default)]
    pub name: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub status: ProjectStatus,
    pub source_path: PathBuf,
    pub quality: Quality,
    #[serde(default)]
    pub project_path: PathBuf,
    #[serde(default)]
    pub output_path: Option<PathBuf>,
    #[serde(default)]
    pub output: Option<ProjectOutput>,
    #[serde(default)]
    pub failure_message: Option<String>,
}

const fn schema_version() -> u32 {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameState {
    pub retention_ratio: f64,
    pub sampling_fps: f64,
    pub estimated_frames: u64,
    pub extracted_frames: Option<u64>,
}

impl From<&FramePlan> for FrameState {
    fn from(plan: &FramePlan) -> Self {
        Self {
            retention_ratio: plan.retention_ratio,
            sampling_fps: plan.sampling_fps,
            estimated_frames: plan.estimated_frames,
            extracted_frames: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineStateFile {
    pub stage: PipelineStage,
    pub preset: Quality,
    pub video: Option<VideoInfo>,
    pub frames: Option<FrameState>,
    pub features_complete: bool,
    pub matching_complete: bool,
    pub reconstruction_complete: bool,
    pub brush_complete: bool,
}

impl PipelineStateFile {
    pub fn created(preset: Quality) -> Self {
        Self {
            stage: PipelineStage::Created,
            preset,
            video: None,
            frames: None,
            features_complete: false,
            matching_complete: false,
            reconstruction_complete: false,
            brush_complete: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn state_uses_frame_strategy_vocabulary_only() {
        let json = serde_json::to_string(&PipelineStateFile::created(Quality::Balanced)).unwrap();
        assert!(json.contains("\"preset\":\"balanced\""));
        assert!(!json.contains("targetFrames"));
    }
}
