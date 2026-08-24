//! Deterministic, PTS-aware planning for the adaptive SfM extraction path.
//!
//! This module deliberately has no decoder or FFmpeg dependency. The proxy
//! scan supplies `ProxyFrame` measurements; keeping the decision logic pure
//! makes it testable and prevents average FPS from becoming a hidden time base.

use std::cmp::Ordering;

use image::{imageops::FilterType, DynamicImage, GrayImage};
use serde::{Deserialize, Serialize};

use crate::presets::Quality;

use super::{FramePlan, FrameSelectionStrategyKind, SourceFrameTimestamp, VideoInfo};

// FFmpeg proxy JPEGs remain capped at 640 px. This is the working resolution
// for geometry analysis; raising only the JPEG resolution would not help while
// this stage downsamples it again.
const TRACK_WIDTH: u32 = 320;
const TRACK_HEIGHT: u32 = 240;
const GRID_COLUMNS: u32 = 16;
const GRID_ROWS: u32 = 10;
const PATCH_RADIUS: i32 = 2;
const PYRAMID_LEVELS: usize = 3;
const COARSE_SEARCH_RADIUS: i32 = 6;
const REFINE_SEARCH_RADIUS: i32 = 2;
const MAX_FULL_SEARCH_RADIUS: i32 = COARSE_SEARCH_RADIUS * 4 + REFINE_SEARCH_RADIUS * 2 + REFINE_SEARCH_RADIUS;
const MIN_PATCH_TEXTURE: f64 = 18.0;
const MAX_MATCH_ERROR: f64 = 32.0;
const INLIER_RESIDUAL: f64 = 2.5;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AdaptiveFrameProfile {
    pub anchor_fps: f64,
    pub analysis_fps: f64,
    pub local_refine_fps: f64,
    /// Normalized by the proxy image diagonal.
    pub target_motion: f64,
    pub max_motion: f64,
    pub min_interval_ms: u64,
    pub min_textured_cells: u32,
    pub min_matched_cells: u32,
    pub min_inliers_floor: u32,
    pub min_inlier_ratio: f64,
    pub min_three_view_floor: u32,
    pub min_three_view_ratio: f64,
}

impl AdaptiveFrameProfile {
    pub fn for_quality(quality: Quality, source_fps: f64) -> Option<Self> {
        let source_fps = source_fps.max(1.0);
        match quality {
            // Fast remains the documented fixed 1 FPS path.
            Quality::Draft => None,
            Quality::Standard => Some(Self {
                anchor_fps: 2.0,
                analysis_fps: source_fps.min(6.0),
                local_refine_fps: source_fps.min(12.0),
                target_motion: 0.035,
                max_motion: 0.08,
                min_interval_ms: 120,
                min_textured_cells: 12,
                min_matched_cells: 8,
                min_inliers_floor: 6,
                min_inlier_ratio: 0.45,
                min_three_view_floor: 3,
                min_three_view_ratio: 0.35,
            }),
            Quality::High => Some(Self {
                anchor_fps: 4.0,
                analysis_fps: source_fps.min(8.0),
                local_refine_fps: source_fps.min(16.0),
                target_motion: 0.025,
                max_motion: 0.06,
                min_interval_ms: 80,
                min_textured_cells: 20,
                min_matched_cells: 12,
                min_inliers_floor: 10,
                min_inlier_ratio: 0.55,
                min_three_view_floor: 5,
                min_three_view_ratio: 0.45,
            }),
        }
    }
}

/// A proxy frame measured during the low-resolution scan. `pts_seconds` and
/// `source_index` must refer to the original decoded source frame.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProxyFrame {
    pub source_index: u64,
    pub pts_seconds: f64,
    pub phash: u64,
    pub sharpness: f64,
    pub textured_cells: u32,
    pub matched_cells: u32,
    /// Background inlier motion from the preceding proxy sample, normalized
    /// by the proxy image diagonal.
    pub background_motion: f64,
    pub inliers: u32,
    pub grid_coverage: f64,
    pub three_view_tracks: u32,
    /// Set by the proxy analyzer only after both scene score and track-loss
    /// confirmation; an exposure jump alone must remain false.
    #[serde(default)]
    pub confirmed_scene_change: bool,
}

