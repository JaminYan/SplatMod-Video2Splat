use crate::error::{Result, SplatError};
use serde::Serialize;
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GaussianPlyInfo {
    pub file_size: u64,
    pub splat_count: u64,
    pub header: String,
}

#[derive(Default)]
struct Element {
    name: String,
    count: u64,
    stride: u64,
    properties: Vec<String>,
}

/// Validate Gaussian attributes and binary layout before publishing a PLY. A vertex count alone
/// is not enough: ordinary point clouds must not be accepted as 3D Gaussian output.
pub fn inspect_gaussian_ply(path: &Path) -> Result<GaussianPlyInfo> {
    if !path.is_file() {
        return Err(SplatError::InvalidPath(path.to_path_buf()));
    }
    let file = File::open(path)?;
    let file_size = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    let mut header = String::new();
    let mut elements = Vec::<Element>::new();
    let mut active: Option<Element> = None;
    let mut binary_little_endian = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(SplatError::Process(
                "PLY 文件在读取到 end_header 前结束".into(),
            ));
        }
        header.push_str(&line);
        if header.len() > 64 * 1024 {
            return Err(SplatError::Process(
                "PLY 头部超过 64KB，疑似格式异常".into(),
            ));
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        match fields.as_slice() {
            ["format", "binary_little_endian", "1.0"] => binary_little_endian = true,
            ["element", name, count] => {
                if let Some(element) = active.take() {
                    elements.push(element);
                }
                active = Some(Element {
                    name: (*name).into(),
                    count: count
                        .parse()
                        .map_err(|_| SplatError::Process("PLY vertex 数量无效".into()))?,
                    ..Element::default()
                });
            }
            ["property", ty, name] => {
                let element = active.as_mut().ok_or_else(|| {
                    SplatError::Process("PLY property 出现在 element 之前".into())
                })?;
                element.stride = element
                    .stride
                    .checked_add(scalar_size(ty)?)
                    .ok_or_else(|| SplatError::Process("PLY 顶点布局溢出".into()))?;
                element.properties.push((*name).into());
            }
            ["property", "list", ..] => {
                return Err(SplatError::Process("不支持带 list 属性的 PLY 输出".into()))
            }
            ["end_header"] => {
                if let Some(element) = active.take() {
                    elements.push(element);
                }
                break;
            }
            _ => {}
        }
    }
    if !header.starts_with("ply\n") && !header.starts_with("ply\r\n") {
        return Err(SplatError::Process(
            "文件不是合法 PLY：缺少 ply 魔数".into(),
        ));
    }
    if !binary_little_endian {
        return Err(SplatError::Process(
            "PLY 必须是 binary_little_endian 1.0".into(),
        ));
    }
    let vertex = elements
        .iter()
        .find(|element| element.name.eq_ignore_ascii_case("vertex"))
        .ok_or_else(|| SplatError::Process("PLY 缺少 vertex 元素".into()))?;
    if vertex.count == 0 {
        return Err(SplatError::Process("PLY vertex 数量必须大于 0".into()));
    }
    let required = [
        "x", "y", "z", "f_dc_0", "f_dc_1", "f_dc_2", "opacity", "scale_0", "scale_1", "scale_2",
        "rot_0", "rot_1", "rot_2", "rot_3",
    ];
    if let Some(name) = required
        .iter()
        .find(|name| !vertex.properties.iter().any(|property| property == **name))
    {
        return Err(SplatError::Process(format!(
            "PLY 不是标准 Gaussian 输出：缺少属性 {name}"
        )));
    }
    let header_bytes = reader.stream_position()?;
    let data_bytes = elements.iter().try_fold(0_u64, |total, element| {
        let bytes = element
            .count
            .checked_mul(element.stride)
            .ok_or_else(|| SplatError::Process("PLY 数据长度溢出".into()))?;
        total
            .checked_add(bytes)
            .ok_or_else(|| SplatError::Process("PLY 数据长度溢出".into()))
    })?;
    let expected = header_bytes
        .checked_add(data_bytes)
        .ok_or_else(|| SplatError::Process("PLY 文件长度溢出".into()))?;
    if expected != file_size {
        return Err(SplatError::Process(format!(
            "PLY 数据长度不匹配：header 预计 {expected} 字节，实际 {file_size} 字节"
        )));
    }
    Ok(GaussianPlyInfo {
        file_size,
        splat_count: vertex.count,
        header,
    })
}

