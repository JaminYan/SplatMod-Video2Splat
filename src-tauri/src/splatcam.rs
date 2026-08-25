//! Strict inspection for Splatcam exports before they enter the training pipeline.
//!
//! Splatcam exports already contain RGB images, COLMAP text poses and an RGB point cloud.
//! They are deliberately kept separate from the video workflow: inspecting an export must not
//! invoke FFmpeg, feature extraction, matching or a mapper.

use crate::error::{Result, SplatError};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};

const MINIMUM_INITIAL_POINTS: u64 = 100;
const PROJECTION_SAMPLE_LIMIT: usize = 4_096;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SplatcamImportReport {
    pub source_path: PathBuf,
    pub coordinate_convention: &'static str,
    pub has_depth: bool,
    pub has_transforms: bool,
    pub image_count: u64,
    pub camera_count: u64,
    pub pose_count: u64,
    pub point_count: u64,
    pub points_have_observation_tracks: bool,
    pub positive_depth_projection_ratio: f64,
    pub in_image_projection_ratio: f64,
    pub camera_trajectory_extent: f64,
    pub geometry_gate: SplatcamGeometryGate,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SplatcamGeometryGate {
    pub passed: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
struct Camera {
    width: u32,
    height: u32,
    fx: f64,
    fy: f64,
    cx: f64,
    cy: f64,
}

#[derive(Debug, Clone)]
struct Pose {
    camera_id: u64,
    name: String,
    q: [f64; 4],
    t: [f64; 3],
}

/// Reads and validates the currently supported Splatcam RGB + COLMAP-text + RGB-point-cloud
/// export. This is intentionally synchronous so callers can use `spawn_blocking` for large PLYs.
pub fn inspect_export(source: &Path) -> Result<SplatcamImportReport> {
    let images_dir = source.join("images");
    let model_dir = source.join("sparse").join("0");
    let cameras = parse_cameras(&model_dir.join("cameras.txt"))?;
    let poses = parse_poses(&model_dir.join("images.txt"))?;
    let image_count = validate_images(&images_dir, &cameras, &poses)?;
    let ply = parse_rgb_point_cloud(&model_dir.join("points3D.ply"))?;
    if ply.points.len() < MINIMUM_INITIAL_POINTS as usize {
        return Err(SplatError::Process(format!(
            "Splatcam 点云只有 {} 点，低于训练初始化门槛 {MINIMUM_INITIAL_POINTS}",
            ply.points.len()
        )));
    }
    let (positive_depth_projection_ratio, in_image_projection_ratio, camera_trajectory_extent) =
        projection_statistics(&cameras, &poses, &ply.points);
    let geometry_gate = geometry_gate(
        positive_depth_projection_ratio,
        in_image_projection_ratio,
        camera_trajectory_extent,
    );
    Ok(SplatcamImportReport {
        source_path: source.to_path_buf(),
        coordinate_convention: "colmap-world-to-camera",
        has_depth: contains_depth_files(source),
        has_transforms: source.join("transforms.json").is_file(),
        image_count,
        camera_count: cameras.len() as u64,
        pose_count: poses.len() as u64,
        point_count: ply.points.len() as u64,
        points_have_observation_tracks: false,
        positive_depth_projection_ratio,
        in_image_projection_ratio,
        camera_trajectory_extent,
        geometry_gate,
    })
}

/// Creates a text COLMAP model in a new isolated directory. The exported PLY becomes
/// `points3D.txt` with stable one-based IDs and zero-length tracks; no synthetic 2D matches
/// are introduced. The caller can pass this directory to COLMAP `model_converter`.
pub fn prepare_normalized_text_model(
    source: &Path,
    destination: &Path,
) -> Result<SplatcamImportReport> {
    let report = inspect_export(source)?;
    if destination.exists() {
        return Err(SplatError::Process(format!(
            "Splatcam 标准化目录已存在，拒绝覆盖：{}",
            destination.display()
        )));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| SplatError::InvalidPath(destination.to_path_buf()))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".splatcam-normalized-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        fs::create_dir_all(&temporary)?;
        let model_source = source.join("sparse").join("0");
        fs::copy(
            model_source.join("cameras.txt"),
            temporary.join("cameras.txt"),
        )?;
        fs::copy(
            model_source.join("images.txt"),
            temporary.join("images.txt"),
        )?;
        let cloud = parse_rgb_point_cloud(&model_source.join("points3D.ply"))?;
        write_points3d_text(&temporary.join("points3D.txt"), &cloud.points)
    })();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&temporary);
        return Err(error);
    }
    fs::rename(temporary, destination)?;
    Ok(report)
}

