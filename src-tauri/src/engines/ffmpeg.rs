use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
};

use crate::{
    engines::FfmpegHwAccel,
    error::{Result, SplatError},
    process::{ProcessManager, ProcessObserver, ProcessSpec},
    video::{FramePlan, SelectedSourceFrame, SourceFrameTimestamp},
};

#[derive(Debug, Clone)]
pub struct ProxyExtractionReport {
    pub frames: Vec<SourceFrameTimestamp>,
    pub filter_script: PathBuf,
    pub buffered_frame_limit: usize,
    pub memory_budget_bytes: u64,
}

const MIN_PROXY_BUFFERED_FRAMES: usize = 32;
const SYSTEM_MEMORY_RESERVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Extracts a bounded-memory proxy sequence for adaptive analysis. FFmpeg's
/// default decoder and filter thread counts are deliberately left enabled; only
/// the filtergraph's buffered-frame count is capped. `stats_mux_pre` writes the
/// decoded input-frame index and presentation timestamp beside each emitted
/// JPEG. That mapping is the authoritative PTS source for later exact-source
/// extraction, so adaptive runs do not need a separate full-video probe pass.
pub async fn extract_proxy_frames(
    executable: &Path,
    input: &Path,
    output_directory: &Path,
    work_directory: &Path,
    source_width: u32,
    source_height: u32,
    analysis_fps: f64,
    log_path: Option<PathBuf>,
    process_manager: &ProcessManager,
    observer: Option<ProcessObserver>,
) -> Result<ProxyExtractionReport> {
    if !input.is_file() {
        return Err(SplatError::InvalidPath(input.to_path_buf()));
    }
    if !analysis_fps.is_finite() || analysis_fps <= 0.0 {
        return Err(SplatError::Process("代理抽帧 FPS 无效".into()));
    }
    tokio::fs::create_dir_all(output_directory).await?;
    tokio::fs::create_dir_all(work_directory).await?;
    ensure_no_jpegs(output_directory).await?;
    let mapping_path = work_directory.join("adaptive-proxy-map.txt");
    let (buffered_frame_limit, memory_budget_bytes) =
        proxy_buffered_frame_limit(source_width, source_height);
    let filter = format!("fps=fps={analysis_fps:.8}:round=near,scale='min(640,iw)':'min(640,ih)':force_original_aspect_ratio=decrease");
    let output_pattern = output_directory.join("proxy_%06d.jpg");
    let output = process_manager
        .run(ProcessSpec {
            executable: executable.to_path_buf(),
            args: vec![
                OsString::from("-hide_banner"),
                OsString::from("-nostdin"),
                OsString::from("-nostats"),
                OsString::from("-y"),
                OsString::from("-filter_buffered_frames"),
                OsString::from(buffered_frame_limit.to_string()),
                OsString::from("-i"),
                input.as_os_str().to_owned(),
                OsString::from("-vf"),
                OsString::from(filter),
                OsString::from("-q:v"),
                OsString::from("5"),
                OsString::from("-start_number"),
                OsString::from("1"),
                OsString::from("-stats_mux_pre:v"),
                mapping_path.as_os_str().to_owned(),
                OsString::from("-stats_mux_pre_fmt:v"),
                OsString::from("{ni} {ti}"),
                OsString::from("-progress"),
                OsString::from("pipe:1"),
                output_pattern.as_os_str().to_owned(),
            ],
            working_directory: work_directory.parent().map(Path::to_path_buf),
            log_path,
            observer,
        })
        .await?;
    if output.cancelled {
        return Err(SplatError::Cancelled);
    }
    if !output.success {
        return Err(SplatError::Process(format!(
            "FFmpeg 高速代理抽帧退出码 {:?}",
            output.exit_code
        )));
    }
    let mapped = parse_proxy_mapping(&tokio::fs::read_to_string(&mapping_path).await?)?;
    let count = jpeg_count(output_directory).await?;
    if count != mapped.len() as u64 {
        return Err(SplatError::Process(format!(
            "代理抽帧数量不匹配：PTS 映射 {} 帧，实际输出 {count} 帧",
            mapped.len()
        )));
    }
    Ok(ProxyExtractionReport {
        frames: mapped,
        filter_script: mapping_path,
        buffered_frame_limit,
        memory_budget_bytes,
    })
}

