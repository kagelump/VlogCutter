import { useLayoutEffect, useMemo, useState } from "react";
import type { CSSProperties, RefObject, UIEvent } from "react";
import { api } from "../api";
import type { Asset, AssetAnalysisIndexEntry } from "../types";
import { clipTitle, formatDuration } from "../review/format";
import { mediaAvailability, mediaAvailabilityDescription } from "../review/summaries";
import { isRawOrientationThumbnail, renderedRotation } from "../review/rotation";
import { QUEUE_ROW_HEIGHT, queueWindow, scrollTopForQueueIndex } from "./queueWindow";

// The first browser render happens before ResizeObserver can report the real
// workbench height. This estimate is intentionally generous while still
// bounding a 10,000-clip project's initial DOM to a few dozen rows.
const INITIAL_QUEUE_VIEWPORT_HEIGHT = 720;

export interface QueuePanelProps {
  projectName: string | null;
  /** Already filtered and ordered: the panel renders the queue, it does not solve it. */
  assets: Asset[];
  selectedId: string;
  query: string;
  /** Read only for the unusable verdict; the reason text itself lives in the shelf. */
  analysisIndex: Record<string, AssetAnalysisIndexEntry>;
  showRemoved: boolean;
  showUnusableOnly: boolean;
  /** Totals across the project, not the filtered view — they decide whether a toggle exists at all. */
  unusableCount: number;
  removedCount: number;
  /** The shell scrolls the selection into view through this. */
  listRef: RefObject<HTMLDivElement | null>;
  /** The project's reads are in flight. */
  loading: boolean;
  /** Why the queue could not be read; empty when it was read. */
  loadError: string;
  /** Read the project again — the only way back from a failed load. */
  onReload(): void;
  onQueryChange(value: string): void;
  onSelect(id: string): void;
  onToggleRemovedView(): void;
  onToggleUnusableView(): void;
}

/**
 * The label queue (spec §3.3).
 *
 * Every row is a fixed height and carries the same four facts, because this
 * list is read by position at speed rather than word by word: pip, thumbnail
 * with queue index, title over filename, duration. The verdict rides inside
 * the flexible column so a long reason truncates instead of widening the row.
 */
