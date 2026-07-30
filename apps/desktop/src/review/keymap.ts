export type ReviewAction =
  | { kind: "move"; delta: number }
  | { kind: "toggleRemoved" }
  | { kind: "zoom"; delta: number }
  | { kind: "centerSelection" };

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
 * This covers exactly the four review actions the app dispatches from its
 * keydown handler: move (J/K/ArrowUp/ArrowDown), toggleRemoved (R), zoom
 * (`[`/`]`), and centerSelection (C). Every other shortcut the handler
 * implements (new project, open catalog, settings, help, search focus,
 * playback, label/notes focus, media generation) is not a review action and
 * is intentionally out of scope for this module; App.tsx keeps handling
 * those directly.
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

  // Never shadow system shortcuts: any Cmd/Ctrl-held combination is left for
  // the OS or the app's own explicit Meta+key handlers.
  if (event.metaKey || event.ctrlKey) return null;

  const key = event.key.toLocaleLowerCase();

  if (event.key === "ArrowDown" || key === "j") return { kind: "move", delta: 1 };
  if (event.key === "ArrowUp" || key === "k") return { kind: "move", delta: -1 };
  if (key === "r") return { kind: "toggleRemoved" };
  if (key === "[") return { kind: "zoom", delta: -1 };
  if (key === "]") return { kind: "zoom", delta: 1 };
  if (key === "c") return { kind: "centerSelection" };

  return null;
}
