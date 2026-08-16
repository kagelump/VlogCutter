import { describe, expect, it } from "vitest";
import type { Asset } from "../types";
import { canonicalLabel, projectLabelVocabulary, suggestLabels } from "./labelVocabulary";

function clip(id: string, labels: string[]): Asset {
  return { id, labels } as unknown as Asset;
}

describe("projectLabelVocabulary", () => {
  it("counts every use and puts the most-used first", () => {
    expect(projectLabelVocabulary([
      clip("a", ["market", "night"]),
      clip("b", ["market"]),
      clip("c", ["market", "night"]),
    ])).toEqual([
      { label: "market", count: 3 },
      { label: "night", count: 2 },
    ]);
  });

  it("breaks a tie alphabetically, so the same project reads the same twice", () => {
    expect(projectLabelVocabulary([clip("a", ["zoo", "arcade"])]).map((use) => use.label))
      .toEqual(["arcade", "zoo"]);
  });

  it("treats casings as one label and offers the spelling the project uses most", () => {
    // The fragmentation this exists to prevent: three spellings, one concept.
    expect(projectLabelVocabulary([
      clip("a", ["Night Walk"]),
      clip("b", ["night walk"]),
      clip("c", ["night walk"]),
    ])).toEqual([{ label: "night walk", count: 3 }]);
  });

  it("ignores blank and whitespace-only labels", () => {
    expect(projectLabelVocabulary([clip("a", ["  ", "", "market"])]))
      .toEqual([{ label: "market", count: 1 }]);
  });

  it("is empty for a project with no labels yet", () => {
    expect(projectLabelVocabulary([clip("a", [])])).toEqual([]);
  });
});

describe("suggestLabels", () => {
  const vocabulary = projectLabelVocabulary([
    clip("a", ["night walk", "market", "night train"]),
    clip("b", ["night walk", "market"]),
    clip("c", ["night walk", "harbour at night"]),
  ]);

  it("matches anywhere in the label but ranks prefixes first", () => {
    expect(suggestLabels(vocabulary, "night").map((use) => use.label))
      .toEqual(["night walk", "night train", "harbour at night"]);
  });

  it("ignores case, which is the whole point", () => {
    expect(suggestLabels(vocabulary, "NIGHT W").map((use) => use.label)).toEqual(["night walk"]);
  });

  it("offers the whole vocabulary before anything is typed", () => {
    expect(suggestLabels(vocabulary, "")).toHaveLength(4);
  });

  it("leaves out labels the clip already carries", () => {
    // Offering one would be offering a no-op.
    expect(suggestLabels(vocabulary, "", { exclude: ["Night Walk", "market"] }).map((use) => use.label))
      .toEqual(["harbour at night", "night train"]);
  });

  it("returns nothing when nothing matches, rather than falling back to everything", () => {
    expect(suggestLabels(vocabulary, "zzz")).toEqual([]);
  });

  it("bounds the list, because the rail cannot show a thousand labels", () => {
    const many = projectLabelVocabulary([clip("a", Array.from({ length: 40 }, (_, i) => `label-${i}`))]);
    expect(suggestLabels(many, "label")).toHaveLength(6);
    expect(suggestLabels(many, "label", { limit: 2 })).toHaveLength(2);
  });
});

describe("canonicalLabel", () => {
  const vocabulary = projectLabelVocabulary([clip("a", ["market"]), clip("b", ["market"])]);

  it("reuses the project's spelling for a label that differs only by case", () => {
    expect(canonicalLabel(vocabulary, "Market")).toBe("market");
    expect(canonicalLabel(vocabulary, "  MARKET  ")).toBe("market");
  });

  it("writes a new label exactly as typed", () => {
    // Creating a label is always allowed; only near-duplicates are steered.
    expect(canonicalLabel(vocabulary, "Night Walk")).toBe("Night Walk");
  });

  it("trims, so a stray space cannot create a second label", () => {
    expect(canonicalLabel(vocabulary, "  harbour ")).toBe("harbour");
  });
});
