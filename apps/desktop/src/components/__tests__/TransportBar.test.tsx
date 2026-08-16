import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { Asset } from "../../types";
import { TransportBar, type TransportBarProps } from "../TransportBar";

function asset(overrides: Partial<Asset> = {}): Asset {
  return {
    id: "a", projectId: "p", path: "/media/a.mp4", filename: "a.mp4", fingerprint: "fp_a",
    sizeBytes: 1_000_000, durationSeconds: 75, frameRate: 29.97, width: 1920, height: 1080,
    codec: "h264", camera: "Sony", capturedAtRaw: null, bestTime: null, offsetSeconds: 0,
    proxyPath: null, proxyRepresentation: null, metadataRotationDegrees: 0,
    defaultRotationDegrees: 0, rotationDegrees: 0, orientationProbed: true,
    originalAvailable: true, proxyAvailable: false, thumbnailPath: null, thumbnailQuality: 0,
    notes: "", title: "", description: "", labels: [], removed: false,
    mediaObjectId: null, mediaVerificationState: "unlinked", available: true,
    ...overrides,
  } as Asset;
}

function renderBar(overrides: Partial<TransportBarProps> = {}) {
  const props: TransportBarProps = {
    asset: asset(),
    playing: false,
    muted: false,
    positionSeconds: 5,
    durationSeconds: 75,
    bestMomentMs: null,
    segmentStartsMs: [],
    rotationOverridden: false,
    onTogglePlay: vi.fn(),
    onSeek: vi.fn(),
    onSeekBy: vi.fn(),
    onToggleMute: vi.fn(),
    onStep: vi.fn(),
    onRotate: vi.fn(),
    onSetRotation: vi.fn(),
    onResetRotation: vi.fn(),
    onEnterFocus: vi.fn(),
    ...overrides,
  };
  return { props, ...render(<TransportBar {...props} />) };
}

describe("frame orientation menu", () => {
  it("offers Auto and all four cardinal orientations as direct choices", async () => {
    renderBar({ asset: asset({ defaultRotationDegrees: 90 }) });

    await userEvent.click(screen.getByRole("button", { name: "Frame orientation" }));

    // Someone correcting a wrong guess knows which way up the clip is; the
    // control this replaces made them count clicks to get there.
    const choices = screen.getAllByRole("menuitemradio").map((item) => item.textContent);
    expect(choices).toEqual([
      "Auto (90°)Use camera metadata",
      "0°",
      "90°same as automatic",
      "180°",
      "270°",
    ]);
  });

  it("names the inferred angle on Auto, and marks it current when nothing overrides it", async () => {
    renderBar({ asset: asset({ defaultRotationDegrees: 270, rotationDegrees: 270 }), rotationOverridden: false });

    await userEvent.click(screen.getByRole("button", { name: "Frame orientation" }));

    expect(screen.getByRole("menuitemradio", { name: /Auto \(270°\)/ }).getAttribute("aria-checked")).toBe("true");
    expect(screen.getByText("Following camera metadata")).toBeTruthy();
  });

  it("marks the overriding angle instead, and says what it is overriding", async () => {
    renderBar({
      asset: asset({ defaultRotationDegrees: 90, rotationDegrees: 270 }),
      rotationOverridden: true,
    });

    await userEvent.click(screen.getByRole("button", { name: "Frame orientation" }));

    expect(screen.getByRole("menuitemradio", { name: "270°" }).getAttribute("aria-checked")).toBe("true");
    expect(screen.getByRole("menuitemradio", { name: /Auto/ }).getAttribute("aria-checked")).toBe("false");
    expect(screen.getByText("Overriding automatic 90°")).toBeTruthy();
  });

  it("sets an absolute angle rather than a relative rotation", async () => {
    const { props } = renderBar({ asset: asset({ defaultRotationDegrees: 90, rotationDegrees: 90 }) });

    await userEvent.click(screen.getByRole("button", { name: "Frame orientation" }));
    await userEvent.click(screen.getByRole("menuitemradio", { name: "180°" }));

    expect(props.onSetRotation).toHaveBeenCalledWith(180);
    expect(props.onRotate).not.toHaveBeenCalled();
  });

  it("clears the override through Auto rather than storing another angle", async () => {
    const { props } = renderBar({
      asset: asset({ defaultRotationDegrees: 90, rotationDegrees: 270 }),
      rotationOverridden: true,
    });

    await userEvent.click(screen.getByRole("button", { name: "Frame orientation" }));
    await userEvent.click(screen.getByRole("menuitemradio", { name: /Auto/ }));

    expect(props.onResetRotation).toHaveBeenCalledOnce();
    expect(props.onSetRotation).not.toHaveBeenCalled();
  });

  it("keeps the keyboard rotations advertised even though they are not choices", async () => {
    renderBar();

    await userEvent.click(screen.getByRole("button", { name: "Frame orientation" }));

    // The shortcuts are the fast path and predate the chooser; the menu says
    // they exist without pretending a relative nudge is an orientation.
    const keys = screen.getByRole("menu", { name: "Frame orientation" }).querySelector(".orientation-menu-keys");
    expect(keys?.textContent).toContain("O");
    expect(keys?.textContent).toContain("180");
  });

  it("distinguishes an overridden clip on the transport glyph itself", () => {
    const { rerender, props } = renderBar({ asset: asset({ defaultRotationDegrees: 90 }) });
    const glyph = () => screen.getByRole("button", { name: "Frame orientation" });
    expect(glyph().className).not.toContain("on");
    expect(glyph().getAttribute("title")).toContain("Automatic orientation (90°)");

    rerender(
      <TransportBar
        {...props}
        asset={asset({ defaultRotationDegrees: 90, rotationDegrees: 180 })}
        rotationOverridden
      />,
    );

    expect(glyph().className).toContain("on");
    expect(glyph().getAttribute("title")).toContain("set to 180°");
  });

  it("closes with Escape and returns focus to the transport control", async () => {
    renderBar();
    const trigger = screen.getByRole("button", { name: "Frame orientation" });
    await userEvent.click(trigger);

    await userEvent.keyboard("{Escape}");

    expect(screen.queryByRole("menu", { name: "Frame orientation" })).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });
});