export function QueuePanel({
  projectName,
  assets,
  selectedId,
  query,
  analysisIndex,
  showRemoved,
  showUnusableOnly,
  unusableCount,
  removedCount,
  listRef,
  loading,
  loadError,
  onReload,
  onQueryChange,
  onSelect,
  onToggleRemovedView,
  onToggleUnusableView,
}: QueuePanelProps) {
  // ResizeObserver is available in the desktop webview and modern browsers.
  // A layoutless or older engine falls back to the complete list, preserving
  // correctness when it cannot provide a trustworthy viewport measurement.
  const [viewportHeight, setViewportHeight] = useState(() => (
    typeof ResizeObserver === "undefined" ? Number.POSITIVE_INFINITY : INITIAL_QUEUE_VIEWPORT_HEIGHT
  ));
  const [scrollTop, setScrollTop] = useState(0);

  useLayoutEffect(() => {
    const list = listRef.current;
    if (!list || typeof ResizeObserver === "undefined") return;

    const measure = (height: number) => {
      // Focus presentation hides the mounted queue with display:none. Keep the
      // last useful viewport in that state so navigation performed in Focus
      // still lands on a visible row when the queue returns.
      if (height <= 0) return;
      const next = height;
      setViewportHeight((current) => current === next ? current : next);
    };
    const observer = new ResizeObserver((entries) => {
      measure(entries[0]?.contentRect.height ?? list.clientHeight);
    });
    observer.observe(list);
    if (list.clientHeight > 0) measure(list.clientHeight);
    return () => observer.disconnect();
  }, [listRef]);

  const assetIndexById = useMemo(
    () => new Map(assets.map((asset, index) => [asset.id, index])),
    [assets],
  );
  const selectedIndex = assetIndexById.get(selectedId) ?? -1;

  // Unlike DOM scrollIntoView, this can move to a selected row that has not
  // been mounted yet. The App's existing scrollIntoView effect then remains
  // the final nearest-edge adjustment once the row is present.
  useLayoutEffect(() => {
    const list = listRef.current;
    if (!list || selectedIndex < 0) return;
    const measuredHeight = list.clientHeight > 0 ? list.clientHeight : viewportHeight;
    const next = scrollTopForQueueIndex({
      index: selectedIndex,
      itemCount: assets.length,
      scrollTop: list.scrollTop,
      viewportHeight: measuredHeight,
    });
    if (Math.abs(next - list.scrollTop) < 1) return;
    list.scrollTop = next;
    setScrollTop(next);
  }, [assets.length, listRef, selectedId, selectedIndex]);

  const window = queueWindow({
    itemCount: assets.length,
    scrollTop,
    viewportHeight,
  });
  const visibleAssets = assets.slice(window.start, window.end);
  const isVirtualized = window.start > 0 || window.end < assets.length;

  const onListScroll = (event: UIEvent<HTMLDivElement>) => {
    const next = event.currentTarget.scrollTop;
    setScrollTop((current) => current === next ? current : next);
  };

  const rows = visibleAssets.map((asset, visibleIndex) => {
    const index = window.start + visibleIndex;
    const thumbnailUrl = api.thumbnailUrl(asset);
    const rotation = renderedRotation(asset, isRawOrientationThumbnail(asset));
    const verdict = analysisIndex[asset.id];
    const availability = mediaAvailability(asset);
    return (
      <button
        className={`queue-item ${asset.id === selectedId ? "selected" : ""}`}
        key={asset.id}
        onClick={() => onSelect(asset.id)}
        role="option"
        aria-selected={asset.id === selectedId}
        aria-posinset={index + 1}
        aria-setsize={assets.length}
        style={isVirtualized ? {
          position: "absolute",
          top: index * QUEUE_ROW_HEIGHT,
          left: 0,
        } : undefined}
      >
        <span className={`label-pip ${asset.labels.length ? "labeled" : ""}`} aria-hidden="true" />
        <span
          className={`queue-thumbnail ${rotation % 180 !== 0 ? "quarter-turn" : ""}`}
          style={{ "--thumbnail-rotation": `${rotation}deg` } as CSSProperties}
        >
          {thumbnailUrl ? <img src={thumbnailUrl} alt="" loading="lazy" /> : <span className="thumbnail-empty">Q0</span>}
          <span className="queue-index">{String(index + 1).padStart(3, "0")}</span>
        </span>
        <span className="queue-main">
          <strong>{clipTitle(asset)}</strong>
          {/* The filename stays visible but stops being the headline
              once analysis has named the clip. The verdict sits on
              this line, inside the flexible column, so a long reason
              truncates instead of widening the row. */}
          <span className="queue-meta">
            {/* Whether the source media is reachable is worth knowing while
                scanning, before an operation that needs it fails. It is a
                fact, not an alarm: an offline original with a proxy is the
                mode this application was built for. */}
            {availability !== "available" && (
              <span
                className={`offline-pip ${availability === "blocked" ? "blocked" : ""}`}
                title={mediaAvailabilityDescription(asset)}
              >
                <span aria-hidden="true">{availability === "blocked" ? "no media" : "offline"}</span>
                {/* The row is read by its text, so the mark says what it means
                    rather than making a screen-reader user find the tooltip. */}
                <span className="sr-only">{mediaAvailabilityDescription(asset)}</span>
              </span>
            )}
            {verdict?.usable === false && (
              <span className="unusable-pip" title={`Analysis flagged this clip: ${verdict.unusableReason || "unusable"}`}>
                {verdict.unusableReason || "unusable"}
              </span>
            )}
            <span>{asset.title.trim() ? `${asset.filename} · ${asset.camera}` : asset.camera}</span>
          </span>
        </span>
        <span className="queue-tail">
          <time>{formatDuration(asset.durationSeconds)}</time>
          {asset.labels.length > 0 && <span className="label-count">{asset.labels.length} label{asset.labels.length === 1 ? "" : "s"}</span>}
        </span>
      </button>
    );
  });

  return (
    <aside className="queue-panel">
      <div className="panel-heading">
        <div>
          <span className="eyebrow">{showRemoved ? "Removed clips" : "Label queue"}</span>
          <h1>{projectName ?? "No project"}</h1>
        </div>
        <div className="queue-heading-actions">
          {/* The project's reads have an owner that can be asked to run them
              again, so the interface offers it. Without this control a read
              that failed — a locked catalog, a volume still spinning up — left
              the session empty until the app was relaunched, and a change made
              elsewhere could only be noticed by writing something. */}
          <button
            className="queue-reload"
            onClick={onReload}
            disabled={loading}
            aria-label="Reload clips"
            title="Read this project again"
          >⟳</button>
          {unusableCount > 0 && !showRemoved && (
            <button
              className={`queue-view-toggle ${showUnusableOnly ? "active" : ""}`}
              onClick={onToggleUnusableView}
              aria-pressed={showUnusableOnly}
              title="Clips the analysis judged unusable"
            >{showUnusableOnly ? "All clips" : `Unusable ${unusableCount}`}</button>
          )}
          {removedCount > 0 && (
            <button
              className={`queue-view-toggle ${showRemoved ? "active" : ""}`}
              onClick={onToggleRemovedView}
              aria-pressed={showRemoved}
            >{showRemoved ? "Back to labels" : `Removed ${removedCount}`}</button>
          )}
          <span className="count-badge">{assets.length}</span>
        </div>
      </div>
      <label className="search-box">
        <span className="sr-only">Filter clips</span>
        <input
          id="queue-search"
          value={query}
          onChange={(event) => onQueryChange(event.target.value)}
          placeholder="Filter titles, labels, notes, speech…"
        />
        <kbd>/</kbd>
      </label>
      <div
        className="queue-list"
        ref={listRef}
        role="listbox"
        aria-label="Clips"
        tabIndex={0}
        onScroll={onListScroll}
      >
        {assets.length === 0 && loadError ? (
          <div className="empty-queue">
            <strong>This project could not be read.</strong>
            <span>{loadError}</span>
            <button className="button secondary" onClick={onReload} disabled={loading}>Try again</button>
          </div>
        ) : assets.length === 0 ? (
          <div className="empty-queue">
            <strong>{showRemoved ? "No removed clips." : "No clips here yet."}</strong>
            <span>{showRemoved ? "Removed clips can be restored here." : "Add a NAS or drive folder to begin."}</span>
          </div>
        ) : isVirtualized ? (
          <div role="presentation" style={{ position: "relative", height: window.totalHeight }}>
            {rows}
          </div>
        ) : rows}
      </div>
      <div className="shortcut-strip" aria-label="Keyboard shortcuts">
        <span><kbd>J</kbd><kbd>K</kbd> move</span>
        <span><kbd>Space</kbd> play</span>
        <span><kbd>L</kbd> label</span>
        <span><kbd>R</kbd> remove</span>
      </div>
    </aside>
  );
}
