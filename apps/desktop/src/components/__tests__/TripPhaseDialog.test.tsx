import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { PhaseKind, TripPhase } from "../../chronologyTypes";
import type { Asset } from "../../types";
import { TripPhaseDialog } from "../TripPhaseDialog";

function asset(id: string, filename: string): Asset {
  return {
    id, projectId: "p", path: `/m/${filename}`, filename, fingerprint: id,
    sizeBytes: 1, durationSeconds: 5, frameRate: 30, width: 1920, height: 1080,
    codec: "h264", camera: "Sony", capturedAtRaw: "2026-01-01T00:00:00Z",
    bestTime: "2026-01-01T00:00:00Z", offsetSeconds: 0, proxyPath: null,
    proxyRepresentation: null, metadataRotationDegrees: 0, defaultRotationDegrees: 0,
    rotationDegrees: 0, orientationProbed: true, originalAvailable: true,
    proxyAvailable: false, thumbnailPath: null, thumbnailQuality: 0, notes: "",
    title: "", description: "", labels: [], removed: false, mediaObjectId: null,
    mediaVerificationState: "unlinked", available: true,
  };
}

const boundaryAssets = [
  asset("a", "A0001.MP4"),
  asset("b", "A0002.MP4"),
  asset("c", "A0003.MP4"),
  asset("d", "A0004.MP4"),
  asset("e", "A0005.MP4"),
];

function persistedPhase(kind: PhaseKind): TripPhase {
  return {
    id: `phase_${kind}`,
    label: kind === "location" ? "Osaka" : "Train to Kyoto",
    kind,
    startAnchorAssetId: "b",
    endAnchorAssetId: "d",
    startTripTime: "2026-01-01T00:00:00Z",
    endTripTime: "2026-01-01T01:00:00Z",
    timezone: kind === "location" ? "Asia/Tokyo" : null,
    originTimezone: kind === "transit" ? "Asia/Tokyo" : null,
    destinationTimezone: kind === "transit" ? "Asia/Tokyo" : null,
    displayMode: kind === "location" ? "local" : "dual",
  };
}

