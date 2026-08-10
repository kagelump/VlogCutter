export type ReviewAction =
  | { kind: "move"; delta: number }
  | { kind: "toggleRemoved" }
  | { kind: "zoom"; delta: number }
  | { kind: "centerSelection" }
  | { kind: "toggleFocus" }
  | { kind: "toggleOverlays" }
  | { kind: "toggleShelf" }
  | { kind: "toggleRibbon" }
  | { kind: "ribbonOverlay" }
  | { kind: "jumpBestMoment" }
  | { kind: "editTitle" }
  | { kind: "rotate"; addition: 90 | 180 }
  | { kind: "setMode"; mode: "review" | "chronology" | "editPlan" };

export interface KeyEventLike {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  target: { tagName: string; isContentEditable: boolean } | null;
}

/**
 * Resolves a keydown event to a review-queue action, or null when the event
 * is not a review shortcut.
 *
 * This covers the review-queue actions — move (J/K/ArrowUp/ArrowDown),
 * toggleRemoved (R), zoom (`[`/`]`), centerSelection (C) — plus the
 * presentation and mode shortcuts: Focus (F), overlays (H), the evidence
 * shelf (D), the ribbon (T, Shift+T), the best moment (B), the title (I),
 * orientation (O, Shift+O), and the three modes (Cmd+1/2/3).
 *
 * Every other shortcut the handler implements (new project, open catalog,
 * settings, help, search focus, playback, label/notes focus, media
 * generation) is not a review action and is intentionally out of scope for
 * this module; App.tsx keeps handling those directly.
 */
export function resolveAction(event: KeyEventLike): ReviewAction | null {
  const target = event.target;
  if (
    target &&
    (target.tagName === "INPUT" ||
      target.tagName === "TEXTAREA" ||
      target.tagName === "SELECT" ||
      target.isContentEditable)
  ) {
    return null;
  }

  const key = event.key.toLocaleLowerCase();

  // The three mode digits are the only Cmd combinations this module claims.
  // Everything else Cmd/Ctrl-held is left for the OS or for App.tsx's own
  // explicit Meta+key handlers.
  if (event.metaKey || event.ctrlKey) {
    if (event.metaKey && !event.ctrlKey && !event.altKey && !event.shiftKey) {
      if (key === "1") return { kind: "setMode", mode: "review" };
      if (key === "2") return { kind: "setMode", mode: "chronology" };
      if (key === "3") return { kind: "setMode", mode: "editPlan" };
    }
    return null;
  }

  if (event.key === "ArrowDown" || key === "j") return { kind: "move", delta: 1 };
  if (event.key === "ArrowUp" || key === "k") return { kind: "move", delta: -1 };
  if (key === "r") return { kind: "toggleRemoved" };
  if (key === "[") return { kind: "zoom", delta: -1 };
  if (key === "]") return { kind: "zoom", delta: 1 };
  if (key === "c") return { kind: "centerSelection" };
  if (key === "f") return { kind: "toggleFocus" };
  if (key === "h") return { kind: "toggleOverlays" };
  if (key === "d") return { kind: "toggleShelf" };
  // Bare T collapses the ribbon; Shift+T expands it as an overlay. Thumbnail
  // generation gave up this key and moved to Shift+G: it is a once-per-project
  // setup action, while the ribbon toggle is pressed many times a session.
  if (key === "t") return event.shiftKey ? { kind: "ribbonOverlay" } : { kind: "toggleRibbon" };
  if (key === "b") return { kind: "jumpBestMoment" };
  if (key === "i") return { kind: "editTitle" };
  // R already owns remove/restore. O keeps rotation mnemonic without putting
  // a destructive queue action and a frame correction on the same key.
  if (key === "o") return { kind: "rotate", addition: event.shiftKey ? 180 : 90 };

  return null;
}
