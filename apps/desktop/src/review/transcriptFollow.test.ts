import { describe, expect, it } from "vitest";
import type { TranscriptSegment } from "../types";
import { activeSegmentIndex, followScrollTop } from "./transcriptFollow";

function segment(startMs: number, endMs: number, text = "…"): TranscriptSegment {
  return { startMs, endMs, language: "en", text, meanTokenProbability: 0.9 };
}

const speech = [segment(4000, 6000), segment(9000, 11_000), segment(20_000, 21_000)];

describe("activeSegmentIndex", () => {
  it("is nothing before the first word is spoken", () => {
    expect(activeSegmentIndex(speech, 0)).toBe(-1);
    expect(activeSegmentIndex(speech, 3.999)).toBe(-1);
  });

  it("is the segment the playhead is inside", () => {
    expect(activeSegmentIndex(speech, 4)).toBe(0);
    expect(activeSegmentIndex(speech, 5.5)).toBe(0);
    expect(activeSegmentIndex(speech, 10)).toBe(1);
  });

  it("holds the last segment through the silence after it", () => {
    // A highlight that blinked off between sentences would be worse than
    // useless in the one clip this feature is for.
    expect(activeSegmentIndex(speech, 7)).toBe(0);
    expect(activeSegmentIndex(speech, 15)).toBe(1);
    expect(activeSegmentIndex(speech, 600)).toBe(2);
  });

  it("has nothing to point at when there is no speech", () => {
    expect(activeSegmentIndex([], 12)).toBe(-1);
  });
});

describe("followScrollTop", () => {
  const viewport = { scrollTop: 100, viewportHeight: 100 };

  it("leaves a row that is already on screen alone", () => {
    // Scrolling a list to move something the reader can already see is how
    // auto-follow features earn their reputation.
    expect(followScrollTop({ itemTop: 120, itemHeight: 22, ...viewport })).toBeNull();
    expect(followScrollTop({ itemTop: 100, itemHeight: 22, ...viewport })).toBeNull();
    expect(followScrollTop({ itemTop: 178, itemHeight: 22, ...viewport })).toBeNull();
  });

  it("brings a row above the viewport back to the top edge", () => {
    expect(followScrollTop({ itemTop: 40, itemHeight: 22, ...viewport })).toBe(40);
  });

  it("brings a row below the viewport back to the bottom edge, and no further", () => {
    expect(followScrollTop({ itemTop: 240, itemHeight: 22, ...viewport })).toBe(162);
  });

  it("never scrolls above the top of the list", () => {
    expect(followScrollTop({ itemTop: 10, itemHeight: 400, scrollTop: 50, viewportHeight: 100 })).toBe(10);
    expect(followScrollTop({ itemTop: 0, itemHeight: 22, scrollTop: 30, viewportHeight: 100 })).toBe(0);
  });

  it("does nothing when the list has not been measured yet", () => {
    expect(followScrollTop({ itemTop: 500, itemHeight: 22, scrollTop: 0, viewportHeight: 0 })).toBeNull();
  });
});
