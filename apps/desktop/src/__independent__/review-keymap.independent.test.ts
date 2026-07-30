// Independent falsification suite for apps/desktop/src/review/keymap.ts.
//
// Owned exclusively by the independent tester for D01-T003. Does NOT import
// or adapt apps/desktop/src/review/keymap.test.ts. Fixtures and assertions
// below are written from the task packet's spec sections only.

import { describe, expect, it } from "vitest";
import { resolveAction, type KeyEventLike, type ReviewAction } from "../review/keymap";

// Independently-authored event builder — deliberately shaped differently
// from any implementer helper (named fields spelled out per call, plus a
// separate helper specifically for editable targets).
function plainKeydown(key: string, mods?: Partial<Pick<KeyEventLike, "metaKey" | "ctrlKey" | "altKey" | "shiftKey">>): KeyEventLike {
  return {
    key,
    metaKey: mods?.metaKey ?? false,
    ctrlKey: mods?.ctrlKey ?? false,
    altKey: mods?.altKey ?? false,
    shiftKey: mods?.shiftKey ?? false,
    target: null,
  };
}

function editableKeydown(key: string, tagName: string, isContentEditable: boolean): KeyEventLike {
  return {
    key,
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    target: { tagName, isContentEditable },
  };
}

describe("independent: acceptance oracle case 7 — typing must never trigger a review shortcut", () => {
  it("returns null for a keydown whose target is a contenteditable DIV", () => {
    // The packet's charter names this exact scenario explicitly: a
    // contenteditable div (not an INPUT/TEXTAREA), which only the
    // isContentEditable branch — not the tagName branch — can catch.
    const event = editableKeydown("j", "DIV", true);
    expect(resolveAction(event)).toBeNull();
  });

  it("returns null for a contenteditable SPAN too (isContentEditable is tag-independent)", () => {
    expect(resolveAction(editableKeydown("r", "SPAN", true))).toBeNull();
  });

  it("returns null when target tagName is INPUT", () => {
    expect(resolveAction(editableKeydown("c", "INPUT", false))).toBeNull();
  });

  it("returns null when target tagName is TEXTAREA", () => {
    expect(resolveAction(editableKeydown("[", "TEXTAREA", false))).toBeNull();
  });

  it("still resolves normally on a non-editable, non-special target", () => {
    expect(resolveAction(editableKeydown("j", "DIV", false))).toEqual({ kind: "move", delta: 1 });
  });
});

describe("independent: acceptance oracle case 8 + charter — system shortcuts are not shadowed", () => {
  const alphabet = "abcdefghijklmnopqrstuvwxyz".split("");

  it("returns null for every single-letter key a-z while metaKey is held", () => {
    for (const letter of alphabet) {
      expect(resolveAction(plainKeydown(letter, { metaKey: true }))).toBeNull();
    }
  });

  it("returns null for every single-letter key a-z while ctrlKey is held", () => {
    for (const letter of alphabet) {
      expect(resolveAction(plainKeydown(letter, { ctrlKey: true }))).toBeNull();
    }
  });

  it("returns null for review-bound punctuation keys while metaKey is held", () => {
    for (const key of ["[", "]"]) {
      expect(resolveAction(plainKeydown(key, { metaKey: true }))).toBeNull();
    }
  });

  it("does not null out on shiftKey or altKey alone (only meta/ctrl guard system shortcuts)", () => {
    expect(resolveAction(plainKeydown("j", { shiftKey: true }))).toEqual({ kind: "move", delta: 1 });
    expect(resolveAction(plainKeydown("c", { altKey: true }))).toEqual({ kind: "centerSelection" });
  });
});

describe("independent: acceptance oracle case 9 — every current App.tsx review binding resolves", () => {
  // Enumerated independently from AGENTS.md section 10 ("Keyboard contract")
  // and the packet's own description of the keydown handler, NOT copied from
  // any implementer test file: J/K or arrow keys move; R toggles removed;
  // [ / ] zoom the story ribbon; C centers the selection.
  const expectedBindings: Array<{ key: string; expected: ReviewAction }> = [
    { key: "ArrowDown", expected: { kind: "move", delta: 1 } },
    { key: "j", expected: { kind: "move", delta: 1 } },
    { key: "J", expected: { kind: "move", delta: 1 } }, // shift+j must still resolve, not be swallowed
    { key: "ArrowUp", expected: { kind: "move", delta: -1 } },
    { key: "k", expected: { kind: "move", delta: -1 } },
    { key: "K", expected: { kind: "move", delta: -1 } },
    { key: "r", expected: { kind: "toggleRemoved" } },
    { key: "R", expected: { kind: "toggleRemoved" } },
    { key: "[", expected: { kind: "zoom", delta: -1 } },
    { key: "]", expected: { kind: "zoom", delta: 1 } },
    { key: "c", expected: { kind: "centerSelection" } },
    { key: "C", expected: { kind: "centerSelection" } },
  ];

  for (const { key, expected } of expectedBindings) {
    it(`"${key}" resolves to ${JSON.stringify(expected)}`, () => {
      expect(resolveAction(plainKeydown(key))).toEqual(expected);
    });
  }

  it("no review binding is silently dropped: all six distinct action shapes are reachable", () => {
    const seen = new Set(
      expectedBindings.map(({ key }) => JSON.stringify(resolveAction(plainKeydown(key)))),
    );
    // move+1, move-1, toggleRemoved, zoom-1, zoom+1, centerSelection
    expect(seen.size).toBe(6);
    expect([...seen].every((entry) => entry !== "null")).toBe(true);
  });

  it("non-review shortcuts that App.tsx handles itself are correctly left as null", () => {
    // These are handled directly in App.tsx's keydown handler (help, search
    // focus, playback, label/notes focus, media generation) and are
    // explicitly out of scope for the extracted review dispatch table per
    // the module's own doc comment.
    for (const key of ["?", "/", " ", "l", "L", "n", "N", "Enter", "p", "P", "t", "T", "m", "M", "Escape"]) {
      expect(resolveAction(plainKeydown(key))).toBeNull();
    }
  });
});

describe("independent: resolveAction is a total function returning only known shapes or null", () => {
  it("never throws and only ever returns one of the four action kinds or null", () => {
    const validKinds = new Set(["move", "toggleRemoved", "zoom", "centerSelection"]);
    const probeKeys = ["j", "k", "r", "[", "]", "c", "z", "1", "!", "\t", "", "Tab", "Shift"];
    for (const key of probeKeys) {
      let action: ReviewAction | null = null;
      expect(() => {
        action = resolveAction(plainKeydown(key));
      }).not.toThrow();
      if (action !== null) {
        expect(validKinds.has((action as ReviewAction).kind)).toBe(true);
      }
    }
  });

  it("handles a null target gracefully (no crash on missing event.target)", () => {
    expect(() => resolveAction(plainKeydown("j"))).not.toThrow();
  });
});
