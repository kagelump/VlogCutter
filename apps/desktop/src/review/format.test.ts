import { describe, expect, it } from "vitest";
import type { Asset } from "../types";
import {
  clipTitle,
  formatCost,
  formatDate,
  formatDuration,
  formatEta,
  formatTimecode,
  syncStateLabel,
} from "./format";

function asset(overrides: Partial<Asset> = {}): Asset {
  return { title: "", filename: "C0001.MP4", ...overrides } as Asset;
}

describe("clipTitle", () => {
  it("prefers the analysis title", () => {
    expect(clipTitle(asset({ title: "Ramen alley night walk" }))).toBe("Ramen alley night walk");
  });

  it("falls back to the filename when the title is blank or whitespace", () => {
    expect(clipTitle(asset({ title: "" }))).toBe("C0001.MP4");
    expect(clipTitle(asset({ title: "   " }))).toBe("C0001.MP4");
  });
});

describe("formatCost", () => {
  it("names the budget rather than printing $NaN for a non-finite value", () => {
    expect(formatCost(Number.NaN)).toBe("the run budget");
    expect(formatCost(Number.POSITIVE_INFINITY)).toBe("the run budget");
  });

  it("distinguishes nothing spent from a sub-cent spend", () => {
    expect(formatCost(0)).toBe("$0.00");
    expect(formatCost(0.004)).toBe("<$0.01");
    expect(formatCost(1.5)).toBe("$1.50");
  });
});

describe("formatTimecode", () => {
  it("renders milliseconds as m:ss", () => {
    expect(formatTimecode(0)).toBe("0:00");
    expect(formatTimecode(9_500)).toBe("0:10");
    expect(formatTimecode(125_000)).toBe("2:05");
  });

  it("floors a negative position at zero", () => {
    expect(formatTimecode(-5_000)).toBe("0:00");
  });
});

describe("formatDuration", () => {
  it("renders seconds as m:ss, truncating rather than rounding", () => {
    expect(formatDuration(0)).toBe("0:00");
    expect(formatDuration(9.9)).toBe("0:09");
    expect(formatDuration(125)).toBe("2:05");
  });

  it("treats a non-finite or negative duration as zero", () => {
    expect(formatDuration(Number.NaN)).toBe("0:00");
    expect(formatDuration(-12)).toBe("0:00");
  });
});

describe("formatEta", () => {
  it("says it is estimating when there is no figure yet", () => {
    expect(formatEta(null)).toBe("Estimating…");
    expect(formatEta(Number.NaN)).toBe("Estimating…");
  });

  it("steps the unit up as the remaining time grows", () => {
    expect(formatEta(45)).toBe("45s remaining");
    expect(formatEta(90)).toBe("2m remaining");
    expect(formatEta(7_200)).toBe("2h 0m remaining");
  });
});

describe("syncStateLabel", () => {
  it("says each known state the way a person would", () => {
    expect(syncStateLabel("synced")).toBe("Synced");
    expect(syncStateLabel("credential_revoked")).toBe("Credential revoked");
  });

  it("passes an unknown state through rather than blanking it", () => {
    expect(syncStateLabel("quarantined")).toBe("quarantined");
  });
});

describe("formatDate", () => {
  it("names the absence rather than rendering an empty slot", () => {
    expect(formatDate(null)).toBe("No camera time");
    expect(formatDate("")).toBe("No camera time");
  });

  it("passes an unparseable value through unchanged", () => {
    expect(formatDate("not a date")).toBe("not a date");
  });

  it("formats a real timestamp without a year", () => {
    const formatted = formatDate("2026-08-04T19:30:00Z");
    expect(formatted).not.toContain("2026");
    expect(formatted).toMatch(/\d{2}:\d{2}:\d{2}/);
  });
});
