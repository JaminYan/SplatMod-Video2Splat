# Product

## Register

product

## Platform

web

## Users

普通创作者，使用桌面影视后期工具完成视频或 Splatcam 已重建数据到 Gaussian Splatting PLY 的转换。用户不一定熟悉 COLMAP、CUDA 或坐标系，需要以任务状态、可执行错误和结果预览为主要信息。

## Product Purpose

SplatMod-Video2Splat 是一个 Tauri 桌面工具，用于将视频或 Splatcam 导出数据转换为 Gaussian PLY。工具支持视频抽帧、筛选、COLMAP/CASPAR 重建，以及 Brush/gsplat 训练；对已有 Splatcam RGB 帧、LiDAR 深度、位姿和稠密 PLY 的输入，可直接进入后续处理，跳过重复的画面筛选和相机重建。成功标准是：创作者能启动任务、理解当前阶段、处理质量或环境问题，并获得可用的 PLY，而不必依赖命令行。

## Brand Personality

专业、电影感、易接近。整体体验应像影视后期工具：克制、准确、让用户有掌控感；避免卡通化和游戏化表达。

## Anti-references

- 卡通化或游戏化界面
- 充满 KPI 卡片的通用 SaaS 仪表盘
- 过多大圆角容器、玻璃拟态和装饰性渐变
- 把工程控制台术语作为普通创作者的主要入口
- 用加载动画或模糊的“处理中”隐藏失败原因和质量风险

## Design Principles

1. 让普通创作者始终清楚下一步应该做什么。
2. 展示真实的流水线阶段、证据和可恢复路径。
3. 保持专业后期工具需要的信息密度，但隐藏不必要的实现细节。
4. 将 CUDA、COLMAP、Brush 和 gsplat 作为具备明确就绪状态的能力，而不是不可解释的默认魔法。
5. 动画用于阶段切换、进度和状态反馈，不用于装饰；在 `prefers-reduced-motion` 下提供减弱或关闭路径。

## Accessibility & Inclusion

文字必须保持高对比度。按照当前产品方向，键盘操作支持和强化焦点样式不是本轮重点。状态颜色必须同时配合文字或图标表达；动画不能成为唯一信号，并应在用户偏好减少动态效果时自动减弱或停用。