/// Measures proxy frames without depending on a native computer-vision SDK.
/// It tracks textured grid patches, estimates the dominant translation by
/// median consensus, and only reports tracks agreeing with that background
/// motion as inliers. The caller owns decoding and must keep `samples` and
/// `images` in the same source/PTS order.
pub fn analyze_proxy_images(
    samples: &[SourceFrameTimestamp],
    images: &[DynamicImage],
) -> crate::error::Result<Vec<ProxyFrame>> {
    analyze_proxy_images_with_progress(samples, images, |_, _| {})
}

/// Same deterministic analysis as [`analyze_proxy_images`], with a callback
/// after each proxy frame has been measured. Callers can surface a determinate
/// progress counter without changing frame-selection behaviour.
pub fn analyze_proxy_images_with_progress<F>(
    samples: &[SourceFrameTimestamp],
    images: &[DynamicImage],
    mut on_progress: F,
) -> crate::error::Result<Vec<ProxyFrame>>
where
    F: FnMut(u64, u64),
{
    if samples.len() != images.len() {
        return Err(crate::error::SplatError::Process(format!(
            "代理时间戳与图像数量不一致：{} / {}", samples.len(), images.len()
        )));
    }
    let normalized = images.iter().map(to_tracking_gray).collect::<Vec<_>>();
    let mut result = Vec::with_capacity(samples.len());
    let mut previous_inliers = vec![false; (GRID_COLUMNS * GRID_ROWS) as usize];
    for (index, (sample, image)) in samples.iter().zip(images.iter()).enumerate() {
        let (motion, textured_cells, matched_cells, inliers, coverage, three_view_tracks, confirmed_scene_change, current_inliers) =
            if index == 0 {
                (0.0, 0, 0, 0, 0.0, 0, false, previous_inliers.clone())
            } else {
                measure_background_motion(&normalized[index - 1], &normalized[index], &previous_inliers)
            };
        previous_inliers = current_inliers;
        result.push(ProxyFrame {
            source_index: sample.source_index,
            pts_seconds: sample.pts_seconds,
            phash: super::select::perceptual_hash(image),
            sharpness: super::select::laplacian_variance(image),
            textured_cells,
            matched_cells,
            background_motion: motion,
            inliers,
            grid_coverage: coverage,
            three_view_tracks,
            confirmed_scene_change,
        });
        on_progress((index + 1) as u64, samples.len() as u64);
    }
    Ok(result)
}

fn to_tracking_gray(image: &DynamicImage) -> GrayImage {
    image.resize_exact(TRACK_WIDTH, TRACK_HEIGHT, FilterType::Triangle).to_luma8()
}

