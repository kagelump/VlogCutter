import { convertFileSrc } from "@tauri-apps/api/core";
import type { RefObject } from "react";
import { useEffect, useRef } from "react";
import type { ImportCutoff, ImportPreflight, ImportPreviewItem, ImportRunReport, PreflightItem } from "../types";
import { formatDuration } from "../review/format";

/**
 * The per-card file list shown inside Smart ingest, both as a live preflight
 * preview and as the completed-run report. Kept in its own file because
 * later tasks grow this row (#66 adds a per-row "Import from here" cutoff),
 * and both preflight and the completed-run report reuse the same row
 * presentation (#65) rather than the report dropping back to filename-only
 * rows.
 */

export function isAttentionItem(item: PreflightItem) {
  return item.classification === "pathCollision" || item.classification === "unreadable";
}

/**
 * Where a row sits relative to the stored "start here" boundary (#66).
 *
 * Derived from the row's own classification and the plan's cutoff rather than
 * re-deriving the comparison here: preflight already applied the rule, and a
 * second implementation of "is this before the boundary" is a second chance to
 * disagree with the one that actually decides what gets copied.
 */
export function cutoffRole(
  item: PreflightItem,
  cutoff: ImportCutoff | null | undefined,
): "boundary" | "excluded" | "relinkBeforeCutoff" | null {
  if (!cutoff) return null;
  if (item.relativePath === cutoff.relativePath) return "boundary";
  if (item.classification === "excludedByCutoff") return "excluded";
  // A relink the cutoff deliberately let through. Marked so it reads as a
  // decision rather than as the filter having missed something.
  if (item.classification === "knownRelinkable" && item.relativePath < cutoff.relativePath) {
    return "relinkBeforeCutoff";
  }
  return null;
}

const CUTOFF_ROLE_LABELS: Record<NonNullable<ReturnType<typeof cutoffRole>>, string> = {
  boundary: "Import starts here",
  excluded: "Before the cutoff",
  relinkBeforeCutoff: "Before the cutoff · original still needed",
};

type ReportFile = ImportRunReport["files"][number];

/**
 * Capture time has three possible sources, most to least trustworthy, and
 * this order is the whole point of keeping it explicit rather than picking
 * "whichever is set":
 *
 *  1. `reportProbed` — the completed import's own ffprobe read of the asset
 *     that now actually exists (`ImportRunReport.files[].timestamp`).
 *  2. `probed` — import-preview's on-demand ffprobe of the source file,
 *     before import, for a row the dialog is actually showing.
 *  3. `filesystem` — preflight's cheap, always-available filesystem mtime
 *     (`PreflightItem.filesystemModifiedAt`). Not a capture time at all, just
 *     the best free guess.
 *
 * A later, cheaper source never overrides an earlier, more trustworthy one
 * that is already known, even if both happen to be present.
 */
function resolveCaptureTime(
  item: PreflightItem,
  preview: ImportPreviewItem | undefined,
  reportFile: ReportFile | undefined,
): { value: string | null; source: "reportProbed" | "probed" | "filesystem" | "unknown" } {
  if (reportFile?.timestamp) return { value: reportFile.timestamp, source: "reportProbed" };
  if (preview?.capturedAt) return { value: preview.capturedAt, source: "probed" };
  if (item.filesystemModifiedAt) return { value: item.filesystemModifiedAt, source: "filesystem" };
  return { value: null, source: "unknown" };
}

const CAPTURE_TIME_TITLES: Record<ReturnType<typeof resolveCaptureTime>["source"], string> = {
  reportProbed: "Probed from the imported file",
  probed: "Probed from the source file",
  filesystem: "Estimated from the file's last-modified time, not a probed capture time",
  unknown: "",
};

/** Same precedence as capture time: preflight itself never knows duration. */
function resolveDuration(preview: ImportPreviewItem | undefined, reportFile: ReportFile | undefined): number | null {
  if (reportFile?.durationSeconds != null) return reportFile.durationSeconds;
  if (preview?.durationSeconds != null) return preview.durationSeconds;
  return null;
}

/** A short, stable placeholder for a row with no poster (yet, or ever). */
function extensionGlyph(filename: string): string {
  const extension = filename.split(".").pop();
  return extension ? extension.slice(0, 4).toUpperCase() : "FILE";
}

