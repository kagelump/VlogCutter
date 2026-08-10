import { useEffect, useMemo, useState } from "react";
import type { ArchiveRoot, CardCandidate, ImportPreflight, ImportProgress, ImportRunReport } from "../types";
import { formatBytes } from "../review/summaries";
import { formatDuration } from "../review/format";

export interface ImportDialogProps {
  projectName: string;
  sourcePath: string;
  card: CardCandidate | null;
  roots: ArchiveRoot[];
  selectedRootId: string;
  plan: ImportPreflight | null;
  loading: boolean;
  running: boolean;
  progress: ImportProgress | null;
  report: ImportRunReport | null;
  error: string;
  verifyCopies: boolean;
  makeProxies: boolean;
  onSelectRoot(id: string): void;
  onCreateRoot(name: string): void;
  onLocateRoot(root: ArchiveRoot): void;
  onVerifyCopies(value: boolean): void;
  onMakeProxies(value: boolean): void;
  onImport(): void;
  onCancel(): void;
  onClose(): void;
}

const stageLabels: Record<ImportProgress["stage"], string> = {
  scanning: "Scanning and classifying",
  copying: "Copying and verifying originals",
  reading: "Reading clip metadata",
  proxies: "Making laptop proxies",
  complete: "Import complete",
};

function noun(count: number, singular: string, plural = `${singular}s`) {
  return `${count} ${count === 1 ? singular : plural}`;
}

