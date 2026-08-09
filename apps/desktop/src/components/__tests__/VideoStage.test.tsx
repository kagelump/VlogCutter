import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createRef } from "react";
import { describe, expect, it, vi } from "vitest";
import type { Asset } from "../../types";
import { VideoStage, type BufferedMedia, type VideoStageProps } from "../VideoStage";

function asset(overrides: Partial<Asset> = {}): Asset {
  return {
    id: "a", projectId: "p", path: "/m/a.mp4", filename: "C0001.MP4", fingerprint: "a",
    sizeBytes: 1_000_000, durationSeconds: 12, frameRate: 29.97, width: 1920, height: 1080,
    codec: "h264", camera: "Sony", capturedAtRaw: null, bestTime: null, offsetSeconds: 0,
    proxyPath: null, proxyRepresentation: null, metadataRotationDegrees: 0,
    defaultRotationDegrees: 0, rotationDegrees: 0, orientationProbed: true,
    originalAvailable: true, proxyAvailable: false, thumbnailPath: null, thumbnailQuality: 0,
    notes: "", title: "", description: "", labels: [], removed: false,
    mediaObjectId: null, mediaVerificationState: "unlinked", available: true,
    ...overrides,
  } as unknown as Asset;
}

function media(overrides: Partial<BufferedMedia> = {}): BufferedMedia {
  return {
    asset: asset(),
    offset: 0,
    active: true,
    url: "blob:current",
    mediaIsRawOriented: false,
    rotation: 0,
    displayAspect: 16 / 9,
    posterUrl: null,
    ...overrides,
  };
}

function renderStage(overrides: Partial<VideoStageProps> = {}) {
  const props: VideoStageProps = {
    selected: asset(),
    media: [media()],
    mediaUrl: "blob:current",
    playing: false,
    buffering: false,
    muted: false,
    autoplay: false,
    canMakeProxy: false,
    busy: false,
    stageRef: createRef<HTMLDivElement>(),
    videoRef: createRef<HTMLVideoElement>(),
    onTogglePlay: vi.fn(),
    onBufferingChange: vi.fn(),
    onPlayingChange: vi.fn(),
    onDurationChange: vi.fn(),
    onPositionChange: vi.fn(),
    onMakeProxy: vi.fn(),
    ...overrides,
  };
  return { props, ...render(<VideoStage {...props} />) };
}

describe("placeholder states", () => {
  it("invites a choice when nothing is selected", () => {
    renderStage({ selected: null, media: [], mediaUrl: null });
    expect(screen.getByText("Choose a clip")).toBeTruthy();
  });

  it("says it is probing while orientation is unknown, rather than showing a wrong frame", () => {
    renderStage({
      selected: asset({ orientationProbed: false, originalAvailable: true }),
      media: [],
      mediaUrl: null,
    });
    expect(screen.getByText("Reading frame orientation…")).toBeTruthy();
    expect(document.querySelector("video")).toBeNull();
  });

  it("distinguishes a proxy that was never made from one that went missing", () => {
    renderStage({ selected: asset({ proxyPath: null }), media: [], mediaUrl: null });
    expect(screen.getByText("No review proxy yet")).toBeTruthy();

    document.body.innerHTML = "";
    renderStage({
      selected: asset({ proxyPath: "/p/a.mp4", proxyAvailable: false }),
      media: [],
      mediaUrl: null,
    });
    expect(screen.getByText("Proxy is missing")).toBeTruthy();
  });

  it("offers to make a proxy only where one can be made", async () => {
    renderStage({ selected: asset(), media: [], mediaUrl: null, canMakeProxy: false });
    expect(screen.queryByRole("button", { name: "Make proxy" })).toBeNull();

    document.body.innerHTML = "";
    const { props } = renderStage({ selected: asset(), media: [], mediaUrl: null, canMakeProxy: true });
    await userEvent.click(screen.getByRole("button", { name: "Make proxy" }));
    expect(props.onMakeProxy).toHaveBeenCalled();
  });
});

