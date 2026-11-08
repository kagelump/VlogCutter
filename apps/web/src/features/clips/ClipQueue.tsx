import { useEffect, useRef, useState, type RefObject } from "react";
import type { ProjectAssetItem } from "@rcdvlog/project-client";
import { formatClock } from "../../lib/format";
import { Button, Icon } from "../../ui";
import styles from "./Clips.module.css";

const SEARCH_DEBOUNCE_MS = 300;

export function ClipQueue({ assets, selectedId, onSelect, search, onSearch, searchRef, hasMore, loadingMore, onLoadMore, autoFocusSearch }: {
  assets: ProjectAssetItem[];
  selectedId: string | null;
  onSelect: (assetId: string) => void;
  search: string;
  onSearch: (value: string) => void;
  searchRef: RefObject<HTMLInputElement | null>;
  hasMore: boolean;
  loadingMore: boolean;
  onLoadMore: () => void;
  autoFocusSearch?: boolean;
}) {
  const [draft, setDraft] = useState(search);
  const listRef = useRef<HTMLOListElement>(null);

  useEffect(() => { setDraft(search); }, [search]);
  useEffect(() => {
    if (draft === search) return;
    const timer = window.setTimeout(() => onSearch(draft.trim()), SEARCH_DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [draft, onSearch, search]);

  // Keep the current clip visible as J/K move through the queue.
  useEffect(() => {
    listRef.current?.querySelector("[aria-current='true']")?.scrollIntoView?.({ block: "nearest" });
  }, [selectedId]);

  return (
    <div className={styles.queue}>
      <div className={styles.queueSearch}>
        <Icon name="search" size={16} />
        <input
          ref={searchRef}
          type="search"
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={(event) => { if (event.key === "Enter") onSearch(draft.trim()); if (event.key === "Escape") (event.target as HTMLInputElement).blur(); }}
          placeholder="Search filename, camera, note, label"
          aria-label="Search clips"
          aria-keyshortcuts="/"
          autoFocus={autoFocusSearch}
        />
      </div>
      <ol ref={listRef} className={styles.queueList}>
        {assets.map((asset) => (
          <li key={asset.projectAssetId}>
            <button
              type="button"
              className={styles.queueItem}
              aria-current={asset.projectAssetId === selectedId}
              data-removed={asset.removed || undefined}
              onClick={() => onSelect(asset.projectAssetId)}
            >
              <span className={styles.queueName}>{asset.filename}</span>
              <span className={styles.queueMeta}>
                <span>{asset.camera || "Unknown camera"}</span>
                <span>{formatClock(asset.durationSeconds)}</span>
                {asset.removed ? <span className={styles.removedTag}>Removed</span> : null}
                {asset.activeNote ? <span title={asset.activeNote}><Icon name="edit" size={12} /><span className="visually-hidden">Has note</span></span> : null}
                {asset.labels.length ? <span title={asset.labels.join(", ")}>{asset.labels.length} {asset.labels.length === 1 ? "label" : "labels"}</span> : null}
              </span>
            </button>
          </li>
        ))}
      </ol>
      {hasMore ? (
        <div className={styles.queueMore}>
          <Button size="sm" busy={loadingMore} onClick={onLoadMore}>Load more</Button>
        </div>
      ) : null}
    </div>
  );
}
