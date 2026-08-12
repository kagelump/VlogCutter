import { fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Asset, MediaOrigin } from "../types";
import { buildChronologyOverview } from "../test/harness/chronologyFixtures";
import { installTauriRuntime } from "../test/harness/ipc";
import { CompareDeck } from "./CompareDeck";

const ORIGIN: MediaOrigin = { baseUrl: "http://127.0.0.1:7788", token: "tok" };

// The deck resolves thumbnails through Tauri's asset protocol and mounts real
// media elements. Neither exists in jsdom, and the harness owns both stubs.
let uninstall: (() => void) | null = null;
beforeEach(() => {
  uninstall = installTauriRuntime({
    invoke: () => Promise.resolve(null),
    transformCallback: () => 0,
    unregisterListener: () => {},
  });
});
afterEach(() => {
  uninstall?.();
  uninstall = null;
});

function asset(id: string, filename: string, overrides: Partial<Asset> = {}): Asset {
  return {
    id, projectId: "project", path: `/media/${filename}`, filename,
    fingerprint: `fingerprint-${id}`, sizeBytes: 1,
    title: "", description: "", durationSeconds: 12.5, frameRate: 24,
    width: 1920, height: 1080, defaultRotationDegrees: 0, rotationDegrees: 0,
    metadataRotationDegrees: 0, orientationProbed: true, codec: "h264", camera: "Sony",
    capturedAtRaw: null, bestTime: null, offsetSeconds: 0, labels: [], notes: "",
    removed: false, available: true, originalAvailable: true, proxyAvailable: true,
    proxyPath: `/proxies/${id}.v3.mp4`, proxyRepresentation: "raw_v3",
    thumbnailPath: `/thumbs/${id}.v2.jpg`, thumbnailQuality: 1,
    mediaObjectId: null, mediaVerificationState: "verified", phase: "",
    location: "", timezone: "", storyBeat: "", ...overrides,
  };
}

/**
 * Two cards interleaved by the solver, which is the arrangement the deck exists
 * for: C0902's neighbours in solved order are on the *other* card, and no
 * per-lane view would put them next to each other.
 */
function scene() {
  const overview = buildChronologyOverview({
    order: ["sony-1", "dji-1", "sony-2", "dji-2"],
    sequences: [
      { id: "card-sony", cameraKey: "Sony", label: "Sony (C-series)", assetIds: ["sony-1", "sony-2"] },
      { id: "card-dji", cameraKey: "DJI", label: "DJI Pocket 3", assetIds: ["dji-1", "dji-2"] },
    ],
    startAt: "2026-02-08T00:10:00.000Z",
    spacingSeconds: 600,
  });
  const assetsById = new Map([
    ["sony-1", asset("sony-1", "C0901.MP4")],
    ["dji-1", asset("dji-1", "DJI_0001.MP4", { camera: "DJI" })],
    ["sony-2", asset("sony-2", "C0902.MP4")],
    ["dji-2", asset("dji-2", "DJI_0002.MP4", { camera: "DJI" })],
  ]);
  return { overview, assetsById };
}

function renderDeck(props: Partial<Parameters<typeof CompareDeck>[0]> = {}) {
  const { overview, assetsById } = scene();
  const onFocusAsset = vi.fn();
  const onToggleCollapsed = vi.fn();
  render(
    <CompareDeck
      overview={overview}
      assetsById={assetsById}
      focusedAssetId="sony-2"
      mediaOrigin={ORIGIN}
      collapsed={false}
      onFocusAsset={onFocusAsset}
      onToggleCollapsed={onToggleCollapsed}
      {...props}
    />,
  );
  return { onFocusAsset, onToggleCollapsed };
}