/// Stages the supported export in the project without mutating the original. JPEGs and model
/// files use hard links where possible, then fall back to a regular copy (for example across
/// volumes). The destination must be new so a failed or stale import is never silently reused.
pub fn stage_source_export(source: &Path, destination: &Path) -> Result<()> {
    if destination.exists() {
        return Err(SplatError::Process(format!(
            "Splatcam 来源快照已存在，拒绝覆盖：{}",
            destination.display()
        )));
    }
    let temporary =
        destination.with_file_name(format!(".splatcam-source-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let image_destination = temporary.join("images");
        let model_destination = temporary.join("sparse").join("0");
        fs::create_dir_all(&image_destination)?;
        fs::create_dir_all(&model_destination)?;
        for entry in fs::read_dir(source.join("images"))? {
            let source_file = entry?.path();
            if source_file.is_file() && is_jpeg(&source_file) {
                link_or_copy(
                    &source_file,
                    &image_destination.join(
                        source_file
                            .file_name()
                            .ok_or_else(|| SplatError::InvalidPath(source_file.clone()))?,
                    ),
                )?;
            }
        }
        for name in ["cameras.txt", "images.txt", "points3D.ply"] {
            let source_file = source.join("sparse").join("0").join(name);
            link_or_copy(&source_file, &model_destination.join(name))?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&temporary);
        return Err(error);
    }
    fs::rename(temporary, destination)?;
    Ok(())
}

fn link_or_copy(source: &Path, destination: &Path) -> Result<()> {
    if fs::hard_link(source, destination).is_err() {
        fs::copy(source, destination)?;
    }
    Ok(())
}

/// Verifies the three binary model count headers generated by COLMAP. This is intentionally
/// minimal and avoids accepting an empty or unrelated binary directory as a training model.
pub fn verify_binary_model_counts(
    model: &Path,
    expected_cameras: u64,
    expected_images: u64,
    expected_points: u64,
) -> Result<()> {
    for (name, expected) in [
        ("cameras.bin", expected_cameras),
        ("images.bin", expected_images),
        ("points3D.bin", expected_points),
    ] {
        let path = model.join(name);
        let mut file = fs::File::open(&path).map_err(|_| missing(&path))?;
        let mut bytes = [0_u8; 8];
        file.read_exact(&mut bytes).map_err(|_| {
            SplatError::Process(format!("COLMAP 二进制模型损坏：{}", path.display()))
        })?;
        let actual = u64::from_le_bytes(bytes);
        if actual != expected {
            return Err(SplatError::Process(format!(
                "COLMAP 二进制记录数不一致 {name}：{actual}，预期 {expected}"
            )));
        }
        if file.metadata()?.len() <= 8 {
            return Err(SplatError::Process(format!(
                "COLMAP 二进制模型为空：{}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn write_points3d_text(path: &Path, points: &[RgbPoint]) -> Result<()> {
    let mut output = fs::File::create(path)?;
    use std::io::Write;
    writeln!(output, "# 3D point list with one line of data per point:")?;
    writeln!(output, "#   POINT3D_ID, X, Y, Z, R, G, B, ERROR, TRACK[]")?;
    writeln!(
        output,
        "# Number of points: {}, mean track length: 0",
        points.len()
    )?;
    for (index, point) in points.iter().enumerate() {
        writeln!(
            output,
            "{} {:.9} {:.9} {:.9} {} {} {} 0",
            index + 1,
            point.position[0],
            point.position[1],
            point.position[2],
            point.rgb[0],
            point.rgb[1],
            point.rgb[2]
        )?;
    }
    Ok(())
}

fn parse_cameras(path: &Path) -> Result<HashMap<u64, Camera>> {
    let file = fs::File::open(path).map_err(|_| missing(path))?;
    let mut cameras = HashMap::new();
    for (line_number, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 8 || fields[1] != "PINHOLE" {
            return Err(SplatError::Process(format!(
                "{}:{} 仅支持 PINHOLE WIDTH HEIGHT FX FY CX CY",
                path.display(),
                line_number + 1
            )));
        }
        let id = parse_u64(fields[0], path, line_number)?;
        let width = parse_u32(fields[2], path, line_number)?;
        let height = parse_u32(fields[3], path, line_number)?;
        let fx = parse_finite(fields[4], path, line_number)?;
        let fy = parse_finite(fields[5], path, line_number)?;
        let cx = parse_finite(fields[6], path, line_number)?;
        let cy = parse_finite(fields[7], path, line_number)?;
        if width == 0 || height == 0 || fx <= 0.0 || fy <= 0.0 {
            return Err(SplatError::Process(format!(
                "{}:{} 相机尺寸与焦距必须大于 0",
                path.display(),
                line_number + 1
            )));
        }
        if !(-(width as f64)..=width as f64 * 2.0).contains(&cx)
            || !(-(height as f64)..=height as f64 * 2.0).contains(&cy)
        {
            return Err(SplatError::Process(format!(
                "{}:{} 主点超出合理图像范围",
                path.display(),
                line_number + 1
            )));
        }
        if cameras
            .insert(
                id,
                Camera {
                    width,
                    height,
                    fx,
                    fy,
                    cx,
                    cy,
                },
            )
            .is_some()
        {
            return Err(SplatError::Process(format!(
                "{} 相机 ID {id} 重复",
                path.display()
            )));
        }
    }
    if cameras.is_empty() {
        return Err(SplatError::Process(format!(
            "{} 没有相机记录",
            path.display()
        )));
    }
    Ok(cameras)
}

fn parse_poses(path: &Path) -> Result<Vec<Pose>> {
    let file = fs::File::open(path).map_err(|_| missing(path))?;
    let mut poses = Vec::new();
    let mut image_ids = HashSet::new();
    let mut names = HashSet::new();
    let lines: Vec<_> = BufReader::new(file)
        .lines()
        .collect::<std::io::Result<_>>()?;
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index].trim();
        let line_number = index;
        index += 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 10 {
            return Err(SplatError::Process(format!(
                "{}:{} 不是有效的 COLMAP 图像位姿记录",
                path.display(),
                line_number + 1
            )));
        }
        let image_id = match fields[0].parse::<u64>() {
            Ok(value) => value,
            Err(_) => continue,
        };
        let q = [
            parse_finite(fields[1], path, line_number)?,
            parse_finite(fields[2], path, line_number)?,
            parse_finite(fields[3], path, line_number)?,
            parse_finite(fields[4], path, line_number)?,
        ];
        let t = [
            parse_finite(fields[5], path, line_number)?,
            parse_finite(fields[6], path, line_number)?,
            parse_finite(fields[7], path, line_number)?,
        ];
        if q.iter().map(|v| v * v).sum::<f64>().sqrt() <= 1e-9 {
            return Err(SplatError::Process(format!(
                "{}:{} 四元数长度为零",
                path.display(),
                line_number + 1
            )));
        }
        let camera_id = parse_u64(fields[8], path, line_number)?;
        let name = fields[9].to_string();
        if !image_ids.insert(image_id) || !names.insert(name.clone()) {
            return Err(SplatError::Process(format!(
                "{} 图像 ID 或文件名重复",
                path.display()
            )));
        }
        poses.push(Pose {
            camera_id,
            name,
            q,
            t,
        });
        // COLMAP stores one POINTS2D line after every image pose. It may be empty;
        // consume it unconditionally so a long numeric track line can never be
        // mistaken for another pose.
        if index >= lines.len() {
            return Err(SplatError::Process(format!(
                "{}:{} 缺少位姿对应的 POINTS2D 行",
                path.display(),
                line_number + 1
            )));
        }
        index += 1;
    }
    if poses.is_empty() {
        return Err(SplatError::Process(format!(
            "{} 没有位姿记录",
            path.display()
        )));
    }
    Ok(poses)
}

fn validate_images(
    images_dir: &Path,
    cameras: &HashMap<u64, Camera>,
    poses: &[Pose],
) -> Result<u64> {
    if !images_dir.is_dir() {
        return Err(missing(images_dir));
    }
    let mut disk = HashSet::new();
    for entry in fs::read_dir(images_dir)? {
        let path = entry?.path();
        if !path.is_file() || !is_jpeg(&path) {
            continue;
        }
        disk.insert(
            path.file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| SplatError::InvalidPath(path.clone()))?
                .to_string(),
        );
    }
    if disk.len() != poses.len() {
        return Err(SplatError::Process(format!(
            "Splatcam RGB 数量 {} 与位姿数量 {} 不一致",
            disk.len(),
            poses.len()
        )));
    }
    for pose in poses {
        let camera = cameras.get(&pose.camera_id).ok_or_else(|| {
            SplatError::Process(format!(
                "图像 {} 引用了不存在的相机 {}",
                pose.name, pose.camera_id
            ))
        })?;
        let path = images_dir.join(&pose.name);
        if !disk.contains(&pose.name) {
            return Err(SplatError::Process(format!(
                "缺少位姿对应 RGB：{}",
                pose.name
            )));
        }
        let (width, height) = image::image_dimensions(&path).map_err(|error| {
            SplatError::Process(format!("RGB 无法解码 {}：{error}", path.display()))
        })?;
        if width != camera.width || height != camera.height {
            return Err(SplatError::Process(format!(
                "RGB 尺寸与相机不一致 {}：{}x{}，预期 {}x{}",
                pose.name, width, height, camera.width, camera.height
            )));
        }
    }
    Ok(disk.len() as u64)
}

struct RgbPointCloud {
    points: Vec<RgbPoint>,
}

struct RgbPoint {
    position: [f64; 3],
    rgb: [u8; 3],
}

fn parse_rgb_point_cloud(path: &Path) -> Result<RgbPointCloud> {
    let mut file = fs::File::open(path).map_err(|_| missing(path))?;
    let mut header = Vec::new();
    let mut byte = [0_u8; 1];
    while file.read_exact(&mut byte).is_ok() {
        header.push(byte[0]);
        if header.ends_with(b"end_header\n") {
            break;
        }
        if header.len() > 64 * 1024 {
            return Err(SplatError::Process("PLY 头部过大或缺少 end_header".into()));
        }
    }
    let text = std::str::from_utf8(&header)
        .map_err(|_| SplatError::Process("PLY 头部不是 UTF-8 文本".into()))?;
    let mut vertex_count = None;
    let mut properties = Vec::new();
    for line in text.lines() {
        let field: Vec<_> = line.split_whitespace().collect();
        if field.starts_with(&["format", "binary_little_endian", "1.0"]) {
            continue;
        }
        if field.first() == Some(&"element") && field.get(1) == Some(&"vertex") {
            vertex_count = field.get(2).and_then(|value| value.parse::<usize>().ok());
        }
        if field.first() == Some(&"property") {
            if field.get(1) == Some(&"list") {
                return Err(SplatError::Process("当前不支持 PLY list 属性".into()));
            }
            if let (Some(kind), Some(name)) = (field.get(1), field.get(2)) {
                properties.push(((*name).to_string(), scalar_type(kind)?));
            }
        }
    }
    if !text.starts_with("ply\n") || !text.contains("format binary_little_endian 1.0") {
        return Err(SplatError::Process(
            "仅支持 binary_little_endian PLY".into(),
        ));
    }
    let count = vertex_count
        .filter(|count| *count > 0)
        .ok_or_else(|| SplatError::Process("PLY 没有有效 vertex 数量".into()))?;
    let offsets = property_offsets(&properties)?;
    for name in ["x", "y", "z", "red", "green", "blue"] {
        if !offsets.contains_key(name) {
            return Err(SplatError::Process(format!("PLY 缺少 {name} 属性")));
        }
    }
    for name in ["red", "green", "blue"] {
        if offsets[name].1 != Scalar::U8 {
            return Err(SplatError::Process(format!(
                "PLY 的 {name} 必须是 uchar RGB 属性"
            )));
        }
    }
    let stride: usize = properties.iter().map(|(_, kind)| kind.size()).sum();
    let mut payload = Vec::new();
    file.read_to_end(&mut payload)?;
    if payload.len()
        != count
            .checked_mul(stride)
            .ok_or_else(|| SplatError::Process("PLY 顶点数据溢出".into()))?
    {
        return Err(SplatError::Process("PLY 文件长度与顶点布局不一致".into()));
    }
    let mut points = Vec::with_capacity(count);
    for record in payload.chunks_exact(stride) {
        let x = read_number(record, offsets["x"])?;
        let y = read_number(record, offsets["y"])?;
        let z = read_number(record, offsets["z"])?;
        if !(x.is_finite() && y.is_finite() && z.is_finite()) {
            return Err(SplatError::Process("PLY 含有 NaN 或无穷 XYZ".into()));
        }
        points.push(RgbPoint {
            position: [x, y, z],
            rgb: [
                read_number(record, offsets["red"])? as u8,
                read_number(record, offsets["green"])? as u8,
                read_number(record, offsets["blue"])? as u8,
            ],
        });
    }
    Ok(RgbPointCloud { points })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scalar {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    F32,
    F64,
}
impl Scalar {
    fn size(self) -> usize {
        match self {
            Self::U8 | Self::I8 => 1,
            Self::U16 | Self::I16 => 2,
            Self::U32 | Self::I32 | Self::F32 => 4,
            Self::F64 => 8,
        }
    }
}
fn scalar_type(value: &str) -> Result<Scalar> {
    match value {
        "uchar" | "uint8" => Ok(Scalar::U8),
        "char" | "int8" => Ok(Scalar::I8),
        "ushort" | "uint16" => Ok(Scalar::U16),
        "short" | "int16" => Ok(Scalar::I16),
        "uint" | "uint32" => Ok(Scalar::U32),
        "int" | "int32" => Ok(Scalar::I32),
        "float" | "float32" => Ok(Scalar::F32),
        "double" | "float64" => Ok(Scalar::F64),
        _ => Err(SplatError::Process(format!("不支持 PLY 属性类型：{value}"))),
    }
}
fn property_offsets(properties: &[(String, Scalar)]) -> Result<HashMap<&str, (usize, Scalar)>> {
    let mut offset = 0;
    let mut result = HashMap::new();
    for (name, kind) in properties {
        result.insert(name.as_str(), (offset, *kind));
        offset += kind.size();
    }
    Ok(result)
}
fn read_number(data: &[u8], descriptor: (usize, Scalar)) -> Result<f64> {
    let (offset, kind) = descriptor;
    let bytes = &data[offset..offset + kind.size()];
    Ok(match kind {
        Scalar::U8 => bytes[0] as f64,
        Scalar::I8 => bytes[0] as i8 as f64,
        Scalar::U16 => u16::from_le_bytes(bytes.try_into().unwrap()) as f64,
        Scalar::I16 => i16::from_le_bytes(bytes.try_into().unwrap()) as f64,
        Scalar::U32 => u32::from_le_bytes(bytes.try_into().unwrap()) as f64,
        Scalar::I32 => i32::from_le_bytes(bytes.try_into().unwrap()) as f64,
        Scalar::F32 => f32::from_le_bytes(bytes.try_into().unwrap()) as f64,
        Scalar::F64 => f64::from_le_bytes(bytes.try_into().unwrap()),
    })
}

fn projection_statistics(
    cameras: &HashMap<u64, Camera>,
    poses: &[Pose],
    points: &[RgbPoint],
) -> (f64, f64, f64) {
    let step = (points.len() / PROJECTION_SAMPLE_LIMIT).max(1);
    let sample: Vec<_> = points
        .iter()
        .step_by(step)
        .take(PROJECTION_SAMPLE_LIMIT)
        .collect();
    let mut positive = 0_u64;
    let mut covered_points = 0_u64;
    let total = (sample.len() * poses.len()).max(1) as u64;
    let mut centers = Vec::with_capacity(poses.len());
    for pose in poses {
        let r = rotation_matrix(pose.q);
        centers.push(camera_center(r, pose.t));
        for point in &sample {
            let z = r[2][0] * point.position[0]
                + r[2][1] * point.position[1]
                + r[2][2] * point.position[2]
                + pose.t[2];
            if z > 0.0 {
                positive += 1;
            }
        }
    }
    // Splatcam's point cloud has no 2D observation tracks. A point is therefore
    // useful when it projects into at least one imported image, rather than when
    // it is visible from every camera along the capture path.
    for point in &sample {
        let visible = poses.iter().any(|pose| {
            let camera = &cameras[&pose.camera_id];
            let r = rotation_matrix(pose.q);
            let x = r[0][0] * point.position[0]
                + r[0][1] * point.position[1]
                + r[0][2] * point.position[2]
                + pose.t[0];
            let y = r[1][0] * point.position[0]
                + r[1][1] * point.position[1]
                + r[1][2] * point.position[2]
                + pose.t[1];
            let z = r[2][0] * point.position[0]
                + r[2][1] * point.position[1]
                + r[2][2] * point.position[2]
                + pose.t[2];
            if z <= 0.0 {
                return false;
            }
            let u = camera.fx * x / z + camera.cx;
            let v = camera.fy * y / z + camera.cy;
            (0.0..camera.width as f64).contains(&u) && (0.0..camera.height as f64).contains(&v)
        });
        if visible {
            covered_points += 1;
        }
    }
    let (mut min, mut max) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for center in centers {
        for axis in 0..3 {
            min[axis] = min[axis].min(center[axis]);
            max[axis] = max[axis].max(center[axis]);
        }
    }
    let extent = ((0..3)
        .map(|axis| (max[axis] - min[axis]).powi(2))
        .sum::<f64>())
    .sqrt();
    (
        positive as f64 / total as f64,
        covered_points as f64 / sample.len().max(1) as f64,
        extent,
    )
}
fn rotation_matrix(q: [f64; 4]) -> [[f64; 3]; 3] {
    let n = q.iter().map(|v| v * v).sum::<f64>().sqrt();
    let (w, x, y, z) = (q[0] / n, q[1] / n, q[2] / n, q[3] / n);
    [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - z * w),
            2.0 * (x * z + y * w),
        ],
        [
            2.0 * (x * y + z * w),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - x * w),
        ],
        [
            2.0 * (x * z - y * w),
            2.0 * (y * z + x * w),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ]
}
fn camera_center(r: [[f64; 3]; 3], t: [f64; 3]) -> [f64; 3] {
    [
        -(r[0][0] * t[0] + r[1][0] * t[1] + r[2][0] * t[2]),
        -(r[0][1] * t[0] + r[1][1] * t[1] + r[2][1] * t[2]),
        -(r[0][2] * t[0] + r[1][2] * t[1] + r[2][2] * t[2]),
    ]
}
fn geometry_gate(positive: f64, inside: f64, extent: f64) -> SplatcamGeometryGate {
    let reason = if positive < 0.70 {
        Some(format!("正深度投影比例 {:.1}% 低于 70%", positive * 100.0))
    } else if inside < 0.50 {
        Some(format!("图像内投影比例 {:.1}% 低于 50%", inside * 100.0))
    } else if !extent.is_finite() || extent <= 1e-9 {
        Some("相机轨迹范围近似为零".into())
    } else {
        None
    };
    SplatcamGeometryGate {
        passed: reason.is_none(),
        reason,
    }
}
fn contains_depth_files(source: &Path) -> bool {
    ["depth", "depths"]
        .iter()
        .any(|name| source.join(name).is_dir())
}
fn is_jpeg(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg"))
}
fn missing(path: &Path) -> SplatError {
    SplatError::Process(format!("Splatcam 导出缺少：{}", path.display()))
}
fn parse_u64(value: &str, path: &Path, line: usize) -> Result<u64> {
    value
        .parse()
        .map_err(|_| SplatError::Process(format!("{}:{} 不是有效整数", path.display(), line + 1)))
}
fn parse_u32(value: &str, path: &Path, line: usize) -> Result<u32> {
    value
        .parse()
        .map_err(|_| SplatError::Process(format!("{}:{} 不是有效整数", path.display(), line + 1)))
}
fn parse_finite(value: &str, path: &Path, line: usize) -> Result<f64> {
    let parsed: f64 = value.parse().map_err(|_| {
        SplatError::Process(format!("{}:{} 不是有效数字", path.display(), line + 1))
    })?;
    if parsed.is_finite() {
        Ok(parsed)
    } else {
        Err(SplatError::Process(format!(
            "{}:{} 不能是 NaN 或无穷值",
            path.display(),
            line + 1
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    fn fixture() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("images")).unwrap();
        fs::create_dir_all(root.join("sparse/0")).unwrap();
        let image = image::RgbImage::new(2, 2);
        image.save(root.join("images/0000.jpg")).unwrap();
        fs::write(root.join("sparse/0/cameras.txt"), "1 PINHOLE 2 2 2 2 1 1\n").unwrap();
        fs::write(
            root.join("sparse/0/images.txt"),
            "1 1 0 0 0 0 0 0 1 0000.jpg\n\n",
        )
        .unwrap();
        let mut ply = fs::File::create(root.join("sparse/0/points3D.ply")).unwrap();
        ply.write_all(b"ply\nformat binary_little_endian 1.0\nelement vertex 100\nproperty float x\nproperty float y\nproperty float z\nproperty uchar red\nproperty uchar green\nproperty uchar blue\nend_header\n").unwrap();
        for _ in 0..100 {
            for value in [0_f32, 0_f32, 1_f32] {
                ply.write_all(&value.to_le_bytes()).unwrap();
            }
            ply.write_all(&[255, 0, 0]).unwrap();
        }
        temp
    }
    #[test]
    fn inspects_supported_export() {
        let temp = fixture();
        let report = inspect_export(temp.path()).unwrap();
        assert_eq!(report.image_count, 1);
        assert_eq!(report.point_count, 100);
        assert!(!report.geometry_gate.passed);
    }
    #[test]
    fn rejects_missing_pose_image() {
        let temp = fixture();
        fs::remove_file(temp.path().join("images/0000.jpg")).unwrap();
        assert!(inspect_export(temp.path()).is_err());
    }
    #[test]
    fn rejects_non_finite_ply_coordinate() {
        let temp = fixture();
        let path = temp.path().join("sparse/0/points3D.ply");
        let mut bytes = fs::read(&path).unwrap();
        let offset = bytes
            .windows(11)
            .position(|value| value == b"end_header\n")
            .unwrap()
            + 11;
        bytes[offset..offset + 4].copy_from_slice(&f32::NAN.to_le_bytes());
        fs::write(path, bytes).unwrap();
        assert!(inspect_export(temp.path()).is_err());
    }

    #[test]
    fn writes_trackless_points_text_without_inventing_observations() {
        let temp = fixture();
        let output = temp.path().join("normalized");
        let report = prepare_normalized_text_model(temp.path(), &output).unwrap();
        assert_eq!(report.point_count, 100);
        let text = fs::read_to_string(output.join("points3D.txt")).unwrap();
        assert!(text.contains("1 0.000000000 0.000000000 1.000000000 255 0 0 0"));
        assert_eq!(text.lines().last().unwrap().split_whitespace().count(), 8);
    }

    #[test]
    fn verifies_binary_count_headers() {
        let temp = tempfile::tempdir().unwrap();
        for (name, count) in [
            ("cameras.bin", 2_u64),
            ("images.bin", 3),
            ("points3D.bin", 4),
        ] {
            let mut file = fs::File::create(temp.path().join(name)).unwrap();
            file.write_all(&count.to_le_bytes()).unwrap();
            file.write_all(&[1]).unwrap();
        }
        verify_binary_model_counts(temp.path(), 2, 3, 4).unwrap();
        assert!(verify_binary_model_counts(temp.path(), 2, 3, 5).is_err());
    }

    #[test]
    fn stages_source_without_mutating_the_export() {
        let temp = fixture();
        let source_ply = temp.path().join("sparse/0/points3D.ply");
        let original_size = source_ply.metadata().unwrap().len();
        let destination = temp.path().join("project/source/splatcam");
        stage_source_export(temp.path(), &destination).unwrap();
        assert!(destination.join("images/0000.jpg").is_file());
        assert_eq!(
            destination
                .join("sparse/0/points3D.ply")
                .metadata()
                .unwrap()
                .len(),
            original_size
        );
        assert_eq!(source_ply.metadata().unwrap().len(), original_size);
    }
}