export function ImportDialog({
  projectName,
  sourcePath,
  card,
  roots,
  selectedRootId,
  plan,
  loading,
  running,
  progress,
  report,
  error,
  verifyCopies,
  makeProxies,
  onSelectRoot,
  onCreateRoot,
  onLocateRoot,
  onVerifyCopies,
  onMakeProxies,
  onImport,
  onCancel,
  onClose,
}: ImportDialogProps) {
  const [newRootName, setNewRootName] = useState("");
  const [showNewRoot, setShowNewRoot] = useState(false);
  const selectedRoot = roots.find((root) => root.id === selectedRootId) ?? null;
  const title = card ? `${card.label} detected` : "Add media folder";
  const subtitle = card?.cameraHint || (card ? "Camera card" : "Reference originals in place");
  const actionable = (plan?.newClips ?? 0) + (plan?.relinkable ?? 0);
  const workPercent = useMemo(() => {
    if (!progress) return 0;
    if (progress.totalBytes > 0) return Math.min(100, (progress.completedBytes / progress.totalBytes) * 100);
    return progress.total > 0 ? Math.min(100, (progress.completed / progress.total) * 100) : 0;
  }, [progress]);

  useEffect(() => {
    if (roots.length === 0 && card) setShowNewRoot(true);
  }, [card, roots.length]);

  const canImport = Boolean(
    plan
    && plan.destination.ready
    && actionable > 0
    && !loading
    && !running,
  );

  return (
    <div className="modal-backdrop import-backdrop" role="presentation">
      <section className="modal import-modal" role="dialog" aria-modal="true" aria-labelledby="import-title">
        <header className="import-header">
          <div>
            <p className="eyebrow">Smart ingest</p>
            <h2 id="import-title">{title}</h2>
            <p>{subtitle}{card ? ` · ${noun(card.clipCount, "clip")}` : ""}</p>
          </div>
          <button className="context-modal-close" aria-label="Close import dialog" disabled={running} onClick={onClose}>×</button>
        </header>

        {report ? (
          <div className="import-result">
            <div className="import-result-mark" aria-hidden="true">✓</div>
            <div>
              <p className="eyebrow">{report.outcome.cancelled ? "Stopped cleanly" : "Import complete"}</p>
              <h3>{noun(report.outcome.imported, "clip")} added to {projectName}</h3>
              <p>
                {noun(report.outcome.copied, "original")} copied · {noun(report.outcome.relinked, "original")} reconnected · {noun(report.outcome.skipped, "known clip")} left alone
              </p>
              {report.proxies && <p>{noun(report.proxies.generated, "proxy", "proxies")} made for laptop review.</p>}
            </div>
            <details className="import-file-ledger">
              <summary>Review every file</summary>
              <FileLedger plan={plan} report={report} />
            </details>
          </div>
        ) : running || (loading && progress) ? (
          <div className="import-running" aria-live="polite">
            <p className="eyebrow">{progress ? stageLabels[progress.stage] : "Preparing import"}</p>
            <h3>{progress?.currentFile || "Reading the card…"}</h3>
            <div className="import-progress-track" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(workPercent)}>
              <span style={{ width: `${workPercent}%` }} />
            </div>
            <div className="import-progress-copy">
              <span>{progress ? `${progress.completed} of ${progress.total}` : "Starting…"}</span>
              {progress && progress.totalBytes > 0 && <span>{formatBytes(progress.completedBytes)} of {formatBytes(progress.totalBytes)} work</span>}
            </div>
            <p>Finished files are committed atomically. Stopping keeps them and leaves the current file uncommitted.</p>
          </div>
        ) : (
          <div className="import-body">
            <div className="import-delta" aria-label="Import summary">
              <strong>{plan?.scanned ?? card?.clipCount ?? 0}</strong><span>clips scanned</span>
              <strong>{plan?.newClips ?? 0}</strong><span>new to {projectName}</span>
              <strong>{(plan?.knownClips ?? 0) + (plan?.missingProxies ?? 0) - (plan?.newClips ?? 0)}</strong><span>already known</span>
              <strong>{plan?.missingProxies ?? 0}</strong><span>need proxies</span>
            </div>

            <div className="import-route" aria-label="Media destinations">
              <div className="import-route-node source">
                <span className="import-route-kicker">Source</span>
                <strong>{card?.label ?? "Selected folder"}</strong>
                <small>{sourcePath}</small>
              </div>
              <span className="import-route-arrow" aria-hidden="true">→</span>
              <div className="import-route-node originals">
                <span className="import-route-kicker">Originals</span>
                {card ? (
                  selectedRoot ? (
                    <>
                      <strong>{selectedRoot.displayName}</strong>
                      <small>{plan?.destination.resolvedPath || selectedRoot.localMountPath || "Not connected"}</small>
                      <span className="import-capacity">
                        copies {formatBytes(plan?.destination.bytesToCopy ?? 0)}
                        {plan?.destination.availableBytes != null ? ` · ${formatBytes(plan.destination.availableBytes)} free` : ""}
                      </span>
                    </>
                  ) : (
                    <><strong>Choose an archive</strong><small>The card stays read-only; VlogCutter needs a destination for its copies.</small></>
                  )
                ) : (
                  <><strong>Stay in place</strong><small>{sourcePath}</small><span className="import-capacity">No originals are copied</span></>
                )}
              </div>
              <span className="import-route-arrow" aria-hidden="true">→</span>
              <div className="import-route-node proxies">
                <span className="import-route-kicker">Review media</span>
                <strong>{projectName} cache</strong>
                <small>This laptop · 720p</small>
              </div>
            </div>

            {card && (
              <div className="import-destination-controls">
                {roots.length > 0 && (
                  <label>
                    <span>Originals destination</span>
                    <select value={selectedRootId} disabled={loading} onChange={(event) => onSelectRoot(event.target.value)}>
                      <option value="">Choose an archive…</option>
                      {roots.map((root) => (
                        <option key={root.id} value={root.id}>
                          {root.displayName} — {root.localMountPath || "Not connected"}{root.available ? "" : " (not connected)"}
                        </option>
                      ))}
                    </select>
                  </label>
                )}
                <button className="text-button" onClick={() => setShowNewRoot((current) => !current)}>{showNewRoot ? "Use existing" : "New destination…"}</button>
                {selectedRoot && !selectedRoot.available && <button className="button secondary" onClick={() => onLocateRoot(selectedRoot)}>Locate {selectedRoot.displayName}…</button>}
              </div>
            )}

            {card && showNewRoot && (
              <div className="import-new-root">
                <label htmlFor="archive-root-name">Name this archive</label>
                <input id="archive-root-name" value={newRootName} onChange={(event) => setNewRootName(event.target.value)} placeholder="NAS Video or Shuttle SSD" />
                <button className="button secondary" disabled={!newRootName.trim()} onClick={() => onCreateRoot(newRootName.trim())}>Choose folder…</button>
              </div>
            )}

            {loading && <p className="import-loading" role="status">Scanning without changing the project…</p>}
            {plan?.destination.blockedReason && <p className="context-modal-error" role="alert">{plan.destination.blockedReason}</p>}
            {plan && (plan.collisions > 0 || plan.unreadable > 0) && (
              <p className="import-warning">{noun(plan.collisions, "path collision")} · {noun(plan.unreadable, "unreadable file")}. These stay untouched and appear in the file ledger.</p>
            )}
            {error && <p className="context-modal-error" role="alert">{error}</p>}

            <div className="import-options">
              {card && <label><input type="checkbox" checked={verifyCopies} onChange={(event) => onVerifyCopies(event.target.checked)} /> Verify copies with a full-file hash</label>}
              <label><input type="checkbox" checked={makeProxies} onChange={(event) => onMakeProxies(event.target.checked)} /> Make missing proxies after import</label>
            </div>

            {plan && <details className="import-file-ledger"><summary>Preview every file</summary><FileLedger plan={plan} /></details>}
          </div>
        )}

        <footer className="modal-actions import-actions">
          <span>{plan?.destination.summary ?? (card ? "Choose where originals will be copied." : "Originals remain in the selected folder.")}</span>
          <div>
            {report ? (
              <button className="button primary" onClick={onClose}>Done</button>
            ) : running ? (
              <button className="button secondary" onClick={onCancel}>Stop import</button>
            ) : (
              <>
                <button className="button secondary" onClick={onClose}>Cancel</button>
                <button className="button primary" disabled={!canImport} onClick={onImport}>
                  {plan?.newClips ? `Import ${noun(plan.newClips, "clip")}` : plan?.relinkable ? `Reconnect ${noun(plan.relinkable, "clip")}` : "Nothing new"}
                </button>
              </>
            )}
          </div>
        </footer>
      </section>
    </div>
  );
}

function FileLedger({ plan, report }: { plan: ImportPreflight | null; report?: ImportRunReport }) {
  if (!plan) return <p>No file plan is available.</p>;
  const failed = new Map(report?.outcome.failures.map((failure) => [failure.path, failure.error]));
  const metadata = new Map(report?.files.map((file) => [file.path, file]));
  return (
    <ul>
      {plan.items.map((item) => (
        <li key={item.path}>
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
