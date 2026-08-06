import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createRef } from "react";
import { describe, expect, it, vi } from "vitest";
import type { Asset, AssetAnalysisIndexEntry } from "../../types";
import { QueuePanel, type QueuePanelProps } from "../QueuePanel";

function asset(overrides: Partial<Asset> = {}): Asset {
  return {
    id: "a", projectId: "p", path: "/m/a.mp4", filename: "C0001.MP4", fingerprint: "a",
    sizeBytes: 1_000_000, durationSeconds: 75, frameRate: 29.97, width: 1920, height: 1080,
    codec: "h264", camera: "Sony", capturedAtRaw: null, bestTime: null, offsetSeconds: 0,
    proxyPath: null, proxyRepresentation: null, metadataRotationDegrees: 0,
    defaultRotationDegrees: 0, rotationDegrees: 0, orientationProbed: true,
    originalAvailable: true, proxyAvailable: false, thumbnailPath: null, thumbnailQuality: 0,
    notes: "", title: "", description: "", labels: [], removed: false,
    mediaObjectId: null, mediaVerificationState: "unlinked", available: true,
    ...overrides,
  } as unknown as Asset;
}

function renderPanel(overrides: Partial<QueuePanelProps> = {}) {
  const props: QueuePanelProps = {
    projectName: "Kyoto autumn",
    assets: [asset()],
    selectedId: "",
    query: "",
    analysisIndex: {},
    showRemoved: false,
    showUnusableOnly: false,
    unusableCount: 0,
    removedCount: 0,
    listRef: createRef<HTMLDivElement>(),
    onQueryChange: vi.fn(),
    onSelect: vi.fn(),
    onToggleRemovedView: vi.fn(),
    onToggleUnusableView: vi.fn(),
    ...overrides,
  };
  return { props, ...render(<QueuePanel {...props} />) };
}

describe("the clip list", () => {
  it("numbers rows by queue position, not by asset id", () => {
    const { container } = renderPanel({
      assets: [asset({ id: "a" }), asset({ id: "b" }), asset({ id: "c" })],
    });
    expect([...container.querySelectorAll(".queue-index")].map((node) => node.textContent))
      .toEqual(["001", "002", "003"]);
  });

  it("headlines the analysis title once there is one, keeping the filename below", () => {
    renderPanel({ assets: [asset({ title: "Ramen alley night walk" })] });
    expect(screen.getByText("Ramen alley night walk")).toBeTruthy();
    expect(screen.getByText(/C0001\.MP4 · Sony/)).toBeTruthy();
  });

  it("headlines the filename while the clip is unnamed, without repeating it", () => {
    renderPanel({ assets: [asset({ title: "" })] });
    expect(screen.getByText("C0001.MP4")).toBeTruthy();
    expect(screen.queryByText(/C0001\.MP4 · Sony/)).toBeNull();
    expect(screen.getByText("Sony")).toBeTruthy();
  });

  it("reports the selected row to assistive tech", () => {
    renderPanel({ assets: [asset({ id: "a" }), asset({ id: "b" })], selectedId: "b" });
    const options = screen.getAllByRole("option");
    expect(options[0].getAttribute("aria-selected")).toBe("false");
    expect(options[1].getAttribute("aria-selected")).toBe("true");
  });

  it("selects the clip that was clicked", async () => {
    const { props } = renderPanel({ assets: [asset({ id: "a" }), asset({ id: "b" })] });
    await userEvent.click(screen.getAllByRole("option")[1]);
    expect(props.onSelect).toHaveBeenCalledWith("b");
  });

  it("shows a duration and a label count per row", () => {
    renderPanel({ assets: [asset({ durationSeconds: 75, labels: ["ramen"] })] });
    expect(screen.getByText("1:15")).toBeTruthy();
    expect(screen.getByText("1 label")).toBeTruthy();
  });
});

describe("empty states", () => {
  it("distinguishes an empty queue from an empty removed view", () => {
    renderPanel({ assets: [] });
    expect(screen.getByText("No clips here yet.")).toBeTruthy();

    document.body.innerHTML = "";
    renderPanel({ assets: [], showRemoved: true });
    expect(screen.getByText("No removed clips.")).toBeTruthy();
  });
});

describe("view toggles", () => {
  it("hides both toggles while there is nothing to toggle to", () => {
    renderPanel();
    expect(screen.queryByRole("button", { name: /Removed/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /Unusable/ })).toBeNull();
  });

  it("offers the removed view once a clip has been removed", async () => {
    const { props } = renderPanel({ removedCount: 3 });
    await userEvent.click(screen.getByRole("button", { name: "Removed 3" }));
    expect(props.onToggleRemovedView).toHaveBeenCalled();
  });

  it("does not offer the unusable filter inside the removed view", () => {
    renderPanel({ unusableCount: 2, showRemoved: true });
    expect(screen.queryByRole("button", { name: /Unusable/ })).toBeNull();
  });

  it("counts what is on screen rather than what exists", () => {
    const { container } = renderPanel({ assets: [asset({ id: "a" }), asset({ id: "b" })] });
    expect(container.querySelector(".count-badge")?.textContent).toBe("2");
  });
});

describe("the unusable verdict", () => {
  it("shows the analysis reason on the row it belongs to", () => {
    const index: Record<string, AssetAnalysisIndexEntry> = {
      a: { usable: false, unusableReason: "out of focus" } as AssetAnalysisIndexEntry,
    };
    renderPanel({ assets: [asset({ id: "a" })], analysisIndex: index });
    expect(screen.getByText("out of focus")).toBeTruthy();
  });

  it("falls back to a bare verdict when analysis gave no reason", () => {
    const index: Record<string, AssetAnalysisIndexEntry> = {
      a: { usable: false, unusableReason: null } as unknown as AssetAnalysisIndexEntry,
    };
    renderPanel({ assets: [asset({ id: "a" })], analysisIndex: index });
    expect(screen.getByText("unusable")).toBeTruthy();
  });

  it("marks nothing on a clip analysis judged usable", () => {
    const index: Record<string, AssetAnalysisIndexEntry> = {
      a: { usable: true, unusableReason: null } as unknown as AssetAnalysisIndexEntry,
    };
    const { container } = renderPanel({ assets: [asset({ id: "a" })], analysisIndex: index });
    expect(container.querySelector(".unusable-pip")).toBeNull();
  });
});

describe("search", () => {
  it("reports what was typed", async () => {
    const { props } = renderPanel();
    await userEvent.type(screen.getByRole("textbox"), "r");
    expect(props.onQueryChange).toHaveBeenCalledWith("r");
  });
});
