import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { Asset, AssetAnalysisView, ClipAnalysis, ClipTranscript } from "../../types";
import { EvidenceShelf, type EvidenceShelfProps } from "../EvidenceShelf";

function asset(overrides: Partial<Asset> & { id: string }): Asset {
  return {
    projectId: "project_1",
    path: "/media/a.mp4",
    filename: "C0163.MP4",
    fingerprint: "fp",
    sizeBytes: 2_100_000_000,
    durationSeconds: 107,
    frameRate: 59.94,
    width: 3840,
    height: 2160,
    codec: "hevc",
    camera: "Sony A7IV",
    capturedAtRaw: "2026-02-08T09:14:08Z",
    bestTime: "2026-02-08T08:24:08Z",
    offsetSeconds: -3000,
    proxyPath: null,
    proxyRepresentation: null,
    metadataRotationDegrees: 0,
    defaultRotationDegrees: 0,
    rotationDegrees: 0,
    orientationProbed: true,
    originalAvailable: true,
    proxyAvailable: true,
    thumbnailPath: null,
    thumbnailQuality: 0,
    notes: "",
    title: "",
    description: "A local train slows along the platform.",
    labels: [],
    removed: false,
    mediaObjectId: null,
    mediaVerificationState: "unlinked",
    available: true,
    ...overrides,
  } as Asset;
}

function transcript(overrides: Partial<ClipTranscript> = {}): ClipTranscript {
  return {
    assetId: "a",
    recipeKey: "r",
    sourceFingerprint: "f",
    status: "speech",
    rejectReason: "",
    primaryLanguage: "ja",
    speechSeconds: 4,
    windowCount: 2,
    segments: [
      { startMs: 4000, endMs: 6000, language: "ja", text: "we finally found the place", meanTokenProbability: 0.9 },
      { startMs: 9000, endMs: 11000, language: "ja", text: "it's like six seats total", meanTokenProbability: 0.9 },
    ],
    ...overrides,
  };
}

function analysis(overrides: Partial<ClipAnalysis> = {}): ClipAnalysis {
  return {
    assetId: "a", recipeKey: "r", promptVersion: "1", provider: "openrouter",
    model: "gemini-2.5-flash", status: "ok", rejectReason: "", usable: true,
    unusableReason: null, bestMomentStartMs: 4000, bestMomentEndMs: 6000,
    thumbnailTimestampMs: null, cameraMovement: "pan", ocrText: [], transcriptUsed: true,
    promptTokens: 1, completionTokens: 1, costUsd: 0.001, latencyMs: 1, attempts: 1,
    createdAt: "2026-06-03T10:00:00Z",
    ...overrides,
  };
}

function view(overrides: Partial<AssetAnalysisView> = {}): AssetAnalysisView {
  return { analysis: analysis(), aiLabels: [], current: true, ...overrides };
}

function renderShelf(overrides: Partial<EvidenceShelfProps> = {}) {
  const props: EvidenceShelfProps = {
    asset: asset({ id: "a" }),
    transcript: transcript(),
    observation: view(),
    open: true,
    tab: "transcript" as const,
    onTabChange: vi.fn(),
    onToggleOpen: vi.fn(),
    onSeek: vi.fn(),
    onChangeAsset: vi.fn(),
    onCommitAsset: vi.fn(),
    onLocateOriginals: vi.fn(),
    busy: false,
    ...overrides,
  };
  return { props, ...render(<EvidenceShelf {...props} />) };
}

