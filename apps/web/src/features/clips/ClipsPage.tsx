import { useCallback, useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router";
import { Page } from "../../app/Shell";
import { useDocumentTitle, useHotkeys, useMediaQuery } from "../../lib/hooks";
import { Button, Dialog, EmptyState, ErrorMessage, Kbd, Loading } from "../../ui";
import { useProjectId } from "../projects/ProjectLayout";
import { ClipInspector, type InspectorHandle } from "./ClipInspector";
import { ClipPlayer, type PlayerHandle } from "./ClipPlayer";
import { ClipQueue } from "./ClipQueue";
import { useClipQueue, useReviewAsset } from "./data";
import styles from "./Clips.module.css";

const SHORTCUTS: Array<[string, string]> = [
  ["J  →", "Next clip"],
  ["K  ←", "Previous clip"],
  ["Space", "Play / pause"],
  ["N", "Edit note"],
  ["L", "Add label"],
  ["R", "Remove or restore clip"],
  ["/", "Search clips"],
  ["Enter", "Save note (Shift+Enter for a new line)"],
  ["Esc", "Leave a field and discard the draft"],
];

export function ClipsPage() {
  const projectId = useProjectId();
  const [params, setParams] = useSearchParams();
  const search = params.get("q") ?? "";
  const queue = useClipQueue(projectId, search);
  const { assets } = queue;
  const selectedParam = params.get("clip");
  const selectedId = selectedParam && assets.some((asset) => asset.projectAssetId === selectedParam)
    ? selectedParam
    : assets[0]?.projectAssetId ?? null;
  const review = useReviewAsset(projectId, selectedId, queue.live);
  const index = assets.findIndex((asset) => asset.projectAssetId === selectedId);
  const wide = useMediaQuery("(min-width: 1100px)");
  const [queueOpen, setQueueOpen] = useState(false);
  const [helpOpen, setHelpOpen] = useState(false);
  const player = useRef<PlayerHandle>(null);
  const inspector = useRef<InspectorHandle>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  useDocumentTitle(review.data?.filename ?? "Clips");

  const select = useCallback((assetId: string) => {
    setParams((current) => {
      const next = new URLSearchParams(current);
      next.set("clip", assetId);
      return next;
    }, { replace: true });
    setQueueOpen(false);
  }, [setParams]);

  const setSearch = useCallback((value: string) => {
    setParams((current) => {
      const next = new URLSearchParams(current);
      if (value) next.set("q", value); else next.delete("q");
      next.delete("clip");
      return next;
    }, { replace: true });
  }, [setParams]);

  const canNext = index < assets.length - 1 || queue.hasNextPage;
  // URL updates land asynchronously; rapid J/K presses build on the pending position, not the rendered one.
  const pending = useRef<number | null>(null);
  useEffect(() => { pending.current = null; }, [index]);
  const move = useCallback(async (delta: number) => {
    const from = pending.current ?? index;
    const target = from + delta;
    if (from < 0 || target < 0) return;
    if (target < assets.length) {
      pending.current = target;
      select(assets[target].projectAssetId);
      return;
    }
    if (!queue.hasNextPage || queue.isFetchingNextPage) return;
    const result = await queue.fetchNextPage();
    const next = result.data?.pages.flatMap((page) => page.items)[target];
    if (next) { pending.current = target; select(next.projectAssetId); }
  }, [assets, index, queue, select]);

  const notOnMedia = (event: KeyboardEvent) => !(event.target instanceof HTMLMediaElement);
  useHotkeys({
    j: () => void move(1),
    k: () => void move(-1),
    ArrowRight: (event) => { if (notOnMedia(event)) void move(1); },
    ArrowLeft: (event) => { if (notOnMedia(event)) void move(-1); },
    " ": (event) => { if (notOnMedia(event)) player.current?.toggle(); },
    n: () => inspector.current?.focusNote(),
    l: () => inspector.current?.focusLabel(),
    r: () => inspector.current?.toggleRemoved(),
    "/": () => { if (wide) searchRef.current?.focus(); else setQueueOpen(true); },
    "?": () => setHelpOpen(true),
  });

  // Keep the URL pointing at the clip actually shown, so reloads and shared links land on it.
  useEffect(() => {
    if (selectedId && selectedParam !== selectedId && !queue.isPlaceholderData) {
      setParams((current) => { const next = new URLSearchParams(current); next.set("clip", selectedId); return next; }, { replace: true });
    }
  }, [queue.isPlaceholderData, selectedId, selectedParam, setParams]);

  const queueList = (
    <ClipQueue
      assets={assets}
      selectedId={selectedId}
      onSelect={select}
      search={search}
      onSearch={setSearch}
      searchRef={searchRef}
      hasMore={Boolean(queue.hasNextPage)}
      loadingMore={queue.isFetchingNextPage}
      onLoadMore={() => void queue.fetchNextPage()}
      autoFocusSearch={!wide && queueOpen}
    />
  );

  if (queue.isPending) return <Page><Loading /></Page>;
  if (queue.error && !assets.length) return <Page><ErrorMessage error={queue.error} onRetry={() => void queue.refetch()} /></Page>;

  const counter = assets.length
    ? `${index + 1} / ${assets.length}${queue.hasNextPage ? "+" : ""}`
    : "0";

  return (
    <Page full>
      <div className={styles.workspace}>
        {wide ? <aside className={styles.queueColumn} aria-label="Clips">{queueList}</aside> : null}

        {!assets.length ? (
          <div className={styles.emptyColumn}>
            <EmptyState
              title={search ? "No matching clips" : "No clips in this project"}
              action={search ? <Button onClick={() => setSearch("")}>Clear search</Button> : undefined}
            >
              {search ? `Nothing matches “${search}”.` : "Add footage from the Library."}
            </EmptyState>
          </div>
        ) : (
          <>
            <section className={styles.stageColumn} aria-label="Player">
              <ClipPlayer ref={player} projectId={projectId} asset={review.data ?? null} loading={review.isPending} />
              <div className={styles.transport}>
                <Button icon="chevronLeft" iconOnly label="Previous clip (K)" disabled={index <= 0} onClick={() => void move(-1)} />
                <span className={styles.counter} aria-live="polite">{counter}</span>
                <Button icon="chevronRight" iconOnly label="Next clip (J)" disabled={!canNext} busy={queue.isFetchingNextPage} onClick={() => void move(1)} />
                {!wide ? <Button icon="list" onClick={() => setQueueOpen(true)}>All clips</Button> : null}
                <span className={styles.transportSpacer} />
                <Button variant="ghost" size="sm" icon="keyboard" className={styles.helpButton} onClick={() => setHelpOpen(true)}>Shortcuts</Button>
              </div>
            </section>
            <aside className={styles.inspectorColumn} aria-label="Clip details">
              {review.error ? <ErrorMessage error={review.error} onRetry={() => void review.refetch()} /> : null}
              {review.data ? <ClipInspector ref={inspector} projectId={projectId} asset={review.data} /> : review.isPending ? <Loading /> : null}
            </aside>
          </>
        )}
      </div>

      {!wide ? (
        <Dialog open={queueOpen} onClose={() => setQueueOpen(false)} title={queue.hasNextPage ? `Clips (first ${assets.length})` : `Clips (${assets.length})`} wide>
          {queueList}
        </Dialog>
      ) : null}

      <Dialog open={helpOpen} onClose={() => setHelpOpen(false)} title="Keyboard shortcuts">
        <dl className={styles.shortcuts}>
          {SHORTCUTS.map(([key, action]) => (
            <div key={key}><dt>{key.split(/\s+/).map((part) => <Kbd key={part}>{part}</Kbd>)}</dt><dd>{action}</dd></div>
          ))}
        </dl>
      </Dialog>
    </Page>
  );
}
