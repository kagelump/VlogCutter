import { useEffect, useState } from "react";
import { useSearchParams } from "react-router";
import { useInfiniteQuery } from "@tanstack/react-query";
import type { GlobalMediaVerificationState, LibrarySearchItem, LibrarySort, NodeAvailabilityState } from "@rcdvlog/project-client";
import { keys, useClient } from "../../app/client";
import { Page } from "../../app/Shell";
import { formatBytes, plural } from "../../lib/format";
import { useDocumentTitle } from "../../lib/hooks";
import { Badge, Button, EmptyState, ErrorMessage, Icon, Loading, PageHeader, type Tone } from "../../ui";
import { AddToProjectDialog } from "./AddToProjectDialog";
import styles from "./Library.module.css";

const SORTS: Array<{ value: LibrarySort; label: string }> = [
  { value: "createdDesc", label: "Newest" },
  { value: "filenameAsc", label: "Filename" },
  { value: "sizeDesc", label: "Largest" },
];

const IDENTITY: Record<GlobalMediaVerificationState, { label: string; tone: Tone }> = {
  verified: { label: "Verified", tone: "positive" },
  provisional: { label: "Provisional", tone: "warning" },
  unverified: { label: "Unverified", tone: "warning" },
  mismatch: { label: "Mismatch", tone: "critical" },
};

const AVAILABILITY: Partial<Record<NodeAvailabilityState, string>> = {
  accessible_on_another_node: "On another Mac",
  temporarily_unavailable: "Offline",
  missing_after_completed_observation: "Missing",
  fingerprint_mismatch: "Changed on disk",
};

export function LibraryPage() {
  const client = useClient();
  const [params, setParams] = useSearchParams();
  const query = params.get("q") ?? "";
  // A filename search is always listed by filename; the identity filter only works on the newest-first listing.
  const sort: LibrarySort = query ? "filenameAsc" : ((params.get("sort") as LibrarySort | null) ?? "createdDesc");
  const identity = sort === "createdDesc" ? ((params.get("identity") as GlobalMediaVerificationState | null) ?? "") : "";
  const [draft, setDraft] = useState(query);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [adding, setAdding] = useState(false);
  useDocumentTitle("Library");

  useEffect(() => setDraft(query), [query]);

  const filters = { query, sort, identity };
  const library = useInfiniteQuery({
    queryKey: keys.library(filters),
    queryFn: ({ pageParam }) => client.listLibraryMedia({
      limit: 48,
      sort,
      ...(query ? { filenamePrefix: query } : {}),
      ...(identity ? { verificationState: identity } : {}),
      ...(pageParam ? { after: pageParam } : {}),
    }),
    initialPageParam: null as string | null,
    getNextPageParam: (page) => page.nextCursor,
  });
  const items = library.data?.pages.flatMap((page) => page.items) ?? [];

  function update(values: Record<string, string>) {
    setParams((current) => {
      const next = new URLSearchParams(current);
      for (const [key, value] of Object.entries(values)) if (value) next.set(key, value); else next.delete(key);
      return next;
    }, { replace: true });
  }

  function toggle(id: string) {
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id); else next.add(id);
      return next;
    });
  }

  return (
    <Page>
      <PageHeader title="Library" subtitle="Every file the server has indexed, across all storage." />

      <form className={styles.filters} role="search" onSubmit={(event) => { event.preventDefault(); update({ q: draft.trim() }); }}>
        <div className={styles.search}>
          <Icon name="search" size={16} />
          <input type="search" value={draft} onChange={(event) => setDraft(event.target.value)} placeholder="Filename starts with…" aria-label="Filename starts with" />
        </div>
        <select aria-label="Sort" value={sort} disabled={Boolean(query)} title={query ? "Search results are sorted by filename" : undefined} onChange={(event) => update({ sort: event.target.value, identity: "" })}>
          {SORTS.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
        </select>
        <select aria-label="Identity" value={identity} disabled={sort !== "createdDesc"} title={sort !== "createdDesc" ? "Available when sorted by newest" : undefined} onChange={(event) => update({ identity: event.target.value })}>
          <option value="">Any identity</option>
          {(Object.keys(IDENTITY) as GlobalMediaVerificationState[]).map((state) => <option key={state} value={state}>{IDENTITY[state].label}</option>)}
        </select>
        <Button type="submit" className={styles.searchButton}>Search</Button>
      </form>

      <ErrorMessage error={library.error} onRetry={() => void library.refetch()} />
      {library.isPending ? <Loading /> : null}
      {library.isSuccess && !items.length ? (
        <EmptyState title="No files" action={query || identity ? <Button onClick={() => update({ q: "", identity: "" })}>Clear filters</Button> : undefined}>
          {query || identity ? "Nothing matches these filters." : "Scan a storage root from Admin to index footage."}
        </EmptyState>
      ) : null}

      <ul className={styles.grid} aria-busy={library.isFetching}>
        {items.map((item) => <MediaCard key={item.media.id} item={item} selected={selected.has(item.media.id)} onToggle={() => toggle(item.media.id)} />)}
      </ul>

      {library.hasNextPage ? (
        <div className={styles.more}><Button busy={library.isFetchingNextPage} onClick={() => void library.fetchNextPage()}>Load more</Button></div>
      ) : null}

      {selected.size ? (
        <div className={styles.selectionBar} role="region" aria-label="Selection">
          <strong>{plural(selected.size, "file")} selected</strong>
          <Button variant="ghost" onClick={() => setSelected(new Set())}>Clear</Button>
          <Button variant="primary" icon="plus" onClick={() => setAdding(true)}>Add to project</Button>
        </div>
      ) : null}

      <AddToProjectDialog
        open={adding}
        mediaObjectIds={[...selected]}
        onClose={() => setAdding(false)}
        onAdded={() => { setSelected(new Set()); setAdding(false); void library.refetch(); }}
      />
    </Page>
  );
}

function MediaCard({ item, selected, onToggle }: { item: LibrarySearchItem; selected: boolean; onToggle: () => void }) {
  const camera = [item.cameraMake, item.cameraModel].filter(Boolean).join(" ") || "Unknown camera";
  const identity = IDENTITY[item.media.verificationState];
  const availability = AVAILABILITY[item.availability];
  return (
    <li className={styles.card} data-selected={selected || undefined}>
      <label className={styles.thumb}>
        <input type="checkbox" checked={selected} onChange={onToggle} aria-label={`Select ${item.filename}`} />
        {item.thumbnailRepresentationId
          ? <img src={`/api/v1/media/${encodeURIComponent(item.thumbnailRepresentationId)}/content`} alt="" loading="lazy" />
          : <Icon name="film" size={28} />}
      </label>
      <div className={styles.cardBody}>
        <strong title={item.filename}>{item.filename}</strong>
        <span>{camera} · {formatBytes(item.media.sizeBytes)}</span>
        {item.media.verificationState !== "verified" || availability || item.projectMembershipCount ? (
          <span className={styles.cardTags}>
            {item.media.verificationState !== "verified" ? <Badge tone={identity.tone}>{identity.label}</Badge> : null}
            {availability ? <Badge tone="warning">{availability}</Badge> : null}
            {item.projectMembershipCount ? <Badge tone="accent">In {plural(item.projectMembershipCount, "project")}</Badge> : null}
          </span>
        ) : null}
      </div>
    </li>
  );
}