describe("collapsed state", () => {
  it("keeps the tab strip visible with no pane content, so nothing is hidden", () => {
    const { container } = renderShelf({ open: false });
    expect(container.querySelectorAll(".shelf-tab")).toHaveLength(4);
    expect(container.querySelector(".shelf-pane")).toBeNull();
  });

  it("badges the transcript tab with its segment count", () => {
    const { container } = renderShelf({ open: false });
    expect(container.querySelector(".shelf-badge")).toHaveTextContent("2");
  });

  it("omits the badge when there are no segments", () => {
    const { container } = renderShelf({ open: false, transcript: transcript({ segments: [] }) });
    expect(container.querySelector(".shelf-badge")).toBeNull();
  });

  it("marks the analysis tab when the clip is unusable", () => {
    const { container } = renderShelf({
      open: false,
      observation: view({ analysis: analysis({ usable: false, unusableReason: "blurry" }) }),
    });
    expect(container.querySelector('.shelf-tab[data-attention="true"]')).toHaveTextContent("Analysis");
  });

  it("marks the analysis tab when the observation is stale", () => {
    const { container } = renderShelf({ open: false, observation: view({ current: false }) });
    expect(container.querySelector('.shelf-tab[data-attention="true"]')).toHaveTextContent("Analysis");
  });

  it("leaves the analysis tab unmarked when the verdict is good and current", () => {
    const { container } = renderShelf({ open: false });
    expect(container.querySelector('.shelf-tab[data-attention="true"]')).toBeNull();
  });
});

describe("transcript pane", () => {
  it("seeks to a segment's start when its row is clicked", async () => {
    const { props } = renderShelf();
    await userEvent.click(screen.getByText("we finally found the place"));
    expect(props.onSeek).toHaveBeenCalledWith(4);
  });

  it("renders one row per segment", () => {
    const { container } = renderShelf();
    expect(container.querySelectorAll(".segment")).toHaveLength(2);
  });

  it("states the reason rather than rendering an empty list when there is no speech", () => {
    const { container } = renderShelf({ transcript: transcript({ status: "no_speech", segments: [] }) });
    expect(container.querySelector(".segment")).toBeNull();
    expect(screen.getByText(/no speech in this clip/i)).toBeInTheDocument();
  });

  it("says the clip was never transcribed when there is no transcript at all", () => {
    renderShelf({ transcript: null });
    expect(screen.getByText(/has not been transcribed/i)).toBeInTheDocument();
  });
});

describe("file pane", () => {
  it("shows the camera clock and the corrected time as a pair", () => {
    renderShelf({ tab: "file" });
    expect(screen.getByText("Camera clock")).toBeInTheDocument();
    expect(screen.getByText("Corrected")).toBeInTheDocument();
  });

  it("reports proxy and original availability", () => {
    renderShelf({ tab: "file" });
    expect(screen.getByText("720p present")).toBeInTheDocument();
    expect(screen.getByText("mounted")).toBeInTheDocument();
  });
});

describe("analysis pane", () => {
  it("names the unusable reason", () => {
    renderShelf({
      tab: "analysis",
      observation: view({ analysis: analysis({ usable: false, unusableReason: "blurry" }) }),
    });
    expect(screen.getByText(/unusable — blurry/i)).toBeInTheDocument();
  });

  it("says so when nothing has been analyzed", () => {
    renderShelf({ tab: "analysis", observation: null });
    expect(screen.getByText(/has not been analyzed/i)).toBeInTheDocument();
  });
});

describe("tab behaviour", () => {
  it("reports a tab change without owning the state", async () => {
    const { props } = renderShelf();
    await userEvent.click(screen.getByRole("tab", { name: /file/i }));
    expect(props.onTabChange).toHaveBeenCalledWith("file");
  });

  it("collapses when the already-active tab is clicked again", async () => {
    const { props } = renderShelf({ tab: "transcript", open: true });
    await userEvent.click(screen.getByRole("tab", { name: /transcript/i }));
    expect(props.onToggleOpen).toHaveBeenCalled();
  });

  it("opens when a tab is clicked while collapsed", async () => {
    const { props } = renderShelf({ open: false });
    await userEvent.click(screen.getByRole("tab", { name: /analysis/i }));
    expect(props.onTabChange).toHaveBeenCalledWith("analysis");
    expect(props.onToggleOpen).toHaveBeenCalled();
  });
});
