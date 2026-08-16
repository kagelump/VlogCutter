import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createRef } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
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
    loading: false,
    loadError: "",
    onReload: vi.fn(),
    onQueryChange: vi.fn(),
    onSelect: vi.fn(),
    onToggleRemovedView: vi.fn(),
    onToggleUnusableView: vi.fn(),
    ...overrides,
  };
  return { props, ...render(<QueuePanel {...props} />) };
}

function manyAssets(count: number): Asset[] {
  return Array.from({ length: count }, (_, index) => asset({
    id: `asset-${index}`,
    fingerprint: `fingerprint-${index}`,
    filename: `C${String(index + 1).padStart(5, "0")}.MP4`,
  }));
}

function measureQueueAt(height: number) {
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(height);

  class ImmediateResizeObserver implements ResizeObserver {
    constructor(private readonly callback: ResizeObserverCallback) {}

    observe(target: Element) {
      this.callback([{
        target,
        contentRect: { height } as DOMRectReadOnly,
      } as ResizeObserverEntry], this);
    }

    unobserve() {}
    disconnect() {}
  }

  vi.stubGlobal("ResizeObserver", ImmediateResizeObserver);
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

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

  it("mounts only the viewport around a ten-thousand-clip queue", () => {
    measureQueueAt(260);
    const { container } = renderPanel({ assets: manyAssets(10_000) });

    const options = screen.getAllByRole("option");
    expect(options).toHaveLength(11);
    expect(options[0]).toHaveAttribute("aria-posinset", "1");
    expect(options[0]).toHaveAttribute("aria-setsize", "10000");
    expect(container.querySelector('[role="presentation"]')).toHaveStyle({ height: "520000px" });
  });

  it("keeps global queue numbers and accessibility positions while scrolling", () => {
    measureQueueAt(260);
    const { props } = renderPanel({ assets: manyAssets(10_000) });
    const list = screen.getByRole("listbox", { name: "Clips" });

    fireEvent.scroll(list, { target: { scrollTop: 5_000 * 52 } });

    expect(screen.getByText("5001")).toBeTruthy();
    const middle = screen.getByText("5001").closest('[role="option"]');
    expect(middle).toHaveAttribute("aria-posinset", "5001");
    expect(screen.getAllByRole("option").length).toBeLessThanOrEqual(17);
    fireEvent.click(middle!);
    expect(props.onSelect).toHaveBeenCalledWith("asset-5000");
  });

  it("reveals a remembered selection that starts outside the mounted window", () => {
    measureQueueAt(260);
    renderPanel({ assets: manyAssets(10_000), selectedId: "asset-9000" });

    const selected = screen.getByRole("option", { selected: true });
    expect(selected).toHaveAttribute("aria-posinset", "9001");
    expect(selected).toHaveTextContent("C09001.MP4");
    expect((screen.getByRole("listbox", { name: "Clips" }) as HTMLDivElement).scrollTop)
      .toBeGreaterThan(0);
  });

  it("resets positions to the filtered queue without retaining thousands of rows", () => {
    measureQueueAt(260);
    const all = manyAssets(10_000);
    const rendered = renderPanel({ assets: all, selectedId: "asset-5000" });

    rendered.rerender(<QueuePanel {...rendered.props} assets={[all[5_000]]} query="C05001" />);

    expect(screen.getAllByRole("option")).toHaveLength(1);
    expect(screen.getByRole("option")).toHaveAttribute("aria-posinset", "1");
    expect(screen.getByRole("option")).toHaveAttribute("aria-setsize", "1");
    expect(screen.getByText("001")).toBeTruthy();
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

describe("offline originals", () => {
  it("marks a clip whose original is offline but whose proxy is present", () => {
    const { container } = renderPanel({
      assets: [asset({ id: "a", originalAvailable: false, proxyAvailable: true })],
    });
    const pip = container.querySelector(".offline-pip");
    expect(pip?.textContent).toContain("offline");
    // Quiet, not an alarm: reviewing from a proxy is what proxies are for.
    expect(pip?.classList.contains("blocked")).toBe(false);
    expect(pip?.getAttribute("title")).toMatch(/reviewing from the 720p proxy/i);
  });

  it("distinguishes a clip with neither original nor proxy", () => {
    const { container } = renderPanel({
      assets: [asset({ id: "a", originalAvailable: false, proxyAvailable: false })],
    });
    const pip = container.querySelector(".offline-pip");
    expect(pip?.classList.contains("blocked")).toBe(true);
    expect(pip?.getAttribute("title")).toMatch(/cannot be played/i);
  });

  it("marks nothing when the original is mounted", () => {
    const { container } = renderPanel({
      assets: [asset({ id: "a", originalAvailable: true, proxyAvailable: true })],
    });
    expect(container.querySelector(".offline-pip")).toBeNull();
  });

  it("explains the state in the row's text, not only in a tooltip", () => {
    renderPanel({ assets: [asset({ id: "a", originalAvailable: false, proxyAvailable: true })] });
    const row = screen.getByRole("option");
    expect(row.textContent).toContain("Original is offline — reviewing from the 720p proxy");
  });
});
