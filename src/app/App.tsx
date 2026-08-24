import { useEffect, useMemo, useRef, useState, type CSSProperties, type PointerEvent as ReactPointerEvent } from "react";
import {
  Aperture, ChevronRight, CircleAlert, Clapperboard, Cpu, Download, Eye, FileBox, Info,
  FolderOpen, LoaderCircle, MapPin, Minus, Play, Plus, RotateCcw, Settings as SettingsIcon, Square, Trash2,
  Zap, ZapOff,
} from "lucide-react";
import {
  cancelPipeline, checkEngines, confirmAndDeleteProject, getProjectOverview, getSettings,
  onPipelineEvent, openProjectViewer, probeAndPlan, revealProject, selectProjectsRoot, selectVideo,
  setProjectsRoot, startPipeline,
} from "../lib/backend";
import { useAppStore } from "../stores/appStore";
import type {
  ColmapBackend,
  CudaColmapFlavor,
  EngineStatus,
  FfmpegHwAccel,
  BrushTrainingPreset,
  GsplatSplatCap,
  TrainingBackend,
  ProjectStatus,
  ProjectSummary,
  Quality,
} from "../types/pipeline";

const qualities: Array<{ value: Quality; label: string; description: string }> = [
  { value: "fast", label: "快速", description: "快速验证素材与拍摄路径" },
  { value: "balanced", label: "均衡", description: "质量与处理时间的推荐平衡" },
  { value: "high", label: "精细", description: "更充分地利用视频画面细节" },
];

const stages = [
  ["probingVideo", "视频分析"], ["extractingFrames", "画面提取"], ["selectingFrames", "画面筛选"],
  ["extractingFeatures", "特征提取"], ["matching", "顺序匹配"],
  ["reconstructing", "相机重建"], ["validatingReconstruction", "重建校验"], ["trainingSplats", "Splat 训练"],
  ["exporting", "结果发布"],
] as const;

