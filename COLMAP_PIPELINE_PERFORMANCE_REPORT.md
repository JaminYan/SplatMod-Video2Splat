# OOOSplat 抽帧、筛选与 COLMAP 加速分析报告

> 文档状态：主线已部分实施；自适应 SfM 与 CASPAR 仍需真实素材性能/质量验收
> 基线日期：2026-08-22  
> 适用项目：`A:\project\splat`  
> 主要读者：OOOSplat 后端、打包与性能验证人员

## 技术摘要

当前 PLY 生成链路的主线改造已经落地一部分：候选帧筛选已采用受控 CPU 并行，COLMAP CPU/CUDA 参数已分流，COLMAP 4.1.1、CASPAR CUDA 和 Brush/gsplat 工作流已接入。仍需通过同一输入、同一设备和同一质量预设完成完整 A/B 基准，才能把局部加速结果写成端到端性能结论。现阶段仍不把 GLOMAP、LightGlue 或自研 CUDA pHash/Laplacian 与主线混合宣传。

本地历史任务说明不同规模的瓶颈明显不同：45–84 张候选帧时，筛选约耗 15–58 秒；631 张任务中，COLMAP 匹配约 14 分钟、增量映射约 29.6 分钟。Caspar 对后者有明显价值，但对 Mapper 仅耗 3–14 秒的小任务无法产生同等级端到端收益。64 张高质量任务的 Brush 训练约 345 秒，仍占总耗时的大头，因此本报告中的 COLMAP 或 CASPAR 局部加速数字不能直接解释为最终 PLY 等比例提速。

本期建议明确排除两项工作：独立 GLOMAP 实验路线及其自动质量回退、预置 ONNX 模型的 LightGlue 恢复模式。两项都保留为备选，但不进入当前实施范围。

## 已测数据表明：小任务先优化筛选，大任务再优化 Mapper

下表来自现有项目元数据、日志计时器和文件时间戳。三个任务的输入规模、质量档位和代码版本并不完全相同，因此这些数字用于定位阶段瓶颈，不构成严格的前后版本性能对比。

| 样本 | 候选/保留帧 | 总耗时 | 筛选估算 | COLMAP 特征 | COLMAP 匹配 | COLMAP 映射 | Brush 训练 |
|---|---:|---:|---:|---:|---:|---:|---:|
| IMG_5450，balanced | 45 / 42 | 122.0 秒 | 约 15 秒 | 5.2 秒 | 4.7 秒 | 3.2 秒 | 约 65 秒 |
| copy，high | 84 / 64 | 453.5 秒 | 约 58 秒 | 4.3 秒 | 22.5 秒 | 14.0 秒 | 约 345 秒 |
| IMG_5451，旧大任务 | 未记录 / 631 | 2812.9 秒 | 不可比 | 38.2 秒 | 839.8 秒 | 1773.5 秒 | 约 147 秒 |

关键解释如下：

- 42–64 张的 Mapper 只有数秒到十余秒，使用 Caspar 即使显著加快 BA，也无法节省数十分钟。
- 84 张候选帧的筛选约 58 秒，已经超过该任务 COLMAP 的任一单独阶段，是当前小任务最明确的上游瓶颈。
- 631 张任务的匹配和映射合计约 43.6 分钟，适合使用 COLMAP 4.1.1 修复、CUDA SIFT/CUDA 匹配和 Caspar。
- 高质量 64 张任务中，Brush 约占总时间的四分之三；只优化 COLMAP 不会让最终 PLY 时间按 COLMAP 加速倍数下降。

没有制作趋势图或速度倍数图，因为仅有三个异构历史样本，且其输入规模、质量档位和二进制版本不一致。用图形连接或比较这些点会暗示并不存在的受控趋势；精确阶段表更适合本次审计目的。

## 当前实现确认了两个直接瓶颈

### 抽帧已经使用 NVDEC，但不是完整 GPU 图像管线

当前 `src-tauri/src/engines/ffmpeg.rs` 在 CUDA 模式下传入 `-hwaccel cuda`，因此视频解码可以使用 NVDEC。随后仍使用普通 `fps`、`scale` 滤镜并输出 JPEG。当前实现没有通过 `-hwaccel_output_format cuda` 和 `scale_cuda` 保持图像驻留显存，JPEG 编码也不是 NVENC 的常规输出类型。

因此，现有抽帧应描述为“CUDA 硬件解码 + CPU/系统内存后处理”，而不是完整 CUDA 化。NVIDIA 官方 FFmpeg 指南说明，完整 GPU 缩放需要保持 CUDA 帧并使用 `scale_cuda`；但最终 JPEG 输出仍会引入显存下载或额外的自定义 JPEG 路径。

这意味着：可以原型验证 CUDA 缩放，但不应把它放在当前最高优先级。现有样本的抽帧时间约 1–8 秒，明显小于筛选、匹配、映射或训练时间。