describe("the buffered window", () => {
  it("mounts the neighbours alongside the current clip so stepping does not reload", () => {
    renderStage({
      media: [
        media({ asset: asset({ id: "prev" }), offset: -1, active: false, url: "blob:prev" }),
        media({ asset: asset({ id: "cur" }), offset: 0, active: true, url: "blob:cur" }),
        media({ asset: asset({ id: "next" }), offset: 1, active: false, url: "blob:next" }),
      ],
    });
    const videos = [...document.querySelectorAll("video")];
    expect(videos.map((node) => node.dataset.cacheSlot)).toEqual(["previous", "current", "next"]);
    expect(videos.map((node) => node.dataset.assetId)).toEqual(["prev", "cur", "next"]);
  });

  it("hides the preloaded neighbours from assistive tech", () => {
    const { container } = renderStage({
      media: [
        media({ asset: asset({ id: "cur" }), active: true }),
        media({ asset: asset({ id: "next" }), offset: 1, active: false, url: "blob:next" }),
      ],
    });
    const frames = [...container.querySelectorAll(".viewer-media")];
    expect(frames.map((node) => node.getAttribute("aria-hidden"))).toEqual(["false", "true"]);
    expect(frames.map((node) => node.className.includes("active"))).toEqual([true, false]);
  });

  it("preloads the whole next clip but only metadata for the previous one", () => {
    renderStage({
      media: [
        media({ asset: asset({ id: "prev" }), offset: -1, active: false, url: "blob:prev" }),
        media({ asset: asset({ id: "cur" }), active: true }),
        media({ asset: asset({ id: "next" }), offset: 1, active: false, url: "blob:next" }),
      ],
    });
    const videos = [...document.querySelectorAll("video")];
    expect(videos.map((node) => node.getAttribute("preload"))).toEqual(["metadata", "auto", "auto"]);
  });
});

describe("rotation", () => {
  it("marks a quarter turn and passes the aspect pair as custom properties", () => {
    const { container } = renderStage({ media: [media({ rotation: 90, displayAspect: 9 / 16 })] });
    const frame = container.querySelector<HTMLElement>(".viewer-media")!;
    expect(frame.className).toContain("quarter-turn");
    expect(frame.style.getPropertyValue("--viewer-rotation")).toBe("90deg");
    expect(frame.style.getPropertyValue("--display-aspect")).toBe(String(9 / 16));
    expect(frame.style.getPropertyValue("--inverse-display-aspect")).toBe(String(16 / 9));
  });

  it("leaves a half turn unmarked — the frame still fills the same box", () => {
    const { container } = renderStage({ media: [media({ rotation: 180 })] });
    expect(container.querySelector(".viewer-media")!.className).not.toContain("quarter-turn");
  });
});

describe("the poster frame", () => {
  it("shows a poster only where the thumbnail and the media agree on orientation", () => {
    // Raw-orientation thumbnail, baked media: the poster would flash the frame
    // at the wrong rotation before the first video frame paints.
    renderStage({
      media: [media({
        asset: asset({ thumbnailPath: "/t/a.v2.jpg" }),
        mediaIsRawOriented: false,
        posterUrl: "blob:poster",
      })],
    });
    expect(document.querySelector("video")!.getAttribute("poster")).toBeNull();

    document.body.innerHTML = "";
    renderStage({
      media: [media({ asset: asset({ thumbnailPath: "/t/a.jpg" }), mediaIsRawOriented: false, posterUrl: "blob:poster" })],
    });
    expect(document.querySelector("video")!.getAttribute("poster")).toBe("blob:poster");
  });

  it("never posters an inactive frame", () => {
    renderStage({
      media: [
        media({ asset: asset({ id: "cur" }), active: true, posterUrl: null }),
        media({ asset: asset({ id: "next" }), offset: 1, active: false, url: "blob:next", posterUrl: "blob:poster" }),
      ],
    });
    const videos = [...document.querySelectorAll("video")];
    expect(videos[1].getAttribute("poster")).toBeNull();
  });
});

