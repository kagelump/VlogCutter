/**
 * Proposing a display timezone for a clip no trip phase covers (#72).
 *
 * The core resolves local display time from the covering phase and falls back
 * to UTC when nothing covers a clip. That fallback is data-safe and stays: a
 * project that has never said where it was should not be shown a confident
 * local clock. What it lacked was a way forward, and this module supplies the
 * *proposal* half of it.
 *
 * Three rules the suggestion has to obey, and why:
 *
 * 1. **Never the host's timezone.** The laptop's current zone says where the
 *    editor is sitting today, which has nothing to do with where the footage
 *    was shot. A Madrid laptop must not relabel Tokyo footage.
 * 2. **Only project evidence.** A suggestion comes from a phase the user
 *    already created, and it names that phase, so the reader can judge it.
 * 3. **Proposed, never committed.** Nothing here writes. The caller offers the
 *    suggestion as something to accept; until the user saves a phase, the
 *    display stays on the UTC fallback.
 */

import type { TripPhase } from "../chronologyTypes";

export interface TimezoneSuggestion {
  /** The IANA identifier being proposed. */
  timezone: string;
  /** The phase it came from, so the proposal can show its own evidence. */
  phaseLabel: string;
  /** Whether that phase runs before or after the clip. */
  direction: "before" | "after";
}

/** The zone a phase displays a clip in, by its kind. */
function phaseTimezone(phase: TripPhase, side: "start" | "end"): string | null {
  if (phase.kind === "transit") {
    // A transit phase spans two zones. The one nearer the clip is the one worth
    // proposing: a clip just before departure was in the origin zone, and one
    // just after arrival was in the destination zone.
    const zone = side === "start" ? phase.originTimezone : phase.destinationTimezone;
    return zone?.trim() || phase.timezone?.trim() || null;
  }
  return phase.timezone?.trim() || null;
}

/**
 * The nearest phase timezone to a trip time, or null when the project has no
 * evidence to offer.
 *
 * Nearest by gap in time, ties resolved toward the phase that *precedes* the
 * clip: footage tends to be shot after arriving somewhere rather than before
 * leaving for it, so the earlier phase is the better guess when both are
 * equally close.
 */
export function suggestTimezoneFromPhases(
  phases: TripPhase[],
  tripTime: string | null,
): TimezoneSuggestion | null {
  const at = tripTime ? Date.parse(tripTime) : NaN;
  if (Number.isNaN(at)) return null;

  let best: { gap: number; suggestion: TimezoneSuggestion } | null = null;
  for (const phase of phases) {
    const start = Date.parse(phase.startTripTime);
    const end = Date.parse(phase.endTripTime);
    if (Number.isNaN(start) || Number.isNaN(end)) continue;
    // A phase that already covers this time is not a suggestion — it is the
    // authoritative answer, and the caller would not be asking.
    if (at >= start && at <= end) continue;

    const before = at > end;
    const gap = before ? at - end : start - at;
    const zone = phaseTimezone(phase, before ? "end" : "start");
    if (!zone) continue;
    const suggestion: TimezoneSuggestion = {
      timezone: zone,
      phaseLabel: phase.label,
      direction: before ? "before" : "after",
    };
    // Strictly closer wins; an exact tie keeps the earlier phase, which is the
    // one already held when `before` was seen first in start-time order.
    if (!best || gap < best.gap || (gap === best.gap && suggestion.direction === "before" && best.suggestion.direction === "after")) {
      best = { gap, suggestion };
    }
  }
  return best?.suggestion ?? null;
}

/** One line of copy for the proposal, naming its evidence. */
export function describeTimezoneSuggestion(suggestion: TimezoneSuggestion): string {
  const relation = suggestion.direction === "before" ? "just before" : "just after";
  return `Suggested from “${suggestion.phaseLabel}”, the trip context ${relation} this clip.`;
}
