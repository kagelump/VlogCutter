/** The queue row geometry is a product contract shared with styles.css. */
export const QUEUE_ROW_HEIGHT = 52;

/**
 * A few rows on either side keep fast wheel and trackpad scrolling from
 * revealing an empty seam while React commits the next window.
 */
export const QUEUE_OVERSCAN_ROWS = 6;

export interface QueueWindow {
  start: number;
  end: number;
  totalHeight: number;
}

interface QueueWindowInput {
  itemCount: number;
  scrollTop: number;
  viewportHeight: number;
  rowHeight?: number;
  overscanRows?: number;
}

/**
 * Return the half-open slice that can paint a fixed-height queue viewport.
 *
 * An infinite viewport deliberately means "render everything". jsdom and
 * older engines without ResizeObserver have no trustworthy layout geometry;
 * correctness is a better fallback there than guessing at a window.
 */
export function queueWindow({
  itemCount,
  scrollTop,
  viewportHeight,
  rowHeight = QUEUE_ROW_HEIGHT,
  overscanRows = QUEUE_OVERSCAN_ROWS,
}: QueueWindowInput): QueueWindow {
  const count = Math.max(0, Math.floor(itemCount));
  const safeRowHeight = Math.max(1, rowHeight);
  const totalHeight = count * safeRowHeight;
  if (count === 0) return { start: 0, end: 0, totalHeight: 0 };
  if (!Number.isFinite(viewportHeight)) return { start: 0, end: count, totalHeight };

  const height = Math.max(0, viewportHeight);
  const overscan = Math.max(0, Math.floor(overscanRows));
  const maximumScrollTop = Math.max(0, totalHeight - height);
  const top = Math.min(maximumScrollTop, Math.max(0, scrollTop));
  const firstVisible = Math.floor(top / safeRowHeight);
  const pastLastVisible = Math.min(count, Math.ceil((top + height) / safeRowHeight));

  return {
    start: Math.max(0, firstVisible - overscan),
    end: Math.min(count, Math.max(firstVisible + 1, pastLastVisible) + overscan),
    totalHeight,
  };
}

interface ScrollTopForIndexInput {
  index: number;
  itemCount: number;
  scrollTop: number;
  viewportHeight: number;
  rowHeight?: number;
}

/**
 * The fixed-height equivalent of `scrollIntoView({ block: "nearest" })`.
 * It works before a virtual row is mounted, which is what lets a project open
 * directly on a remembered clip thousands of rows into the queue.
 */
export function scrollTopForQueueIndex({
  index,
  itemCount,
  scrollTop,
  viewportHeight,
  rowHeight = QUEUE_ROW_HEIGHT,
}: ScrollTopForIndexInput): number {
  const count = Math.max(0, Math.floor(itemCount));
  if (count === 0 || index < 0 || index >= count || !Number.isFinite(viewportHeight)) {
    return Math.max(0, scrollTop);
  }

  const safeRowHeight = Math.max(1, rowHeight);
  const height = Math.max(0, viewportHeight);
  const maximumScrollTop = Math.max(0, count * safeRowHeight - height);
  const current = Math.min(maximumScrollTop, Math.max(0, scrollTop));
  const rowTop = index * safeRowHeight;
  const rowBottom = rowTop + safeRowHeight;

  if (rowTop < current) return rowTop;
  if (rowBottom > current + height) return Math.min(maximumScrollTop, rowBottom - height);
  return current;
}