/// `fps` runs before scale, so use the decoded source-frame size (YUV 4:2:0
/// worst normal case) and a three-copy safety multiplier. The selected limit is
/// a permission ceiling, not an allocation: ordinary linear proxy extraction
/// keeps only a few frames resident. We reserve 2 GiB for Windows and the rest
/// of the pipeline, then allow at most 80% of the currently available RAM.
fn proxy_buffered_frame_limit(source_width: u32, source_height: u32) -> (usize, u64) {
    let pixels = u64::from(source_width.max(1)) * u64::from(source_height.max(1));
    let estimated_bytes_per_frame = pixels
        .saturating_mul(3)
        .saturating_div(2)
        .saturating_mul(3)
        .max(1);
    let available =
        available_physical_memory_bytes().unwrap_or(estimated_bytes_per_frame.saturating_mul(96));
    let memory_budget_bytes = available
        .saturating_mul(4)
        .saturating_div(5)
        .min(available.saturating_sub(SYSTEM_MEMORY_RESERVE_BYTES));
    let frames = (memory_budget_bytes / estimated_bytes_per_frame)
        .max(MIN_PROXY_BUFFERED_FRAMES as u64)
        .min(i32::MAX as u64) as usize;
    (frames, memory_budget_bytes)
}

#[cfg(windows)]
fn available_physical_memory_bytes() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..unsafe { std::mem::zeroed() }
    };
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status.ullAvailPhys)
}

#[cfg(not(windows))]
fn available_physical_memory_bytes() -> Option<u64> {
    None
}

fn parse_proxy_mapping(text: &str) -> Result<Vec<SourceFrameTimestamp>> {
    let mut mapped = Vec::new();
    let mut last_pts = f64::NEG_INFINITY;
    for (line_number, line) in text.lines().enumerate() {
        let mut fields = line.split_whitespace();
        let source_index = fields
            .next()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| {
                SplatError::Process(format!(
                    "代理 PTS 映射第 {} 行缺少有效输入帧索引：{line}",
                    line_number + 1
                ))
            })?;
        let pts_seconds = fields
            .next()
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite())
            .ok_or_else(|| {
                SplatError::Process(format!(
                    "代理 PTS 映射第 {} 行缺少输入时间：{line}",
                    line_number + 1
                ))
            })?;
        if pts_seconds + f64::EPSILON < last_pts {
            return Err(SplatError::Process(format!(
                "代理 PTS 映射第 {} 行不是显示顺序：{line}",
                line_number + 1
            )));
        }
        mapped.push(SourceFrameTimestamp {
            source_index,
            pts_seconds,
        });
        last_pts = pts_seconds;
    }
    if mapped.is_empty() {
        return Err(SplatError::Process("FFmpeg 未写出代理 PTS 映射".into()));
    }
    Ok(mapped)
}

