import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createRef } from "react";
import { describe, expect, it, vi } from "vitest";
import type { Asset } from "../../types";
import { StoryRibbon, type StoryRibbonProps } from "../StoryRibbon";

function asset(id: string, camera: string): Asset {
  return {
    id, projectId: "p", path: `/m/${id}.mp4`, filename: `${id}.MP4`, fingerprint: id,
    sizeBytes: 1_000_000, durationSeconds: 12, frameRate: 29.97, width: 1920, height: 1080,
    codec: "h264", camera, capturedAtRaw: null, bestTime: null, offsetSeconds: 0,
    proxyPath: null, proxyRepresentation: null, metadataRotationDegrees: 0,
    defaultRotationDegrees: 0, rotationDegrees: 0, orientationProbed: true,
    originalAvailable: true, proxyAvailable: false, thumbnailPath: null, thumbnailQuality: 0,
    notes: "", title: "", description: "", labels: [], removed: false,
    mediaObjectId: null, mediaVerificationState: "unlinked", available: true,
  } as unknown as Asset;
}

function lanesFor(cameras: string[]): Array<[string, Asset[]]> {
  return cameras.map((camera, i) => [camera, [asset(`a${i}`, camera)]]);
}

function renderRibbon(overrides: Partial<StoryRibbonProps> = {}) {
  const cameras = ["Sony", "DJI"];
  const lanes = lanesFor(cameras);
  const props: StoryRibbonProps = {
    assets: lanes.flatMap(([, list]) => list),
    lanes,
    contextRuns: [],
    selected: null,
    selectedId: "",
    zoom: 68,
    collapsed: false,
    merged: false,
    scrollRef: createRef<HTMLDivElement>(),
    formatDate: () => "no time",
    onZoomChange: vi.fn(),
    onCenter: vi.fn(),
    onSelect: vi.fn(),
    onToggleCollapsed: vi.fn(),
    ...overrides,
  };
  return { props, ...render(<StoryRibbon {...props} />) };
}

describe("adaptive lanes", () => {
  it("emits one lane track per camera rather than a hardcoded five", () => {
    const { container } = renderRibbon();
    expect(container.querySelectorAll(".lane-track")).toHaveLength(2);
    const canvas = container.querySelector<HTMLElement>(".story-canvas")!;
    expect(canvas.style.gridTemplateRows).toContain("repeat(2, 20px)");
  });

  it("renders a sixth camera rather than clipping it", () => {
    const lanes = lanesFor(["a", "b", "c", "d", "e", "f"]);
    const { container } = renderRibbon({ lanes, assets: lanes.flatMap(([, l]) => l) });
    expect(container.querySelectorAll(".lane-track")).toHaveLength(6);
    expect(container.querySelectorAll(".lane-name")).toHaveLength(6);
  });

  it("sizes itself from the visible lane count, capped so six cameras do not grow it", () => {
    const two = renderRibbon().container.querySelector<HTMLElement>(".chronology")!;
    const lanes = lanesFor(["a", "b", "c", "d", "e", "f"]);
    const six = renderRibbon({ lanes, assets: lanes.flatMap(([, l]) => l) })
      .container.querySelector<HTMLElement>(".chronology")!;
    expect(two.style.height).toBe("96px");
    expect(six.style.height).toBe("156px");
  });
});

describe("merged variant", () => {
  it("collapses every camera into one lane for the Focus overlay", () => {
    const { container } = renderRibbon({ merged: true });
    expect(container.querySelectorAll(".lane-track")).toHaveLength(1);
    expect(container.querySelectorAll(".lane-name")).toHaveLength(0);
  });

  it("still renders every clip in the single lane", () => {
    const { container } = renderRibbon({ merged: true });
    expect(container.querySelectorAll(".timeline-clip")).toHaveLength(2);
  });
});

describe("collapsed", () => {
  it("drops to the collapsed height and renders no lanes", () => {
    const { container } = renderRibbon({ collapsed: true });
    const root = container.querySelector<HTMLElement>(".chronology")!;
    expect(root.style.height).toBe("28px");
    expect(container.querySelector(".lane-track")).toBeNull();
  });
});

describe("selection", () => {
  it("selects a clip when its block is clicked", async () => {
    const { props, container } = renderRibbon();
    await userEvent.click(container.querySelectorAll<HTMLElement>(".timeline-clip")[1]);
    expect(props.onSelect).toHaveBeenCalledWith("a1");
  });

  it("reports zoom changes without owning the value", async () => {
    const { props } = renderRibbon();
    await userEvent.click(screen.getByLabelText(/zoom story ribbon in/i));
    expect(props.onZoomChange).toHaveBeenCalledWith(80);
  });
});