fn measure_background_motion(
    previous: &GrayImage,
    current: &GrayImage,
    previous_inliers: &[bool],
) -> (f64, u32, u32, u32, f64, u32, bool, Vec<bool>) {
    let positions = grid_positions();
    let previous_pyramid = tracking_pyramid(previous);
    let current_pyramid = tracking_pyramid(current);
    let mut matches = Vec::new();
    let mut textured_cells = 0_u32;
    for (index, (x, y)) in positions.iter().copied().enumerate() {
        if patch_texture(previous, x, y) < MIN_PATCH_TEXTURE {
            continue;
        }
        textured_cells += 1;
        if let Some((dx, dy, error)) = best_patch_match_pyramid(&previous_pyramid, &current_pyramid, x, y) {
            if error <= MAX_MATCH_ERROR {
                matches.push((index, dx, dy));
            }
        }
    }
    let mut current_inliers = vec![false; positions.len()];
    if matches.is_empty() {
        return (0.0, textured_cells, 0, 0, 0.0, 0, scene_cut(previous, current, 0), current_inliers);
    }
    let matched_cells = matches.len() as u32;
    let median_dx = median(matches.iter().map(|(_, dx, _)| *dx).collect());
    let median_dy = median(matches.iter().map(|(_, _, dy)| *dy).collect());
    let mut inlier_count = 0_u32;
    let mut three_view_tracks = 0_u32;
    for (index, dx, dy) in matches {
        if ((dx - median_dx).powi(2) + (dy - median_dy).powi(2)).sqrt() <= INLIER_RESIDUAL {
            current_inliers[index] = true;
            inlier_count += 1;
            if previous_inliers.get(index).copied().unwrap_or(false) {
                three_view_tracks += 1;
            }
        }
    }
    let diagonal = (f64::from(TRACK_WIDTH).powi(2) + f64::from(TRACK_HEIGHT).powi(2)).sqrt();
    let motion = (median_dx.powi(2) + median_dy.powi(2)).sqrt() / diagonal;
    let coverage = inlier_count as f64 / positions.len() as f64;
    let confirmed_scene_change = scene_cut(previous, current, inlier_count);
    (motion, textured_cells, matched_cells, inlier_count, coverage, three_view_tracks, confirmed_scene_change, current_inliers)
}

fn grid_positions() -> Vec<(i32, i32)> {
    let x_margin = MAX_FULL_SEARCH_RADIUS + PATCH_RADIUS + 1;
    let y_margin = MAX_FULL_SEARCH_RADIUS + PATCH_RADIUS + 1;
    (0..GRID_ROWS)
        .flat_map(|row| (0..GRID_COLUMNS).map(move |column| {
            let x = x_margin + (column as i32 * (TRACK_WIDTH as i32 - 2 * x_margin - 1)) / (GRID_COLUMNS as i32 - 1);
            let y = y_margin + (row as i32 * (TRACK_HEIGHT as i32 - 2 * y_margin - 1)) / (GRID_ROWS as i32 - 1);
            (x, y)
        }))
        .collect()
}

fn patch_texture(image: &GrayImage, x: i32, y: i32) -> f64 {
    let mut sum = 0.0;
    let mut square_sum = 0.0;
    let mut count = 0.0;
    for dy in -PATCH_RADIUS..=PATCH_RADIUS {
        for dx in -PATCH_RADIUS..=PATCH_RADIUS {
            let value = f64::from(image.get_pixel((x + dx) as u32, (y + dy) as u32)[0]);
            sum += value;
            square_sum += value * value;
            count += 1.0;
        }
    }
    (square_sum / count - (sum / count).powi(2)).sqrt()
}

fn tracking_pyramid(image: &GrayImage) -> Vec<GrayImage> {
    let mut pyramid = Vec::with_capacity(PYRAMID_LEVELS);
    pyramid.push(image.clone());
    for level in 1..PYRAMID_LEVELS {
        let previous = &pyramid[level - 1];
        pyramid.push(image::imageops::resize(
            previous,
            (previous.width() / 2).max(1),
            (previous.height() / 2).max(1),
            FilterType::Triangle,
        ));
    }
    pyramid
}

/// Coarse-to-fine matching reaches substantially farther than a full-resolution
/// ±6 px search, without paying for a full-resolution ±30 px exhaustive scan.
fn best_patch_match_pyramid(
    previous: &[GrayImage],
    current: &[GrayImage],
    x: i32,
    y: i32,
) -> Option<(f64, f64, f64)> {
    let mut displacement = (0_i32, 0_i32);
    let mut final_error = 0.0;
    let levels = previous.len().min(current.len());
    for level in (0..levels).rev() {
        let scale = 1_i32 << level;
        let radius = if level == levels - 1 {
            COARSE_SEARCH_RADIUS
        } else {
            REFINE_SEARCH_RADIUS
        };
        let base = if level == levels - 1 {
            (0, 0)
        } else {
            (displacement.0 * 2, displacement.1 * 2)
        };
        let (dx, dy, error) = best_patch_match_near(
            &previous[level],
            &current[level],
            x / scale,
            y / scale,
            base,
            radius,
        )?;
        displacement = (dx, dy);
        final_error = error;
    }
    Some((f64::from(displacement.0), f64::from(displacement.1), final_error))
}

