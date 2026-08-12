import { fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Asset } from "../types";
import { buildChronologyOverview } from "../test/harness/chronologyFixtures";
import { ChronologyCanvas } from "./ChronologyCanvas";

function asset(id: string, filename: string): Asset {
  return {
    id, projectId: "project", path: `/media/${filename}`, filename,
    fingerprint: `fingerprint-${id}`, sizeBytes: 1,
    title: "", description: "", durationSeconds: 1, frameRate: 24,
    width: 1920, height: 1080, defaultRotationDegrees: 0, rotationDegrees: 0,
    metadataRotationDegrees: 0, orientationProbed: true, codec: "h264", camera: "Sony", capturedAtRaw: null,
    bestTime: null, offsetSeconds: 0, labels: [], notes: "", removed: false,
    available: true, originalAvailable: true, proxyAvailable: false, proxyPath: null,
    proxyRepresentation: null, thumbnailPath: null, thumbnailQuality: 0,
    mediaObjectId: null, mediaVerificationState: "verified", phase: "",
    location: "", timezone: "", storyBeat: "",
  };
}

/**
 * jsdom lays nothing out: every `clientWidth` is 0 and `ResizeObserver` does
 * not exist. Both are stubbed here so the virtual window can be exercised at a
 * realistic desk width. This says nothing about real layout — that is the
 * headless-Chromium tier's job — only that the component asks the container how
 * wide it is instead of assuming.
 */
function stubLayout(width: number) {
  const observed = new Set<() => void>();
  const clientWidth = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientWidth");
  Object.defineProperty(HTMLElement.prototype, "clientWidth", {
    configurable: true,
    get() {
      return this.className === "chronology-canvas-scroll" ? width : 0;
    },
  });
  class StubResizeObserver {
    constructor(private readonly callback: () => void) {}
    observe() { observed.add(this.callback); }
    unobserve() { observed.delete(this.callback); }
    disconnect() { observed.delete(this.callback); }
  }
  const previousObserver = globalThis.ResizeObserver;
  globalThis.ResizeObserver = StubResizeObserver as unknown as typeof ResizeObserver;
  return () => {
    globalThis.ResizeObserver = previousObserver;
    if (clientWidth) Object.defineProperty(HTMLElement.prototype, "clientWidth", clientWidth);
    else Reflect.deleteProperty(HTMLElement.prototype, "clientWidth");
  };
}

describe("ChronologyCanvas virtual window", () => {
  let restoreLayout: (() => void) | null = null;
  afterEach(() => {
    restoreLayout?.();
    restoreLayout = null;
  });

  it("fills a desk wider than the fallback width before anyone scrolls", () => {
    // 2400px of track past the 160px gutter is ~26 columns at 92px. A canvas
    // that assumes a 900px viewport paints ~16 and leaves the rest of a
    // maximised window blank until the first scroll event.
    restoreLayout = stubLayout(2560);
    const order = Array.from({ length: 60 }, (_, index) => `clip-${index}`);
    const overview = buildChronologyOverview({
      order,
      sequences: [{ id: "card-a", cameraKey: "Sony", assetIds: order }],
    });
    const assets = order.map((id, index) => asset(id, `A${String(index).padStart(3, "0")}.MP4`));

    render(
      <ChronologyCanvas
        overview={overview}
        assetsById={new Map(assets.map((item) => [item.id, item]))}
        focusedAssetId="clip-0"
        selectedIds={["clip-0"]}
        impact={null}
        onFocusAsset={vi.fn()}
        onSelectRange={vi.fn()}
        onPlaceSelection={vi.fn()}
        onAdjustPhase={vi.fn()}
      />,
    );

    expect(screen.getByRole("button", { name: /A025\.MP4/ })).toBeInTheDocument();
  });
});

const GUTTER = 160;
const COLUMN = 92;

/** The clientX that `boundaryIndexAt` reads back as `index`, given a 0,0 origin. */
function clientXForIndex(index: number) {
  return GUTTER + COLUMN / 2 + index * COLUMN;
}

