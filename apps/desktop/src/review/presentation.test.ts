import { describe, expect, it } from "vitest";
import { INITIAL_VIEW, focusesNotesOnEntry, reduceView, type ViewState } from "./presentation";

const focused: ViewState = reduceView(INITIAL_VIEW, { kind: "enterFocus", overlays: true });

describe("initial state", () => {
  it("opens in Review with the ribbon shown and the shelf collapsed", () => {
    expect(INITIAL_VIEW).toEqual({
      mode: "review",
      presentation: "review",
      overlays: true,
      ribbon: true,
      rightStack: false,
      shelf: false,
    });
  });
});

describe("entering Focus", () => {
  it("switches presentation and keeps the mode as review", () => {
    expect(focused.presentation).toBe("focus");
    expect(focused.mode).toBe("review");
  });

  it("can enter with overlays hidden, which is what the expand control does", () => {
    const bare = reduceView(INITIAL_VIEW, { kind: "enterFocus", overlays: false });
    expect(bare.presentation).toBe("focus");
    expect(bare.overlays).toBe(false);
  });

  it("focuses the notes field only when entering with overlays shown", () => {
    expect(focusesNotesOnEntry({ kind: "enterFocus", overlays: true })).toBe(true);
    expect(focusesNotesOnEntry({ kind: "enterFocus", overlays: false })).toBe(false);
    expect(focusesNotesOnEntry({ kind: "exitFocus" })).toBe(false);
  });
});

describe("leaving Focus", () => {
  it("returns to the review presentation and restores overlay visibility", () => {
    const bare = reduceView(INITIAL_VIEW, { kind: "enterFocus", overlays: false });
    const back = reduceView(bare, { kind: "exitFocus" });
    expect(back.presentation).toBe("review");
    expect(back.overlays).toBe(true);
  });

  it("preserves the ribbon and shelf toggles across the round trip", () => {
    const tweaked = reduceView(reduceView(INITIAL_VIEW, { kind: "toggleRibbon" }), { kind: "toggleShelf" });
    const roundTrip = reduceView(reduceView(tweaked, { kind: "enterFocus", overlays: true }), { kind: "exitFocus" });
    expect(roundTrip.ribbon).toBe(tweaked.ribbon);
    expect(roundTrip.shelf).toBe(tweaked.shelf);
  });
});

describe("modes", () => {
  it("leaves Focus when switching to another mode", () => {
    const chronology = reduceView(focused, { kind: "setMode", mode: "chronology" });
    expect(chronology.mode).toBe("chronology");
    expect(chronology.presentation).toBe("review");
  });

  it("returns to Review rather than to Focus", () => {
    const chronology = reduceView(focused, { kind: "setMode", mode: "chronology" });
    const back = reduceView(chronology, { kind: "setMode", mode: "review" });
    expect(back.presentation).toBe("review");
  });
});

describe("toggles", () => {
  it("toggleRibbon and toggleShelf flip exactly one flag each", () => {
    const ribbon = reduceView(INITIAL_VIEW, { kind: "toggleRibbon" });
    expect(ribbon.ribbon).toBe(false);
    expect({ ...ribbon, ribbon: true }).toEqual(INITIAL_VIEW);

    const shelf = reduceView(INITIAL_VIEW, { kind: "toggleShelf" });
    expect(shelf.shelf).toBe(true);
    expect({ ...shelf, shelf: false }).toEqual(INITIAL_VIEW);
  });

  it("toggleRightStack and toggleOverlays flip exactly one flag each", () => {
    const stack = reduceView(focused, { kind: "toggleRightStack" });
    expect(stack.rightStack).toBe(true);
    expect({ ...stack, rightStack: false }).toEqual(focused);

    const hidden = reduceView(focused, { kind: "toggleOverlays" });
    expect(hidden.overlays).toBe(false);
    expect({ ...hidden, overlays: true }).toEqual(focused);
  });

  it("ignores toggleOverlays and toggleRightStack outside Focus", () => {
    expect(reduceView(INITIAL_VIEW, { kind: "toggleOverlays" })).toEqual(INITIAL_VIEW);
    expect(reduceView(INITIAL_VIEW, { kind: "toggleRightStack" })).toEqual(INITIAL_VIEW);
  });
});