const messageOf = (error: unknown) => typeof error === "string" ? error : error instanceof Error ? error.message : "处理失败，请查看项目日志。";
const basename = (path: string) => path.split(/[\\/]/).at(-1) ?? path;
const formatBytes = (bytes: number | null) => bytes == null ? "—" : bytes >= 1024 ** 3 ? `${(bytes / 1024 ** 3).toFixed(2)} GB` : bytes >= 1024 ** 2 ? `${(bytes / 1024 ** 2).toFixed(1)} MB` : `${(bytes / 1024).toFixed(1)} KB`;
const formatDuration = (milliseconds: number | null) => {
  if (milliseconds == null) return "—";
  const seconds = Math.floor(milliseconds / 1000);
  if (seconds < 60) return `${seconds} 秒`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分 ${seconds % 60} 秒`;
  return `${Math.floor(seconds / 3600)} 小时 ${Math.floor((seconds % 3600) / 60)} 分`;
};
const formatVideoDuration = (seconds: number) => `${Math.floor(seconds / 60)}:${Math.round(seconds % 60).toString().padStart(2, "0")}`;
const formatDate = (value: string | null) => value ? new Intl.DateTimeFormat("zh-CN", { year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" }).format(new Date(value)) : "—";
const qualityLabel = (quality: Quality) => qualities.find((item) => item.value === quality)?.label ?? quality;
const trainingLabel = (project: ProjectSummary) => {
  if (project.trainingBackend === "brush") return `Brush ${project.brushTrainingPreset.toUpperCase()}`;
  const cap: Record<GsplatSplatCap, string> = { auto: "自动安全", "1m": "100 万", "2m": "200 万", "4m": "400 万" };
  return `gsplat Splat${cap[project.gsplatSplatCap]}`;
};
const statusLabel: Record<ProjectStatus, string> = { running: "处理中", completed: "已完成", failed: "失败", cancelled: "已取消", interrupted: "已中断", needsSupplement: "等待补充素材" };
const stagePosition = (stage?: string) => {
  if (!stage || ["created", "probingVideo", "planningFrames"].includes(stage)) return 0;
  if (["completed", "failed", "cancelled", "needsSupplement"].includes(stage)) return 8;
  const index = stages.findIndex(([key]) => key === stage);
  return index < 0 ? 0 : index;
};
const currentStageLabel = (stage: string | undefined, activeStageIndex: number) => {
  if (stage === "completed") return "已完成";
  if (stage === "failed") return "任务失败";
  if (stage === "cancelled") return "已取消";
  if (stage === "needsSupplement") return "等待补充素材";
  return stages[activeStageIndex]?.[1] ?? "准备";
};
const readSavedNumber = (key: string, fallback: number) => {
  try {
    const value = Number(window.localStorage.getItem(key));
    return Number.isFinite(value) && value > 0 ? value : fallback;
  } catch {
    return fallback;
  }
};

function engineReady(engine: EngineStatus) {
  if (engine.kind !== "colmap") return engine.canStart;
  return engine.canStart;
}

function ProjectRow({ project, busy, opening, onDelete, onView }: { project: ProjectSummary; busy: boolean; opening: boolean; onDelete: (project: ProjectSummary) => void; onView: (project: ProjectSummary) => void }) {
  const [detailsOpen, setDetailsOpen] = useState(false);
  return <article className="project-row">
    <div className="project-row-main">
      <div className="project-title-line">
        <span className={`project-status ${project.status}`} />
        <strong>{project.name}</strong>
        <span className="status-copy">{statusLabel[project.status]}</span>
      </div>
      <p className="project-path" title={project.projectPath}>{project.projectPath}</p>
      {project.failureMessage && <p className="project-failure">{project.failureMessage}</p>}
    </div>
    <div className="project-actions">
      <button className="viewer-action" type="button" disabled={busy || opening || !project.finalPly} onClick={() => onView(project)}>{opening ? <LoaderCircle className="spin" size={14} /> : <Eye size={14} />}{opening ? "正在打开" : "查看 3D"}</button>
      <button type="button" onClick={() => void revealProject(project)}><MapPin size={14} />资源管理器</button>
      <button className="danger-link" type="button" disabled={busy} onClick={() => onDelete(project)}><Trash2 size={14} />删除</button>
      <button type="button" onClick={() => setDetailsOpen((open) => !open)}><Info size={14} />{detailsOpen ? "收起详情" : "详情"}</button>
    </div>
    {detailsOpen && <dl className="project-stats">
      <div><dt>PLY</dt><dd>{formatBytes(project.fileSize)}</dd></div>
      <div><dt>SPLAT</dt><dd>{project.splatCount?.toLocaleString() ?? "—"}</dd></div>
      <div><dt>生成日期</dt><dd>{formatDate(project.completedAt ?? project.createdAt)}</dd></div>
      <div><dt>耗时</dt><dd>{formatDuration(project.durationMs)}</dd></div>
      <div><dt>训练</dt><dd>{trainingLabel(project)}</dd></div>
      <div><dt>档位</dt><dd>{qualityLabel(project.quality)}</dd></div>
      <div><dt>SfM 注册</dt><dd>{project.registeredRatio == null ? "—" : `${(project.registeredRatio * 100).toFixed(1)}%`}</dd></div>
      <div><dt>三维点</dt><dd>{project.points3d?.toLocaleString() ?? "—"}</dd></div>
    </dl>}
  </article>;
}

function ColmapBackendBlock() {
  const store = useAppStore();
  const settings = store.settings;
  if (!settings) {
    return <p className="settings-empty">正在读取设置…</p>;
  }
  const cpu = settings.cpuColmap;
  const cuda = settings.cudaColmap;
  const current: ColmapBackend = settings.colmapBackend;
  const isRunning = store.phase === "running";
  const switchBackend = (backend: ColmapBackend) => {
    if (isRunning) {
      store.setError("运行中不能切换 COLMAP 后端");
      return;
    }
    void store.setColmapBackend(backend);
  };
  const requestCudaDownload = () => void store.downloadCudaColmap();
  const cpuReady = cpu?.exists && cpu.canStart && cpu.cpuOnly === true;
  const cudaReady = cuda?.exists && cuda.canStart && cuda.cudaAvailable === true;
  return (
    <div className="settings-block">
      <div className="settings-block-title">COLMAP 后端</div>
      <p className="settings-block-hint">默认选择 CUDA；首次使用请确认 CUDA COLMAP 已安装且显卡驱动可用。无法使用 CUDA 时可切换为 CPU/no-CUDA。</p>
      <div className="backend-toggle" role="radiogroup" aria-label="COLMAP 后端">
        <button
          type="button"
          role="radio"
          aria-checked={current === "cpu"}
          className={current === "cpu" ? "backend-option selected" : "backend-option"}
          disabled={isRunning || !cpuReady}
          onClick={() => switchBackend("cpu")}
        >
          <span className={`backend-badge ${cpuReady ? "ok" : "warn"}`}>{cpuReady ? "就绪" : cpu?.exists ? "异常" : "未找到"}</span>
          <span className="backend-icon"><Cpu size={16} /></span>
          <span className="backend-text"><strong>CPU / no-CUDA</strong><small>随安装包分发，无 GPU 依赖</small></span>
        </button>
        <button
          type="button"
          role="radio"
          aria-checked={current === "cuda"}
          className={current === "cuda" ? "backend-option selected" : "backend-option"}
          disabled={isRunning || !cudaReady}
          onClick={() => switchBackend("cuda")}
        >
          <span className={`backend-badge ${cudaReady ? "ok" : "warn"}`}>{cudaReady ? "就绪" : cuda?.exists ? "异常" : "未下载"}</span>
          <span className="backend-icon"><Aperture size={16} /></span>
          <span className="backend-text"><strong>CUDA（GPU 加速）</strong><small>官方 CUDA 构建，需要下载并启用 GPU</small></span>
        </button>
      </div>
      <div className="settings-row">
        <button className="secondary-action" type="button" disabled={isRunning} onClick={requestCudaDownload}>
          <Download size={14} />
          {cudaReady ? "重新校验 CUDA COLMAP" : "检查 CUDA COLMAP 是否可下载"}
        </button>
        <code className="settings-cli-hint">npm run download:colmap-cuda</code>
      </div>
    </div>
  );
}

function CudaColmapFlavorBlock() {
  const store = useAppStore();
  const settings = store.settings;
  if (!settings) return <p className="settings-empty">正在读取设置…</p>;
  const current: CudaColmapFlavor = settings.settings.cudaColmapFlavor;
  const isRunning = store.phase === "running";
  const cudaSelected = settings.colmapBackend === "cuda";
  const officialReady = settings.cudaColmap?.exists && settings.cudaColmap.canStart && settings.cudaColmap.cudaAvailable === true;
  const casparReady = settings.casparColmap?.exists && settings.casparColmap.canStart
    && settings.casparColmap.cudaAvailable === true && settings.casparColmap.casparAvailable === true;
  return <div className="settings-block">
    <div className="settings-block-title">COLMAP 引擎版本</div>
    <p className="settings-block-hint">两套 CUDA 引擎并存。切换只改变本次使用的可执行文件，不会覆盖官方 CUDA 引擎。{cudaSelected ? "" : " 当前为 CPU，开始任务时不会使用此选择。"}</p>
    <div className="backend-toggle" role="radiogroup" aria-label="COLMAP CUDA 引擎版本">
      <button type="button" role="radio" aria-checked={current === "official"} className={current === "official" ? "backend-option selected" : "backend-option"} disabled={isRunning || !officialReady} onClick={() => void store.setCudaColmapFlavor("official")}>
        <span className={`backend-badge ${officialReady ? "ok" : "warn"}`}>{officialReady ? "就绪" : "未找到"}</span>
        <span className="backend-text"><strong>官方 CUDA</strong><small>稳定基线；Mapper BA 使用 Ceres。</small></span>
      </button>
      <button type="button" role="radio" aria-checked={current === "caspar"} className={current === "caspar" ? "backend-option selected" : "backend-option"} title="适合中大型项目；使用 GPU 加速全局 BA，小项目可能受初始化开销影响。" disabled={isRunning || !casparReady} onClick={() => void store.setCudaColmapFlavor("caspar")}>
        <span className={`backend-badge ${casparReady ? "ok" : "warn"}`}>{casparReady ? "就绪" : "未安装"}</span>
        <span className="backend-text"><strong>CASPAR CUDA</strong><small>全局 BA 使用 GPU；本次同素材建图快约 70%，注册率保持 100%。</small></span>
      </button>
    </div>
  </div>;
}

function FfmpegHwAccelBlock() {
  const store = useAppStore();
  const settings = store.settings;
  if (!settings) {
    return <p className="settings-empty">正在读取设置…</p>;
  }
  const current: FfmpegHwAccel = settings.settings.ffmpegHwAccel;
  const isRunning = store.phase === "running";
  const ffmpegEngine = store.engines.find((engine) => engine.kind === "ffmpeg");
  const ffmpegReady = ffmpegEngine?.exists === true && ffmpegEngine.canStart;
  const switchMode = (mode: FfmpegHwAccel) => {
    if (isRunning) {
      store.setError("运行中不能切换 FFmpeg 硬件加速");
      return;
    }
    void store.setFfmpegHwAccel(mode);
  };
  const options: Array<{ value: FfmpegHwAccel; icon: typeof Zap; title: string; hint: string }> = [
    { value: "off", icon: ZapOff, title: "关闭", hint: "CPU 软解码，不使用硬件加速" },
    { value: "auto", icon: Zap, title: "自动", hint: "FFmpeg 自动选择可用运行时" },
    { value: "d3d11va", icon: Zap, title: "D3D11VA", hint: "Direct3D 11 视频加速（覆盖主流显卡）" },
    { value: "cuda", icon: Zap, title: "CUDA / NVDEC", hint: "锁定 NVIDIA 硬件解码器" },
  ];
  return (
    <div className="settings-block">
      <div className="settings-block-title">FFmpeg 抽帧硬件加速</div>
      <p className="settings-block-hint">仅影响视频解码阶段；JPEG 帧写入仍走 CPU 软编码（FFmpeg 无 JPEG GPU 编码器）。当前 FFmpeg 未就绪时不可切换。</p>
      <div className="backend-toggle" role="radiogroup" aria-label="FFmpeg 硬件加速">
        {options.map((option) => {
          const selected = current === option.value;
          const Icon = option.icon;
          return (
            <button
              key={option.value}
              type="button"
              role="radio"
              aria-checked={selected}
              className={selected ? "backend-option selected" : "backend-option"}
              title={option.hint}
              disabled={isRunning || !ffmpegReady}
              onClick={() => switchMode(option.value)}
            >
              <span className={`backend-badge ${ffmpegReady ? "ok" : "warn"}`}>{ffmpegReady ? "就绪" : "异常"}</span>
              <span className="backend-icon"><Icon size={16} /></span>
              <span className="backend-text"><strong>{option.title}</strong><small>{option.hint}</small></span>
            </button>
          );
        })}
      </div>
    </div>
  );
}

function BrushTrainingPresetBlock() {
  const store = useAppStore();
  const settings = store.settings;
  if (!settings) return <p className="settings-empty">正在读取设置…</p>;
  if (settings.settings.trainingBackend !== "brush") return null;
  const current: BrushTrainingPreset = settings.settings.brushTrainingPreset;
  const options: Array<{ value: BrushTrainingPreset; title: string; hint: string }> = [
    { value: "a", title: "A · 稳定均衡（默认）", hint: "适合多数设备；控制显存与训练时间。" },
    { value: "b", title: "B · 显存优先", hint: "适合 6–8GB 显存；降低 splat 上限以减少中断。" },
    { value: "c", title: "C · 质量优先", hint: "建议 12GB+ 显存；更多迭代与 splats，耗时更长。" },
  ];
  return <div className="settings-block"><div className="settings-block-title">Brush 训练预设</div><p className="settings-block-hint">仅影响 Brush 训练参数；不改变自适应 SfM 关键帧规划，也不改变固定策略回退的 1 / 2 / 4 FPS 基准。</p><div className="backend-toggle" role="radiogroup" aria-label="Brush 训练预设">{options.map((option) => <button key={option.value} type="button" role="radio" aria-checked={current === option.value} className={current === option.value ? "backend-option selected" : "backend-option"} title={option.hint} disabled={store.phase === "running"} onClick={() => void store.setBrushTrainingPreset(option.value)}><span className="backend-text"><strong>{option.title}</strong><small>{option.hint}</small></span></button>)}</div></div>;
}

function TrainingBackendBlock() {
  const store = useAppStore();
  const settings = store.settings;
  if (!settings) return null;
  const current: TrainingBackend = settings.settings.trainingBackend;
  const options: Array<{ value: TrainingBackend; title: string; hint: string; available: boolean }> = [
    { value: "brush", title: "Brush（稳定，推荐）", hint: "默认后端；跨设备稳定回退。", available: true },
    { value: "gsplat", title: "gsplat CUDA（实验）", hint: settings.gsplatAvailable ? "Python、CUDA 与 rasterization 健康检查已通过。" : "正在编译并验证 sm_120 CUDA 运行时；通过健康检查后才能选择。", available: settings.gsplatAvailable },
  ];
  return <div className="settings-block"><div className="settings-block-title">训练后端</div><p className="settings-block-hint">任务开始后锁定后端。gsplat 未通过 Python、CUDA 和 rasterization 三项检查前始终禁用。</p><div className="backend-toggle" role="radiogroup" aria-label="训练后端">{options.map((option) => <button key={option.value} type="button" role="radio" aria-checked={current === option.value} className={current === option.value ? "backend-option selected" : "backend-option"} title={option.hint} disabled={store.phase === "running" || !option.available} onClick={() => void store.setTrainingBackend(option.value)}><span className={`backend-badge ${option.available ? "ok" : "warn"}`}>{option.available ? "就绪" : "未就绪"}</span><span className="backend-text"><strong>{option.title}</strong><small>{option.hint}</small></span></button>)}</div></div>;
}

function GsplatSplatCapBlock() {
  const store = useAppStore();
  const settings = store.settings;
  if (!settings || settings.settings.trainingBackend !== "gsplat") return null;
  const current: GsplatSplatCap = settings.settings.gsplatSplatCap;
  const options: Array<{ value: GsplatSplatCap; title: string; hint: string }> = [
    { value: "auto", title: "自动安全（默认）", hint: "按质量档并受显存保护限制，最高 400 万。" },
    { value: "1m", title: "100 万", hint: "模型体积优先；适合 8GB 显存或快速发布。" },
    { value: "2m", title: "200 万", hint: "中等场景的手动硬上限。" },
    { value: "4m", title: "400 万", hint: "仅作硬上限；收敛后会自动停止增殖，非质量承诺。" },
  ];
  return <div className="settings-block"><div className="settings-block-title">gsplat splat 上限</div><p className="settings-block-hint">MCMC 会在验证损失停滞后冻结增殖，并过滤透明无效 splat；上限不是目标数量。</p><div className="backend-toggle" role="radiogroup" aria-label="gsplat splat 上限">{options.map((option) => <button key={option.value} type="button" role="radio" aria-checked={current === option.value} className={current === option.value ? "backend-option selected" : "backend-option"} title={option.hint} disabled={store.phase === "running"} onClick={() => void store.setGsplatSplatCap(option.value)}><span className="backend-text"><strong>{option.title}</strong><small>{option.hint}</small></span></button>)}</div></div>;
}

function PhotometricModeBlock() {
 const store = useAppStore();
 const settings = store.settings;
 if (!settings || settings.settings.trainingBackend !== "gsplat") return null;
 const current = settings.settings.photometricMode;
 const options = [
  { value: "none" as const, title: "关闭（M0 基线）", hint: "使用标准 L1 + DSSIM，不增加 PPISP 运行时成本。" },
  { value: "ppisp" as const, title: "PPISP（实验）", hint: "补偿曝光、白平衡、暗角与色调变化；单帧训练，PLY 不含 controller。" },
 ];
 return <div className="settings-block"><div className="settings-block-title">光度一致性</div><p className="settings-block-hint">仅 gsplat 生效。PPISP 适合曝光变化明显的视频；请与同素材 M0 基线对照后再用于正式交付。</p><div className="backend-toggle" role="radiogroup" aria-label="光度一致性">{options.map((option) => <button key={option.value} type="button" role="radio" aria-checked={current === option.value} className={current === option.value ? "backend-option selected" : "backend-option"} title={option.hint} disabled={store.phase === "running"} onClick={() => void store.setPhotometricMode(option.value)}><span className="backend-text"><strong>{option.title}</strong><small>{option.hint}</small></span></button>)}</div></div>;
}

function SettingsDrawer({ open, onClose }: { open: boolean; onClose: () => void }) {
  const store = useAppStore();
  return (
    <aside className={open ? "settings-drawer open" : "settings-drawer"} aria-hidden={!open}>
      <header className="settings-drawer-head">
        <h2>设置</h2>
        <button type="button" onClick={onClose} aria-label="关闭设置">×</button>
      </header>
      <div className="settings-drawer-body">
        <ColmapBackendBlock />
        <CudaColmapFlavorBlock />
        <FfmpegHwAccelBlock />
        <TrainingBackendBlock />
<GsplatSplatCapBlock />
<PhotometricModeBlock />
        <BrushTrainingPresetBlock />
        {store.settingsNotice && (
          <div className="settings-notice">
            <span>{store.settingsNotice}</span>
            <button type="button" onClick={() => store.clearSettingsNotice()}>关闭</button>
          </div>
        )}
      </div>
    </aside>
  );
}

export function App() {
  const store = useAppStore();
  const isRunning = store.phase === "running";
  const workspaceRef = useRef<HTMLElement>(null);
  const [leftPanePercent, setLeftPanePercent] = useState(() => Math.min(68, Math.max(32, readSavedNumber("ooo-splat-left-pane", 44))));
  const [uiScale, setUiScale] = useState(() => Math.min(140, Math.max(80, readSavedNumber("ooo-splat-ui-scale", 100))));
  const [isResizing, setIsResizing] = useState(false);
  const [showZoomControls, setShowZoomControls] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [openingProjectId, setOpeningProjectId] = useState<string | null>(null);
  const [clockMs, setClockMs] = useState(() => Date.now());
  const missingEngines = store.engines.filter((engine) => !engineReady(engine));
  const completed = useMemo(() => store.projects.filter((project) => project.status === "completed"), [store.projects]);
  const unfinished = useMemo(() => store.projects.filter((project) => project.status !== "completed"), [store.projects]);
  const activeStageIndex = stagePosition(store.latestEvent?.stage);
  const refreshProjects = async () => {
    const overview = await getProjectOverview();
    store.setProjectsRoot(overview.projectsRoot);
    store.setProjects(overview.projects);
  };
  useEffect(() => {
    void Promise.all([checkEngines(), getProjectOverview(), getSettings()])
      .then(([engines, overview, settings]) => {
        store.setEngines(engines);
        store.setProjectsRoot(overview.projectsRoot);
        store.setProjects(overview.projects);
        if (settings) store.setSettings(settings);
      })
      .catch((error) => store.setError(messageOf(error)));
  }, [store.setEngines, store.setProjects, store.setProjectsRoot, store.setSettings, store.setError]);
  useEffect(() => {
    let unlisten: undefined | (() => void);
    void onPipelineEvent(store.receiveEvent).then((fn) => { unlisten = fn; });
    return () => unlisten?.();
  }, [store.receiveEvent]);
  useEffect(() => {
    if (!isRunning) return;
    setClockMs(Date.now());
    const timer = window.setInterval(() => setClockMs(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [isRunning]);
  useEffect(() => {
    try { window.localStorage.setItem("ooo-splat-left-pane", leftPanePercent.toFixed(1)); } catch { /* optional preference */ }
  }, [leftPanePercent]);
  useEffect(() => {
    try { window.localStorage.setItem("ooo-splat-ui-scale", String(uiScale)); } catch { /* optional preference */ }
  }, [uiScale]);
  const resizePanes = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!isResizing || !workspaceRef.current) return;
    const bounds = workspaceRef.current.getBoundingClientRect();
    const next = ((event.clientX - bounds.left) / bounds.width) * 100;
    setLeftPanePercent(Math.min(68, Math.max(32, next)));
  };
  const stopResizing = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
    setIsResizing(false);
  };
  const changeScale = (delta: number) => setUiScale((current) => Math.min(140, Math.max(80, current + delta)));
  const analyze = async (path: string, quality: Quality) => {
    store.setPhase("analyzing");
    store.setError(null);
    try {
      const result = await probeAndPlan(path, quality);
      store.setAnalysis(result.video, result.plan);
      store.setPhase("idle");
    } catch (error) {
      store.setError(messageOf(error));
      store.setPhase("failed");
    }
  };
  const chooseVideo = async () => {
    const selected = await selectVideo();
    if (selected) { store.setVideoPath(selected); await analyze(selected, store.quality); }
  };
  const chooseRoot = async () => {
    const selected = await selectProjectsRoot(store.projectsRoot);
    if (!selected) return;
    try {
      const settings = await setProjectsRoot(selected);
      store.setProjectsRoot(settings.projectsRoot);
      await refreshProjects();
    } catch (error) { store.setError(messageOf(error)); }
  };
  const chooseQuality = async (quality: Quality) => {
    store.setQuality(quality);
    if (store.videoPath) await analyze(store.videoPath, quality);
  };
  const generate = async () => {
    if (!store.videoPath || !store.plan || !store.projectsRoot) return;
    store.beginRun();
    try {
      const result = await startPipeline(store.videoPath, store.quality, store.projectsRoot, store.autoBridgeFrames);
      store.setResult(result);
      store.setPhase("completed");
    } catch (error) {
      const message = messageOf(error);
      store.setError(message);
      store.setPhase(message.includes("取消") ? "cancelled" : message.includes("需要补充素材") ? "needsSupplement" : "failed");
    } finally {
      try { await refreshProjects(); } catch { /* the generated project remains on disk */ }
    }
  };
  const removeProject = async (project: ProjectSummary) => {
    try {
      if (await confirmAndDeleteProject(project)) await refreshProjects();
    } catch (error) { store.setError(messageOf(error)); }
  };
  const viewProject = async (project: ProjectSummary) => {
    if (openingProjectId) return;
    setOpeningProjectId(project.id);
    store.setError(null);
    try {
      await openProjectViewer(project);
    } catch (error) {
      store.setError(messageOf(error));
    } finally {
      setOpeningProjectId(null);
    }
  };
  const currentBackendLabel = store.settings ? (store.settings.colmapBackend === "cuda" ? "CUDA" : "CPU") : "CPU";
  const latestElapsedMs = store.latestEvent
    ? store.latestEvent.elapsedMs + (isRunning ? Math.max(0, clockMs - new Date(store.latestEvent.timestamp).getTime()) : 0)
    : 0;
  return (
    <main className={isResizing ? "app-shell resizing" : "app-shell"}>
      <div className="interface-frame" style={{ "--ui-scale": uiScale / 100, "--ui-size": `${10000 / uiScale}%` } as CSSProperties}>
      <header className="topbar">
        <div className="brand-lockup">
          <span className="brand-mark"><Aperture size={17} /></span>
          <span className="brand-name">OOO<span>Splat</span></span>
          <span className="version-tag">LOCAL / 0.47 · MOD By Jamin</span>
        </div>
        <div className="engine-summary">
          <span className={missingEngines.length ? "status-light warning" : "status-light"} />
          {store.engines.length === 0
            ? "正在检查内置引擎"
            : missingEngines.length
              ? `${missingEngines.length} 个引擎异常`
              : `FFmpeg · COLMAP ${currentBackendLabel} · Brush 就绪`}
        </div>
        <button type="button" className="topbar-settings" onClick={() => setSettingsOpen(true)} aria-label="打开设置">
          <SettingsIcon size={16} />设置
        </button>
      </header>

      <section className="workspace" ref={workspaceRef} style={{ "--left-pane-width": `${leftPanePercent}%` } as CSSProperties}>
        <section className="control-pane" aria-label="生成控制台">
          <div className="pane-header"><h1>01 创建新任务</h1><span className={isRunning ? "run-state active" : "run-state"}>{isRunning ? "运行中" : "待命"}</span></div>

          <div className="form-section">
            <label className="field-label">输入视频</label>
            <button className="path-picker" type="button" disabled={isRunning} onClick={() => void chooseVideo()}>
              <Clapperboard size={18} /><span><strong>{store.videoPath ? basename(store.videoPath) : "选择 MP4 或 MOV 视频"}</strong><small>{store.videoPath ?? "从本机选择环绕拍摄素材"}</small></span><FolderOpen size={16} />
            </button>
            <label className="field-note" style={{ display: "flex", alignItems: "center", gap: 8, marginTop: 10, cursor: isRunning ? "default" : "pointer" }}>
              <input type="checkbox" checked={store.autoBridgeFrames} disabled={isRunning} onChange={(event) => store.setAutoBridgeFrames(event.currentTarget.checked)} />
              自动从原视频补帧
            </label>
          </div>

          <div className="form-section">
            <label className="field-label">项目根目录</label>
            <button className="path-picker compact" type="button" disabled={isRunning} onClick={() => void chooseRoot()}>
              <FolderOpen size={18} /><span><strong>{store.projectsRoot ? basename(store.projectsRoot) : "正在读取默认目录"}</strong><small>{store.projectsRoot || "Documents\\SplatStudio\\Projects"}</small></span><ChevronRight size={16} />
            </button>
            <p className="field-note">每次生成会在此处创建独立项目文件夹，final.ply 直接保存在项目根部。</p>
          </div>

          <div className="form-section">
            <label className="field-label">生成质量</label>
            <div className="quality-list" role="radiogroup">
              {qualities.map((quality) => <button key={quality.value} type="button" role="radio" disabled={isRunning} aria-checked={store.quality === quality.value} className={store.quality === quality.value ? "quality-option selected" : "quality-option"} onClick={() => void chooseQuality(quality.value)}>
                <span className="radio-mark"><span /></span><span><strong>{quality.label}</strong><small>{quality.description}</small></span>
              </button>)}
            </div>
          </div>

          {store.video && store.plan && <div className="source-metrics">
            <span><small>时长</small><b>{formatVideoDuration(store.video.duration)}</b></span>
            <span><small>分辨率</small><b>{store.video.width} × {store.video.height}</b></span>
            <span><small>预计帧数</small><b>约 {store.plan.estimatedFrames.toLocaleString()}</b></span>
          </div>}

          {!isRunning && <button className="primary-action" type="button" disabled={!store.videoPath || !store.plan || !store.projectsRoot || store.phase === "analyzing" || missingEngines.length > 0} onClick={() => void generate()}>
            {store.phase === "analyzing" ? <LoaderCircle className="spin" size={17} /> : <Play size={16} fill="currentColor" />}
            {store.phase === "analyzing" ? "正在分析视频" : "开始生成"}<ChevronRight size={16} />
          </button>}

          {(isRunning || store.events.length > 0) && <section className="live-process">
            <div className="live-heading"><div><span className="live-dot" /><strong>实时进程</strong></div><span className="mono">总进度 {store.progress.toFixed(1)}%</span></div>
            <p className="current-message">{store.progressMessage || "正在准备任务"}</p>
            <div className="process-metrics">
              <span><small>当前阶段</small><b>{currentStageLabel(store.latestEvent?.stage, activeStageIndex)}</b></span>
              <span><small>阶段进度</small><b>{store.latestEvent?.current != null ? `${store.latestEvent.current.toLocaleString()}${store.latestEvent.total ? ` / ${store.latestEvent.total.toLocaleString()}` : ""}` : "持续运行"}</b></span>
              <span><small>总耗时</small><b>{formatDuration(latestElapsedMs)}</b></span>
            </div>
            <ol className="stage-timeline">
              {stages.map(([key, label], index) => <li key={key} className={index < activeStageIndex || store.phase === "completed" ? "done" : index === activeStageIndex && isRunning ? "active" : ""}><span /><b>{label}</b>{index === activeStageIndex && isRunning && <small>{store.latestEvent?.indeterminate ? "运行中" : `${(store.latestEvent?.stageProgress ?? 0).toFixed(0)}%`}</small>}</li>)}
            </ol>
            <div className="log-toolbar"><span>任务日志</span><small>最近 {store.events.length} / 500 条</small></div>
            <div className="live-log" aria-live="polite">
              {store.events.map((event, index) => <div className={`log-line ${event.level}`} key={`${event.sequence}-${index}`}><time>{new Date(event.timestamp).toLocaleTimeString("zh-CN", { hour12: false })}</time><span>{event.engine ?? "system"}</span><p>{event.message}</p></div>)}
            </div>
            {isRunning && <button className="cancel-action" type="button" onClick={() => void cancelPipeline()}><Square size={12} fill="currentColor" />取消任务并终止所有进程</button>}
          </section>}

          {store.error && <div className="inline-error"><CircleAlert size={16} /><span>{store.error}</span><button type="button" onClick={() => store.setError(null)}>关闭</button></div>}
        </section>

        <div
          className="pane-resizer"
          role="separator"
          tabIndex={0}
          aria-label="调整左右面板宽度"
          aria-orientation="vertical"
          aria-valuemin={32}
          aria-valuemax={68}
          aria-valuenow={Math.round(leftPanePercent)}
          onPointerDown={(event) => {
            if (event.button !== 0) return;
            event.currentTarget.setPointerCapture(event.pointerId);
            setIsResizing(true);
          }}
          onPointerMove={resizePanes}
          onPointerUp={stopResizing}
          onPointerCancel={stopResizing}
          onDoubleClick={() => setLeftPanePercent(44)}
          onKeyDown={(event) => {
            if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
              event.preventDefault();
              setLeftPanePercent((current) => Math.min(68, Math.max(32, current + (event.key === "ArrowLeft" ? -2 : 2))));
            }
            if (event.key === "Home") setLeftPanePercent(44);
          }}
        ><span /></div>

        <section className="projects-pane" aria-label="历史任务">
          <div className="pane-header"><h2>02 历史任务</h2><button className="refresh-action" type="button" disabled={isRunning} onClick={() => void refreshProjects()}><RotateCcw size={14} />刷新</button></div>
          <div className="archive-summary"><span><b>{completed.length}</b><small>已完成</small></span><span><b>{unfinished.length}</b><small>未完成</small></span></div>

          {completed.length === 0 && unfinished.length === 0 && <div className="empty-state"><FileBox size={30} strokeWidth={1.4} /><strong>还没有生成项目</strong><p>选择视频和项目目录后开始生成，成果会自动出现在这里。</p></div>}

          {completed.length > 0 && <div className="project-group"><div className="group-heading"><span>已完成</span><small>{completed.length} 个项目</small></div>{completed.map((project) => <ProjectRow key={project.id} project={project} busy={isRunning} opening={openingProjectId === project.id} onDelete={(item) => void removeProject(item)} onView={(item) => void viewProject(item)} />)}</div>}
          {unfinished.length > 0 && <div className="project-group unfinished"><div className="group-heading"><span>未完成</span><small>{unfinished.length} 个项目</small></div>{unfinished.map((project) => <ProjectRow key={project.id} project={project} busy={isRunning} opening={openingProjectId === project.id} onDelete={(item) => void removeProject(item)} onView={(item) => void viewProject(item)} />)}</div>}
        </section>
      </section>
      </div>

      <aside className={showZoomControls ? "zoom-dock open" : "zoom-dock"} aria-label="界面缩放">
        {showZoomControls && <div className="zoom-controls">
          <button type="button" aria-label="缩小界面" disabled={uiScale <= 80} onClick={() => changeScale(-10)}><Minus size={16} /></button>
          <button className="zoom-reset" type="button" title="恢复 100%" onClick={() => setUiScale(100)}>恢复</button>
          <button type="button" aria-label="放大界面" disabled={uiScale >= 140} onClick={() => changeScale(10)}><Plus size={16} /></button>
        </div>}
        <button className="zoom-trigger" type="button" aria-expanded={showZoomControls} onClick={() => setShowZoomControls((visible) => !visible)}>{uiScale}%</button>
      </aside>

      <SettingsDrawer open={settingsOpen} onClose={() => setSettingsOpen(false)} />
    </main>
  );
}
