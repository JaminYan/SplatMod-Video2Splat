use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::pipeline::{PipelineEngine, PipelineStage};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EventKind {
    Stage,
    Progress,
    Log,
    Heartbeat,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EventLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineEvent {
    pub sequence: u64,
    pub timestamp: DateTime<Utc>,
    pub kind: EventKind,
    pub level: EventLevel,
    pub stage: PipelineStage,
    pub engine: Option<PipelineEngine>,
    pub progress: f32,
    pub stage_progress: Option<f32>,
    pub indeterminate: bool,
    pub message: String,
    pub current: Option<u64>,
    pub total: Option<u64>,
    pub unit: Option<String>,
    pub elapsed_ms: u64,
}

impl PipelineEvent {
    pub fn mapped(stage: PipelineStage, progress: f32, message: impl Into<String>) -> Self {
        Self {
            sequence: 0,
            timestamp: Utc::now(),
            kind: EventKind::Stage,
            level: EventLevel::Info,
            stage,
            engine: Some(PipelineEngine::System),
            progress,
            stage_progress: Some(progress * 100.0),
            indeterminate: false,
            message: message.into(),
            current: None,
            total: None,
            unit: None,
            elapsed_ms: 0,
        }
    }
}
