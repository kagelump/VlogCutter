import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { collectCustomProperties, collectStyleObjectProperties } from "./tokens";

const here = dirname(fileURLToPath(import.meta.url));
const srcRoot = resolve(here, "..");
const css = readFileSync(join(srcRoot, "styles.css"), "utf8");

function sourceFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return sourceFiles(path);
    return /\.tsx?$/.test(entry.name) ? [path] : [];
  });
}

const styleObjectProperties = new Set(
  sourceFiles(srcRoot).flatMap((path) => [
    ...collectStyleObjectProperties(readFileSync(path, "utf8")),
  ]),
);

describe("collectCustomProperties", () => {
  it("reports properties defined in a rule block", () => {
    const result = collectCustomProperties(":root { --ink: #fff; --line: #222; }");
    expect(result.defined.has("--ink")).toBe(true);
    expect(result.defined.has("--line")).toBe(true);
  });

  it("reports references and whether each carries a fallback", () => {
    const result = collectCustomProperties("a { color: var(--ink); border-color: var(--line, #222); }");
    expect(result.referenced).toEqual([
      { name: "--ink", hasFallback: false },
      { name: "--line", hasFallback: true },
    ]);
  });

  it("does not treat a definition as a reference", () => {
    const result = collectCustomProperties(":root { --ink: #fff; }");
    expect(result.referenced).toEqual([]);
  });
});

describe("collectStyleObjectProperties", () => {
  it("finds properties assigned through a React style object", () => {
    const found = collectStyleObjectProperties(
      'style={{ "--clip-step": `${zoom}px`, "--viewer-rotation": rotation }}',
    );
    expect([...found].sort()).toEqual(["--clip-step", "--viewer-rotation"]);
  });

  it("ignores ordinary string keys", () => {
    expect([...collectStyleObjectProperties('{ "width": 4 }')]).toEqual([]);
  });
});

describe("styles.css", () => {
  // An unresolved var() is invalid at computed-value time: the declaration
  // silently falls back to the property's initial value. That is how
  // .sync-indicator lost its border and inherited full-brightness ink while
  // looking, in source, exactly like every other control.
  it("references no custom property that is undefined, unfallbacked, and never set from JS", () => {
    const { defined, referenced } = collectCustomProperties(css);
    const unresolved = referenced
      .filter(
        (ref) =>
          !ref.hasFallback &&
          !defined.has(ref.name) &&
          !styleObjectProperties.has(ref.name),
      )
      .map((ref) => ref.name);
    expect([...new Set(unresolved)]).toEqual([]);
  });
});