fn best_patch_match_near(
    previous: &GrayImage,
    current: &GrayImage,
    x: i32,
    y: i32,
    base: (i32, i32),
    radius: i32,
) -> Option<(i32, i32, f64)> {
    let mut best: Option<(i32, i32, f64)> = None;
    for dy in base.1 - radius..=base.1 + radius {
        for dx in base.0 - radius..=base.0 + radius {
            if !patch_fits(previous, x, y) || !patch_fits(current, x + dx, y + dy) {
                continue;
            }
            let mut error = 0.0;
            let mut count = 0.0;
            for patch_y in -PATCH_RADIUS..=PATCH_RADIUS {
                for patch_x in -PATCH_RADIUS..=PATCH_RADIUS {
                    let a = f64::from(previous.get_pixel((x + patch_x) as u32, (y + patch_y) as u32)[0]);
                    let b = f64::from(current.get_pixel((x + dx + patch_x) as u32, (y + dy + patch_y) as u32)[0]);
                    error += (a - b).abs();
                    count += 1.0;
                }
            }
            let error = error / count;
            if best.is_none_or(|(_, _, current_best)| error < current_best) {
                best = Some((dx, dy, error));
            }
        }
    }
    best
}

fn patch_fits(image: &GrayImage, x: i32, y: i32) -> bool {
    x - PATCH_RADIUS >= 0
        && y - PATCH_RADIUS >= 0
        && x + PATCH_RADIUS < image.width() as i32
        && y + PATCH_RADIUS < image.height() as i32
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|left, right| left.total_cmp(right));
    values[values.len() / 2]
}

