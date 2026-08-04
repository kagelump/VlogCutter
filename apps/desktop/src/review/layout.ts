/** Story-ribbon geometry (spec §3.8). */

/** Horizontal pixels per clip slot. */
export const RIBBON_ZOOM_MIN = 44;
export const RIBBON_ZOOM_MAX = 116;
export const RIBBON_ZOOM_STEP = 12;
export const RIBBON_ZOOM_DEFAULT = 68;

export const RIBBON_MAX_LANES = 5;
export const RIBBON_LANE_HEIGHT = 20;
/** Control row (22) + trip-context band (20) + position numerals (14). */
export const RIBBON_CHROME_HEIGHT = 56;
export const RIBBON_COLLAPSED_HEIGHT = 28;

function laneFloor(laneCount: number) {
  return Math.max(0, Math.floor(Number.isFinite(laneCount) ? laneCount : 0));
}

/**
 * Lanes actually given a visible track. Past the cap the lane stack scrolls
 * internally rather than growing the ribbon, so the main row's height stays
 * predictable no matter how many cameras a project holds.
 */
export function visibleLaneCount(laneCount: number) {
  return Math.min(RIBBON_MAX_LANES, laneFloor(laneCount));
}

export function ribbonHeight(laneCount: number, collapsed: boolean) {
  if (collapsed) return RIBBON_COLLAPSED_HEIGHT;
  return RIBBON_CHROME_HEIGHT + visibleLaneCount(laneCount) * RIBBON_LANE_HEIGHT;
}

/**
 * The `grid-template-rows` fragment for the lane tracks.
 *
 * Uses the true lane count rather than the capped one: the canvas renders
 * every camera and scrolls, while `ribbonHeight` sizes the viewport that
 * shows at most five of them.
 */
export function ribbonLaneRows(laneCount: number) {
  const lanes = laneFloor(laneCount);
  return lanes === 0 ? "" : `repeat(${lanes}, ${RIBBON_LANE_HEIGHT}px)`;
}