describe("playback reporting", () => {
  it("reports only the active element's events", () => {
    const { props } = renderStage({
      media: [
        media({ asset: asset({ id: "cur" }), active: true }),
        media({ asset: asset({ id: "next" }), offset: 1, active: false, url: "blob:next" }),
      ],
    });
    const videos = [...document.querySelectorAll("video")];

    fireEvent.playing(videos[1]);
    fireEvent.pause(videos[1]);
    expect(props.onBufferingChange).not.toHaveBeenCalled();
    expect(props.onPlayingChange).not.toHaveBeenCalled();

    fireEvent.playing(videos[0]);
    expect(props.onBufferingChange).toHaveBeenCalledWith(false);
    fireEvent.waiting(videos[0]);
    expect(props.onBufferingChange).toHaveBeenLastCalledWith(true);
    fireEvent.play(videos[0]);
    expect(props.onPlayingChange).toHaveBeenLastCalledWith(true);
    fireEvent.ended(videos[0]);
    expect(props.onPlayingChange).toHaveBeenLastCalledWith(false);
  });

  it("falls back to the catalog duration when the element reports a non-finite one", () => {
    const { props } = renderStage({ media: [media({ asset: asset({ durationSeconds: 12 }) })] });
    const video = document.querySelector("video")!;
    Object.defineProperty(video, "duration", { value: Number.NaN, configurable: true });
    Object.defineProperty(video, "currentTime", { value: 0, configurable: true });
    fireEvent.loadedMetadata(video);
    expect(props.onDurationChange).toHaveBeenCalledWith(12);
  });

  it("toggles playback when the frame is clicked", async () => {
    const { props } = renderStage();
    await userEvent.click(document.querySelector("video")!);
    expect(props.onTogglePlay).toHaveBeenCalled();
  });

  it("reports an element error once, from the active element only", () => {
    // The shell reads this as "the media origin may have died" and checks it.
    // A preloaded neighbour failing says nothing about what is on screen.
    const onMediaError = vi.fn();
    const { props } = renderStage({
      onMediaError,
      media: [
        media({ asset: asset({ id: "cur" }), active: true }),
        media({ asset: asset({ id: "next" }), offset: 1, active: false, url: "blob:next" }),
      ],
    });
    const videos = [...document.querySelectorAll("video")];

    fireEvent.error(videos[1]);
    expect(onMediaError).not.toHaveBeenCalled();

    fireEvent.error(videos[0]);
    expect(onMediaError).toHaveBeenCalledTimes(1);
    // The spinner is cleared: an element that failed is not still loading.
    expect(props.onBufferingChange).toHaveBeenLastCalledWith(false);
  });
});

describe("stage chrome", () => {
  it("shows the buffering hairline only while buffering", () => {
    const { container } = renderStage({ buffering: false });
    expect(container.querySelector(".viewer-buffering")).toBeNull();

    document.body.innerHTML = "";
    expect(renderStage({ buffering: true }).container.querySelector(".viewer-buffering")).toBeTruthy();
  });

  it("names the representation on screen and its frame geometry", () => {
    renderStage({ selected: asset({ proxyAvailable: true }) });
    expect(screen.getByText("PROXY")).toBeTruthy();
    expect(screen.getByText("1920×1080 · 29.97 fps")).toBeTruthy();

    document.body.innerHTML = "";
    renderStage({ selected: asset({ proxyAvailable: false }) });
    expect(screen.getByText("ORIGINAL")).toBeTruthy();
  });

  it("carries the playing state on the stage, which is what the CSS presentation keys off", () => {
    expect(renderStage({ playing: true }).container.querySelector(".viewer-stage")!.className)
      .toContain("is-playing");
    expect(renderStage({ playing: false }).container.querySelector(".viewer-stage")!.className)
      .toContain("is-paused");
  });
});
