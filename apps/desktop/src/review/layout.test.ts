import { describe, expect, it } from "vitest";
import {
  RIBBON_COLLAPSED_HEIGHT,
  ribbonHeight,
  ribbonLaneRows,
  visibleLaneCount,
} from "./layout";

describe("ribbonHeight", () => {
  it("is chrome plus one row per lane at or below the cap", () => {
    expect(ribbonHeight(1, false)).toBe(76);
    expect(ribbonHeight(2, false)).toBe(96);
    expect(ribbonHeight(5, false)).toBe(156);
  });

  it("sizes for a five-camera project without scrolling", () => {
    expect(ribbonHeight(5, false)).toBe(156);
  });

  it("caps at eight lanes; past that the user drags it taller", () => {
    expect(ribbonHeight(8, false)).toBe(216);
    expect(ribbonHeight(9, false)).toBe(216);
    expect(ribbonHeight(24, false)).toBe(216);
  });

  it("is the collapsed height regardless of lane count when collapsed", () => {
    expect(ribbonHeight(2, true)).toBe(RIBBON_COLLAPSED_HEIGHT);
    expect(ribbonHeight(9, true)).toBe(RIBBON_COLLAPSED_HEIGHT);
  });

  it("renders chrome only when there are no lanes at all", () => {
    expect(ribbonHeight(0, false)).toBe(56);
  });

  it("treats a negative or fractional lane count as zero lanes", () => {
    expect(ribbonHeight(-3, false)).toBe(56);
    expect(ribbonHeight(2.7, false)).toBe(96);
  });
});

describe("visibleLaneCount", () => {
  it("never exceeds the cap and never goes below zero", () => {
    expect(visibleLaneCount(0)).toBe(0);
    expect(visibleLaneCount(3)).toBe(3);
    expect(visibleLaneCount(8)).toBe(8);
    expect(visibleLaneCount(11)).toBe(8);
    expect(visibleLaneCount(-1)).toBe(0);
  });
});

describe("ribbonLaneRows", () => {
  it("emits one track per actual lane, not a hardcoded five", () => {
    expect(ribbonLaneRows(2)).toBe("repeat(2, 20px)");
    expect(ribbonLaneRows(7)).toBe("repeat(7, 20px)");
  });

  it("emits nothing when there are no lanes", () => {
    expect(ribbonLaneRows(0)).toBe("");
  });
});
