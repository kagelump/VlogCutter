/**
 * What Escape closes, and in what order.
 *
 * Escape used to be resolved in two places that did not know about each other:
 * a global handler in `App.tsx` that knew four surfaces, and four modals that
 * each watched for the key themselves. The surfaces neither one knew about —
 * the import dialog, import history, the trip-phase dialog — got whatever
 * behaviour fell out, and because the global handler could not see them, one
 * Escape could close the dialog *and* exit Focus underneath it.
 *
 * There is one list now, ordered top of the stack first, and one rule: Escape
 * resolves against the topmost open layer and stops there. A layer that is
 * open but locked — a running import, a pairing attempt mid-connect — swallows
 * the key rather than passing it down, because "the keystroke did nothing" is
 * the correct outcome and "the keystroke left Focus instead" is not.
 */

export interface EscapeLayer {
  /** Stable name; the shell dispatches on it and the tests read it. */
  id: string;
  /** Is this surface on screen right now? */
  open: boolean;
  /**
   * May Escape close it? A locked layer still consumes the key. Cancelling a
   * copy in flight, or a pairing exchange half-made, is not something a
   * stray keystroke should be able to do.
   */
  locked?: boolean;
}

export type EscapeResolution =
  /** Close this layer, and nothing else. */
  | { kind: "close"; id: string }
  /** This layer is on top and refuses; the key goes no further. */
  | { kind: "blocked"; id: string }
  /** Nothing is open. */
  | { kind: "none" };

export function resolveEscape(layers: readonly EscapeLayer[]): EscapeResolution {
  const top = layers.find((layer) => layer.open);
  if (!top) return { kind: "none" };
  return top.locked ? { kind: "blocked", id: top.id } : { kind: "close", id: top.id };
}