/// Re-run the same official `fps` sampler used by proxy analysis at high
/// resolution, then promote only selected source indices. `stats_mux_pre` is
/// the binding between every output JPEG and its decoded input-frame index;
/// this avoids a long per-frame equality filter entirely.
pub async fn extract_selected_frames(
    executable: &Path,
    input: &Path,
    output_directory: &Path,
    work_directory: &Path,
    selected: &[SelectedSourceFrame],
    analysis_fps: f64,
    hw_accel: FfmpegHwAccel,
    log_path: Option<PathBuf>,
    process_manager: &ProcessManager,
    observer: Option<ProcessObserver>,
) -> Result<u64> {
    if !input.is_file() {
        return Err(SplatError::InvalidPath(input.to_path_buf()));
    }
    if !analysis_fps.is_finite() || analysis_fps <= 0.0 {
        return Err(SplatError::Process("自适应原图抽帧 FPS 无效".into()));
    }
    if selected.is_empty() {
        return Err(SplatError::Process("自适应抽帧没有可选源帧".into()));
    }
    tokio::fs::create_dir_all(output_directory).await?;
    tokio::fs::create_dir_all(work_directory).await?;
    ensure_no_jpegs(output_directory).await?;
    let candidates = work_directory.join("fps-candidates");
    tokio::fs::create_dir_all(&candidates).await?;
    ensure_no_jpegs(&candidates).await?;
    let mapping_path = work_directory.join("adaptive-original-map.txt");
    let filter = format!("fps=fps={analysis_fps:.8}:round=near,scale='min(1920,iw)':'min(1920,ih)':force_original_aspect_ratio=decrease");
    let output_pattern = candidates.join("candidate_%06d.jpg");
    let mut args = vec![
        OsString::from("-hide_banner"),
        OsString::from("-nostdin"),
        OsString::from("-nostats"),
        OsString::from("-y"),
    ];
    match hw_accel {
        FfmpegHwAccel::Off => {}
        FfmpegHwAccel::Auto => args.extend([OsString::from("-hwaccel"), OsString::from("auto")]),
        FfmpegHwAccel::D3d11va => {
            args.extend([OsString::from("-hwaccel"), OsString::from("d3d11va")])
        }
        FfmpegHwAccel::Cuda => args.extend([OsString::from("-hwaccel"), OsString::from("cuda")]),
    }
    args.extend([
        OsString::from("-i"),
        input.as_os_str().to_owned(),
        OsString::from("-vf"),
        OsString::from(filter),
        OsString::from("-q:v"),
        OsString::from("2"),
        OsString::from("-start_number"),
        OsString::from("1"),
        OsString::from("-stats_mux_pre:v"),
        mapping_path.as_os_str().to_owned(),
        OsString::from("-stats_mux_pre_fmt:v"),
        OsString::from("{ni} {ti}"),
        OsString::from("-progress"),
        OsString::from("pipe:1"),
        output_pattern.as_os_str().to_owned(),
    ]);
    let output = process_manager
        .run(ProcessSpec {
            executable: executable.to_path_buf(),
            args,
            working_directory: work_directory.parent().map(Path::to_path_buf),
            log_path,
            observer,
        })
        .await?;
    if !output.success {
        return Err(SplatError::Process(format!(
            "FFmpeg 自适应原图候选抽帧退出码 {:?}",
            output.exit_code
        )));
    }
    let mapped = parse_proxy_mapping(&tokio::fs::read_to_string(&mapping_path).await?)?;
    let candidate_count = jpeg_count(&candidates).await?;
    if candidate_count != mapped.len() as u64 {
        return Err(SplatError::Process(format!(
            "自适应原图候选映射 {} 帧，实际输出 {candidate_count} 帧",
            mapped.len()
        )));
    }
    let selected_indices = selected
        .iter()
        .map(|frame| frame.source_index)
        .collect::<BTreeSet<_>>();
    let mut promoted = 0_u64;
    for (index, source) in mapped.iter().enumerate() {
        let candidate = candidates.join(format!("candidate_{:06}.jpg", index + 1));
        if selected_indices.contains(&source.source_index) {
            let destination = output_directory.join(format!("frame_{:06}.jpg", promoted + 1));
            tokio::fs::rename(&candidate, destination).await?;
            promoted += 1;
        } else {
            tokio::fs::remove_file(&candidate).await?;
        }
    }
    if promoted != selected_indices.len() as u64 {
        return Err(SplatError::Process(format!(
            "自适应原图映射未覆盖全部关键帧：请求 {} 帧，匹配 {promoted} 帧",
            selected_indices.len()
        )));
    }
    Ok(promoted)
}