export function FileLedger({
  plan,
  report,
  listRef,
  previews,
  onVisible,
  onSetCutoff,
  onClearCutoff,
}: {
  plan: ImportPreflight | null;
  report?: ImportRunReport;
  listRef?: RefObject<HTMLUListElement | null>;
  /** Import-preview results keyed by `PreflightItem.path`, when the caller fetches them. */
  previews?: Map<string, ImportPreviewItem>;
  /**
   * Called with row paths that just scrolled into view and have no preview
   * yet. Wiring this is what keeps a several-hundred-clip card from
   * decoding every file just to open the dialog — only rows the user can
   * actually see are ever requested. Omitting it is safe: every row simply
   * keeps its placeholder/best-available data, which is still a complete,
   * non-blocking row.
   */
  onVisible?: (paths: string[]) => void;
  /**
   * Set the boundary to this row (#66). Absent in the completed-run report and
   * for a source with no card identity to hang a durable rule on, which is
   * what keeps the rows read-only in exactly the places a rule cannot exist.
   */
  onSetCutoff?: (item: PreflightItem) => void;
  /** Forget the boundary, offering the whole card again. */
  onClearCutoff?: () => void;
}) {
  const internalListRef = useRef<HTMLUListElement | null>(null);
  const requestedRef = useRef<Set<string>>(new Set());

  function setListRef(node: HTMLUListElement | null) {
    internalListRef.current = node;
    if (listRef) listRef.current = node;
  }

  useEffect(() => {
    if (!onVisible) return;
    // jsdom (and older engines) have no IntersectionObserver; rendering every
    // row's placeholder forever is the correct fallback there, not eagerly
    // requesting previews for a plan that might hold hundreds of files.
    if (typeof IntersectionObserver === "undefined") return;
    const root = internalListRef.current;
    if (!root) return;
    const observer = new IntersectionObserver(
      (entries) => {
        const fresh = entries
          .filter((entry) => entry.isIntersecting)
          .map((entry) => (entry.target as HTMLElement).dataset.path)
          .filter((path): path is string => typeof path === "string")
          .filter((path) => !requestedRef.current.has(path) && !previews?.has(path));
        if (fresh.length === 0) return;
        for (const path of fresh) requestedRef.current.add(path);
        onVisible(fresh);
      },
      { root, rootMargin: "160px" },
    );
    for (const row of root.querySelectorAll<HTMLElement>("li[data-path]")) observer.observe(row);
    return () => observer.disconnect();
    // `previews` is read for its value at intersection time, not to decide
    // whether to re-observe; re-creating the observer on every fetched
    // preview would be wasted work `requestedRef` already makes unnecessary.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [onVisible, plan]);

  if (!plan) return <p>No file plan is available.</p>;
  const failed = new Map(report?.outcome.failures.map((failure) => [failure.path, failure.error]));
  const metadata = new Map(report?.files.map((file) => [file.path, file]));
  // The report view is a record of what happened; a boundary is a decision
  // about what will happen, so the controls exist only in the preflight view.
  const cutoff = report ? null : plan.cutoff ?? null;
  const canSetCutoff = Boolean(onSetCutoff) && !report;
  const excluded = plan.excludedByCutoff ?? 0;
  const relinkBefore = plan.relinkableBeforeCutoff ?? 0;
  return (
    <>
      {canSetCutoff && !cutoff && (
        <p className="import-cutoff-hint">
          Long-lived card? Pick the clip the project starts at — hover any row and choose{" "}
          <strong>Import from here</strong>. Everything before it is ignored, and this card
          remembers it.
        </p>
      )}
      {cutoff && (
        <p className="import-cutoff-banner" data-testid="import-cutoff-banner">
          <span className="import-cutoff-mark" aria-hidden="true" />
          <span>
            <strong>Importing from {cutoff.filename}</strong>
            <small className="import-cutoff-count">
              {excluded === 0
                ? "Nothing earlier is left to import."
                : `${excluded} earlier ${excluded === 1 ? "clip is" : "clips are"} excluded.`}
              {" "}
              Remembered for this card, {cutoff.cardLabel || "this volume"}, until you clear it.
            </small>
            {relinkBefore > 0 && (
              <small className="import-cutoff-relink">
                {relinkBefore} earlier {relinkBefore === 1 ? "clip is" : "clips are"} still
                reconnected — the cutoff never hides an original this project has lost.
              </small>
            )}
          </span>
          {onClearCutoff && (
            <button type="button" className="text-button" onClick={onClearCutoff}>
              Clear cutoff
            </button>
          )}
        </p>
      )}
    <ul ref={setListRef}>
      {plan.items.map((item) => {
        const preview = previews?.get(item.path);
        const reportFile = metadata.get(item.path);
        const capture = resolveCaptureTime(item, preview, reportFile);
        const duration = resolveDuration(preview, reportFile);
        const thumbnailSrc = preview?.thumbnailPath ? convertFileSrc(preview.thumbnailPath) : null;
        const role = cutoffRole(item, cutoff);
        return (
          <li
            key={item.path}
            data-path={item.path}
            data-attention={isAttentionItem(item) ? "true" : undefined}
            data-cutoff={role ?? undefined}
            tabIndex={-1}
          >
            <span className="import-file-thumb">
              {thumbnailSrc ? (
                <img src={thumbnailSrc} alt="" loading="lazy" />
              ) : (
                <span className="import-file-thumb-placeholder" aria-hidden="true">
                  {extensionGlyph(item.filename)}
                </span>
              )}
            </span>
            <span className="import-file-name">{item.filename}</span>
            <time dateTime={capture.value ?? undefined} title={CAPTURE_TIME_TITLES[capture.source]}>
              {formatImportTimestamp(capture.value)}
            </time>
            <span className="import-file-duration">{duration != null ? formatDuration(duration) : "—"}</span>
            <span className="import-file-status">{failed.has(item.path) ? "failed" : item.classification.replace(/([A-Z])/g, " $1").toLowerCase()}</span>
            {canSetCutoff && (
              role === "boundary" ? (
                <span className="import-file-cutoff is-boundary">{CUTOFF_ROLE_LABELS.boundary}</span>
              ) : (
                <button
                  type="button"
                  className="text-button import-file-cutoff"
                  aria-label={`Import from ${item.filename} onward, ignoring everything before it`}
                  onClick={() => onSetCutoff!(item)}
                >
                  Import from here
                </button>
              )
            )}
            {/* Only the surprising case gets a second line. An excluded row
                already says "excluded by cutoff" in its status column, and
                repeating that on every one of a hundred rows is noise; a
                relink the cutoff deliberately let through is the one a reader
                would otherwise mistake for the filter having leaked. */}
            {role === "relinkBeforeCutoff" && (
              <span className="import-file-cutoff-note">{CUTOFF_ROLE_LABELS[role]}</span>
            )}
            {(failed.get(item.path) || item.error) && <small>{failed.get(item.path) || item.error}</small>}
          </li>
        );
      })}
    </ul>
    </>
  );
}

function formatImportTimestamp(value: string | null) {
  if (!value) return "—";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}
