import type { TranscriptSegment } from "../types";

/**
 * Keeping the transcript beside the playhead (issue #13).
 *
 * Both decisions here are made in a pure module because neither is testable
 * where it is used: jsdom has no layout engine to scroll, and the shelf's
 * component test cannot advance a `<video>`.
 */

/**
 * Which segment the transcript should be showing as current.
 *
 * The segment that has most recently *started*, rather than only a segment
 * strictly containing the playhead. Speech has gaps — between sentences, and
 * after the last word — and a highlight that blinked off in every pause would
 * be worse than useless in exactly the clip this feature is for. Before the
 * first segment starts nothing is current, which is a real state and not a gap.
 */
export function activeSegmentIndex(
  segments: readonly TranscriptSegment[],
  positionSeconds: number,
): number {
  const positionMs = positionSeconds * 1000;
  let active = -1;
  for (let index = 0; index < segments.length; index += 1) {
    if (segments[index].startMs > positionMs) break;
    active = index;
  }
  return active;
}

export interface FollowGeometry {
  /** The active row's offset from the top of the scrolling content. */
  itemTop: number;
  itemHeight: number;
  scrollTop: number;
  viewportHeight: number;
}

/**
 * Where to scroll so the active row is visible, or `null` to leave it alone.
 *
 * A row already on screen is never chased: scrolling a list the reader is
 * looking at, to move something they can already see, is the behaviour that
 * makes auto-follow features get switched off. Only a row that has gone off an
 * edge is brought back, and only as far as that edge.
 */
export function followScrollTop({
  itemTop,
  itemHeight,
  scrollTop,
  viewportHeight,
}: FollowGeometry): number | null {
  if (viewportHeight <= 0) return null;
  if (itemTop < scrollTop) return Math.max(0, itemTop);
  const itemBottom = itemTop + itemHeight;
  if (itemBottom > scrollTop + viewportHeight) return Math.max(0, itemBottom - viewportHeight);
  return null;
}

/** How long a manual scroll holds auto-follow off, per the desktop design. */
export const FOLLOW_SUSPENSION_MS = 5000;
