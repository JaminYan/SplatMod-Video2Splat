use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, OnceLock,
    },
};

use image::{imageops::FilterType, DynamicImage, GrayImage};
use rayon::prelude::*;

use crate::error::{Result, SplatError};

const PHASH_SIZE: u32 = 32;
const PHASH_LOW_FREQUENCIES: usize = 8;
const PHASH_DISTANCE_THRESHOLD: u32 = 8;
const SHARPNESS_SIZE: u32 = 256;

#[derive(Debug, Clone, Copy, Default)]
pub struct FrameSelectionReport {
    pub candidates: u64,
    pub retained: u64,
    pub removed_near_duplicates: u64,
}

#[derive(Debug)]
struct CandidateMetrics {
    path: PathBuf,
    hash: u64,
    sharpness: f64,
}

#[derive(Debug)]
struct FrameSelectionPlan {
    candidates: u64,
    discard: Vec<PathBuf>,
}

/// Keep one sharp representative from every contiguous near-duplicate run.
/// Files are named sequentially by FFmpeg, so source order is preserved.
pub fn select_useful_frames(directory: &Path) -> Result<FrameSelectionReport> {
    select_useful_frames_parallel_with_progress(directory, |_, _| {})
}

/// Same selection algorithm as [`select_useful_frames`], with a callback for
/// candidate-level progress reporting.
pub fn select_useful_frames_with_progress(
    directory: &Path,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<FrameSelectionReport> {
    // Compatibility wrapper for callers that require a local FnMut callback.
    // The pipeline uses the parallel API below so no Tokio worker is blocked.
    let paths = candidate_paths(directory)?;
    let metrics = paths
        .into_iter()
        .enumerate()
        .map(|(index, path)| {
            let metric = compute_candidate_metrics(path)?;
            on_progress(index as u64 + 1, metric_count_hint(directory));
            Ok(metric)
        })
        .collect::<Result<Vec<_>>>()?;
    let plan = build_selection_plan(metrics);
    commit_selection_plan(directory, &plan)
}

/// Computes independent image metrics on a bounded Rayon pool, then applies the
/// near-duplicate decision sequentially in source order. No JPEG is deleted
/// until every candidate has been decoded and measured successfully.
pub fn select_useful_frames_parallel_with_progress(
    directory: &Path,
    on_progress: impl Fn(u64, u64) + Send + Sync + 'static,
) -> Result<FrameSelectionReport> {
    let paths = candidate_paths(directory)?;
    let candidates = paths.len() as u64;
    let completed = Arc::new(AtomicU64::new(0));
    let on_progress = Arc::new(on_progress);
    let threads = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
        .min(8);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|error| SplatError::Process(format!("无法创建筛选线程池：{error}")))?;
    let metrics = pool.install(|| {
        paths
            .par_iter()
            .map(|path| {
                let metric = compute_candidate_metrics(path.clone())?;
                let current = completed.fetch_add(1, Ordering::Relaxed) + 1;
                on_progress(current, candidates);
                Ok(metric)
            })
            .collect::<Result<Vec<_>>>()
    })?;
    let plan = build_selection_plan(metrics);
    commit_selection_plan(directory, &plan)
}

fn candidate_paths(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(directory)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("jpg"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    if paths.is_empty() {
        return Err(SplatError::Process("没有可供筛选的 JPEG 画面".into()));
    }
    Ok(paths)
}

fn metric_count_hint(directory: &Path) -> u64 {
    fs::read_dir(directory)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("jpg"))
                })
                .count() as u64
        })
        .unwrap_or(1)
}

fn compute_candidate_metrics(path: PathBuf) -> Result<CandidateMetrics> {
    let image = image::open(&path).map_err(|error| {
        SplatError::Process(format!("无法读取抽帧 {}：{error}", path.display()))
    })?;
    Ok(CandidateMetrics {
        hash: perceptual_hash(&image),
        sharpness: laplacian_variance(&image),
        path,
    })
}

fn build_selection_plan(metrics: Vec<CandidateMetrics>) -> FrameSelectionPlan {
    let candidates = metrics.len() as u64;
    let mut retained: Option<CandidateMetrics> = None;
    let mut discard = Vec::new();
    for candidate in metrics {
        match retained.take() {
            None => retained = Some(candidate),
            Some(previous)
                if hamming_distance(previous.hash, candidate.hash) <= PHASH_DISTANCE_THRESHOLD =>
            {
                // Strictly greater keeps the new frame; ties deliberately keep
                // the earlier one so results do not depend on parallel timing.
                let (keep, rejected) = if candidate.sharpness > previous.sharpness {
                    (candidate, previous)
                } else {
                    (previous, candidate)
                };
                discard.push(rejected.path);
                retained = Some(keep);
            }
            Some(_previous) => retained = Some(candidate),
        }
    }
    FrameSelectionPlan {
        candidates,
        discard,
    }
}

fn commit_selection_plan(
    directory: &Path,
    plan: &FrameSelectionPlan,
) -> Result<FrameSelectionReport> {
    let root = directory.canonicalize()?;
    for path in &plan.discard {
        let resolved = path.canonicalize()?;
        if !resolved.starts_with(&root)
            || !resolved
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("jpg"))
        {
            return Err(SplatError::Process(format!(
                "拒绝删除项目帧目录外的文件：{}",
                path.display()
            )));
        }
        fs::remove_file(&resolved).map_err(|error| {
            SplatError::Process(format!("删除近重复帧 {} 失败：{error}", resolved.display()))
        })?;
    }
    Ok(FrameSelectionReport {
        candidates: plan.candidates,
        retained: plan.candidates - plan.discard.len() as u64,
        removed_near_duplicates: plan.discard.len() as u64,
    })
}

