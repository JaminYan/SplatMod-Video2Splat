use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BrushTrainingPreset {
    #[default]
    A,
    B,
    C,
}

/// gsplat 的 cap 是内存/模型体积的硬上限，不是画质承诺。
/// `Auto` 保留当前质量档的意图，但永不超过实验后端的 4M 安全上限。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GsplatSplatCap {
    #[default]
    Auto,
    #[serde(rename = "1m")]
    M1,
    #[serde(rename = "2m")]
    M2,
    #[serde(rename = "4m")]
    M4,
}

impl GsplatSplatCap {
    pub fn limit(self, quality_cap: u32) -> u32 {
        match self {
            Self::Auto => quality_cap.min(4_000_000),
            Self::M1 => 1_000_000,
            Self::M2 => 2_000_000,
            Self::M4 => 4_000_000,
        }
    }
}

impl BrushTrainingPreset {
    pub fn apply(self, mut preset: QualityPreset) -> QualityPreset {
        let (iterations, resolution, max_splats) = match self {
            Self::A => (
                preset.brush_iterations,
                preset.brush_max_resolution.min(1920),
                match preset.target_sampling_fps as u32 {
                    1 => 1_500_000,
                    2 => 3_000_000,
                    _ => 5_000_000,
                },
            ),
            Self::B => (
                preset.brush_iterations,
                preset.brush_max_resolution.min(1536),
                match preset.target_sampling_fps as u32 {
                    1 => 1_000_000,
                    2 => 2_000_000,
                    _ => 3_000_000,
                },
            ),
            Self::C => (
                preset.brush_iterations.saturating_mul(3) / 2,
                preset.brush_max_resolution.min(1920),
                match preset.target_sampling_fps as u32 {
                    1 => 2_000_000,
                    2 => 5_000_000,
                    _ => 8_000_000,
                },
            ),
        };
        preset.brush_iterations = iterations;
        preset.brush_max_resolution = resolution;
        preset.brush_max_splats = max_splats;
        preset
    }
}

/// `Quality` 是前后端协商用的画质档位枚举，前端把它当作字符串 `"fast" / "balanced" / "high"`。
/// Rust 变体名保持 `Draft / Standard / High` 方便内部阅读；通过 `#[serde(rename = ...)]` 把它
/// 映射到前端约定的标签。clap CLI 子命令沿用同一套标签，避免出现两套字面值。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    #[serde(rename = "fast")]
    Draft,
    #[default]
    #[serde(rename = "balanced")]
    Standard,
    #[serde(rename = "high")]
    High,
}

impl ValueEnum for Quality {
    fn value_variants<'a>() -> &'a [Quality] {
        &[Quality::Draft, Quality::Standard, Quality::High]
    }

    fn from_str(input: &str, ignore_case: bool) -> Result<Self, String> {
        match input {
            "fast" => Ok(Quality::Draft),
            "balanced" => Ok(Quality::Standard),
            "high" => Ok(Quality::High),
            other if ignore_case => match other.to_ascii_lowercase().as_str() {
                "fast" => Ok(Quality::Draft),
                "balanced" => Ok(Quality::Standard),
                "high" => Ok(Quality::High),
                _ => Err(format!(
                    "未知画质档位：{other}（期望 fast / balanced / high）"
                )),
            },
            other => Err(format!(
                "未知画质档位：{other}（期望 fast / balanced / high）"
            )),
        }
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(match self {
            Quality::Draft => clap::builder::PossibleValue::new("fast"),
            Quality::Standard => clap::builder::PossibleValue::new("balanced"),
            Quality::High => clap::builder::PossibleValue::new("high"),
        })
    }
}

impl Quality {
    pub fn preset(self) -> QualityPreset {
        match self {
            Quality::Draft => QualityPreset {
                label: "快速".into(),
                target_sampling_fps: 1.0,
                brush_iterations: 7_000,
                brush_max_resolution: 512,
                brush_max_splats: 1_500_000,
            },
            Quality::Standard => QualityPreset {
                label: "均衡".into(),
                target_sampling_fps: 2.0,
                brush_iterations: 15_000,
                brush_max_resolution: 1024,
                brush_max_splats: 3_000_000,
            },
            Quality::High => QualityPreset {
                label: "精细".into(),
                target_sampling_fps: 4.0,
                brush_iterations: 30_000,
                brush_max_resolution: 1920,
                brush_max_splats: 5_000_000,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct QualityPreset {
    pub label: String,
    /// Candidate extraction rate before visual deduplication.
    pub target_sampling_fps: f64,
    pub brush_iterations: u32,
    pub brush_max_resolution: u32,
    pub brush_max_splats: u32,
}
