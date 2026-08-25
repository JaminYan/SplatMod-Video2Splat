---
name: SplatMod-Video2Splat
description: 面向普通创作者的专业视频到 Gaussian Splatting 后期工作台
colors:
  primary-blue: "#1e5cff"
  primary-blue-dark: "#1748ca"
  ink: "#172033"
  muted: "#667084"
  faint: "#8a94a5"
  paper: "#f8f9fb"
  surface: "#eef1f5"
  line: "#d4dae3"
  line-dark: "#bdc5d1"
  success-green: "#2d9965"
  warning-amber: "#c78314"
  error-red: "#b04a40"
typography:
  display:
    fontFamily: "Bahnschrift, Microsoft YaHei UI, sans-serif"
    fontSize: "29px"
    fontWeight: 650
    lineHeight: 1
    letterSpacing: "-0.025em"
  body:
    fontFamily: "Segoe UI Variable, Microsoft YaHei UI, sans-serif"
    fontSize: "14px"
    fontWeight: 400
    lineHeight: 1.5
  label:
    fontFamily: "Segoe UI Variable, Microsoft YaHei UI, sans-serif"
    fontSize: "12px"
    fontWeight: 650
    lineHeight: 1.35
  mono:
    fontFamily: "Cascadia Mono, Microsoft YaHei UI, monospace"
    fontSize: "11px"
    fontWeight: 500
    lineHeight: 1.6
rounded:
  none: "0px"
  dot: "50%"
  radio: "50%"
spacing:
  xs: "5px"
  sm: "8px"
  md: "12px"
  lg: "20px"
  xl: "28px"
components:
  button-primary:
    backgroundColor: "{colors.primary-blue}"
    textColor: "#ffffff"
    rounded: "{rounded.none}"
    padding: "0 15px"
    height: "48px"
  button-primary-hover:
    backgroundColor: "{colors.primary-blue-dark}"
    textColor: "#ffffff"
    rounded: "{rounded.none}"
  button-ghost:
    backgroundColor: "transparent"
    textColor: "{colors.muted}"
    rounded: "{rounded.none}"
    padding: "4px 0"
  path-picker:
    backgroundColor: "transparent"
    textColor: "{colors.ink}"
    rounded: "{rounded.none}"
    padding: "12px 14px"
  selected-toggle:
    backgroundColor: "#edf2ff"
    textColor: "{colors.primary-blue}"
    rounded: "{rounded.none}"

---

# Design System: SplatMod-Video2Splat

## 1. Overview

**Creative North Star: "The Edit Suite Instrument"**

这个界面应像一间可靠的影视后期剪辑工作台：工具安静地退到背景，当前素材、流水线阶段和可执行动作始终清楚。视觉采用冷静的灰阶层次与克制的蓝色指示，把复杂的 CUDA、COLMAP、CASPAR、Brush 和 gsplat 能力翻译成普通创作者能够理解的任务状态。

信息密度可以专业，但不能像工程日志一样压迫用户。画面筛选、相机重建、训练和导出应形成连续的工作流；已有 Splatcam 数据进入时，应让“跳过重复重建”成为明确的效率收益，而不是隐藏在设置里的分支。系统明确拒绝卡通化、游戏化、装饰性渐变、玻璃拟态和堆叠式 SaaS 仪表盘。

**Key Characteristics:**

- 专业影视后期工作台，而非娱乐化创作应用
- 冷灰表面、蓝色主动作、文本化语义状态
- 平直边界与细分隔线，强调结构而非装饰
- 进度动画只表达运行状态、阶段切换和反馈
- 高对比文字优先，工程细节按需展开

## 2. Colors

这是一个“冷静中性底色 + 单一蓝色行动色”的 restrained 产品配色。蓝色只用于主要动作、当前选择和运行指示；成功、警告、失败使用固定语义色，不能互相替代。

### Primary

- **工作台蓝** (`#1e5cff`): 主按钮、选中状态、当前流水线节点和重要链接。
- **深工作台蓝** (`#1748ca`): 主按钮 hover 和需要更强对比的交互反馈。

### Neutral

- **墨色** (`#172033`): 标题、正文和关键数据，保证高对比阅读。
- **雾灰文字** (`#667084`): 次要说明、路径和辅助信息。
- **淡灰文字** (`#8a94a5`): 元数据和非焦点标签；不能承担关键说明。
- **纸面** (`#f8f9fb`): 顶栏和控制面板等较亮工作表面。
- **工作区灰** (`#eef1f5`): 项目列表区和应用背景。
- **细线** (`#d4dae3`): 组内分隔线和控件边界。
- **深细线** (`#bdc5d1`): 面板边界和需要更强结构感的分隔线。
- **日志深底** (`#202735`): 仅用于实时日志，搭配浅色等宽文本。

### Named Rules

**The One Action Color Rule.** 每个界面只让工作台蓝承担主要动作与当前状态，不用蓝色装饰无关内容。

**The Semantic Pairing Rule.** 绿色、琥珀色和红色必须同时配合文字或图标表达，颜色不能是唯一状态信号。

## 3. Typography

**Display Font:** Bahnschrift (with Microsoft YaHei UI fallback)
**Body Font:** Segoe UI Variable (with Microsoft YaHei UI fallback)
**Label/Mono Font:** Cascadia Mono (with Microsoft YaHei UI fallback)

