import { fireEvent, render, screen } from "@testing-library/react";
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
    height: null,
    scrollRef: createRef<HTMLDivElement>(),
    onZoomChange: vi.fn(),
    onCenter: vi.fn(),
    onSelect: vi.fn(),
    onToggleCollapsed: vi.fn(),
    onRequestContext: vi.fn(),
    onEditContext: vi.fn(),
    onRenameCamera: vi.fn(),
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

  it("sizes itself from the lane count, capped at eight", () => {
    const two = renderRibbon().container.querySelector<HTMLElement>(".chronology")!;
    expect(two.style.height).toBe("96px");

    const six = lanesFor(["a", "b", "c", "d", "e", "f"]);
    expect(
      renderRibbon({ lanes: six, assets: six.flatMap(([, l]) => l) })
        .container.querySelector<HTMLElement>(".chronology")!.style.height,
    ).toBe("176px");

    const twelve = lanesFor("abcdefghijkl".split(""));
    expect(
      renderRibbon({ lanes: twelve, assets: twelve.flatMap(([, l]) => l) })
        .container.querySelector<HTMLElement>(".chronology")!.style.height,
    ).toBe("216px");
  });

  it("lets a dragged height win over the content height", () => {
    const root = renderRibbon({ height: 320 }).container.querySelector<HTMLElement>(".chronology")!;
    expect(root.style.height).toBe("320px");
  });

  it("lets the map fill the ribbon instead of constraining both columns to the context row", () => {
    const map = renderRibbon().container.querySelector<HTMLElement>(".story-map")!;
    expect(map.style.gridTemplateRows).toBe("");
  });

  it("still collapses when collapsed, whatever the dragged height says", () => {
    const root = renderRibbon({ height: 320, collapsed: true })
      .container.querySelector<HTMLElement>(".chronology")!;
    expect(root.style.height).toBe("28px");
  });

  it("renders trip context as one delimited line", () => {
    const { container } = renderRibbon({
      contextRuns: [{ start: 0, end: 0, asset: asset("a", "Sony") }],
    });
    const segment = container.querySelector(".context-segment")!;
    expect(segment.textContent).toContain(" · ");
    expect(segment.querySelector("strong")?.textContent).toBeTruthy();
  });
});

describe("renaming a camera", () => {
  async function openLaneEditor(name: string) {
    const rendered = renderRibbon();
    await userEvent.click(screen.getByRole("button", { name: new RegExp(`^${name}$`) }));
    return { ...rendered, field: screen.getByRole("textbox", { name: `Rename camera ${name}` }) };
  }

  it("commits the new name on Enter", async () => {
    const { props, field } = await openLaneEditor("Sony");
    await userEvent.clear(field);
    await userEvent.type(field, "A7S III{Enter}");
    expect(props.onRenameCamera).toHaveBeenCalledWith("Sony", "A7S III");
  });

  it("commits on blur, so clicking away does not lose the edit", async () => {
    const { props, field } = await openLaneEditor("DJI");
    await userEvent.clear(field);
    await userEvent.type(field, "Pocket 3");
    await userEvent.tab();
    expect(props.onRenameCamera).toHaveBeenCalledWith("DJI", "Pocket 3");
  });

  it("abandons the edit on Escape", async () => {
    const { props, field } = await openLaneEditor("Sony");
    await userEvent.clear(field);
    await userEvent.type(field, "typo{Escape}");
    expect(props.onRenameCamera).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: /^Sony$/ })).toBeTruthy();
  });

  it("writes nothing when the name is unchanged or blank", async () => {
    const { props, field } = await openLaneEditor("Sony");
    await userEvent.type(field, "{Enter}");
    await userEvent.click(screen.getByRole("button", { name: /^Sony$/ }));
    const reopened = screen.getByRole("textbox", { name: "Rename camera Sony" });
    await userEvent.clear(reopened);
    await userEvent.type(reopened, "   {Enter}");
    expect(props.onRenameCamera).not.toHaveBeenCalled();
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

describe("trip context anchoring", () => {
  it("turns an untagged clip column into the two directional choices", async () => {
    const { container } = renderRibbon();
    const track = container.querySelector<HTMLElement>(".context-track")!;
    fireEvent.pointerMove(track, { clientX: 10 });
    await userEvent.click(screen.getByRole("button", { name: "Add context at clip 1" }));
    expect(screen.getByRole("button", { name: "Add through here" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Add from here" })).toBeTruthy();
  });

  it("positions the choices directly above the clicked ghost block", () => {
    const { container } = renderRibbon();
    const track = container.querySelector<HTMLElement>(".context-track")!;
    fireEvent.pointerMove(track, { clientX: 10 });
    const ghost = screen.getByRole("button", { name: "Add context at clip 1" });
    ghost.getBoundingClientRect = () => ({
      x: 380, y: 640, left: 380, top: 640, right: 448, bottom: 656,
      width: 68, height: 16, toJSON: () => ({}),
    });

    fireEvent.click(ghost, { clientX: 412 });

    const chooser = screen.getByRole("group", { name: "Add context at clip 1" });
    expect(chooser.style.left).toBe("412px");
    expect(chooser.style.top).toBe("632px");
    expect(container.querySelector(".context-anchor-popover")).toBeNull();
  });

  it("reports the chosen clip identity instead of a copied timestamp", async () => {
    const { props, container } = renderRibbon();
    fireEvent.pointerMove(container.querySelector<HTMLElement>(".context-track")!, { clientX: 75 });
    await userEvent.click(screen.getByRole("button", { name: "Add context at clip 2" }));
    await userEvent.click(screen.getByRole("button", { name: "Add from here" }));
    expect(props.onRequestContext).toHaveBeenCalledWith("a1", "from", null);
  });

  it("offers both cuts and editing when the hovered clip already has context", async () => {
    const tagged = asset("a", "Sony");
    tagged.phase = "Osaka";
    const run = { start: 0, end: 0, asset: tagged, phaseId: "phase_osaka" };
    const { props, container } = renderRibbon({
      assets: [tagged],
      lanes: [["Sony", [tagged]]],
      contextRuns: [run],
    });
    fireEvent.pointerMove(container.querySelector<HTMLElement>(".context-track")!, { clientX: 10 });
    await userEvent.click(screen.getByRole("button", { name: "Split context at clip 1" }));

    expect(screen.getByRole("button", { name: "New through here" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Edit this context" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "New from here" })).toBeTruthy();

    await userEvent.click(screen.getByRole("button", { name: "New from here" }));
    expect(props.onRequestContext).toHaveBeenCalledWith("a", "from", run);
  });

  it("opens a persisted phase for editing when its segment is clicked", async () => {
    const tagged = asset("a", "Sony");
    tagged.phase = "Osaka";
    const { props } = renderRibbon({
      assets: [tagged],
      lanes: [["Sony", [tagged]]],
      contextRuns: [{ start: 0, end: 0, asset: tagged, phaseId: "phase_osaka" }],
    });
    await userEvent.click(screen.getByRole("button", { name: "Edit Osaka context" }));
    expect(props.onEditContext).toHaveBeenCalledWith("phase_osaka", 0, 0);
  });
});
