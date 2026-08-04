import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const css = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "styles.css"), "utf8");

/**
 * The type scale, spec §5. Six sizes for the main view, plus 21 for modal
 * headings, which sit outside it.
 *
 * The old stylesheet had five steps below 13px bottoming out at 8, where
 * timeline clip names and queue indices read as texture rather than as text.
 */
const ALLOWED = new Set([10, 11, 12, 13, 15, 17, 21]);

function declaredSizes() {
  const sizes = [...css.matchAll(/font-size:\s*(\d+)px/g)].map((m) => Number(m[1]));
  const shorthand = [...css.matchAll(/font:\s*(?:\d+\s+)?(\d+)px/g)].map((m) => Number(m[1]));
  return [...sizes, ...shorthand];
}

describe("type scale", () => {
  it("uses no font size outside the scale", () => {
    const offenders = [...new Set(declaredSizes())].filter((size) => !ALLOWED.has(size));
    expect(offenders.sort((a, b) => a - b)).toEqual([]);
  });

  it("reserves 15px for the notes field and nothing else", () => {
    const rules = css
      .split("}")
      .filter((rule) => /font-size:\s*15px|font:\s*(?:\d+\s+)?15px/.test(rule));
    expect(rules).toHaveLength(1);
    expect(rules[0]).toContain(".notes-field");
  });

  it("keeps 17px for the clip title, the only other distinguished size", () => {
    const rules = css
      .split("}")
      .filter((rule) => /font-size:\s*17px|font:\s*(?:\d+\s+)?17px/.test(rule));
    expect(rules.every((rule) => /clip-title-field|panel-heading h1/.test(rule))).toBe(true);
  });
});

describe("spacing scale", () => {
  // Only `gap` is checked. Padding is often asymmetric to clear an
  // absolutely-positioned child — the search box reserves 40px on the right
  // for its keycap — and that is a layout dimension rather than rhythm.
  it("uses gaps only from the 2/4/6/8/12/16/24 scale", () => {
    const ALLOWED_GAPS = new Set([0, 2, 4, 6, 8, 12, 16, 24]);
    const gaps = [...css.matchAll(/(?:^|[;{\s])gap:\s*([^;}]+)/g)]
      .flatMap((m) => [...m[1].matchAll(/(\d+)px/g)].map((v) => Number(v[1])));
    const offenders = [...new Set(gaps)].filter((gap) => !ALLOWED_GAPS.has(gap));
    expect(offenders.sort((a, b) => a - b)).toEqual([]);
  });
});