pub(crate) fn perceptual_hash(image: &DynamicImage) -> u64 {
    let gray = image
        .resize_exact(PHASH_SIZE, PHASH_SIZE, FilterType::Triangle)
        .to_luma8();
    let mut coefficients = [0.0; PHASH_LOW_FREQUENCIES * PHASH_LOW_FREQUENCIES];
    let cosine = cosine_table();
    for v in 0..PHASH_LOW_FREQUENCIES {
        for u in 0..PHASH_LOW_FREQUENCIES {
            let mut sum = 0.0;
            for y in 0..PHASH_SIZE {
                for x in 0..PHASH_SIZE {
                    let pixel = f64::from(gray.get_pixel(x, y)[0]);
                    sum += pixel * cosine[x as usize][u] * cosine[y as usize][v];
                }
            }
            coefficients[v * PHASH_LOW_FREQUENCIES + u] = sum;
        }
    }
    let mut frequencies = coefficients[1..].to_vec();
    frequencies.sort_by(|left, right| left.total_cmp(right));
    let median = frequencies[frequencies.len() / 2];
    coefficients
        .iter()
        .enumerate()
        .skip(1)
        .fold(0_u64, |hash, (bit, value)| {
            hash | (u64::from(*value > median) << (bit - 1))
        })
}

fn cosine_table() -> &'static [[f64; PHASH_LOW_FREQUENCIES]; PHASH_SIZE as usize] {
    static TABLE: OnceLock<[[f64; PHASH_LOW_FREQUENCIES]; PHASH_SIZE as usize]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut cosine = [[0.0; PHASH_LOW_FREQUENCIES]; PHASH_SIZE as usize];
        for (x, row) in cosine.iter_mut().enumerate() {
            for (frequency, value) in row.iter_mut().enumerate() {
                *value = (std::f64::consts::PI * (2.0 * x as f64 + 1.0) * frequency as f64
                    / (2.0 * f64::from(PHASH_SIZE)))
                .cos();
            }
        }
        cosine
    })
}

pub(crate) fn laplacian_variance(image: &DynamicImage) -> f64 {
    let gray = image
        .resize(SHARPNESS_SIZE, SHARPNESS_SIZE, FilterType::Triangle)
        .to_luma8();
    variance_of_laplacian(&gray)
}

fn variance_of_laplacian(gray: &GrayImage) -> f64 {
    if gray.width() < 3 || gray.height() < 3 {
        return 0.0;
    }
    let mut count = 0.0;
    let mut sum = 0.0;
    let mut squared_sum = 0.0;
    for y in 1..gray.height() - 1 {
        for x in 1..gray.width() - 1 {
            let center = f64::from(gray.get_pixel(x, y)[0]);
            let value = f64::from(gray.get_pixel(x - 1, y)[0])
                + f64::from(gray.get_pixel(x + 1, y)[0])
                + f64::from(gray.get_pixel(x, y - 1)[0])
                + f64::from(gray.get_pixel(x, y + 1)[0])
                - 4.0 * center;
            count += 1.0;
            sum += value;
            squared_sum += value * value;
        }
    }
    squared_sum / count - (sum / count).powi(2)
}

fn hamming_distance(left: u64, right: u64) -> u32 {
    (left ^ right).count_ones()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Luma};

    fn metric(path: &str, hash: u64, sharpness: f64) -> CandidateMetrics {
        CandidateMetrics {
            path: PathBuf::from(path),
            hash,
            sharpness,
        }
    }
    #[test]
    fn laplacian_prefers_edges_over_a_flat_image() {
        let flat = GrayImage::from_pixel(16, 16, image::Luma([128]));
        let detailed = GrayImage::from_fn(16, 16, |x, _| {
            image::Luma([if x % 2 == 0 { 0 } else { 255 }])
        });
        assert!(variance_of_laplacian(&detailed) > variance_of_laplacian(&flat));
    }
    #[test]
    fn hamming_distance_counts_changed_bits() {
        assert_eq!(hamming_distance(0b1010, 0b0011), 2);
    }

    #[test]
    fn equal_sharpness_keeps_the_earlier_frame() {
        let plan = build_selection_plan(vec![
            metric("frame_000001.jpg", 0, 10.0),
            metric("frame_000002.jpg", 0, 10.0),
        ]);
        assert_eq!(plan.discard, vec![PathBuf::from("frame_000002.jpg")]);
    }

    #[test]
    fn selection_plan_is_independent_of_metric_completion_order() {
        let ordered = vec![
            metric("frame_000001.jpg", 0, 1.0),
            metric("frame_000002.jpg", 0, 2.0),
            metric("frame_000003.jpg", u64::MAX, 3.0),
        ];
        let plan = build_selection_plan(ordered);
        assert_eq!(plan.discard, vec![PathBuf::from("frame_000001.jpg")]);
    }

    #[test]
    fn failed_metric_collection_deletes_no_frames() {
        let temp = tempfile::tempdir().unwrap();
        let valid = temp.path().join("frame_000001.jpg");
        let invalid = temp.path().join("frame_000002.jpg");
        ImageBuffer::<Luma<u8>, Vec<u8>>::from_pixel(8, 8, Luma([42]))
            .save(&valid)
            .unwrap();
        fs::write(&invalid, b"not a jpeg").unwrap();

        assert!(select_useful_frames_parallel_with_progress(temp.path(), |_, _| {}).is_err());
        assert!(valid.is_file());
        assert!(invalid.is_file());
    }
}
