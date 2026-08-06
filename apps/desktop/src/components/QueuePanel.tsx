import type { CSSProperties, RefObject } from "react";
import { api } from "../api";
import type { Asset, AssetAnalysisIndexEntry } from "../types";
import { clipTitle, formatDuration } from "../review/format";
import { isRawOrientationThumbnail, renderedRotation } from "../review/rotation";

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
  onQueryChange,
  onSelect,
  onToggleRemovedView,
  onToggleUnusableView,
}: QueuePanelProps) {
  return (
    <aside className="queue-panel">
      <div className="panel-heading">
        <div>
          <span className="eyebrow">{showRemoved ? "Removed clips" : "Label queue"}</span>
          <h1>{projectName ?? "No project"}</h1>
        </div>
        <div className="queue-heading-actions">
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
      <div className="queue-list" ref={listRef} role="listbox" aria-label="Clips" tabIndex={0}>
        {assets.length === 0 ? (
          <div className="empty-queue">
            <strong>{showRemoved ? "No removed clips." : "No clips here yet."}</strong>
            <span>{showRemoved ? "Removed clips can be restored here." : "Add a NAS or drive folder to begin."}</span>
          </div>
        ) : assets.map((asset, index) => {
          const thumbnailUrl = api.thumbnailUrl(asset);
          const rotation = renderedRotation(asset, isRawOrientationThumbnail(asset));
          const verdict = analysisIndex[asset.id];
          return (
            <button
              className={`queue-item ${asset.id === selectedId ? "selected" : ""}`}
              key={asset.id}
              onClick={() => onSelect(asset.id)}
              role="option"
              aria-selected={asset.id === selectedId}
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
        })}
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
