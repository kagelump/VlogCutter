/**
 * View state for the desktop main screen (spec §3.9, §6).
 *
 * Review and Focus are two presentations of the same working set — same clip,
 * filter, search and queue order. Chronology and Edit plan are modes with
 * their own surfaces and no Focus presentation.
 *
 * Overlay visibility lives here, and only explicit actions change it: nothing
 * in this module reacts to hover or to playback state, because a control that
 * appears on mouse-entry is one you cannot aim at, and a notes field that
 * vanishes because you stopped typing is worse.
 */

export type Mode = "review" | "chronology" | "editPlan";
export type Presentation = "review" | "focus";

export interface ViewState {
  mode: Mode;
  presentation: Presentation;
  /** Focus only: whether any overlay is drawn over the frame. */
  overlays: boolean;
  ribbon: boolean;
  /** Focus only: the optional labels + transcript column. Off by default. */
  rightStack: boolean;
  /** Review only: the evidence shelf. Collapsed by default. */
  shelf: boolean;
}

export const INITIAL_VIEW: ViewState = {
  mode: "review",
  presentation: "review",
  overlays: true,
  ribbon: true,
  rightStack: false,
  shelf: false,
};

export type ViewAction =
  | { kind: "enterFocus"; overlays: boolean }
  | { kind: "exitFocus" }
  | { kind: "setMode"; mode: Mode }
  | { kind: "toggleOverlays" }
  | { kind: "toggleRibbon" }
  | { kind: "toggleRightStack" }
  | { kind: "toggleShelf" };

export function reduceView(state: ViewState, action: ViewAction): ViewState {
  switch (action.kind) {
    case "enterFocus":
      return { ...state, presentation: "focus", overlays: action.overlays };
    case "exitFocus":
      // Overlay visibility is a Focus-local concern; leaving resets it so the
      // next entry is never mysteriously bare.
      return { ...state, presentation: "review", overlays: true };
    case "setMode":
      return { ...state, mode: action.mode, presentation: "review", overlays: true };
    case "toggleOverlays":
      if (state.presentation !== "focus") return state;
      return { ...state, overlays: !state.overlays };
    case "toggleRightStack":
      if (state.presentation !== "focus") return state;
      return { ...state, rightStack: !state.rightStack };
    case "toggleRibbon":
      return { ...state, ribbon: !state.ribbon };
    case "toggleShelf":
      return { ...state, shelf: !state.shelf };
  }
}

/**
 * Entering with `F` puts the caret in the notes field, since writing is what
 * you came for. Entering with the expand control starts bare, so focusing an
 * invisible field would strand the caret.
 */
export function focusesNotesOnEntry(action: ViewAction) {
  return action.kind === "enterFocus" && action.overlays;
}