/** jsdom has no pointer capture; the drag handlers call it on every gesture. */
function stubPointerCapture() {
  const captured = new Set<number>();
  const prototype = HTMLElement.prototype as unknown as Record<string, unknown>;
  const previous = {
    set: prototype.setPointerCapture,
    release: prototype.releasePointerCapture,
    has: prototype.hasPointerCapture,
  };
  prototype.setPointerCapture = function set(id: number) { captured.add(id); };
  prototype.releasePointerCapture = function release(id: number) { captured.delete(id); };
  prototype.hasPointerCapture = function has(id: number) { return captured.has(id); };
  return () => {
    prototype.setPointerCapture = previous.set;
    prototype.releasePointerCapture = previous.release;
    prototype.hasPointerCapture = previous.has;
  };
}

const tokyo = {
  id: "phase_tokyo",
  label: "Tokyo",
  kind: "location" as const,
  startAnchorAssetId: "a",
  endAnchorAssetId: "b",
  startTripTime: "",
  endTripTime: "",
  timezone: "Asia/Tokyo",
  originTimezone: null,
  destinationTimezone: null,
  displayMode: "local" as const,
};

function threeClips() {
  const overview = buildChronologyOverview({
    order: ["a", "b", "c"],
    sequences: [{ id: "card-a", cameraKey: "Sony", label: "Sony card", assetIds: ["a", "b", "c"] }],
    phases: [tokyo],
  });
  const assets = [asset("a", "A.MP4"), asset("b", "B.MP4"), asset("c", "C.MP4")];
  return { overview, assetsById: new Map(assets.map((item) => [item.id, item])) };
}

