import { memo, useCallback, useEffect, useMemo, useRef, useState, type CSSProperties, type PointerEvent as ReactPointerEvent } from "react";
import {
  Aperture, ChevronRight, CircleAlert, Clapperboard, Cpu, Download, Eye, FileBox, Info,
  FolderOpen, LoaderCircle, MapPin, Minus, Play, Plus, RotateCcw, Settings as SettingsIcon, Square, Trash2, X,
  Zap, ZapOff,
} from "lucide-react";
import {
  attachSupplementalMediaBatch, cancelPipeline, checkEngines, confirmAndDeleteProject, detachSupplementalMedia, getProjectOverview, getSettings, getSupplementDiagnostics, getSupplementOriginalPreview, getSupplementPreviews,
  inspectSplatcamImport, onPipelineEvent, openProjectViewer, probeAndPlan, revealProject, selectProjectsRoot, selectSplatcamDirectory, selectSupplementalMedia, selectVideo, validateSupplementalMedia,
  setProjectsRoot, startPipeline, startSplatcamPipeline,
} from "../lib/backend";
import { useAppStore } from "../stores/appStore";
import type {
  ColmapBackend,
  CudaColmapFlavor,
  EngineStatus,
  FfmpegHwAccel,
  BrushTrainingPreset,
  GsplatSplatCap,
  GsplatDensificationStrategy,
  TrainingBackend,
  ProjectStatus,
  ProjectSummary,
  SupplementDiagnostics,
  SupplementPreview,
  PipelineEvent,
  Quality,
  InputSource,
  SplatcamImportReport,
} from "../types/pipeline";

const qualities: Array<{ value: Quality; label: string; description: string }> = [
  { value: "fast", label: "快速", description: "快速验证素材与拍摄路径" },
  { value: "balanced", label: "均衡", description: "质量与处理时间的推荐平衡" },
  { value: "high", label: "精细", description: "更充分地利用视频画面细节" },
];

const splatcamTrainingProfiles: Record<Quality, { iterations: number; resolution: number }> = {
  fast: { iterations: 7_000, resolution: 512 },
  balanced: { iterations: 15_000, resolution: 1024 },
  high: { iterations: 30_000, resolution: 1920 },
};

const splatcamBrushCap = (quality: Quality, preset: BrushTrainingPreset) => {
  const caps: Record<BrushTrainingPreset, Record<Quality, number>> = {
    a: { fast: 1_500_000, balanced: 3_000_000, high: 5_000_000 },
    b: { fast: 1_000_000, balanced: 2_000_000, high: 3_000_000 },
    c: { fast: 2_000_000, balanced: 5_000_000, high: 8_000_000 },
  };
  return caps[preset][quality];
};

const splatcamTrainingDetail = (
  quality: Quality,
  trainingBackend: TrainingBackend | undefined,
  brushPreset: BrushTrainingPreset | undefined,
  gsplatCap: GsplatSplatCap | undefined,
) => {
  const profile = splatcamTrainingProfiles[quality];
  if (trainingBackend === "brush" && brushPreset) {
    const iterations = brushPreset === "c" ? Math.floor(profile.iterations * 1.5) : profile.iterations;
    const resolution = brushPreset === "b" ? Math.min(profile.resolution, 1536) : profile.resolution;
    return `${iterations.toLocaleString()} 次迭代 · ${resolution}px · Brush ${brushPreset.toUpperCase()} 上限 ${(splatcamBrushCap(quality, brushPreset) / 10_000).toLocaleString()} 万 splat`;
  }
  const cap = gsplatCap === "1m" ? 1_000_000 : gsplatCap === "2m" ? 2_000_000 : 4_000_000;
  const qualityCap = quality === "fast" ? 1_500_000 : quality === "balanced" ? 3_000_000 : 5_000_000;
  return `${profile.iterations.toLocaleString()} 次迭代 · ${profile.resolution}px · gsplat 上限 ${Math.min(cap, qualityCap) / 10_000} 万 splat`;
};