**Character:** 标题使用略带技术感但不夸张的 Bahnschrift；正文保持系统无衬线的熟悉度；路径、日志和数值使用 Cascadia Mono，形成影视工具的工程可信度。不要在按钮、标签或数据中使用展示性字体。

### Hierarchy

- **Display** (650, 29px, 1): 页面标题和主要面板标题。
- **Title** (650, 15–20px, 1.2): 项目名、重要实体和摘要数值。
- **Body** (400, 14px, 1.5): 普通说明和工作流提示；长段说明控制在约 65–75ch。
- **Label** (650, 11–13px, 1.35–1.5): 字段名、阶段名称和按钮文字。
- **Mono** (500–650, 10–13px, 1.25–1.6): 路径、时间、日志、版本和处理指标。

### Named Rules

**The Readable State Rule.** 关键阶段、失败原因和下一步动作永远使用高对比正文，不用淡灰文字或动画替代说明。

## 4. Elevation

系统以平直的色调分层和边界线表达深度，不把阴影当作默认卡片装饰。顶栏、控制区、项目区和日志区通过背景色与细线区分；浮动缩放控件是少数可使用轻量阴影的固定工具。不要同时堆叠“细边框 + 大范围柔和阴影”的幽灵卡片效果。

### Shadow Vocabulary

- **工具浮层** (`0 7px 20px rgba(23, 72, 202, .2)`): 仅用于固定缩放工具，表示它浮在工作区之上。
- **中性浮层** (`0 7px 20px rgba(23, 32, 51, .12)`): 仅用于缩放控制条等辅助浮层。

### Named Rules

**The Flat-By-Default Rule.** 普通面板默认无阴影；结构由背景、边界和间距承担，阴影只表示确实浮起的工具。

## 5. Components

### Buttons

- **Shape:** 平直矩形，无圆角 (`0px`)；状态点和单选标记才使用圆形。
- **Primary:** 工作台蓝背景、白色文字、48px 高度、水平内边距 15px；用于启动处理和确认主要动作。
- **Hover / Focus:** hover 切换为深工作台蓝；现有 focus-visible 保留 2px 蓝色外框，但不把焦点样式作为本产品的主要视觉语言。
- **Secondary / Ghost:** 透明背景、雾灰文字、无边框；用于刷新、资源管理器、详情等次要动作。
- **Loading / Disabled:** loading 使用轻量旋转图标并保留动作文字；disabled 降低透明度，禁止仅靠颜色判断。

### Chips

- **Style:** 本产品不使用装饰性 pill 标签；运行状态使用文字、短横线或圆点与边界线表达。
- **State:** 选中状态使用浅蓝底和底部 2px 蓝线，不使用高饱和整块填充。

### Cards / Containers

- **Corner Style:** 工作流面板和项目行保持 `0px`；避免 24px 以上大圆角。
- **Background:** 控制面板使用纸面，项目区使用工作区灰，实时日志使用日志深底。
- **Shadow Strategy:** 遵循平直默认规则；仅固定缩放工具使用轻量阴影。
- **Border:** 使用 1px 细线或深细线作为结构分隔，禁止彩色侧边条。
- **Internal Padding:** 常规区块 20px，面板边距约 28–58px，紧凑控件 8–15px。

### Inputs / Fields

- **Style:** 透明或纸面背景、1px 深细线、无圆角；路径选择器最小高度 60–66px，图标、路径和打开提示三列对齐。
- **Focus:** 交互边界转为工作台蓝；用户偏好的焦点强化不是主要设计目标，但不能隐藏可见状态。
- **Error / Disabled:** 错误使用红色文本和上下细线；disabled 降低透明度并保留可读标签。

### Navigation

- **Style:** 顶部 66px 工具栏，品牌锁定在左侧，引擎状态在右侧；不使用复杂侧边导航。
- **State:** 当前运行状态用圆点 + 文本；版本使用等宽小标签，不能喧宾夺主。

### Pipeline Timeline

阶段时间线是本产品的签名组件：已完成阶段用绿色节点，当前阶段用蓝色节点和轻微脉冲，未开始阶段用灰色节点；每个节点必须有文字标签，动画不能独立承担进度含义。

## 6. Do's and Don'ts

### Do:

- **Do** 使用 `#172033` 作为关键文字，确保普通创作者在冷灰背景上能清楚阅读。
- **Do** 让工作台蓝只负责主要动作、选择和运行状态。
- **Do** 用阶段名称、当前消息和可执行错误解释流水线，而不是只显示“处理中”。
- **Do** 为 Splatcam 直入、视频重建、CUDA 和 CPU 切换保留清晰的状态反馈。
- **Do** 使用 150–250ms 的状态过渡；动画服务于阶段切换、加载和反馈，并遵守 `prefers-reduced-motion`。
- **Do** 保持平直矩形、1px 结构分隔线和一致的按钮词汇。

### Don't:

- **Don't** 使用卡通化或游戏化界面。
- **Don't** 做成充满 KPI 卡片的通用 SaaS 仪表盘。
- **Don't** 使用过多大圆角容器、玻璃拟态和装饰性渐变。
- **Don't** 把工程控制台术语作为普通创作者的主要入口。
- **Don't** 用加载动画或模糊的“处理中”隐藏失败原因和质量风险。
- **Don't** 使用装饰性 motion、页面加载编排或无状态意义的炫技动画。
- **Don't** 使用渐变文字、彩色侧边条、重复的卡片网格或 32px 以上卡片圆角。
- **Don't** 用高饱和色填满未激活状态，也不要让状态颜色成为唯一信息来源。
