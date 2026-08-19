import type { RefObject } from "react";
import type { ImportPreflight, ImportRunReport, PreflightItem } from "../types";
import { formatDuration } from "../review/format";

/**
 * The per-card file list shown inside Smart ingest, both as a live preflight
 * preview and as the completed-run report. Kept in its own file because later
 * tasks grow this row (#65 adds a thumbnail/capture time/duration to every
 * row; #66 adds a per-row "Import from here" cutoff), and both reuse the same
 * row presentation the completed-run report already established.
 */

export function isAttentionItem(item: PreflightItem) {
  return item.classification === "pathCollision" || item.classification === "unreadable";
}

export function FileLedger({
  plan,
  report,
  listRef,
}: {
  plan: ImportPreflight | null;
  report?: ImportRunReport;
  listRef?: RefObject<HTMLUListElement | null>;
}) {
  if (!plan) return <p>No file plan is available.</p>;
  const failed = new Map(report?.outcome.failures.map((failure) => [failure.path, failure.error]));
  const metadata = new Map(report?.files.map((file) => [file.path, file]));
  return (
    <ul ref={listRef}>
      {plan.items.map((item) => (
        <li key={item.path} data-path={item.path} data-attention={isAttentionItem(item) ? "true" : undefined} tabIndex={-1}>
          <span className="import-file-name">{item.filename}</span>
          {report && (
            <>
              <time dateTime={metadata.get(item.path)?.timestamp ?? undefined}>
                {formatImportTimestamp(metadata.get(item.path)?.timestamp ?? null)}
              </time>
              <span className="import-file-duration">
                {metadata.get(item.path)?.durationSeconds != null
                  ? formatDuration(metadata.get(item.path)!.durationSeconds!)
                  : "—"}
              </span>
            </>
          )}
          <span className="import-file-status">{failed.has(item.path) ? "failed" : item.classification.replace(/([A-Z])/g, " $1").toLowerCase()}</span>
          {(failed.get(item.path) || item.error) && <small>{failed.get(item.path) || item.error}</small>}
        </li>
      ))}
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
