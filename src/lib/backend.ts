import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { confirm, open, save } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import type {
  AppSettingsLike,
  ColmapBackend,
  CudaColmapFlavor,
  MapperBaMode,
  EffectiveSettings,
  EngineStatus,
  FfmpegHwAccel,
  BrushTrainingPreset,
  GsplatSplatCap,
  TrainingBackend,
  PhotometricMode,
  FramePlan,
  PipelineEvent,
  PipelineResult,
  ProjectOverview,
  ProjectSummary,
  Quality,
  VideoInfo,
} from "../types/pipeline";

const inTauri = () => "__TAURI_INTERNALS__" in window;

export async function selectVideo(): Promise<string | null> {
  if (!inTauri()) return null;
  const selected = await open({
    multiple: false,
    directory: false,
    filters: [{ name: "视频", extensions: ["mp4", "mov"] }],
  });
  return typeof selected === "string" ? selected : null;
}

export async function selectProjectsRoot(current: string): Promise<string | null> {
  if (!inTauri()) return null;
  const selected = await open({ multiple: false, directory: true, defaultPath: current || undefined });
  return typeof selected === "string" ? selected : null;
}

export async function checkEngines(): Promise<EngineStatus[]> {
  return inTauri() ? invoke("check_engines") : [];
}

export async function getSettings(): Promise<EffectiveSettings | null> {
  if (!inTauri()) return null;
  return invoke("get_settings") as Promise<EffectiveSettings>;
}

export async function setColmapBackend(backend: ColmapBackend): Promise<AppSettingsLike> {
  return invoke("set_colmap_backend", { backend }) as Promise<AppSettingsLike>;
}

export async function setFfmpegHwAccel(mode: FfmpegHwAccel): Promise<AppSettingsLike> {
  return invoke("set_ffmpeg_hw_accel", { mode }) as Promise<AppSettingsLike>;
}
export async function setCudaColmapFlavor(flavor: CudaColmapFlavor): Promise<AppSettingsLike> {
  return invoke("set_cuda_colmap_flavor", { flavor }) as Promise<AppSettingsLike>;
}
export async function setMapperBaMode(mode: MapperBaMode): Promise<AppSettingsLike> {
  return invoke("set_mapper_ba_mode", { mode }) as Promise<AppSettingsLike>;
}
export async function setBrushTrainingPreset(preset: BrushTrainingPreset): Promise<AppSettingsLike> {
  return invoke("set_brush_training_preset", { preset }) as Promise<AppSettingsLike>;
}
export async function setTrainingBackend(backend: TrainingBackend): Promise<AppSettingsLike> {
  return invoke("set_training_backend", { backend }) as Promise<AppSettingsLike>;
}
export async function setGsplatSplatCap(cap: GsplatSplatCap): Promise<AppSettingsLike> {
  return invoke("set_gsplat_splat_cap", { cap }) as Promise<AppSettingsLike>;
}
export async function setPhotometricMode(mode: PhotometricMode): Promise<AppSettingsLike> {
  return invoke("set_photometric_mode", { mode }) as Promise<AppSettingsLike>;
}

/**
 * 当切换到 CUDA 但本地还没有 CUDA COLMAP 时调用。
 * 当前实现只回显 Rust 端的提示；真正下载需要执行 `npm run download:colmap-cuda`，
 * 因为 411 MB 的资产不适合在 Tauri 主线程里直接拉取。
 */
export async function downloadColmapCuda(): Promise<EngineStatus> {
  return invoke("download_colmap_cuda") as Promise<EngineStatus>;
}

export async function probeAndPlan(
  path: string,
  quality: Quality,
): Promise<{ video: VideoInfo; plan: FramePlan }> {
  return invoke("probe_and_plan", { path, quality });
}

export async function getProjectOverview(): Promise<ProjectOverview> {
  return invoke("get_project_overview");
}

export async function setProjectsRoot(
  projectsRoot: string,
): Promise<{ projectsRoot: string; colmapBackend: ColmapBackend }> {
  return invoke("set_projects_root", { projectsRoot });
}

export async function startPipeline(
  path: string,
  quality: Quality,
  projectsRoot: string,
  autoBridgeFrames: boolean,
): Promise<PipelineResult> {
  return invoke("start_pipeline", { path, quality, projectsRoot, autoBridgeFrames });
}

export async function cancelPipeline(): Promise<void> {
  return invoke("cancel_pipeline");
}

export async function onPipelineEvent(
  handler: (event: PipelineEvent) => void,
): Promise<UnlistenFn> {
  return listen<PipelineEvent>("pipeline-event", ({ payload }) => handler(payload));
}

export async function revealProject(project: ProjectSummary): Promise<void> {
  await revealItemInDir(project.finalPly ?? project.projectPath);
}

/**
 * 通过随应用分发的 Brush 引擎打开 final.ply 的内置 3D 查看器。
 * 前置校验：必须存在 final.ply，且对应项目根目录必须已注册到项目索引。
 */
export async function openProjectViewer(project: ProjectSummary): Promise<void> {
  if (!project.finalPly) {
    throw new Error("这个项目还没有可查看的 final.ply");
  }
  if (!inTauri()) {
    throw new Error("当前环境不在 Tauri 桌面壳内，无法打开 Brush 查看器。");
  }
  await invoke("open_project_viewer", { sourcePath: project.finalPly });
}

export async function confirmAndDeleteProject(project: ProjectSummary): Promise<boolean> {
  const accepted = await confirm(
    `将"${project.name}"及其中的源视频、抽帧、COLMAP、Brush 和日志全部移入回收站；若回收站不可用（例如所在分区不支持回收站），将直接永久删除。\n\n此操作无法在应用内撤销。`,
    {
      title: "删除项目",
      kind: "warning",
      okLabel: "移入回收站",
      cancelLabel: "取消",
    },
  );
  if (!accepted) return false;
  await invoke("delete_project", { projectId: project.id });
  return true;
}

export async function exportPly(result: PipelineResult): Promise<string | null> {
  const destination = await save({ defaultPath: "final.ply", filters: [{ name: "Gaussian Splat PLY", extensions: ["ply"] }] });
  if (!destination) return null;
  await invoke("export_ply", { sourcePath: result.finalPly, destinationPath: destination });
  return destination;
}
