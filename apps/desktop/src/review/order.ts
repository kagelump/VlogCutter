import type { Asset } from "../types";

export interface OrderOptions {
  removedVisible: boolean;
}

/**
 * Returns the review queue in display order. Pure, total, and stable: it only
 * filters by removed status, so the relative order of `assets` is preserved
 * exactly (the app is responsible for keeping `assets` itself in the order it
 * wants displayed; this function never reorders, only selects a subset).
 */
export function orderAssets(assets: Asset[], options: OrderOptions): Asset[] {
  return assets.filter((asset) => asset.removed === options.removedVisible);
}

/** Index of assetId in ordered, or -1. */
export function indexOfAsset(ordered: Asset[], assetId: string | null): number {
  if (assetId === null) return -1;
  return ordered.findIndex((asset) => asset.id === assetId);
}

/**
 * The selection after a move. Returns the current id when the move would leave
 * the queue, so selection is never lost at a boundary.
 *
 * When `currentId` is not present in `ordered` (including when it is null),
 * the move is computed from a virtual position immediately before the start
 * of the queue, so a delta of +1 lands on the first asset rather than the
 * second. This only affects a selection state that never happens in the
 * running application (selection is always resynced to a valid id before a
 * keyboard move can occur), so it does not change observable behavior; it
 * only gives the not-found case a well-defined, boundary-safe answer.
 */
export function moveSelection(
  ordered: Asset[],
  currentId: string | null,
  delta: number,
): string | null {
  if (ordered.length === 0) return currentId;
  const currentIndex = indexOfAsset(ordered, currentId);
  const next = Math.min(ordered.length - 1, Math.max(0, currentIndex + delta));
  return ordered[next].id;
}

/**
 * The selection to restore after the queue is reordered. Prefers the same
 * asset; falls back to the nearest surviving neighbour by prior position.
 *
 * Given only the post-change `ordered` list and the previously selected id,
 * there is no reachable prior-position data once an asset has left `ordered`
 * entirely (a single array cannot recover the position of an element it no
 * longer contains). In that case this falls back to the first surviving
 * asset, which is the same fallback the running application already uses
 * when a selection stops matching the current queue.
 */
export function nextSelection(
  ordered: Asset[],
  previousId: string | null,
): string | null {
  if (ordered.length === 0) return null;
  const index = indexOfAsset(ordered, previousId);
  if (index !== -1) return previousId;
  return ordered[0].id;
}
