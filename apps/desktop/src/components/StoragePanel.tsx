import { useState } from "react";
import type { CacheCleanupReport, CacheKind, ProjectStorageReport } from "../types";
import { formatBytes } from "../review/summaries";
import { CACHE_KIND_LABELS, CACHE_KIND_ORDER } from "./storageLabels";

/**
 * Understanding and reclaiming what a project costs on this machine (#16).
 *
 * Two things this panel refuses to do. It never presents a single "clear cache"
 * button: the categories differ by orders of magnitude in what they cost to
 * replace — remaking proxies needs the card back in the slot, and replacing
 * downloaded models means gigabytes over the network — so the user picks. And
 * it never leads with the total: the useful number is what each category costs
 * *and* what living without it means, which is why every row carries both.
 *
 * The catalog is shown alongside deliberately, and is not selectable. Seeing
 * that the irreplaceable half of a 40 GB project weighs two megabytes is the
 * fact that makes clearing the rest an easy decision.
 */
export function StoragePanel({
  report,
  busy,
  error,
  lastCleanup,
  onRefresh,
  onClear,
}: {
  report: ProjectStorageReport | null;
  busy: boolean;
  error: string;
  lastCleanup: CacheCleanupReport | null;
  onRefresh(): void;
  onClear(kinds: CacheKind[]): void;
}) {
  const [selected, setSelected] = useState<Set<CacheKind>>(new Set());

  function toggle(kind: CacheKind) {
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(kind)) next.delete(kind);
      else next.add(kind);
      return next;
    });
  }

  const byKind = new Map(report?.categories.map((category) => [category.kind, category]) ?? []);
  const chosen = [...selected].filter((kind) => (byKind.get(kind)?.bytes ?? 0) > 0);
  const chosenBytes = chosen.reduce((total, kind) => total + (byKind.get(kind)?.bytes ?? 0), 0);
  const needsOriginals = chosen.some((kind) => CACHE_KIND_LABELS[kind].needsOriginals);

  return (
    <section className="storage-panel" aria-label="Project storage">
      <div className="storage-heading">
        <div>
          <span className="section-label">Local storage</span>
          {report && (
            <p className="storage-total">
              {formatBytes(report.cacheBytes)} of regenerable cache ·{" "}
              <strong>{formatBytes(report.catalogBytes)}</strong> of your own work
            </p>
          )}
        </div>
        <button type="button" className="text-button" disabled={busy} onClick={onRefresh}>
          {report ? "Recalculate" : "Measure"}
        </button>
      </div>

      {error && <p className="context-modal-error" role="alert">{error}</p>}

      {report ? (
        <>
          <ul className="storage-list">
            {CACHE_KIND_ORDER.map((kind) => {
              const category = byKind.get(kind);
              if (!category) return null;
              const label = CACHE_KIND_LABELS[kind];
              const empty = category.bytes === 0;
              return (
                <li key={kind} data-kind={kind} data-scope={category.scope} data-empty={empty ? "true" : undefined}>
                  <label>
                    <input
                      type="checkbox"
                      checked={selected.has(kind)}
                      disabled={busy || empty}
                      onChange={() => toggle(kind)}
                      aria-label={`Clear ${label.title}`}
                    />
                    <span className="storage-kind">{label.title}</span>
                  </label>
                  <span className="storage-size">{empty ? "nothing stored" : formatBytes(category.bytes)}</span>
                  {category.scope === "catalog" && (
                    <span className="storage-shared">shared by every project in this catalog</span>
                  )}
                  <small>
                    {!empty && `${category.fileCount} file${category.fileCount === 1 ? "" : "s"} · `}
                    {label.regeneration}
                  </small>
                  <small className="storage-unavailable">{label.unavailable}</small>
                </li>
              );
            })}
          </ul>

          <p className="storage-keeps">
            Clearing never touches your originals, your archive copies, or anything you have
            written down. Labels, notes, chronology, and import history all live in the catalog and
            stay exactly as they are.
          </p>

          {needsOriginals && chosen.length > 0 && (
            <p className="storage-warning" role="status">
              Remaking this needs the original media connected again. You can clear it now with the
              drive unplugged — nothing here reads your footage — but it will stay missing until the
              media is back.
            </p>
          )}

          {lastCleanup && (
            <p className="storage-result" role="status">
              Freed {formatBytes(lastCleanup.removedBytes)} across {lastCleanup.removedFiles} file
              {lastCleanup.removedFiles === 1 ? "" : "s"}.
              {lastCleanup.failures.length > 0
                ? ` ${lastCleanup.failures.length} item${lastCleanup.failures.length === 1 ? "" : "s"} could not be removed and were left in place.`
                : ""}
            </p>
          )}

          <div className="storage-actions">
            <button
              type="button"
              className="button secondary"
              disabled={busy || chosen.length === 0}
              onClick={() => onClear(chosen)}
            >
              {chosen.length === 0
                ? "Choose what to clear"
                : `Clear ${formatBytes(chosenBytes)}`}
            </button>
          </div>
        </>
      ) : (
        <p className="storage-empty">
          Measuring walks this project’s cache folder. It reads nothing about your media and never
          needs a drive connected.
        </p>
      )}
    </section>
  );
}
