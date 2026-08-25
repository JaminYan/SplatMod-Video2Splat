# Splatcam 已重建数据导入实施文档

> 文档状态：设计完成，待代码实施  
> 适用版本：SplatMod-Video2Splat 0.47.x  
> 当前范围：RGB JPEG + COLMAP 文本相机/位姿 + RGB 点云 PLY  
> 不在本期范围：LiDAR 深度、`transforms.json`、深度监督训练

## 1. 背景与目标

Splatcam 可以直接导出已经完成相机估计和点云生成的数据。当前项目的视频工作流会依次执行 FFprobe、FFmpeg 抽帧、pHash/Laplacian 筛选、COLMAP 特征提取、匹配和 Mapper。对已经完成 SfM 的 Splatcam 数据再次执行这些步骤会浪费时间，并可能改变已经验证过的帧、相机内参和位姿。

本期新增一条独立的 `SplatcamImport` 输入工作流：导入已有 RGB、COLMAP 文本模型和 RGB 点云，完成标准化后直接复用现有 Brush/gsplat 训练链路。

目标是：

1. 不重新读取视频，不执行 FFmpeg 抽帧和画面筛选。
2. 不重新执行 COLMAP SIFT、匹配、Mapper 或 CASPAR。
3. 将 Splatcam 数据转换为现有 `training-input` 契约。
4. 在训练前验证文件映射、相机参数、位姿、点云和坐标系。
5. 保留原始导出和导入诊断，失败时不覆盖原始数据。

## 2. 当前已验证的输入格式

实际样本目录：

```text
A:\tmp\export_test_2026-08-25_115104
```

目录结构：

```text
export_test_2026-08-25_115104/
  images/
    0000.jpg ... 0068.jpg
  sparse/0/
    cameras.txt
    images.txt
    points3D.ply
```

当前样本证据：

- 68 张 JPEG；
- `cameras.txt` 包含 68 个 `PINHOLE` 相机，图像尺寸为 2160×3840；
- `images.txt` 包含 68 组位姿，文件名与 68 张 JPEG 一一对应；
- `points3D.ply` 为 binary little-endian PLY，包含 189,385 个 `x/y/z + RGB` 点；
- 当前样本没有 `transforms.json`；
- 当前样本没有可检测的深度目录或深度文件；
- 当前 PLY 是普通 RGB 点云，不是包含 opacity、scale、rotation 和 spherical harmonics 的 Gaussian PLY。

## 3. 工作流边界

### 3.1 Splatcam 导入路径

```text
选择 Splatcam 导出目录
  → 读取导出清单
  → 校验 RGB / 相机 / 位姿 / PLY
  → 文本 COLMAP 模型标准化
  → PLY 转 COLMAP points3D.bin
  → 生成标准 training-input
  → Brush 或 gsplat 训练
  → Gaussian PLY 校验与发布
```

### 3.2 明确跳过的阶段

以下阶段在 `SplatcamImport` 中不得执行：

- FFprobe 视频探测；
- FFmpeg 视频解码和抽帧；
- 固定 FPS、pHash、Laplacian 和自适应 SfM 抽帧；
- COLMAP `feature_extractor`；
- COLMAP `sequential_matcher` 或其他 matcher；
- COLMAP `mapper`；
- CASPAR/Ceres 相机重建。

以下阶段仍然保留：

- 导入资产质量检查；
- COLMAP 模型格式转换；
- training-input 原子生成；
- Brush/gsplat 运行时健康检查；
- 训练、PLY 发布和最终 Gaussian 属性校验。

## 4. 数据契约

### 4.1 RGB 图像

输入：`images/*.jpg` 或 `images/*.jpeg`。

规则：

- 每张 RGB 文件必须能解码；
- 文件名必须与 `images.txt` 中的 `NAME` 完全匹配；
- 不允许重复文件名；
- 实际宽高必须与对应相机记录一致；
- 导入阶段优先使用硬链接，失败后使用复制；
- 原始文件只读保留，不在原目录内重命名或压缩。

### 4.2 相机

输入：`sparse/0/cameras.txt`。

本期首先支持：

```text
PINHOLE WIDTH HEIGHT FX FY CX CY
```

要求：

- `WIDTH`、`HEIGHT` 大于 0；
- `FX`、`FY` 大于 0 且为有限值；
- `CX`、`CY` 位于合理图像范围附近；
- 相机 ID 唯一；
- 相机模型必须是当前转换器明确支持的模型。

未知模型、畸变模型或参数数量不匹配时，导入失败并给出明确原因，不静默近似为 `PINHOLE`。

### 4.3 位姿

输入：`sparse/0/images.txt`。

