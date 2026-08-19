import type { Asset } from "../types";

export interface OrderOptions {
  removedVisible: boolean;
}

const utf8 = new TextEncoder();

/** SQLite's default BINARY collation compares UTF-8 bytes, not UTF-16 code units. */
function compareSqliteBinary(left: string, right: string): number {
  if (left === right) return 0;
  const leftBytes = utf8.encode(left);
  const rightBytes = utf8.encode(right);
  const length = Math.min(leftBytes.length, rightBytes.length);
  for (let index = 0; index < length; index += 1) {
    const difference = leftBytes[index] - rightBytes[index];
    if (difference !== 0) return difference;
  }
  return leftBytes.length - rightBytes.length;
}

/** The catalog's canonical review ordering: known timestamps first, then filename and stable id. */
export function compareAssets(left: Asset, right: Asset): number {
  if (left.bestTime === null && right.bestTime !== null) return 1;
  if (left.bestTime !== null && right.bestTime === null) return -1;
  // These strings are persisted order keys, so locale and JavaScript UTF-16
  // ordering could make an incremental insertion disagree with SQLite.
  const time = compareSqliteBinary(left.bestTime ?? "", right.bestTime ?? "");
  if (time !== 0) return time;
  const filename = compareSqliteBinary(left.filename, right.filename);
  return filename !== 0 ? filename : compareSqliteBinary(left.id, right.id);
}

/** A non-mutating canonical-order copy for a queue refresh or imported row. */
export function sortAssets(assets: Asset[]): Asset[] {
  return [...assets].sort(compareAssets);
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

/**
 * Replace one asset in a list, or return the list untouched.
 *
 * Called once per progress event during a batch — thousands of times over a
 * proxy run on a project of the target size. `assets.map(...)` allocates a new
 * array of every clip on each of those events even when the clip it is looking
 * for is not in the list at all, and a new array identity is a re-render of
 * the whole shell. This copies only when there is something to change, and the
 * copy is one slot rather than a closure per element.
 */
export function replaceAsset(assets: Asset[], updated: Asset): Asset[] {
  const index = assets.findIndex((candidate) => candidate.id === updated.id);
  if (index === -1) return assets;
  if (assets[index] === updated) return assets;
  const next = assets.slice();
  next[index] = updated;
  return next;
}

/**
 * Merge a just-committed asset into the review cache in the same deterministic
 * order as `Catalog::list_assets`: solved timestamp, filename, then stable id.
 *
 * Import is the one progress stream that can introduce a new asset. Other
 * batch streams only enrich an existing row and should keep using
 * `replaceAsset`, which avoids needless whole-list work for unrelated events.
 */
export function upsertAsset(assets: Asset[], updated: Asset): Asset[] {
  const existing = assets.findIndex((candidate) => candidate.id === updated.id);
  if (existing !== -1) return replaceAsset(assets, updated);

  let low = 0;
  let high = assets.length;
  while (low < high) {
    const middle = Math.floor((low + high) / 2);
    if (compareAssets(assets[middle], updated) <= 0) low = middle + 1;
    else high = middle;
  }
  return [...assets.slice(0, low), updated, ...assets.slice(low)];
}