fn scene_cut(previous: &GrayImage, current: &GrayImage, inliers: u32) -> bool {
    let mean_difference = previous.pixels().zip(current.pixels())
        .map(|(left, right)| (f64::from(left[0]) - f64::from(right[0])).abs())
        .sum::<f64>() / f64::from(TRACK_WIDTH * TRACK_HEIGHT);
    mean_difference >= 40.0 && inliers < (GRID_COLUMNS * GRID_ROWS) / 5
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SelectionReason {
    SegmentStart,
    MotionTarget,
    Bridge,
    SegmentEnd,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SelectedSourceFrame {
    pub source_index: u64,
    pub pts_seconds: f64,
    pub reason: SelectionReason,
    pub motion: f64,
    pub inliers: u32,
    pub grid_coverage: f64,
    pub sharpness: f64,
}

/// Builds an adaptive plan without pretending that a proxy scan has happened.
/// The runner will only select this plan after it has proxy evidence and can
/// perform exact source-index extraction.
pub fn adaptive_plan(video: &VideoInfo, quality: Quality) -> Option<FramePlan> {
    let profile = AdaptiveFrameProfile::for_quality(quality, video.fps)?;
    let source_fps = if video.fps > 0.0 { video.fps } else { 30.0 };
    Some(FramePlan {
        sampling_fps: profile.anchor_fps,
        estimated_frames: (video.duration.max(0.0) * profile.anchor_fps).round().max(1.0) as u64,
        source_fps,
        source_duration: video.duration,
        strategy: FrameSelectionStrategyKind::AdaptiveSfm,
        anchor_fps: Some(profile.anchor_fps),
        analysis_fps: Some(profile.analysis_fps),
        effective_fps: None,
        proxy_candidates: None,
    })
}

/// Choose presentation-timestamped proxy samples. A sampling grid only limits
/// analysis work; output frames retain exact source indices and PTS.
pub fn choose_proxy_samples(
    source_frames: &[SourceFrameTimestamp],
    analysis_fps: f64,
) -> Vec<SourceFrameTimestamp> {
    if source_frames.is_empty() || !analysis_fps.is_finite() || analysis_fps <= 0.0 {
        return Vec::new();
    }
    let interval = 1.0 / analysis_fps;
    let mut samples = vec![source_frames[0]];
    let mut next_pts = source_frames[0].pts_seconds + interval;
    for frame in &source_frames[1..] {
        if frame.pts_seconds + f64::EPSILON >= next_pts {
            samples.push(*frame);
            next_pts = frame.pts_seconds + interval;
        }
    }
    samples
}

/// Generates a filter script rather than an argv-sized expression. Callers
/// write it into an isolated attempt directory and pass `-filter_script:v`.
pub fn exact_source_select_script(frames: &[SelectedSourceFrame]) -> Option<String> {
    source_indices_select_script(frames.iter().map(|frame| frame.source_index))
}

/// The shared selector used for both low-resolution proxy analysis and final
/// original-resolution extraction. It never serializes timestamps into an FPS
/// expression; FFmpeg receives decoded source indices only.
pub fn source_indices_select_script(
    indices: impl IntoIterator<Item = u64>,
) -> Option<String> {
    let mut indices = indices.into_iter().collect::<Vec<_>>();
    indices.sort_unstable();
    indices.dedup();
    if indices.is_empty() {
        return None;
    }
    let clauses = indices
        .iter()
        .map(|index| format!("eq(n\\,{index})"))
        .collect::<Vec<_>>()
        .join("+");
    Some(format!("select='{clauses}',setpts=N/FRAME_RATE/TB\n"))
}

/// Select source frames from proxy measurements. It is deliberately
/// conservative: frames that fail co-visibility gates never win a sharpness
/// tie, and pHash can suppress a candidate only when geometric motion is low.
pub fn select_adaptive_frames(
    frames: &[ProxyFrame],
    profile: AdaptiveFrameProfile,
) -> Vec<SelectedSourceFrame> {
    let mut ordered = frames.to_vec();
    ordered.sort_by(|left, right| {
        left.pts_seconds
            .partial_cmp(&right.pts_seconds)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.source_index.cmp(&right.source_index))
    });
    ordered.dedup_by_key(|frame| frame.source_index);

    let mut selected = Vec::new();
    let mut segment_start = 0;
    while segment_start < ordered.len() {
        let segment_end = ordered[segment_start + 1..]
            .iter()
            .position(|frame| frame.confirmed_scene_change)
            .map(|offset| segment_start + offset + 1)
            .unwrap_or(ordered.len());
        select_segment(&ordered[segment_start..segment_end], profile, &mut selected);
        segment_start = segment_end;
    }
    selected
}

fn select_segment(
    segment: &[ProxyFrame],
    profile: AdaptiveFrameProfile,
    selected: &mut Vec<SelectedSourceFrame>,
) {
    let Some(first) = segment.first() else { return };
    selected.push(selection(first, SelectionReason::SegmentStart, 0.0));
    let mut last_selected = first;
    let mut accumulated_motion = 0.0;

    for frame in &segment[1..] {
        accumulated_motion += frame.background_motion.max(0.0);
        let interval_ms = ((frame.pts_seconds - last_selected.pts_seconds) * 1000.0).max(0.0);
        if interval_ms < profile.min_interval_ms as f64 {
            continue;
        }
        let geometry_ok = passes_proxy_geometry(frame, profile);
        let low_motion_duplicate = hamming_distance(frame.phash, last_selected.phash) <= 8
            && accumulated_motion < profile.target_motion * 0.5;
        if geometry_ok && !low_motion_duplicate && accumulated_motion >= profile.target_motion {
            selected.push(selection(frame, SelectionReason::MotionTarget, accumulated_motion));
            last_selected = frame;
            accumulated_motion = 0.0;
        } else if geometry_ok && accumulated_motion >= profile.max_motion {
            selected.push(selection(frame, SelectionReason::Bridge, accumulated_motion));
            last_selected = frame;
            accumulated_motion = 0.0;
        }
    }

    if let Some(last) = segment.last() {
        if last.source_index != last_selected.source_index
            && passes_proxy_geometry(last, profile)
            && (last.pts_seconds - last_selected.pts_seconds) * 1000.0 >= profile.min_interval_ms as f64
        {
            selected.push(selection(last, SelectionReason::SegmentEnd, accumulated_motion));
        }
    }
}

fn selection(frame: &ProxyFrame, reason: SelectionReason, motion: f64) -> SelectedSourceFrame {
    SelectedSourceFrame {
        source_index: frame.source_index,
        pts_seconds: frame.pts_seconds,
        reason,
        motion,
        inliers: frame.inliers,
        grid_coverage: frame.grid_coverage,
        sharpness: frame.sharpness,
    }
}

pub fn passes_proxy_geometry(frame: &ProxyFrame, profile: AdaptiveFrameProfile) -> bool {
    if frame.textured_cells < profile.min_textured_cells
        || frame.matched_cells < profile.min_matched_cells
        || frame.matched_cells == 0
        || frame.inliers < profile.min_inliers_floor
        || frame.three_view_tracks < profile.min_three_view_floor
    {
        return false;
    }
    let inlier_ratio = frame.inliers as f64 / frame.matched_cells as f64;
    let three_view_ratio = frame.three_view_tracks as f64 / frame.inliers.max(1) as f64;
    inlier_ratio >= profile.min_inlier_ratio && three_view_ratio >= profile.min_three_view_ratio
}

fn hamming_distance(left: u64, right: u64) -> u32 {
    (left ^ right).count_ones()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Luma};

    fn profile() -> AdaptiveFrameProfile {
        AdaptiveFrameProfile { anchor_fps: 2.0, analysis_fps: 6.0, local_refine_fps: 12.0, target_motion: 0.035, max_motion: 0.08, min_interval_ms: 120, min_textured_cells: 12, min_matched_cells: 8, min_inliers_floor: 6, min_inlier_ratio: 0.45, min_three_view_floor: 3, min_three_view_ratio: 0.35 }
    }

    fn frame(index: u64, pts: f64, motion: f64) -> ProxyFrame {
        ProxyFrame { source_index: index, pts_seconds: pts, phash: index, sharpness: 1.0, textured_cells: 100, matched_cells: 100, background_motion: motion, inliers: 80, grid_coverage: 0.5, three_view_tracks: 70, confirmed_scene_change: false }
    }

    #[test]
    fn uses_pts_not_nominal_source_fps_and_keeps_indices_monotonic() {
        let result = select_adaptive_frames(&[frame(60, 1.0, 0.0), frame(120, 2.0, 0.04), frame(90, 1.5, 0.04)], profile());
        assert_eq!(result.iter().map(|item| item.source_index).collect::<Vec<_>>(), vec![60, 90, 120]);
    }

    #[test]
    fn similar_frames_with_real_motion_are_not_dropped_by_phash() {
        let mut second = frame(2, 0.5, 0.04);
        second.phash = 1;
        let result = select_adaptive_frames(&[frame(1, 0.0, 0.0), second], profile());
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn static_similar_window_does_not_fill_frames() {
        let mut frames = vec![frame(1, 0.0, 0.0)];
        for index in 2..20 { let mut item = frame(index, index as f64 * 0.2, 0.001); item.phash = 1; frames.push(item); }
        let result = select_adaptive_frames(&frames, profile());
        assert_eq!(result.len(), 2, "only segment endpoints are retained");
    }

    #[test]
    fn confirmed_cut_creates_independent_segments() {
        let mut cut = frame(3, 1.0, 0.04);
        cut.confirmed_scene_change = true;
        let result = select_adaptive_frames(&[frame(1, 0.0, 0.0), frame(2, 0.5, 0.04), cut, frame(4, 1.5, 0.04)], profile());
        assert_eq!(result.iter().filter(|item| item.reason == SelectionReason::SegmentStart).count(), 2);
    }

    #[test]
    fn proxy_sampling_follows_vfr_pts_not_frame_number() {
        let source = [
            SourceFrameTimestamp { source_index: 0, pts_seconds: 0.0 },
            SourceFrameTimestamp { source_index: 1, pts_seconds: 0.03 },
            SourceFrameTimestamp { source_index: 2, pts_seconds: 0.51 },
            SourceFrameTimestamp { source_index: 3, pts_seconds: 0.99 },
        ];
        assert_eq!(choose_proxy_samples(&source, 2.0), vec![source[0], source[2]]);
    }

    #[test]
    fn source_select_script_uses_indices_and_never_pts_as_a_fake_fps() {
        let selected = vec![
            selection(&frame(41, 1.37, 0.0), SelectionReason::SegmentStart, 0.0),
            selection(&frame(9, 0.17, 0.0), SelectionReason::MotionTarget, 0.0),
        ];
        assert_eq!(exact_source_select_script(&selected).as_deref(), Some("select='eq(n\\,9)+eq(n\\,41)',setpts=N/FRAME_RATE/TB\n"));
    }

    #[test]
    fn proxy_analysis_preserves_source_pts_and_confirms_untrackable_cut() {
        let black = DynamicImage::ImageLuma8(ImageBuffer::from_pixel(160, 120, Luma([0])));
        let white = DynamicImage::ImageLuma8(ImageBuffer::from_pixel(160, 120, Luma([255])));
        let samples = [
            SourceFrameTimestamp { source_index: 7, pts_seconds: 0.4 },
            SourceFrameTimestamp { source_index: 43, pts_seconds: 1.2 },
        ];
        let result = analyze_proxy_images(&samples, &[black, white]).unwrap();
        assert_eq!(result[1].source_index, 43);
        assert_eq!(result[1].pts_seconds, 1.2);
        assert!(result[1].confirmed_scene_change);
        assert_eq!(result[1].inliers, 0);
    }

    #[test]
    fn proxy_analysis_rejects_mismatched_images_and_timestamps() {
        let image = DynamicImage::ImageLuma8(ImageBuffer::from_pixel(8, 8, Luma([0])));
        let error = analyze_proxy_images(&[], &[image]).unwrap_err();
        assert!(error.to_string().contains("数量不一致"));
    }

    #[test]
    fn pyramidal_tracking_recovers_a_shift_larger_than_single_scale_search() {
        let mut previous = GrayImage::new(TRACK_WIDTH, TRACK_HEIGHT);
        for y in 0..TRACK_HEIGHT {
            for x in 0..TRACK_WIDTH {
                let value = ((x.wrapping_mul(73) ^ y.wrapping_mul(151) ^ x.wrapping_mul(y)) & 0xff) as u8;
                previous.put_pixel(x, y, Luma([value]));
            }
        }
        let shift = 20_u32;
        let mut current = GrayImage::new(TRACK_WIDTH, TRACK_HEIGHT);
        for y in 0..TRACK_HEIGHT {
            for x in 0..TRACK_WIDTH - shift {
                current.put_pixel(x + shift, y, *previous.get_pixel(x, y));
            }
        }

        let (motion, textured, matched, inliers, _, _, _, _) =
            measure_background_motion(&previous, &current, &vec![false; (GRID_COLUMNS * GRID_ROWS) as usize]);
        assert!(textured >= 12);
        assert!(matched >= 8);
        assert!(inliers >= 6);
        assert!(motion > 0.04, "expected 20 px / 400 px diagonal, got {motion}");
    }
}