### pHash/Laplacian 的主要问题是串行和重复计算

`src-tauri/src/video/select.rs` 当前逐张执行以下操作：

1. CPU 解码完整 JPEG。
2. 从原图缩放到 32×32，并计算低频 DCT pHash。
3. 再从原图缩放到不超过 256×256，并计算 Laplacian 方差。
4. 根据相邻 pHash 距离保留更清晰的一张。
5. 立即同步删除被淘汰文件。

DCT 余弦表也在每张图片上重新构造。候选帧的图像解码和指标计算相互独立，却没有并行执行。这类工作先采用受控 CPU 并行、静态余弦表和可分离 DCT，风险和维护成本显著低于引入 CUDA/nvJPEG 后端。

### CUDA 版 COLMAP 被固定参数降级为 CPU

`src-tauri/src/engines/colmap.rs` 固定传入：

```text
--FeatureExtraction.use_gpu 0
--FeatureMatching.use_gpu 0
```

`pipeline/runner.rs` 会根据设置选择 CPU 或 CUDA executable，但传给两个 executable 的计算参数相同。界面和进度事件会显示“CUDA”，实际日志却明确出现 CPU SIFT 警告。因此当前状态是“选择了 CUDA 二进制”，不是“特征与匹配实际运行在 CUDA”。

这属于功能正确性问题，同时也是性能问题，修复优先级高于引入新算法。

## COLMAP 4.1.1、Caspar、GLOMAP 与 LightGlue 不能简单相乘

| 技术 | 替换环节 | 适用场景 | 本地现状 | 本期处理 |
|---|---|---|---|---|
| CUDA SIFT | 特征提取与传统匹配计算 | 所有具备受支持 NVIDIA GPU 的任务 | 二进制存在，但代码强制 CPU | 实施 |
| COLMAP 4.1.1 | 整体版本和匹配修复 | 所有任务 | CUDA 二进制实际为 4.1.1；manifest 仍是旧描述 | 实施 |
| Caspar | 替代增量 Mapper 的 Ceres BA | 中大型增量重建 | 本地 Mapper 暴露 Caspar 后端参数 | 实施 |
| GLOMAP/global_mapper | 替代整个增量 Mapper | 匹配图良好、内参可靠的大任务 | 命令存在；本地 4.1.1 未暴露全局 Caspar 后端 | 备选 |
| LightGlue | 替代 SIFT/ALIKED brute-force 匹配器 | 困难视角、光照和弱纹理恢复 | ONNX Runtime 存在，模型未预置 | 备选 |

### COLMAP 4.1.1 是应先固定的稳定基线

COLMAP 4.1.1 官方变更记录说明，该版本修复了 RANSAC/LORANSAC 中进程级 OpenMP 临界区导致的约 4–6 倍匹配变慢问题。历史日志来自旧流程，不能保证完全命中该问题，但这足以支持先固定 4.1.1，再重新测量，而不是基于旧日志直接选择新匹配器。

当前 `engines/manifest.json` 仍将 CUDA 包描述为旧 release/version，且直接文件哈希清单没有完整反映当前 CUDA 目录。发布前必须以实际分发包重新固定版本、归档哈希、直接文件哈希、许可证和运行时依赖，不能只修改显示字符串。

### Caspar 只会放大 BA 占比高的任务收益

COLMAP 4.1 引入 Caspar 作为 GPU Bundle Adjustment 后端。官方称它在中大型问题中通常比 Ceres CUDA 快 1–2 个数量级，尤其能加速增量 Mapper。该数字针对 BA，不是整个 `mapper`，更不是从视频到 PLY 的端到端时间。

本地 4.1.1 增量 Mapper 支持 `Mapper.ba_local_backend` 和 `Mapper.ba_global_backend`。本期可以让中大型任务尝试 Caspar，并在运行失败、模型为空或质量门禁失败时回退到增量 Ceres。小任务继续使用 Ceres，避免实验性后端和初始化开销抵消收益。

### GLOMAP 是另一条 Mapper 路线，本期不做

GLOMAP 已合并到 COLMAP，可通过 `global_mapper` 使用。它一次性求解全局相机姿态，适合大规模、匹配图良好的数据；官方同时指出，它对错误匹配和焦距先验更敏感。当前视频流程使用 `SIMPLE_RADIAL + single_camera=1`，但没有可靠焦距参数，并且顺序匹配默认不保证环拍首尾闭环。

此外，本地这份 4.1.1 的 `global_mapper -h` 没有 `GlobalMapper.ba_backend`，只有 Ceres GPU BA 相关参数，因此不能在当前包中把 GLOMAP 和 Caspar 当作可直接叠加的稳定组合。独立 GLOMAP 路线、数据库副本、`view_graph_calibrator` 和自动质量回退均列入备选，不进入本期代码改造。

