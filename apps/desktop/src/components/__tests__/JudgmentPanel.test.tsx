import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { Asset, AssetAnalysisView, ClipAnalysis } from "../../types";
import { JudgmentPanel, type JudgmentPanelProps } from "../JudgmentPanel";

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
    capturedAtRaw: "2026-02-08T09:14:08Z",
    bestTime: "2026-02-08T09:14:08Z",
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
    bestMomentStartMs: 4000,
    bestMomentEndMs: 6000,
    thumbnailTimestampMs: null,
    cameraMovement: "pan",
    ocrText: [],
    transcriptUsed: true,
    promptTokens: 1,
    completionTokens: 1,
    costUsd: 0.001,
    latencyMs: 100,
    attempts: 1,
    createdAt: "2026-06-03T10:00:00Z",
    ...overrides,
  };
}

function view(overrides: Partial<AssetAnalysisView> = {}): AssetAnalysisView {
  return { analysis: analysis(), aiLabels: [], current: true, ...overrides };
}

function renderPanel(overrides: Partial<JudgmentPanelProps> = {}) {
  const props: JudgmentPanelProps = {
    asset: asset({ id: "a", title: "Ramen alley night walk", labels: ["ramen", "night"] }),
    index: 118,
    observation: null as AssetAnalysisView | null,
    labelDraft: "",
    saveState: "idle" as const,
    onLabelDraftChange: vi.fn(),
    onAddLabel: vi.fn(),
    onRemoveLabel: vi.fn(),
    onPromoteAiLabel: vi.fn(),
    onChangeAsset: vi.fn(),
    onCommitAsset: vi.fn(),
    onToggleRemoved: vi.fn(),
    onNotesKeyDown: vi.fn(),
    onNotesFocus: vi.fn(),
    onNotesBlur: vi.fn(),
    ...overrides,
  };
  return { props, ...render(<JudgmentPanel {...props} />) };
}

/**
 * Rule 1 is the reason this panel exists in its own file. A priority ordering
 * decays the moment someone adds a field -- that is exactly how the old
 * inspector pushed notes 81px below the fold. A categorical rule can be
 * tested, so it is.
 */
describe("rule 1: the rail holds human-authored content only", () => {
  it("renders no transcript, description, or source-metadata surface", () => {
    const { container } = renderPanel({ observation: view() });
    expect(container.querySelector(".transcript-section")).toBeNull();
    expect(container.querySelector(".observation-section")).toBeNull();
    expect(container.querySelector(".metadata-grid")).toBeNull();
    expect(container.querySelector(".calibration-section")).toBeNull();
    expect(container.querySelector(".description-field")).toBeNull();
    expect(screen.queryByText(/raw time|best time|clock event|transcript|camera movement/i)).toBeNull();
  });

  it("permits the AI suggestion strip, because promoting one is authoring", async () => {
    const { props, container } = renderPanel({
      observation: view({ aiLabels: ["alley", "neon"] }),
    });
    expect(container.querySelectorAll(".ai-suggestion")).toHaveLength(2);
    await userEvent.click(screen.getByRole("button", { name: /alley/ }));
    expect(props.onPromoteAiLabel).toHaveBeenCalledWith("alley");
  });

  it("permits the unusable chip, because it prompts the decision the rail captures", () => {
    const { container } = renderPanel({
      observation: view({ analysis: analysis({ usable: false, unusableReason: "blurry" }) }),
    });
    expect(container.querySelector(".unusable-chip")).not.toBeNull();
  });

  it("shows no unusable chip for a usable clip", () => {
    const { container } = renderPanel({ observation: view() });
    expect(container.querySelector(".unusable-chip")).toBeNull();
  });

  it("drops a suggestion once it has been promoted, rather than duplicating the chip", () => {
    const { container } = renderPanel({
      asset: asset({ id: "a", labels: ["laptop", "seat"] }),
      observation: view({ aiLabels: ["laptop", "seat", "train interior"] }),
    });
    const remaining = [...container.querySelectorAll(".ai-suggestion")].map((n) => n.textContent);
    expect(remaining).toEqual(["train interior"]);
  });

  it("matches a promoted suggestion case-insensitively, as label entry does", () => {
    const { container } = renderPanel({
      asset: asset({ id: "a", labels: ["Laptop"] }),
      observation: view({ aiLabels: ["laptop"] }),
    });
    expect(container.querySelectorAll(".ai-suggestion")).toHaveLength(0);
  });
});

