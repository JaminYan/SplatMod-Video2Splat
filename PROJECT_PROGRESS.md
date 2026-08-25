# OOOSplat 项目进度

> 更新时间：2026-08-24  
> 当前版本：本地升级版 0.48
> 当前分支：`codex/NewInputSfM`

本文记录当前代码、运行时、性能优化、验证和发布边界。已实施功能、局部验证结果和仍待验收的计划分开记录。

## 已完成

- 完成从视频到 Gaussian PLY 的 Brush 默认工作流，保留历史任务、项目状态、日志和内置 3D 查看入口。
- 帧筛选增加 Rayon 并行 pHash/Laplacian、近重复合并和清晰度保留，减少模糊帧与重复帧进入 COLMAP。
- 自适应 SfM 抽帧已接入运行时：低分辨率代理分析、输入帧索引/时间戳映射、固定 2/4 FPS 回退、取消传播和阶段进度已具备。
- COLMAP CPU/no-CUDA、官方 CUDA 与 CASPAR CUDA 工作流可切换；COLMAP 4.1.1 分发物、manifest、GPU 参数和引擎健康检查已接入。
- 中大型重建可尝试 CASPAR 全局 BA；局部 BA 使用 Ceres，CASPAR 启动、Mapper、模型解析或质量门禁失败时保留独立 attempt 并回退 Ceres。
- FFmpeg 支持关闭、自动、D3D11VA 和 CUDA/NVDEC 解码模式。当前 CUDA 路径是硬件解码加 CPU/系统内存后处理，不宣传为完整 GPU 图像管线。
- gsplat CUDA 作为可选实验训练后端接入：隔离 Python/CUDA 运行时、三级健康检查、统一 `training-input`、GPU 图片 LRU、splat 上限、MCMC 正则和 Gaussian PLY 校验。
- UI 增加设置抽屉、引擎状态、后端选择、训练预设、splat 上限、阶段进度、日志和历史任务查看入口。

## 验证状态

| 项目 | 状态 | 说明 |
|---|---|---|
| Rust 单元测试 | 已有通过记录 | `cargo test --manifest-path src-tauri\\Cargo.toml --lib`：22/22 |
| 引擎/许可证校验 | 已有通过记录 | 包括 manifest、引擎健康和许可证检查；仍应在目标机器复核实际 DLL/驱动 |
| 前端测试 | 待复核 | 权限/锁阻塞已修正；此前 3 通过、1 失败，失败为进度状态断言，不能写成全绿 |
| 自适应 SfM | 运行时已接入 | 真实素材的 COLMAP 注册率、点质量和补帧闭环仍待验收 |
| CASPAR CUDA | 实验路径已接入 | 需要固定相同输入、设备和配置完成中大型 A/B 性能与质量对比 |
| Brush/gsplat | 适配器已接入 | 尚未完成三组同输入的完整质量、耗时和查看器验收 |

## 性能优化重点

1. 通过候选帧上限、pHash 去重和 Laplacian 清晰度筛选，降低 COLMAP 特征、匹配和 Mapper 的输入规模。
2. 使用受控 CPU 并行和确定性合并，避免筛选结果因线程完成顺序变化。
3. 让 CUDA COLMAP 真正传递 SIFT 特征提取、匹配和 GPU 参数，避免“选择 CUDA 二进制但实际运行 CPU”的假加速。
4. 使用 COLMAP 4.1.1 作为可校验基线，并在中大型任务启用 CASPAR 全局 BA，失败时回退 Ceres。
5. 通过训练输入复用、后端隔离、显存 LRU 和 splat cap，减少重复数据准备和显存峰值。
6. 通过确定性阶段进度、心跳、日志和 attempt 隔离，降低失败重跑和诊断成本。

历史样本中，相同 273 帧素材的 CASPAR 路径曾记录约 430 秒降至 126 秒、注册率均为 100%；该结果属于已记录样本，不等同于所有项目的端到端保证。完整性能结论必须使用同一帧目录、同一设备、同一质量预设，并至少比较三次运行的中位数。

## 近期维护改进

- 已完成完整代码/文档/引擎/构建产物备份：`A:\project\backup\Splat Back`。
- 已清理项目目录中的约 93.9 GB 临时内容，包括多轮 Cargo 验证目录、vcpkg/CASPAR 编译缓存、Rust Debug 中间产物、Python `__pycache__`、Node/Vite 缓存和 TypeScript 增量缓存。
- 保留 `.git`、`.backup`、源码、实现文档、`engines`、`node_modules`、`dist` 和 `src-tauri\\target\\release` 发行构建产物。
- 后续测试建议使用备用 `CARGO_TARGET_DIR`，避免把大型 Rust 中间产物重新写回项目目录。

## 待完成与发布边界

- 重新执行清理后的完整 `npm test` 和 Rust 全量测试，并修复前端过期进度事件断言。
- 使用真实 42/64/273/631 帧级样本完成 CPU、官方 CUDA、CASPAR、Brush 和 gsplat 的可复现实验矩阵。
- 验收自适应 SfM 的注册率、稀疏点质量、定向补帧和 `needsSupplement` 恢复流程。
- 在目标机器现场验证 COLMAP 4.1.1 的 DLL、驱动、CUDA SIFT 和 CASPAR 依赖；manifest 哈希不能替代现场运行。
- 在三组真实项目上完成 Brush/gsplat 的质量、耗时、显存和 3D Viewer 兼容性比较。
- 在上述证据完成前，不把 CASPAR 或 gsplat 宣传为所有场景默认方案，也不把局部加速数字直接写成端到端固定提升。