### LightGlue 是质量恢复选项，本期不做

COLMAP 支持 `SIFT_LIGHTGLUE` 和 `ALIKED_LIGHTGLUE`。官方定位主要是困难视角、光照变化下获得更多高质量匹配，并不保证连续视频的小范围顺序匹配会比 CUDA SIFT brute-force 更快。

当前 CUDA 包没有预置 LightGlue/ALIKED ONNX 模型，默认模型参数指向 GitHub 下载地址。桌面离线产品不能依赖任务运行期间临时联网，因此还需要版本固定、哈希、许可、安装体积、离线健康检查和旧数据库清理策略。该恢复模式列入备选，最后评估。

## 本期决策：只实施低风险主线

本期实施范围按以下顺序推进：

1. 建立受控基线和细分计时。
2. 并行化现有 pHash/Laplacian 指标计算，保持筛选结果确定性。
3. 让 CPU/CUDA COLMAP 配置真正控制 `use_gpu` 参数，并提供能力错误回退。
4. 固定和校验 COLMAP 4.1.1 分发物、manifest、哈希和许可证。
5. 为中大型任务增加增量 Mapper + Caspar，并保留增量 Ceres 回退。
6. 使用相同输入帧、设备和质量预设完成分阶段基准与质量验收。

独立 GLOMAP 自动质量回退路线、预置 ONNX/LightGlue 恢复模式和完整 CUDA/nvJPEG 去重均不属于本期范围。

## 预期收益是目标区间，不是已验证结果

在不改变保留帧集合和训练预设的前提下，可以把以下数字作为工程目标：

| 场景 | 当前证据 | 本期目标 | 主要贡献 |
|---|---:|---:|---|
| 84 张候选帧筛选 | 约 58 秒 | 5–15 秒 | CPU 并行、余弦表缓存、DCT 优化 |
| 42–64 张端到端任务 | 122–453 秒 | 改善约 10%–20% | 筛选和真实 CUDA SIFT；训练仍占主导 |
| 631 张 COLMAP | 约 44.2 分钟 | 第一阶段目标 6–18 分钟 | 4.1.1、CUDA 提取/匹配、Caspar |

这些区间是容量规划目标，不是性能承诺。631 张任务来自旧流程；4.1.1 修复、CUDA SIFT 和 Caspar 的收益会相互重叠，不能将各自的官方倍数相乘。

## 证据边界与稳健性要求

- 历史项目元数据没有当前新增的完整阶段计时字段；筛选耗时由 FFmpeg、目录和 COLMAP 日志时间戳估算。
- 三个样本不是同一输入、同一档位、同一二进制的 A/B 测试。
- 官方的 Caspar 和 GLOMAP 数字来自中大型公开数据集，不代表本地视频任务必然取得相同比例。
- PLY 总耗时还取决于 Brush 或后续 gsplat 训练后端，COLMAP 加速不能孤立宣传为端到端加速。
- 性能验收必须至少预热一次，正式运行三次并报告中位数；同时记录峰值 RAM、显存和是否发生回退。
- 质量验收至少比较注册图片数/比例、模型数量、三维点数、平均重投影误差、输出目录结构和最终训练是否成功。

## 推荐的下一步

按配套文档 `COLMAP_ACCELERATION_IMPLEMENTATION_PLAN.md` 执行本期五个里程碑。每个里程碑独立验证并保留开关，不把所有变化合并后一次性测量。只有在 42、64 和 631 张代表样本上同时通过确定性、质量和性能门禁，才能将 CUDA 或 Caspar 设为默认。

后续需要回答但不阻塞本期的问题：

- 不同显卡和驱动组合下，官方 CUDA 4.1.1 包是否均包含可运行的 Caspar 依赖？
- Caspar 的启用阈值应固定按保留帧数，还是根据首次全局 BA 时间动态决定？
- 完成 gsplat 后端后，新的端到端瓶颈会回到 COLMAP，还是仍在图像筛选和数据准备？

## 外部资料

- [COLMAP Changelog](https://colmap.github.io/changelog.html)
- [COLMAP Features and Matchers](https://colmap.github.io/features.html)
- [COLMAP FAQ: mapper selection and bundle adjustment](https://colmap.github.io/faq.html)
- [GLOMAP repository](https://github.com/colmap/glomap)
- [GLOMAP paper](https://arxiv.org/abs/2407.20219)
- [LightGlue paper](https://openaccess.thecvf.com/content/ICCV2023/html/Lindenberger_LightGlue_Local_Feature_Matching_at_Light_Speed_ICCV_2023_paper.html)
- [NVIDIA FFmpeg GPU acceleration guide](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.1/ffmpeg-with-nvidia-gpu/index.html)
