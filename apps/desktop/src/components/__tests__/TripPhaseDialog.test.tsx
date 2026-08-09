import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
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
    expect(onSave).toHaveBeenCalledWith(expect.objectContaining({
      label: "Osaka",
      kind: "location",
      timezone: "Asia/Tokyo",
      displayMode: "local",
    }));
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
    expect(onSave).toHaveBeenCalledWith(expect.objectContaining({
      kind: "transit",
      originTimezone: "Asia/Tokyo",
      destinationTimezone: "America/Toronto",
      displayMode: "dual",
    }));
  });

  it("keeps the scrolling fields separate from the fixed actions and closes on Escape", async () => {
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
    await userEvent.keyboard("{Escape}");
    expect(onCancel).toHaveBeenCalledOnce();
  });
});