fn scalar_size(raw: &str) -> Result<u64> {
    match raw.to_ascii_lowercase().as_str() {
        "char" | "int8" | "uchar" | "uint8" => Ok(1),
        "short" | "int16" | "ushort" | "uint16" => Ok(2),
        "int" | "int32" | "uint" | "uint32" | "float" | "float32" => Ok(4),
        "double" | "float64" | "int64" | "uint64" => Ok(8),
        _ => Err(SplatError::Process(format!("不支持的 PLY 属性类型：{raw}"))),
    }
}

pub fn verify_ply_magic(path: &Path) -> Result<u64> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(0))?;
    let mut buf = [0u8; 4];
    file.read_exact(&mut buf)?;
    if &buf != b"ply\n" && &buf[..3] != b"ply" {
        return Err(SplatError::Process(
            "文件不是合法 PLY：缺少 ply 魔数".into(),
        ));
    }
    Ok(file.metadata()?.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    fn write_ply(dir: &Path, gaussian: bool, bytes: usize) -> std::path::PathBuf {
        let path = dir.join("sample.ply");
        let mut file = File::create(&path).unwrap();
        writeln!(
            file,
            "ply\nformat binary_little_endian 1.0\nelement vertex 1"
        )
        .unwrap();
        for property in if gaussian {
            vec![
                "x", "y", "z", "f_dc_0", "f_dc_1", "f_dc_2", "opacity", "scale_0", "scale_1",
                "scale_2", "rot_0", "rot_1", "rot_2", "rot_3",
            ]
        } else {
            vec!["x"]
        } {
            writeln!(file, "property float {property}").unwrap();
        }
        writeln!(file, "end_header").unwrap();
        file.write_all(&vec![0u8; bytes]).unwrap();
        path
    }
    #[test]
    fn validates_standard_gaussian_ply() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(
            inspect_gaussian_ply(&write_ply(temp.path(), true, 56))
                .unwrap()
                .splat_count,
            1
        );
    }
    #[test]
    fn rejects_plain_point_cloud_and_length_mismatch() {
        let temp = tempfile::tempdir().unwrap();
        assert!(inspect_gaussian_ply(&write_ply(temp.path(), false, 4)).is_err());
        assert!(inspect_gaussian_ply(&write_ply(temp.path(), true, 1)).is_err());
    }

    #[test]
    fn accepts_degree_three_sh_gaussian_layout() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("degree-three.ply");
        let mut file = File::create(&path).unwrap();
        writeln!(
            file,
            "ply\nformat binary_little_endian 1.0\nelement vertex 1"
        )
        .unwrap();
        for property in [
            "x", "y", "z", "nx", "ny", "nz", "f_dc_0", "f_dc_1", "f_dc_2",
        ] {
            writeln!(file, "property float {property}").unwrap();
        }
        for index in 0..45 {
            writeln!(file, "property float f_rest_{index}").unwrap();
        }
        for property in [
            "opacity", "scale_0", "scale_1", "scale_2", "rot_0", "rot_1", "rot_2", "rot_3",
        ] {
            writeln!(file, "property float {property}").unwrap();
        }
        writeln!(file, "end_header").unwrap();
        file.write_all(&vec![0u8; 62 * 4]).unwrap();
        assert_eq!(inspect_gaussian_ply(&path).unwrap().splat_count, 1);
    }
}
