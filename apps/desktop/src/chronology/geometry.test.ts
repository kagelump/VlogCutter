import { describe, expect, it } from "vitest";
import type { ChronologyOverview } from "../chronologyTypes";
import { chronologyLanes, contiguousRange, formatChronologyTime, ordinalWindow } from "./geometry";

function overview(): ChronologyOverview {
  const sequences = [
    { id: "card-a", cameraKey: "Sony", label: "Sony card A", volumeHint: "", filenamePrefix: "C" },
    { id: "card-b", cameraKey: "Sony", label: "Sony card B", volumeHint: "", filenamePrefix: "C" },
  ];
  return {
    sequences,
    epochs: [], exceptions: [], phases: [], constraints: [], contradictions: [],
    proposedSplits: [], unplacedAssets: 0,
    assets: ["a", "b", "c"].map((assetId, index) => ({
      assetId, rawTime: null, tripTime: null, displayTime: null, confidence: 0,
      evidence: [], uncertaintySeconds: 0,
      orderKey: {
        tripTimeMillis: index,
        captureSequenceId: index < 2 ? "card-a" : "card-b",
        filenameCounter: index,
        filename: `${assetId}.mp4`,
        assetId,
      },
    })),
  };
}

describe("chronology geometry", () => {
  it("keeps two cards from one camera in distinct lanes", () => {
    expect(chronologyLanes(overview()).map((lane) => lane.sequence.id)).toEqual(["card-a", "card-b"]);
  });

  it("windows thousands of ordinal columns with overscan", () => {
    expect(ordinalWindow(3_000, 68, 6_800, 1_000, 4)).toEqual({ start: 96, end: 119 });
  });

  it("allows a contiguous run in one sequence and rejects a cross-card run", () => {
    expect(contiguousRange(overview(), "a", "b")).toEqual(["a", "b"]);
    expect(contiguousRange(overview(), "b", "c")).toBeNull();
  });

  it("preserves the solver-resolved wall clock instead of using the laptop timezone", () => {
    expect(formatChronologyTime("2026-02-08T09:14:08+09:00", "Asia/Tokyo"))
      .toBe("Feb 8, 09:14:08 · Asia/Tokyo");
    expect(formatChronologyTime("2026-02-08T00:14:08Z"))
      .toBe("Feb 8, 00:14:08 · UTC");
  });
});
