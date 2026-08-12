import { describe, expect, it } from "vitest";
import type { AssetChronology, ChronologyImpact, ChronologyOverview, TripPhase } from "../chronologyTypes";
import {
  changedOrdinalExtent,
  chronologyLanes,
  chronologyTimeTicks,
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
    proposedSplits: [], declinedSplits: [], unplacedAssets: 0,
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

  describe("the time ruler", () => {
    /** `count` clips, one every `spacingMinutes`, from a fixed Tokyo instant. */
    function placed(count: number, spacingMinutes = 10, startHour = 9): AssetChronology[] {
      const base = Date.UTC(2026, 1, 8, startHour - 9, 0, 0);
      return Array.from({ length: count }, (_, index) => {
        const at = new Date(base + index * spacingMinutes * 60_000);
        const local = new Date(at.valueOf() + 9 * 3_600_000);
        const stamp = `${local.toISOString().slice(0, 19)}+09:00`;
        return {
          assetId: `a${index}`,
          rawTime: stamp,
          tripTime: stamp,
          displayTime: { primary: stamp, secondary: null, timezone: "Asia/Tokyo", phaseId: null },
          confidence: 1,
          evidence: [],
          uncertaintySeconds: 0,
          orderKey: {
            tripTimeMillis: at.valueOf(),
            captureSequenceId: "card-a",
            filenameCounter: index,
            filename: `a${index}.mp4`,
            assetId: `a${index}`,
          },
        };
      });
    }

    it("spaces ticks by the requested pixel gap rather than by clip", () => {
      // 140px of gap over 68px columns is one tick every three clips.
      const ticks = chronologyTimeTicks(placed(12), 68, { start: 0, end: 12 }, 140);
      expect(ticks.map((tick) => tick.index)).toEqual([0, 3, 6, 9]);
      expect(ticks.map((tick) => tick.label)).toEqual(["Feb 8", "09:30", "10:00", "10:30"]);
    });

    it("keeps tick positions stable as the window scrolls past them", () => {
      // Anchored to absolute ordinals, not to the window: a ruler whose ticks
      // slid around under scrolling would be unreadable.
      const ticks = chronologyTimeTicks(placed(24), 68, { start: 9, end: 18 }, 140);
      expect(ticks.map((tick) => tick.index)).toEqual([9, 12, 15]);
    });

    it("marks the day rolling over even between two stride ticks", () => {
      // Six clips an hour apart across midnight; the boundary lands at index 4,
      // which no stride tick would have covered.
      const ticks = chronologyTimeTicks(placed(6, 60, 20), 68, { start: 0, end: 6 }, 140);
      const boundary = ticks.find((tick) => tick.isDayBoundary && tick.index > 0);
      expect(boundary).toMatchObject({ index: 4, label: "Feb 9" });
    });

    it("suppresses a stride tick that would collide with a day boundary", () => {
      // The stride tick at 3 sits one 68px column from the midnight boundary at
      // 4. Both labels cannot fit there, and the new day is the more useful of
      // the two, so the regular tick gives way rather than overprinting it.
      const ticks = chronologyTimeTicks(placed(6, 60, 20), 68, { start: 0, end: 6 }, 140);
      expect(ticks.map((tick) => tick.index)).toEqual([0, 4]);
      expect(ticks.map((tick) => tick.isDayBoundary)).toEqual([true, true]);
    });

    it("labels the first tick with a date, so the ruler is never undated", () => {
      const [first] = chronologyTimeTicks(placed(12), 68, { start: 6, end: 12 }, 140);
      expect(first).toMatchObject({ index: 6, label: "Feb 8", isDayBoundary: true });
    });

    it("skips clips the solver could not place rather than labelling them", () => {
      const assets = placed(9);
      for (const index of [3, 4, 5]) {
        assets[index].displayTime = null;
        assets[index].tripTime = null;
      }
      const ticks = chronologyTimeTicks(assets, 68, { start: 0, end: 9 }, 140);
      expect(ticks.map((tick) => tick.index)).toEqual([0, 6]);
    });

    it("falls back to trip time when no display time was resolved", () => {
      const assets = placed(4);
      for (const asset of assets) asset.displayTime = null;
      expect(chronologyTimeTicks(assets, 68, { start: 0, end: 4 }, 140)[0].label).toBe("Feb 8");
    });

    it("names the city where the display timezone changes", () => {
      // Found on the real 2026 Eclipse project: the trip flies Tokyo → Madrid,
      // so the displayed wall clock jumps *backwards* across the transit and a
      // ruler reading "16:46 … 14:25" looks like the solver contradicting
      // itself. The zone marker is what makes the drop legible.
      const assets = placed(9);
      for (let index = 5; index < 9; index += 1) {
        const clock = assets[index].displayTime!.primary.replace("+09:00", "+02:00");
        assets[index].displayTime = {
          primary: clock.replace(/T(\d{2})/, (_, hour) => `T${String(Number(hour) - 7).padStart(2, "0")}`),
          secondary: null,
          timezone: "Europe/Madrid",
          phaseId: null,
        };
      }
      const ticks = chronologyTimeTicks(assets, 68, { start: 0, end: 9 }, 140);
      const arrival = ticks.find((tick) => tick.index === 5);
      expect(arrival).toMatchObject({ zone: "Madrid" });
      // And nothing before the change is labelled with a city it is not in.
      expect(ticks.filter((tick) => tick.index < 5).every((tick) => tick.zone === null || tick.zone === "Tokyo"))
        .toBe(true);
      expect(ticks.filter((tick) => tick.index > 5).every((tick) => tick.zone === null)).toBe(true);
    });

    it("expands an underscored zone into something readable", () => {
      const assets = placed(3);
      for (const asset of assets) {
        asset.displayTime = { ...asset.displayTime!, timezone: "America/Los_Angeles" };
      }
      expect(chronologyTimeTicks(assets, 68, { start: 0, end: 3 }, 140)[0].zone).toBe("Los Angeles");
    });

    it("says nothing about zones on a trip that never leaves one", () => {
      const ticks = chronologyTimeTicks(placed(12), 68, { start: 0, end: 12 }, 140);
      expect(ticks.slice(1).every((tick) => tick.zone === null)).toBe(true);
    });

    it("draws no ruler for an empty project or a zero-width column", () => {
      expect(chronologyTimeTicks([], 68, { start: 0, end: 0 }, 140)).toEqual([]);
      expect(chronologyTimeTicks(placed(9), 0, { start: 0, end: 9 }, 140)).toEqual([]);
    });

    it("never asks for a stride below one clip, however wide the columns get", () => {
      const ticks = chronologyTimeTicks(placed(4), 400, { start: 0, end: 4 }, 140);
      expect(ticks.map((tick) => tick.index)).toEqual([0, 1, 2, 3]);
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