describe("CompareDeck", () => {
  it("names the focused clip and its place in the solved order", () => {
    renderDeck();
    const stage = screen.getByRole("group", { name: /focused clip/i });
    expect(within(stage).getByText("C0902.MP4")).toBeInTheDocument();
    expect(screen.getByText("Clip 3 of 4")).toBeInTheDocument();
  });

  it("draws neighbours from the solved order, not from the focused clip's own card", () => {
    // sony-2 sits between two DJI clips. A lane-local deck would show C0901 and
    // find no successor; the point of the deck is that it shows what the solver
    // actually placed either side.
    renderDeck();
    expect(screen.getByRole("button", { name: /previous in order.*DJI_0001\.MP4/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /next in order.*DJI_0002\.MP4/i })).toBeInTheDocument();
  });

  it("shows each neighbour's placed time, so the comparison is dated", () => {
    renderDeck();
    const previous = screen.getByRole("button", { name: /previous in order/i });
    expect(within(previous).getByText(/00:20/)).toBeInTheDocument();
  });

  it("focuses a neighbour when it is clicked", () => {
    const { onFocusAsset } = renderDeck();
    fireEvent.click(screen.getByRole("button", { name: /next in order/i }));
    expect(onFocusAsset).toHaveBeenCalledWith("dji-2");
  });

  it("offers no neighbour past either end of the order", () => {
    renderDeck({ focusedAssetId: "sony-1" });
    expect(screen.queryByRole("button", { name: /previous in order/i })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /next in order/i })).toBeInTheDocument();
  });

  it("plays the focused clip from the loopback origin", () => {
    const { container } = render(
      <CompareDeck
        {...scene()}
        focusedAssetId="sony-2"
        mediaOrigin={ORIGIN}
        collapsed={false}
        onFocusAsset={vi.fn()}
        onToggleCollapsed={vi.fn()}
      />,
    );
    const active = container.querySelector<HTMLVideoElement>('video[data-cache-slot="current"]');
    expect(active?.getAttribute("src")).toBe(
      "http://127.0.0.1:7788/media/sony-2?rep=proxy&t=tok",
    );
  });

  it("mounts the neighbours too, so stepping pays no load", () => {
    const { container } = render(
      <CompareDeck
        {...scene()}
        focusedAssetId="sony-2"
        mediaOrigin={ORIGIN}
        collapsed={false}
        onFocusAsset={vi.fn()}
        onToggleCollapsed={vi.fn()}
      />,
    );
    expect(container.querySelectorAll("video")).toHaveLength(3);
  });

  it("explains itself instead of going blank when nothing is serving media", () => {
    // The browser preview and a desktop app whose loopback origin failed to
    // bind are both in this state.
    renderDeck({ mediaOrigin: null });
    expect(screen.getByText(/no review proxy|not being served/i)).toBeInTheDocument();
  });

  it("says which clip is missing its proxy rather than showing an empty frame", () => {
    const { overview, assetsById } = scene();
    assetsById.set("sony-2", asset("sony-2", "C0902.MP4", {
      proxyAvailable: false, proxyPath: null, proxyRepresentation: null, originalAvailable: false,
      available: false,
    }));
    render(
      <CompareDeck
        overview={overview}
        assetsById={assetsById}
        focusedAssetId="sony-2"
        mediaOrigin={ORIGIN}
        collapsed={false}
        onFocusAsset={vi.fn()}
        onToggleCollapsed={vi.fn()}
      />,
    );
    const slate = document.querySelector(".offline-slate");
    expect(slate).toHaveTextContent("C0902.MP4");
    expect(slate).toHaveTextContent(/no review proxy|not being served/i);
  });

  it("collapses to a header the user can reopen", () => {
    const { onToggleCollapsed } = renderDeck({ collapsed: true });
    expect(document.querySelectorAll("video")).toHaveLength(0);
    fireEvent.click(screen.getByRole("button", { name: /show the compare deck/i }));
    expect(onToggleCollapsed).toHaveBeenCalled();
  });

  it("steps along the solved order with the arrow keys", () => {
    // Not `[`/`]`: those are the zoom pair, and the desk wants zoom as much as
    // Review does now that the tiles carry frames.
    const { onFocusAsset } = renderDeck();
    fireEvent.keyDown(window, { key: "ArrowRight" });
    expect(onFocusAsset).toHaveBeenCalledWith("dji-2");
    fireEvent.keyDown(window, { key: "ArrowLeft" });
    expect(onFocusAsset).toHaveBeenCalledWith("dji-1");
  });

  it("ignores its shortcuts while a correction preview is up", () => {
    const { onFocusAsset } = renderDeck({ disabled: true });
    fireEvent.keyDown(window, { key: "ArrowRight" });
    expect(onFocusAsset).not.toHaveBeenCalled();
  });

  it("yields the arrow keys to a phase boundary being nudged", () => {
    // The boundary handles arrows itself and calls preventDefault; the deck
    // must not also drag the focus off to another clip.
    const { onFocusAsset } = renderDeck();
    const event = new KeyboardEvent("keydown", { key: "ArrowRight", cancelable: true, bubbles: true });
    event.preventDefault();
    window.dispatchEvent(event);
    expect(onFocusAsset).not.toHaveBeenCalled();
  });

  it("leaves typing in a field alone", () => {
    const { onFocusAsset } = renderDeck();
    const field = document.createElement("input");
    document.body.append(field);
    field.focus();
    fireEvent.keyDown(field, { key: "ArrowRight" });
    // Arrow keys move the caret inside the clock-correction form's number
    // inputs; the deck must not steal them.
    expect(onFocusAsset).not.toHaveBeenCalled();
    field.remove();
  });

  it("plays the focused clip from its own transport", () => {
    renderDeck();
    fireEvent.click(screen.getByRole("button", { name: "Play" }));
    // The harness's media stub turns play() into a real `play` event, the same
    // path the element takes when the user clicks the frame itself.
    expect(screen.getByRole("button", { name: "Pause" })).toBeInTheDocument();
  });

  it("follows the element back to paused rather than tracking its own guess", () => {
    // Playback can stop without the button: the clip ends, or the shell pauses
    // it. The label is driven by the element's events for that reason.
    renderDeck();
    const active = document.querySelector<HTMLVideoElement>('video[data-cache-slot="current"]')!;
    fireEvent.play(active);
    expect(screen.getByRole("button", { name: "Pause" })).toBeInTheDocument();
    fireEvent.ended(active);
    expect(screen.getByRole("button", { name: "Play" })).toBeInTheDocument();
  });

  it("scrubs the focused clip and reports where it landed", () => {
    renderDeck();
    const active = document.querySelector<HTMLVideoElement>('video[data-cache-slot="current"]')!;
    fireEvent.change(screen.getByRole("slider", { name: /Scrub C0902\.MP4/ }), { target: { value: "4.5" } });
    expect(active.currentTime).toBe(4.5);
    expect(screen.getByText("0:04 / 0:12")).toBeInTheDocument();
  });

  it("starts muted and unmutes on request", () => {
    // Muted by default: the desk is a place to scan many clips quickly, and
    // audio blaring on every arrow press would be hostile.
    renderDeck();
    const active = document.querySelector<HTMLVideoElement>('video[data-cache-slot="current"]')!;
    expect(active.muted).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Unmute" }));
    expect(screen.getByRole("button", { name: "Mute" })).toHaveAttribute("aria-pressed", "false");
  });

  it("takes the real duration from the element over the catalog's estimate", () => {
    renderDeck();
    const active = document.querySelector<HTMLVideoElement>('video[data-cache-slot="current"]')!;
    Object.defineProperty(active, "duration", { value: 30, configurable: true });
    Object.defineProperty(active, "currentTime", { value: 0, configurable: true, writable: true });
    fireEvent.loadedMetadata(active);
    expect(screen.getByText("0:00 / 0:30")).toBeInTheDocument();
    Object.defineProperty(active, "currentTime", { value: 7, configurable: true, writable: true });
    fireEvent.timeUpdate(active);
    expect(screen.getByText("0:07 / 0:30")).toBeInTheDocument();
  });

  it("resets the transport when the focus moves to another clip", () => {
    const { onFocusAsset } = renderDeck();
    const active = document.querySelector<HTMLVideoElement>('video[data-cache-slot="current"]')!;
    Object.defineProperty(active, "currentTime", { value: 9, configurable: true, writable: true });
    fireEvent.timeUpdate(active);
    expect(screen.getByText("0:09 / 0:12")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /next in order/i }));
    expect(onFocusAsset).toHaveBeenCalledWith("dji-2");
  });

  it("shows a buffering notice while the clip is still loading", () => {
    renderDeck();
    const active = document.querySelector<HTMLVideoElement>('video[data-cache-slot="current"]')!;
    fireEvent.waiting(active);
    expect(screen.getByRole("status")).toHaveTextContent(/buffering/i);
    fireEvent.canPlay(active);
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("says nothing at all when no clip is focused", () => {
    renderDeck({ focusedAssetId: null });
    expect(screen.getByText(/choose a clip/i)).toBeInTheDocument();
  });
});