/// Re-runs the proxy's official `fps` sampler, but uses the proxy candidate
/// order to select only the already-approved keyframes before they are encoded.
/// This keeps VFR/source-index validation while avoiding temporary JPEG output
/// for every rejected candidate.
pub async fn extract_selected_proxy_frames(
    executable: &Path,
    input: &Path,
    output_directory: &Path,
    work_directory: &Path,
    selected: &[SelectedSourceFrame],
    proxy_candidates: &[SourceFrameTimestamp],
    analysis_fps: f64,
    hw_accel: FfmpegHwAccel,
    log_path: Option<PathBuf>,
    process_manager: &ProcessManager,
    observer: Option<ProcessObserver>,
) -> Result<u64> {
    if !input.is_file() {
        return Err(SplatError::InvalidPath(input.to_path_buf()));
    }
    if !analysis_fps.is_finite() || analysis_fps <= 0.0 {
        return Err(SplatError::Process("自适应原图抽帧 FPS 无效".into()));
    }
    let expected = selected_proxy_candidates(selected, proxy_candidates)?;
    tokio::fs::create_dir_all(output_directory).await?;
    tokio::fs::create_dir_all(work_directory).await?;
    ensure_no_jpegs(output_directory).await?;
    let mapping_path = work_directory.join("adaptive-original-map.txt");
    let filter_script = work_directory.join("adaptive-selected-filter.txt");
    tokio::fs::write(
        &filter_script,
        selected_proxy_filter(analysis_fps, &expected),
    )
    .await?;
    let output_pattern = output_directory.join("frame_%06d.jpg");
    let mut args = vec![
        OsString::from("-hide_banner"),
        OsString::from("-nostdin"),
        OsString::from("-nostats"),
        OsString::from("-y"),
    ];
    match hw_accel {
        FfmpegHwAccel::Off => {}
        FfmpegHwAccel::Auto => args.extend([OsString::from("-hwaccel"), OsString::from("auto")]),
        FfmpegHwAccel::D3d11va => {
            args.extend([OsString::from("-hwaccel"), OsString::from("d3d11va")])
        }
        FfmpegHwAccel::Cuda => args.extend([OsString::from("-hwaccel"), OsString::from("cuda")]),
    }
    args.extend([
        OsString::from("-i"),
        input.as_os_str().to_owned(),
        OsString::from("-filter_script:v"),
        filter_script.as_os_str().to_owned(),
        OsString::from("-fps_mode:v"),
        OsString::from("passthrough"),
        OsString::from("-q:v"),
        OsString::from("2"),
        OsString::from("-start_number"),
        OsString::from("1"),
        OsString::from("-stats_mux_pre:v"),
        mapping_path.as_os_str().to_owned(),
        OsString::from("-stats_mux_pre_fmt:v"),
        OsString::from("{ni} {ti}"),
        OsString::from("-progress"),
        OsString::from("pipe:1"),
        output_pattern.as_os_str().to_owned(),
    ]);
    let output = process_manager
        .run(ProcessSpec {
            executable: executable.to_path_buf(),
            args,
            working_directory: work_directory.parent().map(Path::to_path_buf),
            log_path,
            observer,
        })
        .await?;
    if output.cancelled {
        return Err(SplatError::Cancelled);
    }
    if !output.success {
        return Err(SplatError::Process(format!(
            "FFmpeg 自适应原图定向抽帧退出码 {:?}",
            output.exit_code
        )));
    }
    let mapped = parse_proxy_mapping(&tokio::fs::read_to_string(&mapping_path).await?)?;
    if mapped.len() != expected.len() {
        return Err(SplatError::Process(format!(
            "自适应原图定向抽帧数量不匹配：期望 {}，实际 {}",
            expected.len(),
            mapped.len()
        )));
    }
    if mapped
        .iter()
        .zip(&expected)
        .any(|(actual, expected)| actual.source_index != expected.1.source_index)
    {
        return Err(SplatError::Process(
            "自适应原图定向抽帧的源帧映射与代理候选不一致".into(),
        ));
    }
    let output_count = jpeg_count(output_directory).await?;
    if output_count != expected.len() as u64 {
        return Err(SplatError::Process(format!(
            "自适应原图定向抽帧 JPEG 数量不匹配：期望 {}，实际 {output_count}",
            expected.len()
        )));
    }
    Ok(output_count)
}

fn selected_proxy_candidates(
    selected: &[SelectedSourceFrame],
    proxy_candidates: &[SourceFrameTimestamp],
) -> Result<Vec<(usize, SourceFrameTimestamp)>> {
    if selected.is_empty() {
        return Err(SplatError::Process("自适应抽帧没有可选源帧".into()));
    }
    let selected_indices = selected
        .iter()
        .map(|frame| frame.source_index)
        .collect::<BTreeSet<_>>();
    let expected = proxy_candidates
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, candidate)| selected_indices.contains(&candidate.source_index))
        .collect::<Vec<_>>();
    if expected.len() != selected_indices.len() {
        return Err(SplatError::Process(format!(
            "自适应关键帧不在代理候选映射中：请求 {}，匹配 {}",
            selected_indices.len(),
            expected.len()
        )));
    }
    Ok(expected)
}

