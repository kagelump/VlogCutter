import { describe, expect, it } from "vitest";
import type { Asset, AssetAnalysisIndexEntry, TranscriptSegment } from "../types";
import { nextMatch, searchMatch, snippetAround, transcriptMatches } from "./searchMatch";

function asset(overrides: Partial<Asset> = {}): Asset {
  return {
    id: "a",
    filename: "SONY_001.MP4",
    camera: "Sony A7C II",
    title: "",
    description: "",
    notes: "",
    labels: [],
    ...overrides,
  } as unknown as Asset;
}

function entry(transcriptText: string): AssetAnalysisIndexEntry {
  return { transcriptText } as unknown as AssetAnalysisIndexEntry;
}

function segment(startMs: number, text: string): TranscriptSegment {
  return { startMs, endMs: startMs + 2000, language: "en", text, meanTokenProbability: 0.9 };
}

describe("searchMatch", () => {
  it("finds nothing for an empty query", () => {
    expect(searchMatch(asset({ notes: "otaru" }), undefined, "  ")).toBeNull();
  });

  it("finds nothing when the clip does not match", () => {
    expect(searchMatch(asset({ notes: "otaru" }), undefined, "kyoto")).toBeNull();
  });

  it("names the field the match was found in", () => {
    expect(searchMatch(asset({ notes: "the station clock is legible" }), undefined, "clock"))
      .toMatchObject({ field: "note" });
    expect(searchMatch(asset(), entry("we finally found the place"), "finally"))
      .toMatchObject({ field: "transcript" });
    expect(searchMatch(asset({ labels: ["market", "night walk"] }), undefined, "night"))
      .toMatchObject({ field: "label" });
  });

  it("prefers what a person wrote over what a clip happens to carry", () => {
    // A phrase in both a note and the filename is being remembered from the
    // note; saying "filename" there would explain nothing.
    const both = asset({ notes: "otaru harbour at dusk", filename: "OTARU_004.MP4" });
    expect(searchMatch(both, entry("otaru"), "otaru")).toMatchObject({ field: "note" });
    expect(searchMatch(asset({ filename: "OTARU_004.MP4" }), entry("otaru"), "otaru"))
      .toMatchObject({ field: "transcript" });
    expect(searchMatch(asset({ filename: "OTARU_004.MP4" }), undefined, "otaru"))
      .toMatchObject({ field: "filename" });
  });

  it("falls all the way through to the camera, which is a real way to search", () => {
    expect(searchMatch(asset(), undefined, "a7c")).toMatchObject({ field: "camera" });
  });

  it("reports where the match sits inside the snippet, so it can be marked", () => {
    const match = searchMatch(asset({ notes: "the clock is legible" }), undefined, "clock")!;
    expect(match.text.slice(match.offset, match.offset + match.length)).toBe("clock");
  });

  it("matches case-insensitively and reports the original casing", () => {
    const match = searchMatch(asset({ title: "Otaru Canal" }), undefined, "otaru")!;
    expect(match.text.slice(match.offset, match.offset + match.length)).toBe("Otaru");
  });
});

describe("snippetAround", () => {
  it("returns a short value whole", () => {
    expect(snippetAround("the clock", 4)).toEqual({ text: "the clock", offset: 4 });
  });

  it("elides both ends of a long value and keeps the offset accurate", () => {
    const value = `${"a".repeat(200)}clock${"b".repeat(200)}`;
    const { text, offset } = snippetAround(value, 200, 30);
    expect(text.startsWith("…")).toBe(true);
    expect(text.endsWith("…")).toBe(true);
    expect(text.slice(offset, offset + 5)).toBe("clock");
  });

  it("does not elide a leading match, which has nothing before it", () => {
    const { text, offset } = snippetAround(`clock${"b".repeat(200)}`, 0, 30);
    expect(text.startsWith("…")).toBe(false);
    expect(text.slice(offset, offset + 5)).toBe("clock");
  });

  it("keeps a trailing match reachable at the end of the window", () => {
    const value = `${"a".repeat(200)}clock`;
    const { text, offset } = snippetAround(value, 200, 30);
    expect(text.endsWith("…")).toBe(false);
    expect(text.slice(offset, offset + 5)).toBe("clock");
  });
});

describe("transcriptMatches", () => {
  const segments = [
    segment(1000, "we finally found the place"),
    segment(5000, "six seats total"),
    segment(9000, "the Place was closed"),
  ];

  it("returns every matching segment in playback order", () => {
    expect(transcriptMatches(segments, "place")).toEqual([0, 2]);
  });

  it("is empty for a phrase nobody said", () => {
    expect(transcriptMatches(segments, "harbour")).toEqual([]);
    expect(transcriptMatches(segments, " ")).toEqual([]);
  });
});

describe("nextMatch", () => {
  it("starts at the first match", () => {
    expect(nextMatch([2, 7], -1)).toBe(2);
  });

  it("advances", () => {
    expect(nextMatch([2, 7, 9], 2)).toBe(7);
  });

  it("cycles rather than going dead on the last one", () => {
    // "Say that again" is why anyone presses this twice.
    expect(nextMatch([2, 7], 7)).toBe(2);
  });

  it("has nothing to advance to when nothing matched", () => {
    expect(nextMatch([], 3)).toBeNull();
  });

  it("starts over when the remembered match is no longer in the list", () => {
    expect(nextMatch([4, 5], 99)).toBe(4);
  });
});
