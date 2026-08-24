use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    engines::{ColmapBackend, MapperBaMode, TrainingBackend},
    presets::{BrushTrainingPreset, GsplatSplatCap, Quality},
    video::VideoInfo,
};

pub mod event;
pub mod progress;
pub mod runner;
pub mod state;

pub use event::*;
pub use progress::*;
pub use runner::*;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PipelineStage {
    ProbingVideo,
    PlanningFrames,
    ExtractingFrames,
    SelectingFrames,
    ExtractingFeatures,
    Matching,
    Reconstructing,
    ValidatingReconstruction,
    NeedsSupplement,
    TrainingSplats,
    Exporting,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PipelineEngine {
    System,
    Ffmpeg,
    Colmap,
    Brush,
    Gsplat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameState {
    pub sampling_fps: f64,
    pub source_fps: f64,
    pub source_duration: f64,
    pub estimated_frames: u64,
    #[serde(default)]
    pub strategy: crate::video::FrameSelectionStrategyKind,
    #[serde(default)]
    pub anchor_fps: Option<f64>,
    #[serde(default)]
    pub analysis_fps: Option<f64>,
    #[serde(default)]
    pub effective_fps: Option<f64>,
    #[serde(default)]
    pub proxy_candidates: Option<u64>,
    #[serde(default)]
    pub adaptive_fallback_used: bool,
    #[serde(default)]
    pub adaptive_fallback_reason: Option<String>,
    #[serde(default = "default_auto_bridge_frames")]
    pub auto_bridge_frames: bool,
    pub extracted_frames: Option<u64>,
    #[serde(default)]
    pub selected_frames: Option<u64>,
    #[serde(default)]
    pub removed_near_duplicates: Option<u64>,
}

impl FrameState {
    pub fn from(plan: &crate::video::FramePlan) -> Self {
        Self {
            sampling_fps: plan.sampling_fps,
            source_fps: plan.source_fps,
            source_duration: plan.source_duration,
            estimated_frames: plan.estimated_frames,
            strategy: plan.strategy,
            anchor_fps: plan.anchor_fps,
            analysis_fps: plan.analysis_fps,
            effective_fps: plan.effective_fps,
            proxy_candidates: plan.proxy_candidates,
            adaptive_fallback_used: false,
            adaptive_fallback_reason: None,
            auto_bridge_frames: true,
            extracted_frames: None,
            selected_frames: None,
            removed_near_duplicates: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectMetadata {
    pub id: uuid::Uuid,
    pub name: String,
    pub source_path: PathBuf,
    pub quality: Quality,
    pub status: ProjectStatus,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u64>,
    pub output: Option<ProjectOutput>,
    pub failure_message: Option<String>,
    pub app_id: String,
    #[serde(default)]
    pub training_backend: TrainingBackend,
    #[serde(default)]
    pub colmap_execution: ColmapExecution,
    #[serde(default)]
    pub brush_training_preset: BrushTrainingPreset,
    #[serde(default)]
    pub gsplat_splat_cap: GsplatSplatCap,
    #[serde(default)]
    pub timings: PipelineTimings,
    #[serde(default)]
    pub needs_supplement: Option<SupplementRequirement>,
}

fn default_auto_bridge_frames() -> bool { true }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineTimings {
    pub probe_ms: u64,
    pub extract_ms: u64,
    pub select_ms: u64,
    #[serde(default)]
    pub frame_analysis_ms: u64,
    #[serde(default)]
    pub adaptive_planning_ms: u64,
    #[serde(default)]
    pub selected_extraction_ms: u64,
    pub colmap_features_ms: u64,
    pub colmap_matching_ms: u64,
    pub colmap_mapping_ms: u64,
    pub training_input_ms: u64,
    pub training_ms: u64,
    pub ply_validation_ms: u64,
    pub total_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColmapExecution {
    pub requested_backend: Option<ColmapBackend>,
    pub requested_ba_mode: Option<MapperBaMode>,
    pub effective_backend: Option<ColmapBackend>,
    pub feature_compute_device: Option<String>,
    pub matching_compute_device: Option<String>,
    pub gpu_index: Option<i32>,
    pub cuda_fallback_used: bool,
    pub cuda_fallback_reason: Option<String>,
    pub effective_ba_backend: Option<String>,
    pub caspar_fallback_used: bool,
    pub caspar_fallback_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProjectStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    NeedsSupplement,
}

/// Persisted hand-off after all active workers have exited. The project can be
/// safely closed and reopened while the UI waits for supplemental media.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupplementRequirement {
    pub reason: String,
    pub weak_interval_count: u64,
    pub diagnostics_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
pub struct PipelineStateFile {
    pub schema_version: u32,
    pub stage: PipelineStage,
    pub quality: Quality,
    pub video: Option<VideoInfo>,
    pub frames: Option<FrameState>,
    pub features_complete: bool,
    pub matching_complete: bool,
    pub reconstruction_complete: bool,
    pub brush_complete: bool,
    #[serde(default)]
    pub training_input_complete: bool,
    #[serde(default)]
    pub training_backend: TrainingBackend,
    #[serde(default)]
    pub colmap_execution: ColmapExecution,
    #[serde(default = "default_auto_bridge_frames")]
    pub auto_bridge_frames: bool,
    #[serde(default)]
    pub needs_supplement: Option<SupplementRequirement>,
}

impl PipelineStateFile {
    pub fn created(quality: Quality) -> Self {
        Self {
            schema_version: 1,
            stage: PipelineStage::ProbingVideo,
            quality,
            video: None,
            frames: None,
            features_complete: false,
            matching_complete: false,
            reconstruction_complete: false,
            brush_complete: false,
            training_input_complete: false,
            training_backend: TrainingBackend::Brush,
            colmap_execution: ColmapExecution::default(),
            auto_bridge_frames: true,
            needs_supplement: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPaths {
    pub project: PathBuf,
    pub frames: PathBuf,
    pub colmap: PathBuf,
    pub brush: PathBuf,
    pub training_input: PathBuf,
    pub gsplat: PathBuf,
    pub logs: PathBuf,
    pub metadata: PathBuf,
    pub state: PathBuf,
}

pub const PROJECT_APP_ID: &str = "studio.ooo.splat";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_supplement_state_round_trips_for_reopen() {
        let mut state = PipelineStateFile::created(Quality::Standard);
        state.stage = PipelineStage::NeedsSupplement;
        state.needs_supplement = Some(SupplementRequirement {
            reason: "自动补帧已关闭，检测到未注册关键帧弱区".into(),
            weak_interval_count: 2,
            diagnostics_path: PathBuf::from("logs/adaptive-registered-frames.json"),
        });

        let restored: PipelineStateFile = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert_eq!(restored.stage, PipelineStage::NeedsSupplement);
        assert_eq!(restored.needs_supplement.unwrap().weak_interval_count, 2);
    }
}