fn selected_proxy_filter(analysis_fps: f64, expected: &[(usize, SourceFrameTimestamp)]) -> String {
    let select = balanced_select_expression(expected);
    format!(
        "fps=fps={analysis_fps:.8}:round=near,select='{select}',scale='min(1920,iw)':'min(1920,ih)':force_original_aspect_ratio=decrease"
    )
}

/// FFmpeg's expression parser may report its generic allocation error for a
/// long left-associative `a+b+c+...` select expression. Compact consecutive
/// candidate ordinals into ranges, then combine the terms as a balanced tree
/// so both the token count and parser depth remain bounded on long clips.
fn balanced_select_expression(expected: &[(usize, SourceFrameTimestamp)]) -> String {
    let ordinals = expected
        .iter()
        .map(|(ordinal, _)| *ordinal)
        .collect::<Vec<_>>();
    let mut terms = Vec::new();
    let mut start = ordinals[0];
    let mut end = start;
    for ordinal in ordinals.into_iter().skip(1) {
        if ordinal == end + 1 {
            end = ordinal;
        } else {
            terms.push(select_range_term(start, end));
            start = ordinal;
            end = ordinal;
        }
    }
    terms.push(select_range_term(start, end));
    while terms.len() > 1 {
        terms = terms
            .chunks(2)
            .map(|pair| match pair {
                [left, right] => format!("max({left}\\,{right})"),
                [only] => only.clone(),
                _ => unreachable!("chunks(2) never returns an empty slice"),
            })
            .collect();
    }
    terms
        .pop()
        .expect("selected proxy candidates are never empty")
}

fn select_range_term(start: usize, end: usize) -> String {
    if start == end {
        format!("eq(n\\,{start})")
    } else {
        format!("between(n\\,{start}\\,{end})")
    }
}

async fn ensure_no_jpegs(directory: &Path) -> Result<()> {
    if jpeg_count(directory).await? != 0 {
        return Err(SplatError::Process(
            "代理目录中已有 JPEG；为避免混用残缺结果，任务已停止".into(),
        ));
    }
    Ok(())
}

