import { describe, expect, it } from "vitest";
import type { ChronologyImpact, ChronologyOverview, TripPhase } from "../chronologyTypes";
import {
  changedOrdinalExtent,
  chronologyLanes,
  contiguousRange,
  formatChronologyTime,
  ordinalWindow,
  phaseOrdinalRange,
} from "./geometry";

function phase(overrides: Partial<TripPhase> = {}): TripPhase {
  return {
    id: "phase-tokyo",
    label: "Tokyo",
    kind: "location",
    startAnchorAssetId: null,
    endAnchorAssetId: null,
    startTripTime: "",
    endTripTime: "",
    timezone: "Asia/Tokyo",
    originTimezone: null,
    destinationTimezone: null,
    displayMode: "local",
    ...overrides,
  };
}

function impact(changes: ChronologyImpact["changes"]): ChronologyImpact {
  return { changes, contradictions: [], blocked: false };
}

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

  it("names the offset when no timezone came back, and keeps sub-second precision out of the label", () => {
    expect(formatChronologyTime("2026-02-08T09:14:08.250+09:00")).toBe("Feb 8, 09:14:08 · UTC+09:00");
    expect(formatChronologyTime("2026-02-08T09:14:08+09:00", "   ")).toBe("Feb 8, 09:14:08 · UTC+09:00");
  });

  it("says so when a clip has no camera time, and passes through anything it cannot parse", () => {
    expect(formatChronologyTime(null)).toBe("No camera time");
    expect(formatChronologyTime("")).toBe("No camera time");
    expect(formatChronologyTime("sometime on the ferry")).toBe("sometime on the ferry");
  });

  it("still renders a timestamp that parses but carries no offset, without claiming a zone", () => {
    // No trailing Z or ±HH:MM, so there is no wall clock to trust and the
    // browser's own formatting is the honest fallback.
    const rendered = formatChronologyTime("2026-02-08T09:14:08");
    expect(rendered).not.toContain("·");
    expect(rendered).toMatch(/Feb 8/);
  });

  it("gives clips the solver placed outside every declared card their own lane", () => {
    const withStray = overview();
    withStray.assets[2].orderKey.captureSequenceId = "card-vanished";
    const lanes = chronologyLanes(withStray);
    expect(lanes.map((lane) => lane.sequence.id)).toEqual(["card-a", "card-vanished"]);
    expect(lanes[1].sequence.label).toBe("Unassigned clips");
    expect(lanes[1].assets.map((asset) => asset.assetId)).toEqual(["c"]);
  });

  it("drops a declared card that ended up with no clips rather than drawing an empty lane", () => {
    const empty = overview();
    empty.assets = [];
    expect(chronologyLanes(empty)).toEqual([]);
  });

  describe("phase spans", () => {
    it("prefers the phase's own anchors over the clips it happens to cover", () => {
      const placed = overview();
      expect(phaseOrdinalRange(
        phase({ startAnchorAssetId: "a", endAnchorAssetId: "c" }),
        placed,
      )).toEqual({ start: 0, end: 2 });
    });

    it("falls back to the first and last clip the solver put in the phase", () => {
      const placed = overview();
      for (const index of [1, 2]) {
        placed.assets[index].displayTime = {
          primary: "2026-02-08T00:00:00Z",
          secondary: null,
          timezone: "Asia/Tokyo",
          phaseId: "phase-tokyo",
        };
      }
      expect(phaseOrdinalRange(phase(), placed)).toEqual({ start: 1, end: 2 });
    });

    it("returns nothing for a phase no clip belongs to, so the band draws no ghost", () => {
      expect(phaseOrdinalRange(phase(), overview())).toBeNull();
      expect(phaseOrdinalRange(
        phase({ startAnchorAssetId: "a", endAnchorAssetId: "not-in-this-project" }),
        overview(),
      )).toBeNull();
    });
  });

  describe("impact extent", () => {
    it("spans every position a pending change touches, before and after", () => {
      expect(changedOrdinalExtent(impact([
        { assetId: "b", before: null, after: null, beforePosition: 4, afterPosition: 1 },
        { assetId: "c", before: null, after: null, beforePosition: 2, afterPosition: 7 },
      ]))).toEqual({ start: 1, end: 7 });
    });

    it("has no extent to highlight when nothing is pending or nothing moves", () => {
      expect(changedOrdinalExtent(null)).toBeNull();
      expect(changedOrdinalExtent(impact([]))).toBeNull();
    });
  });

  describe("the ordinal window", () => {
    it("collapses to nothing rather than dividing by a zero column width", () => {
      expect(ordinalWindow(0, 68, 0, 900)).toEqual({ start: 0, end: 0 });
      expect(ordinalWindow(300, 0, 0, 900)).toEqual({ start: 0, end: 0 });
    });

    it("never runs past the last clip or before the first", () => {
      expect(ordinalWindow(10, 68, 0, 900)).toEqual({ start: 0, end: 10 });
      expect(ordinalWindow(10, 68, 100_000, 900, 0)).toEqual({ start: 10, end: 10 });
    });
  });
});
