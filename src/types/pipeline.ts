export type Quality = "fast" | "balanced" | "high";
export type EngineKind = "ffmpeg" | "ffprobe" | "colmap" | "brush";
export type RunPhase = "idle" | "analyzing" | "running" | "completed" | "failed" | "cancelled" | "needsSupplement";
export type ProjectStatus = "running" | "completed" | "failed" | "cancelled" | "interrupted" | "needsSupplement";
export type ColmapBackend = "cpu" | "cuda";
/** CUDA distributions are side-by-side; selecting CASPAR never replaces official CUDA COLMAP. */
export type CudaColmapFlavor = "official" | "caspar";
export type MapperBaMode = "auto" | "ceres" | "caspar";
/** FFmpeg hardware-decoding mode used during uniform frame extraction.
 * `off`     – legacy CPU-only path (no extra flags).
 * `auto`    – let FFmpeg pick the best available runtime.
 * `d3d11va` – lock the decode path to Direct3D 11 video acceleration.
 * `cuda`    – lock the decode path to NVIDIA's hardware decoder (NVDEC). */
export type FfmpegHwAccel = "off" | "auto" | "d3d11va" | "cuda";
export type BrushTrainingPreset = "a" | "b" | "c";
export type GsplatSplatCap = "auto" | "1m" | "2m" | "4m";
export type TrainingBackend = "brush" | "gsplat";
export type InputSource = "video" | "splatcam";
export type PhotometricMode = "none" | "ppisp";
export interface EngineStatus {
  kind: EngineKind;
  path: string;
  exists: boolean;
  canStart: boolean;
  version: string | null;
  cpuOnly: boolean | null;
  detail: string;
  /** 仅 COLMAP 条目有意义；其他引擎始终为 null。 */
  cudaAvailable: boolean | null;
  casparAvailable: boolean | null;
}
export interface AppSettingsLike {
  schemaVersion: number;
  projectsRoot: string;
  colmapBackend: ColmapBackend;
  cudaColmapFlavor: CudaColmapFlavor;
  mapperBaMode: MapperBaMode;
  ffmpegHwAccel: FfmpegHwAccel;
  brushTrainingPreset: BrushTrainingPreset;
  trainingBackend: TrainingBackend;
  gsplatSplatCap: GsplatSplatCap;
  photometricMode: PhotometricMode;
}
export interface EffectiveSettings {
  settings: AppSettingsLike;
  projectsRoot: string;
  colmapBackend: ColmapBackend;
  cpuColmap: EngineStatus | null;
  cudaColmap: EngineStatus | null;
  casparColmap: EngineStatus | null;
  gsplatAvailable: boolean;
}
export interface VideoInfo {
  duration: number;
  width: number;
  height: number;
  fps: number;
  totalFrames: number;
  codec: string;
  rotation: number;
}
export interface FramePlan {
  retentionRatio: number;
  samplingFps: number;
  estimatedFrames: number;
}
export interface PipelineEvent {
  sequence: number;
  timestamp: string;
  kind: "stage" | "progress" | "log" | "heartbeat";
  level: "info" | "warning" | "error";
  stage: string;
  engine: "system" | "ffmpeg" | "colmap" | "brush" | null;
  progress: number;
  stageProgress: number | null;
  indeterminate: boolean;
  message: string;
  current: number | null;
  total: number | null;
  unit: string | null;
  elapsedMs: number;
}
export interface PipelineResult {
  projectId: string;
  projectPath: string;
  finalPly: string;
  fileSize: number;
  splatCount: number;
  inputImages: number;
  registeredImages: number;
  registeredRatio: number;
  points3d: number;
  durationMs: number;
  completedAt: string;
  warning: string | null;
  logsDirectory: string;
  colmapBackend: ColmapBackend;
}
export interface ProjectSummary {
  id: string;
  name: string;
  status: ProjectStatus;
  projectPath: string;
  finalPly: string | null;
  fileSize: number | null;
  splatCount: number | null;
  createdAt: string;
  completedAt: string | null;
  durationMs: number | null;
  quality: Quality;
  inputSource: InputSource;
  trainingBackend: TrainingBackend;
  brushTrainingPreset: BrushTrainingPreset;
  gsplatSplatCap: GsplatSplatCap;
  sourceName: string;
  registeredRatio: number | null;
  points3d: number | null;
  failureMessage: string | null;
}
export interface ProjectOverview {
  projectsRoot: string;
  colmapBackend: ColmapBackend;
  projects: ProjectSummary[];
}
export interface SplatcamImportReport {
  sourcePath: string;
  coordinateConvention: "colmap-world-to-camera";
  hasDepth: boolean;
  hasTransforms: boolean;
  imageCount: number;
  cameraCount: number;
  poseCount: number;
  pointCount: number;
  pointsHaveObservationTracks: boolean;
  positiveDepthProjectionRatio: number;
  inImageProjectionRatio: number;
  cameraTrajectoryExtent: number;
  geometryGate: { passed: boolean; reason: string | null };
}
