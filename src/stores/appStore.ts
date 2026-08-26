import { create } from "zustand";
import {
  downloadColmapCuda,
  setColmapBackend as setColmapBackendInvoke,
  setCudaColmapFlavor as setCudaColmapFlavorInvoke,
  setMapperBaMode as setMapperBaModeInvoke,
  setFfmpegHwAccel as setFfmpegHwAccelInvoke,
  setBrushTrainingPreset as setBrushTrainingPresetInvoke,
  setGsplatSplatCap as setGsplatSplatCapInvoke,
  setGsplatDensificationStrategy as setGsplatDensificationStrategyInvoke,
  setTrainingBackend as setTrainingBackendInvoke,
  setPhotometricMode as setPhotometricModeInvoke,
} from "../lib/backend";
import type {
  ColmapBackend,
  CudaColmapFlavor,
  MapperBaMode,
  EffectiveSettings,
  EngineStatus,
  FfmpegHwAccel,
  BrushTrainingPreset,
  GsplatSplatCap,
  GsplatDensificationStrategy,
  TrainingBackend,
  PhotometricMode,
  FramePlan,
  PipelineEvent,
  PipelineResult,
  ProjectSummary,
  Quality,
  RunPhase,
  VideoInfo,
} from "../types/pipeline";
interface AppState {
  videoPath: string | null;
  projectsRoot: string;
  projects: ProjectSummary[];
  quality: Quality;
  autoBridgeFrames: boolean;
  video: VideoInfo | null;
  plan: FramePlan | null;
  engines: EngineStatus[];
  /** 当前生效的设置（含 COLMAP 后端）。UI 从这里读取与切换。 */
  settings: EffectiveSettings | null;
  phase: RunPhase;
  progress: number;
  progressMessage: string;
  latestEvent: PipelineEvent | null;
  events: PipelineEvent[];
  result: PipelineResult | null;
  error: string | null;
  /** 上次下载 CUDA COLMAP 的提示信息，独立于 error 流。 */
  settingsNotice: string | null;
  setVideoPath: (path: string | null) => void;
  setProjectsRoot: (path: string) => void;
  setProjects: (projects: ProjectSummary[]) => void;
  setQuality: (quality: Quality) => void;
  setAutoBridgeFrames: (enabled: boolean) => void;
  setAnalysis: (video: VideoInfo, plan: FramePlan) => void;
  setEngines: (engines: EngineStatus[]) => void;
  setSettings: (settings: EffectiveSettings) => void;
  setColmapBackend: (backend: ColmapBackend) => Promise<void>;
  setCudaColmapFlavor: (flavor: CudaColmapFlavor) => Promise<void>;
  setMapperBaMode: (mode: MapperBaMode) => Promise<void>;
  setFfmpegHwAccel: (mode: FfmpegHwAccel) => Promise<void>;
  setBrushTrainingPreset: (preset: BrushTrainingPreset) => Promise<void>;
  setGsplatSplatCap: (cap: GsplatSplatCap) => Promise<void>;
  setGsplatDensificationStrategy: (strategy: GsplatDensificationStrategy) => Promise<void>;
  setTrainingBackend: (backend: TrainingBackend) => Promise<void>;
  setPhotometricMode: (mode: PhotometricMode) => Promise<void>;
  downloadCudaColmap: () => Promise<void>;
  clearSettingsNotice: () => void;
  setPhase: (phase: RunPhase) => void;
  beginRun: () => void;
  receiveEvent: (event: PipelineEvent) => void;
  setResult: (result: PipelineResult | null) => void;
  setError: (error: string | null) => void;
}
export const useAppStore = create<AppState>((set, get) => ({
  videoPath: null,
  projectsRoot: "",
  projects: [],
  quality: "balanced",
  autoBridgeFrames: true,
  video: null,
  plan: null,
  engines: [],
  settings: null,
  phase: "idle",
  progress: 0,
  progressMessage: "",
  latestEvent: null,
  events: [],
  result: null,
  error: null,
  settingsNotice: null,
  setVideoPath: (videoPath) =>
    set({ videoPath, video: null, plan: null, result: null, error: null, progress: 0, phase: "idle" }),
  setProjectsRoot: (projectsRoot) => set({ projectsRoot }),
  setProjects: (projects) => set({ projects }),
  setQuality: (quality) => set({ quality, plan: null, result: null, error: null }),
  setAutoBridgeFrames: (autoBridgeFrames) => set({ autoBridgeFrames }),
  setAnalysis: (video, plan) => set({ video, plan }),
  setEngines: (engines) => set({ engines }),
  setSettings: (settings) => set({ settings }),
  setColmapBackend: async (backend) => {
    const { settings } = get();
    if (settings?.colmapBackend === backend) return;
    try {
      const next = await setColmapBackendInvoke(backend);
      set({
        settings: settings
          ? { ...settings, settings: next, colmapBackend: next.colmapBackend }
          : null,
        settingsNotice: `已切换到 ${backend === "cuda" ? "COLMAP CUDA" : "COLMAP CPU/no-CUDA"}。`,
      });
    } catch (error) {
      set({
        error: error instanceof Error ? error.message : String(error),
        settingsNotice:
          backend === "cuda"
            ? "切换失败：请先确认已下载 CUDA 版 COLMAP，且 GPU 驱动支持。"
            : "切换失败：COLMAP CPU 校验未通过。",
      });
    }
  },
  setFfmpegHwAccel: async (mode) => {
    const { settings } = get();
    if (settings?.settings.ffmpegHwAccel === mode) return;
    try {
      const next = await setFfmpegHwAccelInvoke(mode);
      set({
        settings: settings
          ? { ...settings, settings: next }
          : null,
        settingsNotice: `FFmpeg 抽帧硬件加速：${describeFfmpegHwAccel(next.ffmpegHwAccel)}。`,
      });
    } catch (error) {
      set({
        error: error instanceof Error ? error.message : String(error),
        settingsNotice: "切换 FFmpeg 硬件加速失败：未找到 ffmpeg.exe。",
      });
    }
  },
  setCudaColmapFlavor: async (flavor) => {
    const { settings } = get();
    if (!settings || settings.settings.cudaColmapFlavor === flavor) return;
    try {
      const next = await setCudaColmapFlavorInvoke(flavor);
      const label = flavor === "caspar" ? "CASPAR CUDA" : "官方 CUDA";
      set({ settings: { ...settings, settings: next }, settingsNotice: `COLMAP 引擎版本已切换为${label}。` });
    } catch (error) {
      set({
        error: error instanceof Error ? error.message : String(error),
        settingsNotice: flavor === "caspar" ? "CASPAR 引擎不可用，已保留原选择。" : "官方 CUDA COLMAP 校验未通过。",
      });
    }
  },
  setMapperBaMode: async (mode) => {
    const { settings } = get();
    if (!settings || settings.settings.mapperBaMode === mode) return;
    try {
      const next = await setMapperBaModeInvoke(mode);
      const label: Record<MapperBaMode, string> = { auto: "自动", ceres: "强制 Ceres", caspar: "强制 CASPAR" };
      set({ settings: { ...settings, settings: next }, settingsNotice: `Mapper BA 已切换为${label[mode]}。` });
    } catch (error) {
      set({ error: error instanceof Error ? error.message : String(error), settingsNotice: "切换 Mapper BA 失败。" });
    }
  },
  setBrushTrainingPreset: async (preset) => {
    const { settings } = get();
    if (!settings || settings.settings.brushTrainingPreset === preset) return;
    const next = await setBrushTrainingPresetInvoke(preset);
    set({ settings: { ...settings, settings: next }, settingsNotice: `Brush 训练预设已切换为 ${preset.toUpperCase()}。` });
  },
  setGsplatSplatCap: async (cap) => {
    const { settings } = get();
    if (!settings || settings.settings.gsplatSplatCap === cap) return;
    const next = await setGsplatSplatCapInvoke(cap);
    set({ settings: { ...settings, settings: next }, settingsNotice: `gsplat splat 上限已切换为 ${cap === "auto" ? "自动安全" : cap.toUpperCase()}。` });
  },
  setGsplatDensificationStrategy: async (strategy) => {
    const { settings } = get();
    if (!settings || settings.settings.gsplatDensificationStrategy === strategy) return;
    const next = await setGsplatDensificationStrategyInvoke(strategy);
    set({
      settings: { ...settings, settings: next },
      settingsNotice: strategy === "absgrad"
        ? "AbsGS 实验策略已开启；请固定素材、随机种子、质量档与 splat 上限进行对照。"
        : "gsplat 已恢复为已验证的 MCMC 默认策略。",
    });
  },
  setPhotometricMode: async (mode) => {
    const { settings } = get();
    if (!settings || settings.settings.photometricMode === mode) return;
    const next = await setPhotometricModeInvoke(mode);
    set({ settings: { ...settings, settings: next }, settingsNotice: mode === "ppisp" ? "PPISP 实验模式已开启；训练将使用单帧批次。" : mode === "wdr" ? "WD-R 实验已开启；训练使用单帧批次与 VGG-16 感知损失，耗时会明显增加。" : "附加训练模块已关闭，使用 M0 基线。" });
  },
  setTrainingBackend: async (backend) => {
    const { settings } = get();
    if (!settings || settings.settings.trainingBackend === backend) return;
    try {
      const next = await setTrainingBackendInvoke(backend);
      set({ settings: { ...settings, settings: next }, settingsNotice: `训练后端已切换为 ${backend === "brush" ? "Brush" : "gsplat CUDA"}。` });
    } catch (error) {
      set({ error: error instanceof Error ? error.message : String(error), settingsNotice: "gsplat CUDA 尚未通过运行时健康检查，已保留 Brush。" });
    }
  },
  downloadCudaColmap: async () => {
    set({ settingsNotice: "正在请求 CUDA COLMAP 状态…", error: null });
    try {
      const status = await downloadColmapCuda();
      set({
        settingsNotice: status.exists
          ? `CUDA COLMAP 已就绪：${status.version ?? status.path}`
          : "CUDA COLMAP 尚未在本地。请在终端运行 npm run download:colmap-cuda。",
      });
    } catch (error) {
      set({
        settingsNotice:
          error instanceof Error ? error.message : "无法下载 CUDA COLMAP，请稍后重试。",
      });
    }
  },
  clearSettingsNotice: () => set({ settingsNotice: null }),
  setPhase: (phase) => set({ phase }),
  beginRun: () =>
    set({
      phase: "running",
      progress: 0,
      progressMessage: "正在创建项目",
      latestEvent: null,
      events: [],
      result: null,
      error: null,
    }),
  receiveEvent: (event) =>
    set((state) => {
      if (state.latestEvent && event.sequence > 0 && event.sequence <= state.latestEvent.sequence) {
        return state;
      }
      const events = [...state.events, event].slice(-500);
      return {
        events,
        latestEvent: event,
        // The backend emits normalized progress in [0, 1]; the UI always
        // stores and renders task-wide progress in percentage points.
        progress: Math.max(state.progress, Math.min(100, event.progress * 100)),
        progressMessage: event.message,
      };
    }),
  setResult: (result) => set({ result, progress: result ? 100 : 0, progressMessage: result ? "任务完成" : "" }),
  setError: (error) => set({ error }),
}));

function describeFfmpegHwAccel(mode: FfmpegHwAccel): string {
  switch (mode) {
    case "off": return "关闭（CPU 软解码）";
    case "auto": return "自动";
    case "d3d11va": return "D3D11VA";
    case "cuda": return "CUDA / NVDEC";
  }
}