/** Rule 3: fixed geometry. Nothing may move or resize the notes field. */
describe("rule 3: notes geometry", () => {
  it("puts notes before labels in DOM order", () => {
    const { container } = renderPanel();
    const ids = [...container.querySelectorAll("#notes, #label-input")].map((node) => node.id);
    expect(ids).toEqual(["notes", "label-input"]);
  });

  it("gives the notes textarea a fixed class rather than a flexible inline size", () => {
    const { container } = renderPanel();
    const notes = container.querySelector<HTMLTextAreaElement>("#notes")!;
    expect(notes).toHaveClass("notes-field");
    expect(notes.style.flex).toBe("");
    expect(notes.style.height).toBe("");
  });

  it("keeps the notes field in the same position whether or not there are suggestions", () => {
    const without = renderPanel().container.querySelector(".judgment-panel")!;
    const withAi = renderPanel({ observation: view({ aiLabels: ["a", "b", "c"] }) }).container
      .querySelector(".judgment-panel")!;
    const indexOfNotes = (root: Element) =>
      [...root.children].findIndex((child) => child.querySelector("#notes"));
    expect(indexOfNotes(withAi)).toBe(indexOfNotes(without));
  });
});

describe("label entry", () => {
  it("has no separate Add button, because Enter and comma already commit", () => {
    renderPanel();
    expect(screen.queryByRole("button", { name: /^add$/i })).toBeNull();
  });

  it("commits the draft on Enter", async () => {
    const { props } = renderPanel({ labelDraft: "osaka" });
    await userEvent.type(screen.getByLabelText(/add label/i), "{Enter}");
    expect(props.onAddLabel).toHaveBeenCalledWith("osaka");
  });

  it("commits the draft on comma", async () => {
    const { props } = renderPanel({ labelDraft: "osaka" });
    await userEvent.type(screen.getByLabelText(/add label/i), ",");
    expect(props.onAddLabel).toHaveBeenCalledWith("osaka");
  });

  it("removes the last chip on Backspace into an empty field", async () => {
    const { props } = renderPanel({ labelDraft: "" });
    await userEvent.type(screen.getByLabelText(/add label/i), "{Backspace}");
    expect(props.onRemoveLabel).toHaveBeenCalledWith("night");
  });

  it("renders one removable chip per human label", async () => {
    const { props, container } = renderPanel();
    expect(container.querySelectorAll(".chip-well button")).toHaveLength(2);
    await userEvent.click(screen.getByRole("button", { name: /remove ramen/i }));
    expect(props.onRemoveLabel).toHaveBeenCalledWith("ramen");
  });
});

describe("removal", () => {
  it("offers removal from the header rather than a dedicated section", async () => {
    const { props, container } = renderPanel();
    expect(container.querySelector(".remove-section")).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: /remove from labeling/i }));
    expect(props.onToggleRemoved).toHaveBeenCalled();
  });

  it("offers restore instead when the clip is already removed", () => {
    renderPanel({ asset: asset({ id: "a", removed: true }) });
    expect(screen.getByRole("button", { name: /restore clip/i })).toBeInTheDocument();
  });
});

describe("save state", () => {
  it("reports saving without offering a save button", () => {
    renderPanel({ saveState: "saving" });
    expect(screen.getByText(/saving/i)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^save$/i })).toBeNull();
  });

  it("marks a draft the backend refused, and says why on the retry", async () => {
    const user = userEvent.setup();
    const { props } = renderPanel({
      saveState: "unsaved",
      saveError: "database is locked",
      onRetrySave: vi.fn(),
    });

    // An unmarked draft reads exactly like saved text; this is the difference.
    expect(screen.getByText("Unsaved")).toBeInTheDocument();
    const retry = screen.getByRole("button", { name: "Retry" });
    expect(retry).toHaveAttribute("title", "Not saved: database is locked");

    await user.click(retry);
    expect(props.onRetrySave).toHaveBeenCalledTimes(1);
  });

  it("offers no retry while nothing is unsaved", () => {
    renderPanel({ saveState: "saved", onRetrySave: vi.fn() });
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
  });
});
