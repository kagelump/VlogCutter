import { describe, expect, it } from "vitest";
import { resolveAction, type KeyEventLike } from "./keymap";

function keyEvent(overrides: Partial<KeyEventLike> & { key: string }): KeyEventLike {
  return {
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    target: null,
    ...overrides,
  };
}

describe("resolveAction — move", () => {
  it("resolves ArrowDown to move +1", () => {
    expect(resolveAction(keyEvent({ key: "ArrowDown" }))).toEqual({ kind: "move", delta: 1 });
  });
  it("resolves lowercase j to move +1", () => {
    expect(resolveAction(keyEvent({ key: "j" }))).toEqual({ kind: "move", delta: 1 });
  });
  it("resolves uppercase J (shift held) to move +1", () => {
    expect(resolveAction(keyEvent({ key: "J", shiftKey: true }))).toEqual({ kind: "move", delta: 1 });
  });
  it("resolves ArrowUp to move -1", () => {
    expect(resolveAction(keyEvent({ key: "ArrowUp" }))).toEqual({ kind: "move", delta: -1 });
  });
  it("resolves lowercase k to move -1", () => {
    expect(resolveAction(keyEvent({ key: "k" }))).toEqual({ kind: "move", delta: -1 });
  });
});

describe("resolveAction — toggleRemoved", () => {
  it("resolves r to toggleRemoved", () => {
    expect(resolveAction(keyEvent({ key: "r" }))).toEqual({ kind: "toggleRemoved" });
  });
  it("resolves uppercase R to toggleRemoved", () => {
    expect(resolveAction(keyEvent({ key: "R", shiftKey: true }))).toEqual({ kind: "toggleRemoved" });
  });
});

describe("resolveAction — zoom", () => {
  it("resolves [ to zoom out", () => {
    expect(resolveAction(keyEvent({ key: "[" }))).toEqual({ kind: "zoom", delta: -1 });
  });
  it("resolves ] to zoom in", () => {
    expect(resolveAction(keyEvent({ key: "]" }))).toEqual({ kind: "zoom", delta: 1 });
  });
});

describe("resolveAction — centerSelection", () => {
  it("resolves c to centerSelection", () => {
    expect(resolveAction(keyEvent({ key: "c" }))).toEqual({ kind: "centerSelection" });
  });
  it("resolves uppercase C to centerSelection", () => {
    expect(resolveAction(keyEvent({ key: "C", shiftKey: true }))).toEqual({ kind: "centerSelection" });
  });
});

describe("resolveAction — typing must never trigger a shortcut", () => {
  it("returns null when target is an INPUT", () => {
    expect(resolveAction(keyEvent({ key: "j", target: { tagName: "INPUT", isContentEditable: false } }))).toBeNull();
  });
  it("returns null when target is a TEXTAREA", () => {
    expect(resolveAction(keyEvent({ key: "r", target: { tagName: "TEXTAREA", isContentEditable: false } }))).toBeNull();
  });
  it("returns null when target.isContentEditable is true", () => {
    expect(resolveAction(keyEvent({ key: "c", target: { tagName: "DIV", isContentEditable: true } }))).toBeNull();
  });
  it("returns null when target is a SELECT (behavior-preserving beyond the minimum oracle)", () => {
    expect(resolveAction(keyEvent({ key: "k", target: { tagName: "SELECT", isContentEditable: false } }))).toBeNull();
  });
  it("still resolves normally when target is a plain, non-editable element", () => {
    expect(resolveAction(keyEvent({ key: "j", target: { tagName: "BUTTON", isContentEditable: false } }))).toEqual({
      kind: "move",
      delta: 1,
    });
  });
});

describe("resolveAction — system shortcuts are not shadowed", () => {
  it("returns null when metaKey is set", () => {
    expect(resolveAction(keyEvent({ key: "j", metaKey: true }))).toBeNull();
  });
  it("returns null when ctrlKey is set", () => {
    expect(resolveAction(keyEvent({ key: "r", ctrlKey: true }))).toBeNull();
  });
  it("returns null for every review letter while metaKey is held", () => {
    for (const key of ["j", "k", "r", "c", "[", "]"]) {
      expect(resolveAction(keyEvent({ key, metaKey: true }))).toBeNull();
    }
  });
});

describe("resolveAction — non-review keys", () => {
  it("returns null for keys the review dispatch table does not own", () => {
    for (const key of ["?", "/", " ", "l", "n", "Enter", "p", "m", "Escape", "a"]) {
      expect(resolveAction(keyEvent({ key }))).toBeNull();
    }
  });
});

describe("resolveAction — presentation shortcuts", () => {
  it("maps F to toggleFocus and H to toggleOverlays", () => {
    expect(resolveAction(keyEvent({ key: "f" }))).toEqual({ kind: "toggleFocus" });
    expect(resolveAction(keyEvent({ key: "h" }))).toEqual({ kind: "toggleOverlays" });
  });

  it("maps D to the shelf and bare T to the ribbon", () => {
    expect(resolveAction(keyEvent({ key: "d" }))).toEqual({ kind: "toggleShelf" });
    expect(resolveAction(keyEvent({ key: "t" }))).toEqual({ kind: "toggleRibbon" });
  });

  it("maps Shift+T to the expanded ribbon overlay, distinct from bare T", () => {
    expect(resolveAction(keyEvent({ key: "T", shiftKey: true }))).toEqual({ kind: "ribbonOverlay" });
  });

  it("maps B to the best moment and I to editing the title", () => {
    expect(resolveAction(keyEvent({ key: "b" }))).toEqual({ kind: "jumpBestMoment" });
    expect(resolveAction(keyEvent({ key: "i" }))).toEqual({ kind: "editTitle" });
  });

  it("returns null for every new key while a text field has focus", () => {
    for (const key of ["f", "h", "d", "t", "b", "i"]) {
      expect(
        resolveAction(keyEvent({ key, target: { tagName: "TEXTAREA", isContentEditable: false } })),
      ).toBeNull();
    }
  });
});

describe("resolveAction — modes", () => {
  it("maps Cmd+1/2/3 to modes", () => {
    expect(resolveAction(keyEvent({ key: "1", metaKey: true }))).toEqual({ kind: "setMode", mode: "review" });
    expect(resolveAction(keyEvent({ key: "2", metaKey: true }))).toEqual({ kind: "setMode", mode: "chronology" });
    expect(resolveAction(keyEvent({ key: "3", metaKey: true }))).toEqual({ kind: "setMode", mode: "editPlan" });
  });

  it("does not claim the bare digits", () => {
    for (const key of ["1", "2", "3"]) {
      expect(resolveAction(keyEvent({ key }))).toBeNull();
    }
  });

  it("does not claim a digit with any other modifier alongside Meta", () => {
    expect(resolveAction(keyEvent({ key: "1", metaKey: true, shiftKey: true }))).toBeNull();
    expect(resolveAction(keyEvent({ key: "1", metaKey: true, altKey: true }))).toBeNull();
    expect(resolveAction(keyEvent({ key: "1", ctrlKey: true }))).toBeNull();
  });

  it("leaves Cmd+1 unhandled while a text field has focus", () => {
    expect(
      resolveAction(keyEvent({ key: "1", metaKey: true, target: { tagName: "INPUT", isContentEditable: false } })),
    ).toBeNull();
  });

  it("claims no Meta combination beyond the three mode digits", () => {
    for (const key of ["4", "0", "f", "t", "d", "b", "i", "h"]) {
      expect(resolveAction(keyEvent({ key, metaKey: true }))).toBeNull();
    }
  });
});
