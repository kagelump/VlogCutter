import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
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
