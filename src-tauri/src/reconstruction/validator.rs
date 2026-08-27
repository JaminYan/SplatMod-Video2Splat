use std::{
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use serde::Serialize;

use crate::error::{Result, SplatError};

/// 注册率 >= 80% 视为质量良好。
pub const GOOD_REGISTERED_RATIO: f64 = 0.80;
/// 注册率 >= 50% 视为可接受（低于该值判定为失败）。
pub const WARNING_REGISTERED_RATIO: f64 = 0.50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReconstructionQuality {
    Good,
    Warning,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconstructionReport {
    pub quality: ReconstructionQuality,
    pub input_images: u64,
    pub registered_images: u64,
    pub registered_ratio: f64,
    pub points_3d: u64,
}

/// COLMAP `images.bin` 中一条已注册图像的可追溯标识。
///
/// 保留输出文件名即可和 `adaptive-selected-frames.json` 对齐；姿态和二维观测
/// 在弱区诊断阶段按需再解析，避免在普通质量校验路径中持有大量数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisteredImage {
    pub image_id: u32,
    pub name: String,
}

pub struct ReconstructionValidator;

impl ReconstructionValidator {
    /// Inspect the COLMAP sparse model in `model_dir` and report how many of the input
    /// images in `frames_dir` were registered.
    ///
    /// COLMAP 4.1 的 mapper 默认输出二进制模型（cameras.bin / images.bin /
    /// points3D.bin），文件头部即记录数量（u64 LE）。注册率低于 50% 视为失败。
    pub fn validate(frames_dir: &Path, model_dir: &Path) -> Result<ReconstructionReport> {
        let cameras = model_dir.join("cameras.bin");
        let images = model_dir.join("images.bin");
        let points = model_dir.join("points3D.bin");
        for path in [&cameras, &images, &points] {
            if !path.is_file() || path.metadata()?.len() <= 8 {
                return Err(SplatError::Process(format!(
                    "稀疏重建输出不完整：{}",
                    path.display()
                )));
            }
        }
        let input_images = count_jpegs(frames_dir)?;
        let registered_images = read_colmap_count(&images)?;
        let points_3d = read_colmap_count(&points)?;
        if input_images == 0 || registered_images == 0 || points_3d == 0 {
            return Err(SplatError::Process(
                "稀疏重建没有可用的注册图像或三维点".into(),
            ));
        }
        let registered_ratio = registered_images as f64 / input_images as f64;
        let quality = if registered_ratio >= GOOD_REGISTERED_RATIO {
            ReconstructionQuality::Good
        } else if registered_ratio >= WARNING_REGISTERED_RATIO {
            ReconstructionQuality::Warning
        } else {
            ReconstructionQuality::Failed
        };
        Ok(ReconstructionReport {
            quality,
            input_images,
            registered_images,
            registered_ratio,
            points_3d,
        })
    }

    /// 读取 COLMAP `images.bin` 的完整记录，供自适应选帧清单按输出文件名回联。
    pub fn registered_images(model_dir: &Path) -> Result<Vec<RegisteredImage>> {
        let path = model_dir.join("images.bin");
        let file = std::fs::File::open(&path)?;
        read_registered_images(file)
    }
}

fn count_jpegs(directory: &Path) -> Result<u64> {
    let mut count = 0u64;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg"))
        {
            count += 1;
        }
    }
    Ok(count)
}

