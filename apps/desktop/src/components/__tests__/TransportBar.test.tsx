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
    onResetRotation: vi.fn(),
    onEnterFocus: vi.fn(),
    ...overrides,
  };
  return { props, ...render(<TransportBar {...props} />) };
}

describe("frame orientation menu", () => {
  it("offers a direct 180-degree correction", async () => {
    const { props } = renderBar();

    await userEvent.click(screen.getByRole("button", { name: "Frame orientation" }));
    const flip = screen.getByRole("menuitem", { name: "Flip 180°" });
    expect(flip.getAttribute("aria-keyshortcuts")).toBe("Shift+O");
    await userEvent.click(flip);

    expect(props.onRotate).toHaveBeenCalledOnce();
    expect(props.onRotate).toHaveBeenCalledWith(180);
  });

  it("shows the current override and can reset to camera metadata", async () => {
    const current = asset({ defaultRotationDegrees: 90, rotationDegrees: 270 });
    const { props } = renderBar({ asset: current, rotationOverridden: true });

    await userEvent.click(screen.getByRole("button", { name: "Frame orientation" }));
    expect(screen.getByText("Current: 270° override")).toBeTruthy();
    expect(screen.getByRole("menuitemradio", { name: /Automatic/ }).getAttribute("aria-checked")).toBe("false");

    await userEvent.click(screen.getByRole("menuitemradio", { name: /Automatic/ }));
    expect(props.onResetRotation).toHaveBeenCalledOnce();
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
