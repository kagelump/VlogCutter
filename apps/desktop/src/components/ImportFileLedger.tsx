import { convertFileSrc } from "@tauri-apps/api/core";
import type { RefObject } from "react";
import { useEffect, useRef } from "react";
import type { ImportPreflight, ImportPreviewItem, ImportRunReport, PreflightItem } from "../types";
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
  return (
    <ul ref={setListRef}>
      {plan.items.map((item) => {
        const preview = previews?.get(item.path);
        const reportFile = metadata.get(item.path);
        const capture = resolveCaptureTime(item, preview, reportFile);
        const duration = resolveDuration(preview, reportFile);
        const thumbnailSrc = preview?.thumbnailPath ? convertFileSrc(preview.thumbnailPath) : null;
        return (
          <li key={item.path} data-path={item.path} data-attention={isAttentionItem(item) ? "true" : undefined} tabIndex={-1}>
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
            {(failed.get(item.path) || item.error) && <small>{failed.get(item.path) || item.error}</small>}
          </li>
        );
      })}
    </ul>
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