`images.txt` 使用标准 COLMAP 约定：四元数和 `TX/TY/TZ` 表示 world-to-camera 变换，文件名位于每条图像记录的最后一列。导入器必须保留该约定，不直接当作 NeRF/OpenGL 的 camera-to-world 矩阵使用。

要求：

- 每条图像记录的四元数和位移均为有限值；
- 四元数长度不能接近零；
- 图像 ID、相机 ID 和文件名唯一；
- 位姿数量与 RGB 数量一致；
- 所有位姿对应的 RGB 文件存在；
- 点 2D 轨迹可以为空，但必须在报告中标记为“无观测轨迹”，不能伪装成完整 COLMAP 匹配结果。

### 4.4 RGB 点云 PLY

输入：`sparse/0/points3D.ply`。

本期支持 binary little-endian PLY，顶点至少包含：

```text
x y z red green blue
```

导入器必须检查：

- PLY 魔数和 `end_header`；
- binary little-endian 格式；
- vertex 数量大于 0；
- XYZ、颜色和文件长度有效；
- XYZ 不包含 NaN 或无穷值；
- 点数量达到最低训练初始化门槛。

该 PLY 只作为 COLMAP 初始点云输入，不作为最终 Gaussian PLY 发布。最终发布仍必须经过现有 `inspect_gaussian_ply` 的 Gaussian 属性检查。

## 5. 标准化与模型转换

现有 Brush/gsplat 训练输入要求：

```text
training-input/
  images/
  sparse/0/
    cameras.bin
    images.bin
    points3D.bin
```

导入器建议采用以下转换链路：

1. 将 `cameras.txt` 和 `images.txt` 复制到隔离的 `normalized-model/`。
2. 将 `points3D.ply` 逐点转换成 COLMAP `points3D.txt`。
3. 为每个点分配稳定的 `POINT3D_ID`，保留 XYZ、RGB，误差初始为 0；没有 2D track 时不生成虚假的观测关联。
4. 调用固定版本的 COLMAP `model_converter` 将文本模型转换为 binary 模型。
5. 验证 `cameras.bin`、`images.bin`、`points3D.bin` 的记录数和文件大小。
6. 原子创建 `work/training-input`，失败时删除临时目录，不覆盖已有成功输入。

如果当前 COLMAP `model_converter` 拒绝没有完整 track 的 `points3D.txt`，则增加项目内受控的 binary writer，写入零长度 track 的合法 COLMAP point 记录；该 writer 必须有固定 fixture 和 round-trip 测试，不能通过伪造 2D 观测绕过验证。

## 6. 坐标系与几何质量门禁

Splatcam 当前导出没有 `transforms.json`，因此不能直接套用 NeRF 的 `camera-to-world` 转换。第一版按 COLMAP world-to-camera 解释 `images.txt`。

导入后执行：

1. 从四元数/平移构造相机投影矩阵；
2. 从 PLY 采样点投影到对应图像平面；
3. 统计有限投影、图像内投影和深度为正的比例；
4. 检查相机中心轨迹的非零范围和异常跳变；
5. 输出 `splatcam-import-report.json`。

建议初始门槛：

| 指标 | 阻断条件 | 说明 |
|---|---:|---|
| RGB/位姿对应率 | `< 100%` | 防止错帧训练 |
| 相机参数有限率 | `< 100%` | 直接阻断 |
| 有效点坐标率 | `< 100%` | 直接阻断 |
| 正深度投影比例 | `< 70%` | 需要检查坐标系或 PLY 来源 |
| 图像内投影比例 | `< 50%` | 需要检查位姿、旋转方向或尺寸 |
| 相机轨迹有效范围 | 近似为 0 | 可能是重复位姿或单位错误 |

这些是导入门槛，不替代训练质量验收。首个真实样本应把投影统计和可视化结果保存下来，再校准阈值。

## 7. 项目目录与状态

建议在项目目录中保留来源和诊断：

```text
<project>/
  source/splatcam/
    images/
    sparse/0/
    points3D.ply
  frames/                         # training-input 使用的 RGB 链接/副本
  work/splatcam-import/
    normalized-model/
    points3D.txt                  # 中间转换文件，可清理但默认保留
    import-report.json
  work/training-input/
    images/
    sparse/0/
  logs/splatcam-import.log
  project.json
  state.json
  <video-name>.ply
```

新增状态建议：

```text
inputSource = "splatcam"
splatcamImportComplete = true
splatcamSourcePath
splatcamImageCount
splatcamPoseCount
splatcamPointCount
splatcamCoordinateConvention = "colmap-world-to-camera"
splatcamHasDepth = false
splatcamHasTransforms = false
splatcamGeometryGate
```