/// COLMAP 二进制模型文件头部即记录数量（u64 小端）。
fn read_colmap_count(path: &Path) -> Result<u64> {
    let mut file = std::fs::File::open(path)?;
    let mut bytes = [0_u8; 8];
    file.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

/// COLMAP `images.bin`：
/// `u64 count`，随后每项为 `u32 id`、4 个 quaternion f64、3 个 translation
/// f64、`u32 camera_id`、NUL 结尾文件名、`u64 point2D_count`、每点 24 字节。
fn read_registered_images<R: Read + Seek>(mut reader: R) -> Result<Vec<RegisteredImage>> {
    let image_count = read_u64(&mut reader)?;
    let capacity = usize::try_from(image_count).map_err(|_| {
        SplatError::Process(format!("COLMAP 图像数量过大，无法读取：{image_count}"))
    })?;
    let mut images = Vec::with_capacity(capacity);

    for _ in 0..image_count {
        let image_id = read_u32(&mut reader)?;
        // qvec[4] + tvec[3] + camera_id = 7 * f64 + u32 = 60 bytes.
        reader.seek(SeekFrom::Current(60))?;
        let name = read_nul_terminated_name(&mut reader)?;
        let point_count = read_u64(&mut reader)?;
        let point_bytes = point_count.checked_mul(24).ok_or_else(|| {
            SplatError::Process(format!("COLMAP 二维观测数量溢出：{point_count}"))
        })?;
        let point_offset = i64::try_from(point_bytes).map_err(|_| {
            SplatError::Process(format!("COLMAP 二维观测数据过大：{point_bytes} 字节"))
        })?;
        reader.seek(SeekFrom::Current(point_offset))?;
        images.push(RegisteredImage { image_id, name });
    }

    Ok(images)
}

fn read_u32<R: Read>(reader: &mut R) -> Result<u32> {
    let mut bytes = [0_u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64<R: Read>(reader: &mut R) -> Result<u64> {
    let mut bytes = [0_u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn read_nul_terminated_name<R: Read>(reader: &mut R) -> Result<String> {
    const MAX_IMAGE_NAME_BYTES: usize = 16 * 1024;
    let mut bytes = Vec::with_capacity(64);
    for _ in 0..MAX_IMAGE_NAME_BYTES {
        let mut byte = [0_u8; 1];
        reader.read_exact(&mut byte)?;
        if byte[0] == 0 {
            return String::from_utf8(bytes).map_err(|error| {
                SplatError::Process(format!("COLMAP 图像文件名不是 UTF-8：{error}"))
            });
        }
        bytes.push(byte[0]);
    }
    Err(SplatError::Process(format!(
        "COLMAP 图像文件名超过 {MAX_IMAGE_NAME_BYTES} 字节，文件可能已损坏"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn write_jpegs(dir: &Path, count: usize) {
        std::fs::create_dir_all(dir).unwrap();
        for index in 0..count {
            let path = dir.join(format!("frame_{index:06}.jpg"));
            std::fs::write(path, [0u8; 4]).unwrap();
        }
    }

    /// 写入与 COLMAP 二进制输出一致的最小模型（头部数量 + 非空 cameras 文件）。
    fn write_bin_model(dir: &Path, images: u64, points: u64) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("cameras.bin"), vec![0u8; 16]).unwrap();
        let mut image_bytes = images.to_le_bytes().to_vec();
        image_bytes.extend_from_slice(&[0u8; 16]); // 真实文件头部之后还有数据
        std::fs::write(dir.join("images.bin"), image_bytes).unwrap();
        let mut point_bytes = points.to_le_bytes().to_vec();
        point_bytes.extend_from_slice(&[0u8; 16]);
        std::fs::write(dir.join("points3D.bin"), point_bytes).unwrap();
    }

    #[test]
    fn good_quality_when_registration_is_high() {
        let temp = tempfile::tempdir().unwrap();
        let frames = temp.path().join("frames");
        let model = temp.path().join("model");
        write_jpegs(&frames, 10);
        write_bin_model(&model, 9, 5);
        let report = ReconstructionValidator::validate(&frames, &model).unwrap();
        assert_eq!(report.quality, ReconstructionQuality::Good);
        assert_eq!(report.input_images, 10);
        assert_eq!(report.registered_images, 9);
        assert_eq!(report.points_3d, 5);
    }

    #[test]
    fn failed_when_registration_below_half() {
        let temp = tempfile::tempdir().unwrap();
        let frames = temp.path().join("frames");
        let model = temp.path().join("model");
        write_jpegs(&frames, 10);
        write_bin_model(&model, 1, 1);
        let report = ReconstructionValidator::validate(&frames, &model).unwrap();
        assert_eq!(report.quality, ReconstructionQuality::Failed);
    }

    #[test]
    fn rejects_incomplete_binary_model() {
        let temp = tempfile::tempdir().unwrap();
        let frames = temp.path().join("frames");
        let model = temp.path().join("model");
        write_jpegs(&frames, 10);
        std::fs::create_dir_all(&model).unwrap();
        // 缺少 cameras.bin，应判为输出不完整。
        std::fs::write(model.join("images.bin"), 9u64.to_le_bytes()).unwrap();
        std::fs::write(model.join("points3D.bin"), 5u64.to_le_bytes()).unwrap();
        assert!(ReconstructionValidator::validate(&frames, &model).is_err());
    }

    #[test]
    fn reads_registered_image_names_and_skips_observations() {
        let mut bytes = 2u64.to_le_bytes().to_vec();
        write_image_record(&mut bytes, 7, "frame_000003.jpg", 2);
        write_image_record(&mut bytes, 12, "frame_000010.jpg", 0);

        let images = read_registered_images(Cursor::new(bytes)).unwrap();
        assert_eq!(
            images,
            vec![
                RegisteredImage {
                    image_id: 7,
                    name: "frame_000003.jpg".into(),
                },
                RegisteredImage {
                    image_id: 12,
                    name: "frame_000010.jpg".into(),
                },
            ]
        );
    }

    #[test]
    fn rejects_an_unterminated_image_name() {
        let mut bytes = 1u64.to_le_bytes().to_vec();
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 60]);
        bytes.extend(std::iter::repeat_n(b'x', 16 * 1024));
        assert!(read_registered_images(Cursor::new(bytes)).is_err());
    }

    fn write_image_record(bytes: &mut Vec<u8>, id: u32, name: &str, points: u64) {
        bytes.extend_from_slice(&id.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 60]);
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&points.to_le_bytes());
        bytes.extend(std::iter::repeat_n(
            0u8,
            usize::try_from(points * 24).unwrap(),
        ));
    }
}
