import { describe, expect, it } from "vitest";
import type { Asset, AssetAnalysisView, ClipAnalysis, ClipTranscript } from "../types";
import {
  analysisNeedsAttention,
  analysisSummary,
  clockSummary,
  descriptionSummary,
  fileSummary,
  transcriptBadge,
  transcriptSummary,
} from "./summaries";

function asset(overrides: Partial<Asset> & { id: string }): Asset {
  return {
    projectId: "project_1",
    path: `/media/${overrides.id}.mp4`,
    filename: `${overrides.id}.mp4`,
    fingerprint: `fp_${overrides.id}`,
    sizeBytes: 2_100_000_000,
    durationSeconds: 107,
    frameRate: 59.94,
    width: 3840,
    height: 2160,
    codec: "hevc",
    camera: "Sony A7IV",
    capturedAtRaw: null,
    bestTime: null,
    offsetSeconds: 0,
    proxyPath: null,
    proxyRepresentation: null,
    metadataRotationDegrees: 0,
    defaultRotationDegrees: 0,
    rotationDegrees: 0,
    orientationProbed: true,
    originalAvailable: true,
    proxyAvailable: false,
    thumbnailPath: null,
    thumbnailQuality: 0,
    notes: "",
    title: "",
    description: "",
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
    recipeKey: "whisper-v1",
    sourceFingerprint: "fp",
    status: "speech",
    rejectReason: "",
    primaryLanguage: "en",
    speechSeconds: 4,
    windowCount: 2,
    segments: [
      { startMs: 4000, endMs: 6000, language: "en", text: "we finally found the place", meanTokenProbability: 0.9 },
      { startMs: 9000, endMs: 11000, language: "en", text: "it's like six seats total", meanTokenProbability: 0.9 },
    ],
    ...overrides,
  };
}

function analysis(overrides: Partial<ClipAnalysis> = {}): ClipAnalysis {
  return {
    assetId: "a",
    recipeKey: "vlm-v1",
    promptVersion: "1",
    provider: "openrouter",
    model: "gemini-2.5-flash",
    status: "ok",
    rejectReason: "",
    usable: true,
    unusableReason: null,
    bestMomentStartMs: 42_000,
    bestMomentEndMs: 46_000,
    thumbnailTimestampMs: null,
    cameraMovement: "handheld",
    ocrText: [],
    transcriptUsed: true,
    promptTokens: 10,
    completionTokens: 10,
    costUsd: 0.002,
    latencyMs: 900,
    attempts: 1,
    createdAt: "2026-06-03T10:00:00Z",
    ...overrides,
  };
}

function view(overrides: Partial<AssetAnalysisView> = {}): AssetAnalysisView {
  return { analysis: analysis(), aiLabels: [], current: true, ...overrides };
}

describe("transcriptSummary", () => {
  it("names the segment count and language when there is speech", () => {
    expect(transcriptSummary(transcript())).toBe("Speech · 2 segments · en");
  });

  it("says so plainly when there is no speech, which is half a real corpus", () => {
    expect(transcriptSummary(transcript({ status: "no_speech", segments: [] }))).toBe("No speech detected");
  });

  it("surfaces the reject reason when one exists", () => {
    expect(
      transcriptSummary(transcript({ status: "rejected", segments: [], rejectReason: "music only" })),
    ).toBe("Discarded as unreliable · music only");
  });

  it("reports a failure without inventing a reason", () => {
    expect(transcriptSummary(transcript({ status: "failed", segments: [], rejectReason: "" }))).toBe(
      "Transcription failed",
    );
  });

  it("says nothing has run when there is no transcript at all", () => {
    expect(transcriptSummary(null)).toBe("Not transcribed");
  });

  it("uses the singular for one segment", () => {
    expect(
      transcriptSummary(
        transcript({
          segments: [{ startMs: 0, endMs: 1, language: "en", text: "hi", meanTokenProbability: 0.9 }],
        }),
      ),
    ).toBe("Speech · 1 segment · en");
  });

  it("omits an unknown language rather than printing a placeholder", () => {
    expect(transcriptSummary(transcript({ primaryLanguage: "" }))).toBe("Speech · 2 segments");
  });
});

describe("transcriptBadge", () => {
  it("is the segment count when there are segments, and null otherwise", () => {
    expect(transcriptBadge(transcript())).toBe(2);
    expect(transcriptBadge(transcript({ segments: [] }))).toBeNull();
    expect(transcriptBadge(null)).toBeNull();
  });
});

describe("analysisSummary", () => {
  it("leads with the verdict and names the camera movement", () => {
    expect(analysisSummary(view())).toBe("Usable · handheld");
  });

  it("names the unusable reason, which is an enum rather than free text", () => {
    expect(analysisSummary(view({ analysis: analysis({ usable: false, unusableReason: "blurry" }) }))).toBe(
      "Unusable · blurry",
    );
  });

  it("falls back gracefully when a clip is unusable for no recorded reason", () => {
    expect(analysisSummary(view({ analysis: analysis({ usable: false, unusableReason: null }) }))).toBe(
      "Unusable",
    );
  });

  it("says the clip has not been analyzed when there is no observation", () => {
    expect(analysisSummary(null)).toBe("Not analyzed");
  });

  it("marks a stale observation, since the proxy changed underneath it", () => {
    expect(analysisSummary(view({ current: false }))).toBe("Usable · handheld · stale");
  });
});

describe("analysisNeedsAttention", () => {
  it("is false for a current, usable observation", () => {
    expect(analysisNeedsAttention(view())).toBe(false);
  });

  it("is true when the clip is flagged unusable", () => {
    expect(analysisNeedsAttention(view({ analysis: analysis({ usable: false, unusableReason: "black" }) }))).toBe(true);
  });

  it("is true when the observation is stale", () => {
    expect(analysisNeedsAttention(view({ current: false }))).toBe(true);
  });

  it("is false when nothing has been analyzed, which is not a problem to flag", () => {
    expect(analysisNeedsAttention(null)).toBe(false);
  });
});

describe("fileSummary", () => {
  it("reads camera, codec and size", () => {
    expect(fileSummary(asset({ id: "a" }))).toBe("Sony A7IV · HEVC · 2.1 GB");
  });

  it("uses megabytes below a gigabyte", () => {
    expect(fileSummary(asset({ id: "a", sizeBytes: 420_000_000 }))).toBe("Sony A7IV · HEVC · 420 MB");
  });
});

describe("descriptionSummary", () => {
  it("is the description's first line, trimmed", () => {
    expect(descriptionSummary(asset({ id: "a", description: "  Wide shot of the harbour.\nMore. " }))).toBe(
      "Wide shot of the harbour.",
    );
  });

  it("says the field is empty rather than rendering nothing", () => {
    expect(descriptionSummary(asset({ id: "a", description: "   " }))).toBe("No description");
  });
});

describe("clockSummary", () => {
  it("reports no offset when the clock is untouched", () => {
    expect(clockSummary(asset({ id: "a", offsetSeconds: 0 }))).toBe("No offset");
  });

  it("signs the offset in minutes", () => {
    expect(clockSummary(asset({ id: "a", offsetSeconds: 720 }))).toBe("+12 min");
    expect(clockSummary(asset({ id: "a", offsetSeconds: -3000 }))).toBe("−50 min");
  });

  it("keeps a sub-minute offset visible instead of rounding it to nothing", () => {
    expect(clockSummary(asset({ id: "a", offsetSeconds: 30 }))).toBe("+30 sec");
    expect(clockSummary(asset({ id: "a", offsetSeconds: -5 }))).toBe("−5 sec");
  });
});
