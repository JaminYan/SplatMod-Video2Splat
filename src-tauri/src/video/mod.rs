use std::path::PathBuf;

use serde::{Deserialize, Serialize};

mod adaptive;
mod extract;
mod frame_plan;
mod probe;
mod select;

pub use adaptive::*;
pub use extract::*;
pub use frame_plan::*;
pub use probe::*;
pub use select::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoInfo {
    pub path: PathBuf,
    pub duration: f64,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    pub frame_count: u64,
    pub container: String,
    pub video_codec: Option<String>,
    pub pixel_format: Option<String>,
    pub rotation: i32,
}