const stages = [
  ["importingSplatcam", "Splatcam 导入"],
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
  const strategy = project.gsplatDensificationStrategy === "absgrad" ? "AbsGS" : "MCMC";
  const module = project.photometricMode === "wdr" ? " · WD-R" : project.photometricMode === "ppisp" ? " · PPISP" : "";
  return `gsplat ${strategy} · Splat${cap[project.gsplatSplatCap]}${module}`;
};
const statusLabel: Record<ProjectStatus, string> = { running: "处理中", completed: "已完成", failed: "失败", cancelled: "已取消", interrupted: "已中断", needsSupplement: "等待补充素材" };
type ProjectFilter = "all" | "completed" | "needsSupplement" | "failed";
const taskFilters: Array<{ value: ProjectFilter; label: string }> = [
  { value: "all", label: "全部" },
  { value: "completed", label: "已完成" },
  { value: "needsSupplement", label: "等待补充" },
  { value: "failed", label: "失败/已取消" },
];
const stagePosition = (stage?: string) => {
  if (!stage || stage === "created") return 0;
  if (["probingVideo", "planningFrames"].includes(stage)) return 1;
  if (["completed", "failed", "cancelled", "needsSupplement"].includes(stage)) return 9;
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

const ProjectRow = memo(function ProjectRow({ project, busy, opening, onDelete, onView, onSupplementChanged }: { project: ProjectSummary; busy: boolean; opening: boolean; onDelete: (project: ProjectSummary) => void; onView: (project: ProjectSummary) => void; onSupplementChanged: () => Promise<void> }) {
  const [detailsOpen, setDetailsOpen] = useState(false);
  const [supplementOpen, setSupplementOpen] = useState(false);
  const [supplementDiagnostics, setSupplementDiagnostics] = useState<SupplementDiagnostics | null>(null);
  const [supplementError, setSupplementError] = useState<string | null>(null);
  const [loadingSupplement, setLoadingSupplement] = useState(false);
  const [attachingWeakInterval, setAttachingWeakInterval] = useState<number | null>(null);
  const [attachingMediaCount, setAttachingMediaCount] = useState(0);
  const [removingSupplementPath, setRemovingSupplementPath] = useState<string | null>(null);
  const [validatingWeakInterval, setValidatingWeakInterval] = useState<number | null>(null);
  const [supplementPreviews, setSupplementPreviews] = useState<SupplementPreview[]>([]);
  const [originalPreview, setOriginalPreview] = useState<{ label: string; dataUrl: string } | null>(null);
  const [loadingOriginalPreview, setLoadingOriginalPreview] = useState(false);
  const [selectedWeakInterval, setSelectedWeakInterval] = useState(0);
  const toggleSupplement = async () => {
    if (supplementOpen) {
      setSupplementOpen(false);
      return;
    }
    setSupplementError(null);
    setSupplementOpen(true);
    setSelectedWeakInterval(0);
    if (supplementDiagnostics || loadingSupplement) return;
    setLoadingSupplement(true);
    try {
      const [diagnostics, previews] = await Promise.all([
        getSupplementDiagnostics(project.id),
        getSupplementPreviews(project.id),
      ]);
      setSupplementDiagnostics(diagnostics);
      setSupplementPreviews(previews);
    } catch (error) {
      setSupplementError(messageOf(error));
    } finally {
      setLoadingSupplement(false);
    }
  };
  const validateMedia = async (weakIntervalIndex: number) => {
    setSupplementError(null);
    setValidatingWeakInterval(weakIntervalIndex);
    try {
      setSupplementDiagnostics(await validateSupplementalMedia(project.id, weakIntervalIndex));
      void onSupplementChanged();
    } catch (error) {
      setSupplementError(messageOf(error));
    } finally {
      setValidatingWeakInterval(null);
    }
  };
  const openOriginalPreview = async (label: string, outputFile: string) => {
    setSupplementError(null);
    setLoadingOriginalPreview(true);
    try {
      setOriginalPreview({ label, dataUrl: await getSupplementOriginalPreview(project.id, outputFile) });
    } catch (error) {
      setSupplementError(messageOf(error));
    } finally {
      setLoadingOriginalPreview(false);
    }
  };
  const attachMedia = async (weakIntervalIndex: number) => {
    const paths = await selectSupplementalMedia();
    if (paths.length === 0) return;
    setSupplementError(null);
    setAttachingWeakInterval(weakIntervalIndex);
    setAttachingMediaCount(paths.length);
    try {
      setSupplementDiagnostics(await attachSupplementalMediaBatch(project.id, weakIntervalIndex, paths));
      void onSupplementChanged();
    } catch (error) {
      setSupplementError(messageOf(error));
    } finally {
      setAttachingWeakInterval(null);
      setAttachingMediaCount(0);
    }
  };
  const detachMedia = async (weakIntervalIndex: number, path: string) => {
    setSupplementError(null);
    setRemovingSupplementPath(path);
    try {
      setSupplementDiagnostics(await detachSupplementalMedia(project.id, weakIntervalIndex, path));
      void onSupplementChanged();
    } catch (error) {
      setSupplementError(messageOf(error));
    } finally {
      setRemovingSupplementPath(null);
    }
  };
  const activeInterval = supplementDiagnostics?.weakIntervals[selectedWeakInterval];
  const activePreview = supplementPreviews.find((preview) => preview.weakIntervalIndex === selectedWeakInterval);
  const previewSlots = [
    { label: "前锚点", image: activePreview?.beforeAnchor, missing: "缺少前锚点" },
    { label: "弱区画面", image: activePreview?.weakFrame, missing: "预览不可用" },
    { label: "后锚点", image: activePreview?.afterAnchor, missing: activeInterval?.afterAnchor ? "预览不可用" : "视频结束，缺少后锚点" },
  ];
  return <article className="project-row">
    <div className="project-row-main">
      <div className="project-title-line">
        <span className={`project-status ${project.status}`} />
        <strong>{project.name}</strong>
        <span className="status-copy">{statusLabel[project.status]}</span>
      </div>
      <p className="project-path" title={project.projectPath}>{project.projectPath}</p>
      {project.status === "needsSupplement" && <p className="project-supplement-state">{project.weakIntervalCount ?? 0} 个弱区 · 已绑定 {project.supplementalMediaCount} 份素材 · 待验证</p>}
      {project.failureMessage && <p className="project-failure">{project.failureMessage}</p>}
    </div>
    <div className="project-actions">
      <button className="viewer-action" type="button" disabled={busy || opening || !project.finalPly} onClick={() => onView(project)}>{opening ? <LoaderCircle className="spin" size={14} /> : <Eye size={14} />}{opening ? "正在打开" : "查看 3D"}</button>
      <button type="button" onClick={() => void revealProject(project)}><MapPin size={14} />资源管理器</button>
      {project.status === "needsSupplement" && <button className="supplement-action" type="button" disabled={busy || loadingSupplement} onClick={() => void toggleSupplement()}><CircleAlert size={14} />{loadingSupplement ? "正在读取弱区" : supplementOpen ? "收起弱区" : "继续补拍"}</button>}
      <button className="danger-link" type="button" disabled={busy} onClick={() => onDelete(project)}><Trash2 size={14} />删除</button>
      <button type="button" onClick={() => setDetailsOpen((open) => !open)}><Info size={14} />{detailsOpen ? "收起详情" : "详情"}</button>
    </div>
    {detailsOpen && <dl className="project-stats">
      <div><dt>PLY</dt><dd>{formatBytes(project.fileSize)}</dd></div>
      <div><dt>SPLAT</dt><dd>{project.splatCount?.toLocaleString() ?? "—"}</dd></div>
      <div><dt>生成日期</dt><dd>{formatDate(project.completedAt ?? project.createdAt)}</dd></div>
      <div><dt>耗时</dt><dd>{formatDuration(project.durationMs)}</dd></div>
      <div><dt>训练</dt><dd>{trainingLabel(project)}</dd></div>
      <div><dt>数据源</dt><dd>{project.inputSource === "splatcam" ? "Splatcam 已重建数据" : "视频重建"}</dd></div>
      <div><dt>档位</dt><dd>{qualityLabel(project.quality)}</dd></div>
      <div><dt>SfM 注册</dt><dd>{project.registeredRatio == null ? "—" : `${(project.registeredRatio * 100).toFixed(1)}%`}</dd></div>
      <div><dt>三维点</dt><dd>{project.points3d?.toLocaleString() ?? "—"}</dd></div>
    </dl>}
    {supplementOpen && <div className="supplement-dialog-backdrop" role="presentation" onMouseDown={() => setSupplementOpen(false)}><section className="supplement-dialog" role="dialog" aria-modal="true" aria-label="弱区补拍指引" onMouseDown={(event) => event.stopPropagation()}>
      <header><div><strong>继续补拍</strong><span>{project.name} · 已保存，未占用处理资源</span></div><button type="button" aria-label="关闭弱区补拍窗口" onClick={() => setSupplementOpen(false)}><X size={17} /></button></header>
      {supplementError && <p className="supplement-error">无法读取弱区诊断：{supplementError}</p>}
      {loadingSupplement && <p className="supplement-loading">正在读取已持久化的弱区时间轴…</p>}
      {supplementDiagnostics && <>
        <div className="supplement-summary"><strong>需要补充拍摄</strong><span>{supplementDiagnostics.registeredFrames} / {supplementDiagnostics.selectedFrames} 张关键帧已注册</span></div>
        <div className="weak-timeline" role="tablist" aria-label="弱区时间轴">{supplementDiagnostics.weakIntervals.map((interval, index) => <button key={`${interval.startPtsSeconds}-${interval.endPtsSeconds}-${index}`} type="button" role="tab" aria-selected={selectedWeakInterval === index} className={selectedWeakInterval === index ? "selected" : ""} onClick={() => setSelectedWeakInterval(index)} style={{ flexGrow: Math.max(1, interval.endPtsSeconds - interval.startPtsSeconds) }}><span>弱区 {index + 1}</span><small>{formatVideoDuration(interval.startPtsSeconds)}–{formatVideoDuration(interval.endPtsSeconds)}</small></button>)}</div>
        {activeInterval && <div className="weak-interval-detail">
          <div className="weak-interval-copy"><strong>弱区 {selectedWeakInterval + 1} · {formatVideoDuration(activeInterval.startPtsSeconds)}–{formatVideoDuration(activeInterval.endPtsSeconds)}</strong><span>{activeInterval.unregisteredFrames} 张未注册关键帧</span><p>问题：{activeInterval.reason === "unregisteredSelectedFrames" ? "视角重叠或视差不足" : activeInterval.reason}</p></div>
          <div className="weak-preview-strip">{previewSlots.map((slot) => <figure className={slot.image ? undefined : "unavailable"} key={slot.label}>{slot.image ? <img title="单击查看原图" onClick={() => void openOriginalPreview(slot.image!.label, slot.image!.outputFile)} src={slot.image.dataUrl} alt={`${slot.image.label} ${formatVideoDuration(slot.image.ptsSeconds)}`} /> : <div className="weak-preview-missing">{slot.missing}</div>}<figcaption><strong>{slot.image?.label ?? slot.label}</strong><span>{slot.image ? formatVideoDuration(slot.image.ptsSeconds) : "无可用帧"}</span></figcaption></figure>)}</div>
          {loadingOriginalPreview && <p className="supplement-loading">正在读取原图预览…</p>}
          <p className="supplement-hint">请沿“前锚点 → 弱区画面 → 后锚点”继续移动并保持主体重叠，让主体在画面中移动约 15–25% 宽度。单目素材没有可靠绝对尺度，因此不显示厘米或米。</p>
          <div className="supplement-media-row">
            <button type="button" className="secondary-action" disabled={busy || attachingWeakInterval != null} onClick={() => void attachMedia(selectedWeakInterval)}>{attachingWeakInterval === selectedWeakInterval ? <LoaderCircle className="spin" size={14} /> : <FolderOpen size={14} />}{attachingWeakInterval === selectedWeakInterval ? `正在绑定 ${attachingMediaCount} 个文件` : "添加候补文件（可多选）"}</button>
            <button type="button" className="secondary-action" disabled={busy || validatingWeakInterval != null || supplementDiagnostics.supplementalMedia.every((media) => media.weakIntervalIndex !== selectedWeakInterval)} onClick={() => void validateMedia(selectedWeakInterval)}>{validatingWeakInterval === selectedWeakInterval ? <LoaderCircle className="spin" size={14} /> : <CircleAlert size={14} />}{validatingWeakInterval === selectedWeakInterval ? "正在验证候补素材" : "验证候补素材"}</button>
            <div className="supplement-media-list" aria-label="已绑定候补素材">
              {supplementDiagnostics.supplementalMedia.filter((media) => media.weakIntervalIndex === selectedWeakInterval).map((media) => <div className="supplement-media-entry" key={media.path}>
                <span><strong>{media.kind === "video" ? "视频" : "照片"}</strong><small>{basename(media.path)} · {media.validationStatus === "pending" ? "待验证" : media.validationStatus === "passed" ? "已通过" : "未通过"}{media.validationReason ? `：${media.validationReason}` : ""}</small></span>
                <button type="button" disabled={busy || removingSupplementPath != null} onClick={() => void detachMedia(selectedWeakInterval, media.path)}>{removingSupplementPath === media.path ? <LoaderCircle className="spin" size={13} /> : <Trash2 size={13} />}{removingSupplementPath === media.path ? "正在移除" : "移除"}</button>
              </div>)}
            </div>
          </div>
        </div>}
        <p className="supplement-next">已绑定的素材尚未解码或送入 COLMAP。下一阶段会先做重叠、清晰度与视差验证；当前可通过“资源管理器”核对完整日志。</p>
      </>}
    </section></div>}
    {originalPreview && <div className="original-preview-backdrop" role="presentation" onMouseDown={() => setOriginalPreview(null)}><section className="original-preview-dialog" role="dialog" aria-modal="true" aria-label={`${originalPreview.label} 原图预览`} onMouseDown={(event) => event.stopPropagation()}><header><strong>{originalPreview.label} · 原图预览</strong><button type="button" aria-label="关闭原图预览" onClick={() => setOriginalPreview(null)}><X size={17} /></button></header><img src={originalPreview.dataUrl} alt={`${originalPreview.label} 原图`} /></section></div>}
  </article>;
});

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

const ProjectList = memo(function ProjectList({ completed, waitingSupplement, active, failed, busy, openingProjectId, onDelete, onView, onSupplementChanged }: {
  completed: ProjectSummary[];
  waitingSupplement: ProjectSummary[];
  active: ProjectSummary[];
  failed: ProjectSummary[];
  busy: boolean;
  openingProjectId: string | null;
  onDelete: (project: ProjectSummary) => void;
  onView: (project: ProjectSummary) => void;
  onSupplementChanged: () => Promise<void>;
}) {
  return <>
    {completed.length === 0 && waitingSupplement.length === 0 && active.length === 0 && failed.length === 0 && <div className="empty-state"><FileBox size={30} strokeWidth={1.4} /><strong>没有符合筛选条件的任务</strong><p>切换上方筛选，或选择视频后开始生成。</p></div>}
    {waitingSupplement.length > 0 && <div className="project-group waiting-supplement"><div className="group-heading"><span>等待补充</span><small>{waitingSupplement.length} 个任务</small></div>{waitingSupplement.map((project) => <ProjectRow key={project.id} project={project} busy={busy} opening={openingProjectId === project.id} onDelete={onDelete} onView={onView} onSupplementChanged={onSupplementChanged} />)}</div>}
    {active.length > 0 && <div className="project-group"><div className="group-heading"><span>处理中</span><small>{active.length} 个任务</small></div>{active.map((project) => <ProjectRow key={project.id} project={project} busy={busy} opening={openingProjectId === project.id} onDelete={onDelete} onView={onView} onSupplementChanged={onSupplementChanged} />)}</div>}
    {failed.length > 0 && <div className="project-group unfinished"><div className="group-heading"><span>失败 / 已取消</span><small>{failed.length} 个任务</small></div>{failed.map((project) => <ProjectRow key={project.id} project={project} busy={busy} opening={openingProjectId === project.id} onDelete={onDelete} onView={onView} onSupplementChanged={onSupplementChanged} />)}</div>}
    {completed.length > 0 && <div className="project-group completed-group"><div className="group-heading"><span>已完成</span><small>{completed.length} 个项目</small></div>{completed.map((project) => <ProjectRow key={project.id} project={project} busy={busy} opening={openingProjectId === project.id} onDelete={onDelete} onView={onView} onSupplementChanged={onSupplementChanged} />)}</div>}
  </>;
});

const LiveLog = memo(function LiveLog({ events }: { events: PipelineEvent[] }) {
  return <div className="live-log" aria-label="任务日志">
    {events.map((event, index) => <div className={`log-line ${event.level}`} key={`${event.sequence}-${index}`}><time>{new Date(event.timestamp).toLocaleTimeString("zh-CN", { hour12: false })}</time><span>{event.engine ?? "system"}</span><p>{event.message}</p></div>)}
  </div>;
});

const LiveProcess = memo(function LiveProcess({ isRunning, events, progress, progressMessage, latestEvent, activeStageIndex, latestElapsedMs, onCancel }: {
  isRunning: boolean;
  events: PipelineEvent[];
  progress: number;
  progressMessage: string;
  latestEvent: PipelineEvent | null;
  activeStageIndex: number;
  latestElapsedMs: number;
  onCancel: () => void;
}) {
  return <section className="live-process">
    <div className="live-heading"><div><span className="live-dot" /><strong>实时进程</strong></div><span className="mono">总进度 {progress.toFixed(1)}%</span></div>
    <p className="current-message" aria-live="polite">{progressMessage || "正在准备任务"}</p>
    <div className="process-metrics">
      <span><small>当前阶段</small><b>{currentStageLabel(latestEvent?.stage, activeStageIndex)}</b></span>
      <span><small>阶段进度</small><b>{latestEvent?.current != null ? `${latestEvent.current.toLocaleString()}${latestEvent.total ? ` / ${latestEvent.total.toLocaleString()}` : ""}` : "持续运行"}</b></span>
      <span><small>总耗时</small><b>{formatDuration(latestElapsedMs)}</b></span>
    </div>
    <ol className="stage-timeline">
      {stages.map(([key, label], index) => <li key={key} className={index < activeStageIndex || latestEvent?.stage === "completed" ? "done" : index === activeStageIndex && isRunning ? "active" : ""}><span /><b>{label}</b>{index === activeStageIndex && isRunning && <small>{latestEvent?.indeterminate ? "运行中" : `${(latestEvent?.stageProgress ?? 0).toFixed(0)}%`}</small>}</li>)}
    </ol>
    <div className="log-toolbar"><span>任务日志</span><small>最近 {events.length} / 500 条</small></div>
    <LiveLog events={events} />
    {isRunning && <button className="cancel-action" type="button" onClick={onCancel}><Square size={12} fill="currentColor" />取消任务并终止所有进程</button>}
  </section>;
});

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

function BrushTrainingPresetBlock({ inputSource }: { inputSource: InputSource }) {
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
  const hint = inputSource === "splatcam"
    ? "仅影响 Brush 训练参数；Splatcam 会直接使用导出的全部图片、相机与位姿。"
    : "仅影响 Brush 训练参数；不改变自适应 SfM 关键帧规划，也不改变固定策略回退的 1 / 2 / 4 FPS 基准。";
  return <div className="settings-block"><div className="settings-block-title">Brush 训练预设</div><p className="settings-block-hint">{hint}</p><div className="backend-toggle" role="radiogroup" aria-label="Brush 训练预设">{options.map((option) => <button key={option.value} type="button" role="radio" aria-checked={current === option.value} className={current === option.value ? "backend-option selected" : "backend-option"} title={option.hint} disabled={store.phase === "running"} onClick={() => void store.setBrushTrainingPreset(option.value)}><span className="backend-text"><strong>{option.title}</strong><small>{option.hint}</small></span></button>)}</div></div>;
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

function GsplatDensificationStrategyBlock() {
  const store = useAppStore();
  const settings = store.settings;
  if (!settings || settings.settings.trainingBackend !== "gsplat") return null;
  const current: GsplatDensificationStrategy = settings.settings.gsplatDensificationStrategy;
  const options: Array<{ value: GsplatDensificationStrategy; title: string; hint: string }> = [
    { value: "mcmc", title: "MCMC（默认，已验证）", hint: "稳定的采样与增殖路径；适合正式任务。" },
    { value: "absgrad", title: "AbsGS（实验 A/B）", hint: "按绝对屏幕梯度增殖；须和 MCMC 固定同素材、质量档、上限及 seed 对照。" },
  ];
  return <div className="settings-block"><div className="settings-block-title">gsplat 增殖策略</div><p className="settings-block-hint">仅 gsplat 生效。AbsGS 不会改变默认策略；首轮对照请关闭 PPISP，以保证只比较增殖策略。</p><div className="backend-toggle" role="radiogroup" aria-label="gsplat 增殖策略">{options.map((option) => <button key={option.value} type="button" role="radio" aria-checked={current === option.value} className={current === option.value ? "backend-option selected" : "backend-option"} title={option.hint} disabled={store.phase === "running"} onClick={() => void store.setGsplatDensificationStrategy(option.value)}><span className="backend-text"><strong>{option.title}</strong><small>{option.hint}</small></span></button>)}</div></div>;
}

function MultiViewDensificationGateBlock() {
  const store = useAppStore();
  const settings = store.settings;
  if (!settings || settings.settings.trainingBackend !== "gsplat" || settings.settings.gsplatDensificationStrategy !== "mcmc") return null;
  const enabled = settings.settings.multiViewDensificationGate;
  return <div className="settings-block"><div className="settings-block-title">多视角新增点门控（实验）</div><p className="settings-block-hint">仅限制 MCMC 新增点的父点选择；已有 splat、导出与裁剪均不变。请只在固定素材、seed、质量档和 cap 的 A/B 中开启。</p><button type="button" className={enabled ? "backend-option selected" : "backend-option"} disabled={store.phase === "running"} onClick={() => void store.setMultiViewDensificationGate(!enabled)}><span className="backend-text"><strong>{enabled ? "已开启" : "默认关闭"}</strong><small>{enabled ? "父点须获得至少两个采样训练视角支持。" : "标准 MCMC，不施加新增点门控。"}</small></span></button></div>;
}

function FloaterPruningBlock() {
  const store = useAppStore();
  const settings = store.settings;
  if (!settings || settings.settings.trainingBackend !== "gsplat" || settings.settings.gsplatDensificationStrategy !== "mcmc" || settings.settings.multiViewDensificationGate) return null;
  const enabled = settings.settings.floaterPruning;
  return <div className="settings-block"><div className="settings-block-title">保守浮点导出裁剪（实验）</div><p className="settings-block-hint">只在训练结束后对多证据候选生成副本 PLY，并用固定验证视图和 RGB 消融门槛决定是否采用；不改变训练过程。MCMC 默认关闭，且不能与新增点门控同时使用。</p><button type="button" className={enabled ? "backend-option selected" : "backend-option"} disabled={store.phase === "running"} onClick={() => void store.setFloaterPruning(!enabled)}><span className="backend-text"><strong>{enabled ? "已开启（严格回退）" : "默认关闭"}</strong><small>{enabled ? "保存 pre-prune/candidate 产物；质量回退则自动导出原模型。" : "仅写诊断，不生成裁剪候选。"}</small></span></button></div>;
}

function PhotometricModeBlock() {
 const store = useAppStore();
 const settings = store.settings;
 if (!settings || settings.settings.trainingBackend !== "gsplat") return null;
 const current = settings.settings.photometricMode;
 const options = [
  { value: "none" as const, title: "关闭（M0 基线）", hint: "使用标准 L1 + DSSIM，不增加附加模型成本。" },
  { value: "ppisp" as const, title: "PPISP（实验）", hint: "补偿曝光、白平衡、暗角与色调变化；单帧训练，PLY 不含 controller。" },
  { value: "wdr" as const, title: "WD-R（实验）", hint: "VGG-16 Wasserstein 感知损失；3k/20% warm-up 后启用，单帧训练且明显更慢。" },
 ];
 return <div className="settings-block"><div className="settings-block-title">附加训练模块</div><p className="settings-block-hint">仅 gsplat 生效，PPISP 与 WD-R 互斥。WD-R 是感知实验，必须以固定素材、cap、seed 的 M0/MCMC 对照验收。</p><div className="backend-toggle" role="radiogroup" aria-label="附加训练模块">{options.map((option) => <button key={option.value} type="button" role="radio" aria-checked={current === option.value} className={current === option.value ? "backend-option selected" : "backend-option"} title={option.hint} disabled={store.phase === "running"} onClick={() => void store.setPhotometricMode(option.value)}><span className="backend-text"><strong>{option.title}</strong><small>{option.hint}</small></span></button>)}</div></div>;
}

function SettingsDrawer({ open, onClose, inputSource }: { open: boolean; onClose: () => void; inputSource: InputSource }) {
  const store = useAppStore();
  const isSplatcam = inputSource === "splatcam";
  return (
    <aside className={open ? "settings-drawer open" : "settings-drawer"} aria-hidden={!open}>
      <header className="settings-drawer-head">
        <h2>设置</h2>
        <button type="button" onClick={onClose} aria-label="关闭设置">×</button>
      </header>
      <div className="settings-drawer-body">
        {isSplatcam ? <div className="settings-notice settings-context"><span><strong>Splatcam 训练模式</strong><br />已跳过视频抽帧、关键帧筛选和 COLMAP 重建；下方设置仅影响训练。</span></div> : <>
          <ColmapBackendBlock />
          <CudaColmapFlavorBlock />
          <FfmpegHwAccelBlock />
        </>}
        <TrainingBackendBlock />
        <GsplatSplatCapBlock />
        <GsplatDensificationStrategyBlock />
        <MultiViewDensificationGateBlock />
        <FloaterPruningBlock />
        <PhotometricModeBlock />
        <BrushTrainingPresetBlock inputSource={inputSource} />
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
  const [inputSource, setInputSource] = useState<InputSource>("video");
  const [splatcamPath, setSplatcamPath] = useState<string | null>(null);
  const [splatcamReport, setSplatcamReport] = useState<SplatcamImportReport | null>(null);
  const [checkingSplatcam, setCheckingSplatcam] = useState(false);
  const [projectFilter, setProjectFilter] = useState<ProjectFilter>("all");
  const missingEngines = store.engines.filter((engine) => !engineReady(engine));
  const filteredProjects = useMemo(() => store.projects.filter((project) => {
    if (projectFilter === "all") return true;
    if (projectFilter === "failed") return ["failed", "cancelled", "interrupted"].includes(project.status);
    return project.status === projectFilter;
  }), [projectFilter, store.projects]);
  const completed = useMemo(() => filteredProjects.filter((project) => project.status === "completed"), [filteredProjects]);
  const waitingSupplement = useMemo(() => filteredProjects.filter((project) => project.status === "needsSupplement"), [filteredProjects]);
  const activeProjects = useMemo(() => filteredProjects.filter((project) => project.status === "running"), [filteredProjects]);
  const failedProjects = useMemo(() => filteredProjects.filter((project) => ["failed", "cancelled", "interrupted"].includes(project.status)), [filteredProjects]);
  const totalWaitingSupplement = useMemo(() => store.projects.filter((project) => project.status === "needsSupplement").length, [store.projects]);
  const activeStageIndex = stagePosition(store.latestEvent?.stage);
  const refreshProjects = useCallback(async () => {
    const overview = await getProjectOverview();
    store.setProjectsRoot(overview.projectsRoot);
    store.setProjects(overview.projects);
  }, [store.setProjects, store.setProjectsRoot]);
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
  const chooseSplatcam = async () => {
    const selected = await selectSplatcamDirectory();
    if (!selected) return;
    setSplatcamPath(selected);
    setSplatcamReport(null);
  };
  const inspectSplatcam = async () => {
    if (!splatcamPath) return;
    setCheckingSplatcam(true);
    store.setError(null);
    try {
      setSplatcamReport(await inspectSplatcamImport(splatcamPath));
    } catch (error) {
      setSplatcamReport(null);
      store.setError(messageOf(error));
    } finally {
      setCheckingSplatcam(false);
    }
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
    if (inputSource === "video" && store.videoPath) await analyze(store.videoPath, quality);
  };
  const generate = async () => {
    const path = inputSource === "splatcam" ? splatcamPath : store.videoPath;
    if (!path || !store.projectsRoot || (inputSource === "video" && !store.plan)) return;
    store.beginRun();
    try {
      const result = inputSource === "splatcam"
        ? await startSplatcamPipeline(path, store.quality, store.projectsRoot)
        : await startPipeline(path, store.quality, store.projectsRoot, store.autoBridgeFrames);
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
  const removeProject = useCallback(async (project: ProjectSummary) => {
    try {
      if (await confirmAndDeleteProject(project)) await refreshProjects();
    } catch (error) { store.setError(messageOf(error)); }
  }, [refreshProjects, store.setError]);
  const viewProject = useCallback(async (project: ProjectSummary) => {
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
  }, [openingProjectId, store.setError]);
  const cancelRun = useCallback(() => void cancelPipeline(), []);
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
          <span className="version-tag">LOCAL / 0.48 · MOD By Jamin</span>
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
            <label className="field-label">数据源</label>
            <div className="source-toggle" role="radiogroup" aria-label="数据源">
              <button type="button" role="radio" aria-checked={inputSource === "video"} className={inputSource === "video" ? "selected" : ""} disabled={isRunning} onClick={() => setInputSource("video")}>视频重建</button>
              <button type="button" role="radio" aria-checked={inputSource === "splatcam"} className={inputSource === "splatcam" ? "selected" : ""} disabled={isRunning} onClick={() => setInputSource("splatcam")}>Splatcam 已重建数据</button>
            </div>
          </div>

          {inputSource === "video" ? <div className="form-section">
            <label className="field-label">输入视频</label>
            <button className="path-picker" type="button" disabled={isRunning} onClick={() => void chooseVideo()}>
              <Clapperboard size={18} /><span><strong>{store.videoPath ? basename(store.videoPath) : "选择 MP4 或 MOV 视频"}</strong><small>{store.videoPath ?? "从本机选择环绕拍摄素材"}</small></span><FolderOpen size={16} />
            </button>
            <label className="field-note" style={{ display: "flex", alignItems: "center", gap: 8, marginTop: 10, cursor: isRunning ? "default" : "pointer" }}>
              <input type="checkbox" checked={store.autoBridgeFrames} disabled={isRunning} onChange={(event) => store.setAutoBridgeFrames(event.currentTarget.checked)} />
              自动从原视频补帧
            </label>
          </div> : <div className="form-section">
            <label className="field-label">Splatcam 导出目录</label>
            <button className="path-picker" type="button" disabled={isRunning || checkingSplatcam} onClick={() => void chooseSplatcam()}>
              <FolderOpen size={18} /><span><strong>{splatcamPath ? basename(splatcamPath) : "选择包含 images 与 sparse/0 的目录"}</strong><small>{splatcamPath ?? "仅接受 RGB JPEG + COLMAP 文本模型 + RGB 点云"}</small></span><ChevronRight size={16} />
            </button>
            <button className="secondary-action splatcam-check" type="button" disabled={!splatcamPath || isRunning || checkingSplatcam} onClick={() => void inspectSplatcam()}>{checkingSplatcam ? <LoaderCircle className="spin" size={14} /> : <FileBox size={14} />}{checkingSplatcam ? "正在导入检查" : "仅导入检查"}</button>
            {splatcamReport && <div className={splatcamReport.geometryGate.passed ? "splatcam-report passed" : "splatcam-report failed"}>
              <strong>{splatcamReport.geometryGate.passed ? "检查通过：可进入下一阶段标准化" : "检查未通过"}</strong>
              <span>{splatcamReport.imageCount} 张 RGB · {splatcamReport.poseCount} 个位姿 · {splatcamReport.pointCount.toLocaleString()} 个初始化点</span>
              <small>坐标：COLMAP world-to-camera · 正深度 {(splatcamReport.positiveDepthProjectionRatio * 100).toFixed(1)}% · 图像内 {(splatcamReport.inImageProjectionRatio * 100).toFixed(1)}%</small>
              {!splatcamReport.geometryGate.passed && <small>{splatcamReport.geometryGate.reason}</small>}
            </div>}
            <p className="field-note">当前先执行只读检查；标准化模型与训练接入完成前不会启动 FFmpeg、特征提取或相机重建。</p>
          </div>}

          <div className="form-section">
            <label className="field-label">项目根目录</label>
            <button className="path-picker compact" type="button" disabled={isRunning} onClick={() => void chooseRoot()}>
              <FolderOpen size={18} /><span><strong>{store.projectsRoot ? basename(store.projectsRoot) : "正在读取默认目录"}</strong><small>{store.projectsRoot || "Documents\\SplatStudio\\Projects"}</small></span><ChevronRight size={16} />
            </button>
            <p className="field-note">每次生成会在此处创建独立项目文件夹，final.ply 直接保存在项目根部。</p>
          </div>

        <div className="form-section">
          <label className="field-label">{inputSource === "splatcam" ? "训练质量" : "生成质量"}</label>
          {inputSource === "splatcam" && <p className="field-hint">直接使用导出的图片与相机位姿；不会进行视频分析、抽帧或关键帧筛选。</p>}
          <div className="quality-list" role="radiogroup">
            {qualities.map((quality) => <button key={quality.value} type="button" role="radio" disabled={isRunning} aria-checked={store.quality === quality.value} className={store.quality === quality.value ? "quality-option selected" : "quality-option"} onClick={() => void chooseQuality(quality.value)}>
              <span className="radio-mark"><span /></span><span><strong>{quality.label}</strong><small>{inputSource === "splatcam" ? splatcamTrainingDetail(quality.value, store.settings?.settings.trainingBackend, store.settings?.settings.brushTrainingPreset, store.settings?.settings.gsplatSplatCap) : quality.description}</small></span>
            </button>)}
            </div>
          </div>

          {inputSource === "video" && store.video && store.plan && <div className="source-metrics">
            <span><small>时长</small><b>{formatVideoDuration(store.video.duration)}</b></span>
            <span><small>分辨率</small><b>{store.video.width} × {store.video.height}</b></span>
            <span><small>预计帧数</small><b>约 {store.plan.estimatedFrames.toLocaleString()}</b></span>
          </div>}

          {!isRunning && <button className="primary-action" type="button" disabled={inputSource === "splatcam" ? !splatcamPath || !splatcamReport?.geometryGate.passed || !store.projectsRoot : !store.videoPath || !store.plan || !store.projectsRoot || store.phase === "analyzing" || missingEngines.length > 0} onClick={() => void generate()}>
            {store.phase === "analyzing" ? <LoaderCircle className="spin" size={17} /> : <Play size={16} fill="currentColor" />}
            {inputSource === "splatcam" ? "导入并训练" : store.phase === "analyzing" ? "正在分析视频" : "开始生成"}<ChevronRight size={16} />
          </button>}

          {(isRunning || store.events.length > 0) && <LiveProcess
            isRunning={isRunning}
            events={store.events}
            progress={store.progress}
            progressMessage={store.progressMessage}
            latestEvent={store.latestEvent}
            activeStageIndex={activeStageIndex}
            latestElapsedMs={latestElapsedMs}
            onCancel={cancelRun}
          />}

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
          <div className="project-filter" role="tablist" aria-label="任务状态筛选">
            {taskFilters.map((filter) => <button key={filter.value} type="button" role="tab" aria-selected={projectFilter === filter.value} className={projectFilter === filter.value ? "selected" : ""} onClick={() => setProjectFilter(filter.value)}>{filter.label}{filter.value === "needsSupplement" && totalWaitingSupplement > 0 && <span>{totalWaitingSupplement}</span>}</button>)}
          </div>
          <div className="archive-summary"><span><b>{store.projects.filter((project) => project.status === "completed").length}</b><small>已完成</small></span><span><b>{totalWaitingSupplement}</b><small>等待补充</small></span></div>

          <ProjectList completed={completed} waitingSupplement={waitingSupplement} active={activeProjects} failed={failedProjects} busy={isRunning} openingProjectId={openingProjectId} onDelete={removeProject} onView={viewProject} onSupplementChanged={refreshProjects} />
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

      <SettingsDrawer open={settingsOpen} onClose={() => setSettingsOpen(false)} inputSource={inputSource} />
    </main>
  );
}