describe("TripPhaseDialog", () => {
  it("previews the anchored range and submits a location", async () => {
    const onSave = vi.fn();
    render(
      <TripPhaseDialog
        phase={null}
        start={asset("a", "A0001.MP4")}
        end={asset("b", "A0009.MP4")}
        startPosition={3}
        endPosition={11}
        busy={false}
        error=""
        onCancel={vi.fn()}
        onSave={onSave}
      />,
    );
    expect(screen.getByLabelText("Selected clip range").textContent).toContain("9 clips");
    await userEvent.type(screen.getByLabelText("Name"), "Osaka");
    await userEvent.type(screen.getByRole("textbox", { name: /^Timezone/ }), "Asia/Tokyo");
    await userEvent.click(screen.getByRole("button", { name: "Add phase" }));
    expect(onSave).toHaveBeenCalledWith(
      expect.objectContaining({
        label: "Osaka",
        kind: "location",
        timezone: "Asia/Tokyo",
        displayMode: "local",
      }),
      { startPosition: 3, endPosition: 11 },
    );
  });

  it("collects both timezone sides for transit", async () => {
    const onSave = vi.fn();
    render(
      <TripPhaseDialog
        phase={null}
        start={asset("a", "A0001.MP4")}
        end={asset("b", "A0009.MP4")}
        startPosition={0}
        endPosition={1}
        busy={false}
        error=""
        onCancel={vi.fn()}
        onSave={onSave}
      />,
    );
    await userEvent.click(screen.getByRole("radio", { name: /^Transit/ }));
    await userEvent.type(screen.getByLabelText("Name"), "Flight to Toronto");
    await userEvent.type(screen.getByRole("textbox", { name: /^Origin timezone/ }), "Asia/Tokyo");
    await userEvent.type(screen.getByRole("textbox", { name: /^Destination timezone/ }), "America/Toronto");
    await userEvent.click(screen.getByRole("button", { name: "Add phase" }));
    expect(onSave).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: "transit",
        originTimezone: "Asia/Tokyo",
        destinationTimezone: "America/Toronto",
        displayMode: "dual",
      }),
      { startPosition: 0, endPosition: 1 },
    );
  });

  it("snaps a Location start to its end instead of allowing a crossing, then saves the range", async () => {
    const onSave = vi.fn();
    render(
      <TripPhaseDialog
        phase={persistedPhase("location")}
        start={boundaryAssets[1]}
        end={boundaryAssets[3]}
        assets={boundaryAssets}
        startPosition={1}
        endPosition={3}
        busy={false}
        error=""
        onCancel={vi.fn()}
        onSave={onSave}
      />,
    );

    fireEvent.change(screen.getByRole("slider", { name: /Start clip/ }), { target: { value: "4" } });
    expect(screen.getByLabelText("Selected clip range")).toHaveTextContent("#4 · A0004.MP4");
    expect(screen.getByLabelText("Selected clip range")).toHaveTextContent("1 clip");

    await userEvent.click(screen.getByRole("button", { name: "Save phase" }));
    expect(onSave).toHaveBeenCalledWith(
      expect.objectContaining({ kind: "location", timezone: "Asia/Tokyo" }),
      { startPosition: 3, endPosition: 3 },
    );
  });

  it("snaps a Transit end to its start instead of allowing a crossing, then saves both sides", async () => {
    const onSave = vi.fn();
    render(
      <TripPhaseDialog
        phase={persistedPhase("transit")}
        start={boundaryAssets[1]}
        end={boundaryAssets[3]}
        assets={boundaryAssets}
        startPosition={1}
        endPosition={3}
        busy={false}
        error=""
        onCancel={vi.fn()}
        onSave={onSave}
      />,
    );

    fireEvent.change(screen.getByRole("slider", { name: /Start clip/ }), { target: { value: "2" } });
    fireEvent.change(screen.getByRole("slider", { name: /End clip/ }), { target: { value: "0" } });
    expect(screen.getByLabelText("Selected clip range")).toHaveTextContent("#3 · A0003.MP4");
    expect(screen.getByLabelText("Selected clip range")).toHaveTextContent("1 clip");

    await userEvent.click(screen.getByRole("button", { name: "Save phase" }));
    expect(onSave).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: "transit",
        originTimezone: "Asia/Tokyo",
        destinationTimezone: "Asia/Tokyo",
        displayMode: "dual",
      }),
      { startPosition: 2, endPosition: 2 },
    );
  });

  it.each(["location", "transit"] as const)("cancels a %s boundary draft without saving", async (kind) => {
    const onCancel = vi.fn();
    const onSave = vi.fn();
    render(
      <TripPhaseDialog
        phase={persistedPhase(kind)}
        start={boundaryAssets[1]}
        end={boundaryAssets[3]}
        assets={boundaryAssets}
        startPosition={1}
        endPosition={3}
        busy={false}
        error=""
        onCancel={onCancel}
        onSave={onSave}
      />,
    );

    fireEvent.change(screen.getByRole("slider", { name: /Start clip/ }), { target: { value: "2" } });
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onCancel).toHaveBeenCalledOnce();
    expect(onSave).not.toHaveBeenCalled();
  });

  it("keeps the scrolling fields separate from the fixed actions", () => {
    const onCancel = vi.fn();
    const { container } = render(
      <TripPhaseDialog
        phase={null}
        start={asset("a", "A0001_WITH_A_LONG_CAMERA_FILENAME.MP4")}
        end={asset("b", "A0099_WITH_A_LONG_CAMERA_FILENAME.MP4")}
        startPosition={0}
        endPosition={98}
        busy={false}
        error=""
        onCancel={onCancel}
        onSave={vi.fn()}
      />,
    );

    expect(container.querySelector(".context-modal-body")?.contains(screen.getByLabelText("Selected clip range"))).toBe(true);
    expect(container.querySelector(".context-modal-actions")?.contains(screen.getByRole("button", { name: "Cancel" }))).toBe(true);
    // Escape is the shell's to resolve now; see the escape-stack CUJ.
    expect(onCancel).not.toHaveBeenCalled();
  });

  it("returns focus to the boundary control that opened it", async () => {
    const opener = document.createElement("button");
    opener.textContent = "Move phase start";
    document.body.append(opener);
    opener.focus();
    const { unmount } = render(
      <TripPhaseDialog
        phase={persistedPhase("location")}
        start={boundaryAssets[1]}
        end={boundaryAssets[3]}
        assets={boundaryAssets}
        startPosition={1}
        endPosition={3}
        busy={false}
        error=""
        onCancel={vi.fn()}
        onSave={vi.fn()}
      />,
    );
    expect(screen.getByLabelText("Name")).toHaveFocus();
    unmount();
    expect(opener).toHaveFocus();
    opener.remove();
  });
});