describe("ChronologyCanvas trip-phase boundaries", () => {
  let restorePointer: (() => void) | null = null;
  afterEach(() => {
    restorePointer?.();
    restorePointer = null;
  });

  it("drags a phase edge to a new clip and reports the ordinal range it landed on", () => {
    restorePointer = stubPointerCapture();
    const { overview, assetsById } = threeClips();
    const onAdjustPhase = vi.fn();
    render(
      <ChronologyCanvas
        overview={overview}
        assetsById={assetsById}
        focusedAssetId="a"
        selectedIds={[]}
        impact={null}
        onFocusAsset={vi.fn()}
        onSelectRange={vi.fn()}
        onPlaceSelection={vi.fn()}
        onAdjustPhase={onAdjustPhase}
      />,
    );

    const end = screen.getByRole("button", { name: "Move end of Tokyo" });
    expect(end).toHaveAttribute("aria-valuetext", "Ends at clip 2");
    fireEvent.pointerDown(end, { pointerId: 1 });
    fireEvent.pointerMove(end, { pointerId: 1, clientX: clientXForIndex(2) });
    fireEvent.pointerUp(end, { pointerId: 1 });

    expect(onAdjustPhase).toHaveBeenCalledTimes(1);
    expect(onAdjustPhase).toHaveBeenCalledWith(
      expect.objectContaining({ startIndex: 0, endIndex: 2 }),
    );
  });

  it("does not let an edge cross the phase's other edge", () => {
    restorePointer = stubPointerCapture();
    const { overview, assetsById } = threeClips();
    const onAdjustPhase = vi.fn();
    render(
      <ChronologyCanvas
        overview={overview}
        assetsById={assetsById}
        focusedAssetId="a"
        selectedIds={[]}
        impact={null}
        onFocusAsset={vi.fn()}
        onSelectRange={vi.fn()}
        onPlaceSelection={vi.fn()}
        onAdjustPhase={onAdjustPhase}
      />,
    );

    const start = screen.getByRole("button", { name: "Move start of Tokyo" });
    fireEvent.pointerDown(start, { pointerId: 2 });
    fireEvent.pointerMove(start, { pointerId: 2, clientX: clientXForIndex(2) });
    fireEvent.pointerUp(start, { pointerId: 2 });

    expect(onAdjustPhase).toHaveBeenCalledWith(
      expect.objectContaining({ startIndex: 1, endIndex: 1 }),
    );
  });

  it("swallows the click a finished drag leaves behind, so one gesture is one edit", () => {
    restorePointer = stubPointerCapture();
    const { overview, assetsById } = threeClips();
    const onAdjustPhase = vi.fn();
    render(
      <ChronologyCanvas
        overview={overview}
        assetsById={assetsById}
        focusedAssetId="a"
        selectedIds={[]}
        impact={null}
        onFocusAsset={vi.fn()}
        onSelectRange={vi.fn()}
        onPlaceSelection={vi.fn()}
        onAdjustPhase={onAdjustPhase}
      />,
    );

    const end = screen.getByRole("button", { name: "Move end of Tokyo" });
    fireEvent.pointerDown(end, { pointerId: 3 });
    fireEvent.pointerMove(end, { pointerId: 3, clientX: clientXForIndex(2) });
    fireEvent.pointerUp(end, { pointerId: 3 });
    fireEvent.click(end);

    expect(onAdjustPhase).toHaveBeenCalledTimes(1);
  });

  it("nudges an edge one clip at a time from the keyboard, and stays inside the project", () => {
    const { overview, assetsById } = threeClips();
    const onAdjustPhase = vi.fn();
    render(
      <ChronologyCanvas
        overview={overview}
        assetsById={assetsById}
        focusedAssetId="a"
        selectedIds={[]}
        impact={null}
        onFocusAsset={vi.fn()}
        onSelectRange={vi.fn()}
        onPlaceSelection={vi.fn()}
        onAdjustPhase={onAdjustPhase}
      />,
    );

    const start = screen.getByRole("button", { name: "Move start of Tokyo" });
    fireEvent.keyDown(start, { key: "ArrowLeft" });
    expect(onAdjustPhase).toHaveBeenLastCalledWith(
      expect.objectContaining({ startIndex: 0, endIndex: 1 }),
    );

    fireEvent.keyDown(screen.getByRole("button", { name: "Move end of Tokyo" }), { key: "ArrowRight" });
    expect(onAdjustPhase).toHaveBeenLastCalledWith(
      expect.objectContaining({ startIndex: 0, endIndex: 2 }),
    );

    onAdjustPhase.mockClear();
    fireEvent.keyDown(start, { key: "Enter" });
    expect(onAdjustPhase).not.toHaveBeenCalled();
  });

  it("opens the phase for editing when its label is clicked", () => {
    const { overview, assetsById } = threeClips();
    const onAdjustPhase = vi.fn();
    render(
      <ChronologyCanvas
        overview={overview}
        assetsById={assetsById}
        focusedAssetId="a"
        selectedIds={[]}
        impact={null}
        onFocusAsset={vi.fn()}
        onSelectRange={vi.fn()}
        onPlaceSelection={vi.fn()}
        onAdjustPhase={onAdjustPhase}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Edit Tokyo" }));
    expect(onAdjustPhase).toHaveBeenCalledWith(
      expect.objectContaining({ startIndex: 0, endIndex: 1 }),
    );
  });
});

describe("ChronologyCanvas placement and impact", () => {
  it("offers a gap on either side of every clip outside the selected run", () => {
    const { overview, assetsById } = threeClips();
    const onPlaceSelection = vi.fn();
    render(
      <ChronologyCanvas
        overview={overview}
        assetsById={assetsById}
        focusedAssetId="a"
        selectedIds={["a"]}
        impact={null}
        onFocusAsset={vi.fn()}
        onSelectRange={vi.fn()}
        onPlaceSelection={onPlaceSelection}
        onAdjustPhase={vi.fn()}
      />,
    );

    // The selected clip is not its own destination.
    expect(screen.queryByRole("button", { name: "Place selected run before a" })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Place selected run after c" }));
    expect(onPlaceSelection).toHaveBeenCalledWith("c", "after");
  });

  it("shows nowhere to drop a run while nothing is selected", () => {
    const { overview, assetsById } = threeClips();
    render(
      <ChronologyCanvas
        overview={overview}
        assetsById={assetsById}
        focusedAssetId="a"
        selectedIds={[]}
        impact={null}
        onFocusAsset={vi.fn()}
        onSelectRange={vi.fn()}
        onPlaceSelection={vi.fn()}
        onAdjustPhase={vi.fn()}
      />,
    );

    expect(screen.queryByLabelText("Place selected run")).toBeNull();
  });

  it("draws a pending move at its proposed position and leaves a ghost where it is now", () => {
    const { overview, assetsById } = threeClips();
    const { container } = render(
      <ChronologyCanvas
        overview={overview}
        assetsById={assetsById}
        focusedAssetId="a"
        selectedIds={["a"]}
        impact={{
          changes: [{ assetId: "a", before: null, after: null, beforePosition: 0, afterPosition: 2 }],
          contradictions: [],
          blocked: false,
        }}
        onFocusAsset={vi.fn()}
        onSelectRange={vi.fn()}
        onPlaceSelection={vi.fn()}
        onAdjustPhase={vi.fn()}
      />,
    );

    const moving = screen.getByRole("button", { name: /A\.MP4/ });
    expect(moving.className).toContain("will-move");
    expect(moving).toHaveStyle({ left: `${2 * COLUMN + 5}px` });
    expect(container.querySelector(".chronology-ghost-block")).toHaveStyle({ left: "5px" });
    expect(container.querySelector(".chronology-impact-extent")).toBeTruthy();
  });

  it("locks every control while a preview is awaiting confirmation", () => {
    const { overview, assetsById } = threeClips();
    render(
      <ChronologyCanvas
        overview={overview}
        assetsById={assetsById}
        focusedAssetId="a"
        selectedIds={["a"]}
        impact={null}
        disabled
        onFocusAsset={vi.fn()}
        onSelectRange={vi.fn()}
        onPlaceSelection={vi.fn()}
        onAdjustPhase={vi.fn()}
      />,
    );

    expect(screen.getByRole("button", { name: /A\.MP4/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Move end of Tokyo" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Place selected run after c" })).toBeDisabled();
  });
});

describe("ChronologyCanvas range selection", () => {
  it("extends the preselected clip and resets cleanly across cards", () => {
    const overview = buildChronologyOverview({
      order: ["a", "b", "c"],
      sequences: [
        { id: "card-a", cameraKey: "Sony", assetIds: ["a", "b"] },
        { id: "card-b", cameraKey: "Sony", assetIds: ["c"] },
      ],
    });
    const assets = [asset("a", "A.MP4"), asset("b", "B.MP4"), asset("c", "C.MP4")];
    const onFocusAsset = vi.fn();
    const onSelectRange = vi.fn();
    const { rerender } = render(
      <ChronologyCanvas
        overview={overview}
        assetsById={new Map(assets.map((item) => [item.id, item]))}
        focusedAssetId="a"
        selectedIds={["a"]}
        impact={null}
        onFocusAsset={onFocusAsset}
        onSelectRange={onSelectRange}
        onPlaceSelection={vi.fn()}
        onAdjustPhase={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /B\.MP4/ }), { shiftKey: true });
    expect(onSelectRange).toHaveBeenLastCalledWith(["a", "b"]);

    rerender(
      <ChronologyCanvas
        overview={overview}
        assetsById={new Map(assets.map((item) => [item.id, item]))}
        focusedAssetId="b"
        selectedIds={["a", "b"]}
        impact={null}
        onFocusAsset={onFocusAsset}
        onSelectRange={onSelectRange}
        onPlaceSelection={vi.fn()}
        onAdjustPhase={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /C\.MP4/ }), { shiftKey: true });
    expect(onFocusAsset).toHaveBeenLastCalledWith("c");
    expect(onSelectRange).toHaveBeenLastCalledWith(["c"]);
  });
});