导入任务完成后，后端不能再显示“正在抽帧”或“正在 COLMAP 重建”，应显示“已导入相机与点云，进入训练”。

## 8. UI 设计

在输入区域增加数据源选择：

```text
数据源
  视频重建
  Splatcam 已重建数据
```

选择 Splatcam 后：

- 显示目录选择器；
- 显示 RGB、位姿和点云数量；
- 显示坐标约定和质量检查结果；
- 禁用 FPS、pHash、Laplacian、自适应抽帧和 COLMAP 后端选择；
- 保留 Brush/gsplat 训练后端与训练预设；
- 提供“仅导入检查”和“导入并训练”两种操作；
- 把原始点云标记为“训练初始化点云”，避免用户误认为是最终 Gaussian PLY。

## 9. 错误与回退

- 缺少 `images/`、`cameras.txt`、`images.txt` 或 `points3D.ply`：导入失败，不启动训练；
- RGB/位姿不匹配：导入失败，报告缺失文件名；
- PLY 不是支持格式：导入失败，保留原始文件和错误日志；
- 坐标系门禁失败：不进入训练，提示检查 Splatcam 导出版本或提供 `transforms.json`；
- model conversion 失败：保留文本模型和中间报告，允许修复后重试；
- Brush/gsplat 健康检查失败：只切换训练后端，不重新执行视频或 COLMAP；
- 用户取消：删除临时 normalized/training-input，保留 source 和日志。

不允许把导入失败静默回退到原视频工作流，因为当前输入可能没有原始视频，且这会掩盖位姿/坐标系错误。

## 10. 实施阶段

### M0：fixture 与格式解析

- 固定当前 68 帧导出作为只读 fixture；
- 实现 RGB、相机、位姿、PLY 头和点数量解析；
- 完成一一对应和有限值测试；
- 生成 `import-report.json`。

### M1：COLMAP 模型标准化

- 实现 PLY → `points3D.txt`；
- 调用或实现 text → binary 模型转换；
- 完成 binary round-trip 和 Brush 模型完整性检查；
- 生成标准 `training-input`。

### M2：训练工作流接入

- 增加 `InputSource::Splatcam`；
- 在 runner 中绕过视频、筛选和 COLMAP；
- 复用 Brush/gsplat 训练和最终 PLY 发布；
- 写入项目状态和阶段进度。

### M3：UI 与质量门禁

- 增加 Splatcam 目录导入界面；
- 展示导入统计、坐标约定和门禁结果；
- 增加仅检查/导入并训练按钮；
- 完成失败恢复和历史任务展示。

## 11. 测试与验收

### 单元测试

- 解析标准 `cameras.txt`；
- 解析 `images.txt` 的 quaternion/translation/name；
- 检测重复、缺失和错配图像；
- 解析 binary little-endian RGB PLY；
- PLY 坐标有限值和文件长度检查；
- PLY → COLMAP point 记录转换；
- 文本模型转换后 binary 记录数一致。

### 集成测试

- 68 张样本跳过 FFmpeg 和 COLMAP；
- 生成 68 张 `training-input/images`；
- 生成完整 `cameras.bin/images.bin/points3D.bin`；
- Brush 训练可以读取导入模型；
- gsplat 去畸变路径可以读取导入模型；
- 取消、重试和失败不会污染原始导出。

### 验收标准

1. 导入阶段不启动 FFprobe、FFmpeg、feature extractor、matcher 或 mapper。
2. RGB、位姿和相机记录 100% 对应。
3. 生成的 training-input 通过现有 Brush/gsplat 输入校验。
4. 至少完成一次 Brush 训练并输出合法 Gaussian PLY。
5. 训练前后的普通 RGB 点云与最终 Gaussian PLY 清晰区分。
6. 所有门禁、耗时和回退原因写入项目状态与日志。

## 12. 后续扩展

当拿到真正包含 LiDAR depth 和 `transforms.json` 的导出后，再增加独立适配器：

- `transforms.json` 优先作为显式相机位姿来源；
- 解析 `camera_angle_x`、`fl_x/fl_y/cx/cy` 和每帧 `transform_matrix`；
- 明确 OpenGL/NeRF 与 COLMAP world-to-camera 的旋转、轴向和转置转换；
- 使用深度单位和深度有效率校验 RGB/深度对齐；
- 用深度反投影验证 PLY 或生成初始化点云；
- 不把深度数据强行塞进标准 Gaussian PLY，深度应保存在导入元数据和诊断文件中。

本期不因未来深度格式不确定而阻塞当前 RGB + COLMAP + PLY 导入路径。