async fn jpeg_count(directory: &Path) -> Result<u64> {
    let mut count = 0;
    let mut entries = tokio::fs::read_dir(directory).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry
            .path()
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jpg"))
        {
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected(source_index: u64, pts_seconds: f64) -> SelectedSourceFrame {
        SelectedSourceFrame {
            source_index,
            pts_seconds,
            reason: crate::video::SelectionReason::MotionTarget,
            motion: 0.0,
            inliers: 0,
            grid_coverage: 0.0,
            sharpness: 0.0,
        }
    }

    #[test]
    fn proxy_mapping_uses_ffmpeg_input_indices_not_average_fps() {
        let mapped = parse_proxy_mapping("0 0.000000\n2 0.101000\n3 0.500000\n").unwrap();
        assert_eq!(
            mapped,
            vec![
                SourceFrameTimestamp {
                    source_index: 0,
                    pts_seconds: 0.0
                },
                SourceFrameTimestamp {
                    source_index: 2,
                    pts_seconds: 0.101
                },
                SourceFrameTimestamp {
                    source_index: 3,
                    pts_seconds: 0.5
                },
            ]
        );
    }

    #[test]
    fn proxy_mapping_rejects_unknown_source_index() {
        let error = parse_proxy_mapping("-1 0.0\n").unwrap_err();
        assert!(error.to_string().contains("索引"));
    }

    #[test]
    fn proxy_buffer_limit_never_drops_below_the_safe_minimum() {
        let (frames, _) = proxy_buffered_frame_limit(3840, 2160);
        assert!(frames >= MIN_PROXY_BUFFERED_FRAMES);
    }

    #[test]
    fn direct_original_filter_uses_proxy_candidate_order() {
        let candidates = [
            SourceFrameTimestamp {
                source_index: 4,
                pts_seconds: 0.0,
            },
            SourceFrameTimestamp {
                source_index: 11,
                pts_seconds: 0.2,
            },
            SourceFrameTimestamp {
                source_index: 18,
                pts_seconds: 0.4,
            },
        ];
        let selected = [selected(18, 0.4), selected(4, 0.0)];
        let expected = selected_proxy_candidates(&selected, &candidates).unwrap();
        assert_eq!(
            expected
                .iter()
                .map(|(ordinal, _)| *ordinal)
                .collect::<Vec<_>>(),
            vec![0, 2]
        );
        assert_eq!(
            selected_proxy_filter(6.0, &expected),
            "fps=fps=6.00000000:round=near,select='max(eq(n\\,0)\\,eq(n\\,2))',scale='min(1920,iw)':'min(1920,ih)':force_original_aspect_ratio=decrease"
        );
    }

    #[test]
    fn direct_original_filter_compresses_ranges_and_bounds_expression_depth() {
        let expected = [0, 1, 2, 7, 8, 15, 20, 21, 22, 23, 30]
            .into_iter()
            .map(|ordinal| {
                (
                    ordinal,
                    SourceFrameTimestamp {
                        source_index: ordinal as u64,
                        pts_seconds: ordinal as f64,
                    },
                )
            })
            .collect::<Vec<_>>();
        let expression = balanced_select_expression(&expected);
        assert!(expression.contains("between(n\\,0\\,2)"));
        assert!(expression.contains("between(n\\,20\\,23)"));
        assert!(expression.starts_with("max("));
    }

    #[test]
    fn direct_original_filter_rejects_missing_proxy_mapping() {
        let selected = [selected(9, 0.0)];
        let error = selected_proxy_candidates(&selected, &[]).unwrap_err();
        assert!(error.to_string().contains("代理候选映射"));
    }
}

pub async fn extract_uniform_frames(
    executable: &Path,
    input: &Path,
    output_directory: &Path,
    plan: &FramePlan,
    hw_accel: FfmpegHwAccel,
    log_path: Option<PathBuf>,
    process_manager: &ProcessManager,
    observer: Option<ProcessObserver>,
) -> Result<u64> {
    if !input.is_file() {
        return Err(SplatError::InvalidPath(input.to_path_buf()));
    }
    tokio::fs::create_dir_all(output_directory).await?;
    let mut entries = tokio::fs::read_dir(output_directory).await?;
    while let Some(entry) = entries.next_entry().await? {
        let is_jpeg = entry
            .path()
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jpg"));
        if is_jpeg {
            return Err(SplatError::Process(
                "抽帧目录中已有 JPEG；为避免混用残缺结果，任务已停止".into(),
            ));
        }
    }

    let filter = format!(
        "fps={:.8},scale='min(1920,iw)':'min(1920,ih)':force_original_aspect_ratio=decrease",
        plan.sampling_fps,
    );
    let output_pattern = output_directory.join("frame_%06d.jpg");
    let mut args = vec![
        OsString::from("-hide_banner"),
        OsString::from("-nostdin"),
        OsString::from("-nostats"),
        OsString::from("-y"),
    ];
    match hw_accel {
        FfmpegHwAccel::Off => {}
        FfmpegHwAccel::Auto => {
            args.extend([OsString::from("-hwaccel"), OsString::from("auto")]);
        }
        FfmpegHwAccel::D3d11va => {
            args.extend([OsString::from("-hwaccel"), OsString::from("d3d11va")]);
        }
        FfmpegHwAccel::Cuda => {
            args.extend([OsString::from("-hwaccel"), OsString::from("cuda")]);
        }
    }
    args.extend([
        OsString::from("-i"),
        input.as_os_str().to_owned(),
        OsString::from("-vf"),
        OsString::from(filter),
        OsString::from("-q:v"),
        OsString::from("2"),
        OsString::from("-start_number"),
        OsString::from("1"),
        OsString::from("-progress"),
        OsString::from("pipe:1"),
        output_pattern.as_os_str().to_owned(),
    ]);
    let output = process_manager
        .run(ProcessSpec {
            executable: executable.to_path_buf(),
            args,
            working_directory: output_directory.parent().map(Path::to_path_buf),
            log_path,
            observer,
        })
        .await?;
    if !output.success {
        return Err(SplatError::Process(format!(
            "FFmpeg 退出码 {:?}",
            output.exit_code
        )));
    }

    let mut count = 0;
    let mut entries = tokio::fs::read_dir(output_directory).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry
            .path()
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jpg"))
        {
            count += 1;
        }
    }
    if count == 0 {
        return Err(SplatError::Process("FFmpeg 未输出任何画面".into()));
    }
    Ok(count)
}
