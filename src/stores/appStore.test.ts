import { beforeEach, describe, expect, it } from "vitest";
import { useAppStore } from "./appStore";
import type { PipelineEvent } from "../types/pipeline";

const event = (sequence: number, progress: number): PipelineEvent => ({
  sequence,
  timestamp: new Date().toISOString(),
  kind: "progress",
  level: "info",
  stage: "ExtractingFrames",
  engine: "ffmpeg",
  progress,
  stageProgress: progress,
  indeterminate: false,
  message: `event ${sequence}`,
  current: sequence,
  total: 500,
  unit: "张",
  elapsedMs: sequence * 100,
});

describe("app store", () => {
  beforeEach(() => {
    useAppStore.setState({
      videoPath: null, projectsRoot: "", projects: [], quality: "balanced", video: null,
      plan: null, engines: [], phase: "idle", progress: 0, progressMessage: "",
      latestEvent: null, events: [], result: null, error: null,
    });
  });

  it("uses Balanced by default", () => {
    expect(useAppStore.getState().quality).toBe("balanced");
  });

  it("invalidates a plan when quality changes", () => {
    useAppStore.setState({ plan: { retentionRatio: 0.5, samplingFps: 15, estimatedFrames: 900 } });
    useAppStore.getState().setQuality("high");
    expect(useAppStore.getState().quality).toBe("high");
    expect(useAppStore.getState().plan).toBeNull();
  });

  it("keeps progress monotonic and ignores stale sequenced events", () => {
    useAppStore.getState().receiveEvent(event(2, 42));
    useAppStore.getState().receiveEvent(event(1, 12));
    expect(useAppStore.getState().progress).toBe(42);
    expect(useAppStore.getState().events).toHaveLength(1);
  });

  it("caps the friendly live log at 500 entries", () => {
    for (let index = 1; index <= 530; index += 1) useAppStore.getState().receiveEvent(event(index, index / 10));
    expect(useAppStore.getState().events).toHaveLength(500);
    expect(useAppStore.getState().events[0].sequence).toBe(31);
  });
});
