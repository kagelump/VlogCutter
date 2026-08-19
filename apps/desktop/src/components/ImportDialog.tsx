import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api";
import type {
  ArchiveRoot,
  CardCandidate,
  ImportPreflight,
  ImportPreviewItem,
  ImportProgress,
  ImportRunReport,
  ImportSequenceBoundaryEvidence,
  ImportSequenceRunPreview,
} from "../types";
import { formatBytes } from "../review/summaries";
import { FileLedger } from "./ImportFileLedger";

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
  const [ledgerOpen, setLedgerOpen] = useState(false);
  const [ledgerFocusPath, setLedgerFocusPath] = useState<string | null>(null);
  const ledgerListRef = useRef<HTMLUListElement>(null);
  const selectedRoot = roots.find((root) => root.id === selectedRootId) ?? null;

  // #65: posters/duration/capture time for the rows the ledger is actually
  // showing, fetched lazily as they scroll into view (`FileLedger`'s
  // `onVisible`). Kept here rather than in the ledger itself so it survives
  // the preflight ledger unmounting and the completed-run ledger mounting in
  // its place — both read from and write to the same cache.
  const [previews, setPreviews] = useState<Map<string, ImportPreviewItem>>(new Map());
  const previewsInFlight = useRef<Set<string>>(new Set());
  const requestPreviews = useCallback(
    (paths: string[]) => {
      const fresh = paths.filter((path) => !previewsInFlight.current.has(path));
      if (fresh.length === 0) return;
      for (const path of fresh) previewsInFlight.current.add(path);
      const requests = fresh.map((path) => ({
        path,
        fingerprint: plan?.items.find((item) => item.path === path)?.fingerprint ?? null,
      }));
      api
        .generateImportPreview(requests)
        .then((results) => {
          setPreviews((current) => {
            const next = new Map(current);
            for (const result of results) next.set(result.path, result);
            return next;
          });
        })
        .catch(() => {
          // A batch failure degrades these rows to their placeholder/
          // best-available data only; it must never block Import, and a
          // stuck row is simply never retried within this dialog session.
        });
    },
    [plan],
  );

  // A card's `label` is just the mount directory's own name, which is often a
  // generic default like "Untitled" — not an identity worth leading with. The
  // camera family, when known, is the more legible headline; the volume name
  // stays visible as the secondary line rather than disappearing.
  const cameraLabel = card?.cameraHint.trim() || "";
  const title = card ? (cameraLabel ? `${cameraLabel} — Volume ${card.label}` : card.label) : "Add media folder";
  const identityLine = card ? noun(card.clipCount, "clip") : "Reference originals in place";

  const newClips = plan?.newClips ?? 0;
  const relinkable = plan?.relinkable ?? 0;
  const missingProxies = plan?.missingProxies ?? 0;
  const alreadyKnown = (plan?.knownClips ?? 0) + missingProxies - newClips;
  const collisions = plan?.collisions ?? 0;
  const unreadable = plan?.unreadable ?? 0;
  const attentionFileCount = collisions + unreadable;
  const blockedReason = plan?.destination.blockedReason ?? "";
  const needsAttention = attentionFileCount > 0 || Boolean(blockedReason);
  const actionable = newClips + relinkable;
  const summaryState: "attention" | "ready" | "uptodate" = needsAttention
    ? "attention"
    : actionable > 0
      ? "ready"
      : "uptodate";
  const summaryHeadline = needsAttention ? "Needs attention" : actionable > 0 ? "Ready to import" : "Nothing new to import";

  // Only the numbers that change what the user should do lead the summary;
  // bookkeeping totals (scanned, already known) live behind the details
  // disclosure below instead of crowding four counters up front.
  const leadStats: string[] = [];
  if (newClips > 0) leadStats.push(noun(newClips, "new clip"));
  if (relinkable > 0) leadStats.push(`${noun(relinkable, "clip")} to reconnect`);
  if (missingProxies > 0 && actionable > 0 && makeProxies) leadStats.push(`${noun(missingProxies, "proxy", "proxies")} will be made`);
  const staleProxyNote =
    summaryState === "uptodate" && missingProxies > 0
      ? `${noun(missingProxies, "clip")} already imported still need proxies`
      : "";

  function reviewAttentionFiles() {
    const target = plan?.items.find(
      (item) => item.classification === "pathCollision" || item.classification === "unreadable",
    );
    setLedgerFocusPath(target?.path ?? null);
    setLedgerOpen(true);
  }

  useEffect(() => {
    if (!ledgerOpen || !ledgerFocusPath || !ledgerListRef.current) return;
    const row = Array.from(ledgerListRef.current.querySelectorAll<HTMLLIElement>("li")).find(
      (item) => item.dataset.path === ledgerFocusPath,
    );
    row?.scrollIntoView({ block: "center" });
    row?.focus();
  }, [ledgerOpen, ledgerFocusPath]);

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
            <p>{identityLine}</p>
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
              <FileLedger plan={plan} report={report} previews={previews} onVisible={requestPreviews} />
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
            {plan && (
              <div className="import-summary" data-state={summaryState} aria-label="Import summary">
                <p className="import-summary-headline">{summaryHeadline}</p>
                {leadStats.length > 0 && <p className="import-summary-stats">{leadStats.join(" · ")}</p>}
                {staleProxyNote && <p className="import-summary-stats">{staleProxyNote}</p>}
                {attentionFileCount > 0 && (
                  <p className="import-warning">
                    {[collisions > 0 ? noun(collisions, "path collision") : "", unreadable > 0 ? noun(unreadable, "unreadable file") : ""]
                      .filter(Boolean)
                      .join(" · ")}
                    . These stay untouched.{" "}
                    <button type="button" className="text-button" onClick={reviewAttentionFiles}>
                      Review {noun(attentionFileCount, "file")}
                    </button>
                  </p>
                )}
                <details className="import-summary-bookkeeping">
                  <summary>All counts</summary>
                  <div className="import-delta">
                    <strong>{plan.scanned}</strong><span>clips scanned</span>
                    <strong>{newClips}</strong><span>new to {projectName}</span>
                    <strong>{alreadyKnown}</strong><span>already known</span>
                    <strong>{missingProxies}</strong><span>need proxies</span>
                  </div>
                </details>
              </div>
            )}

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

            {plan?.sequencePreview && plan.sequencePreview.newClipCount > 0 && (
              <SequencePreview preview={plan.sequencePreview} />
            )}

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
            {blockedReason && <p className="context-modal-error" role="alert">{blockedReason}</p>}
            {error && <p className="context-modal-error" role="alert">{error}</p>}

            <div className="import-options">
              {card && <label><input type="checkbox" checked={verifyCopies} onChange={(event) => onVerifyCopies(event.target.checked)} /> Verify copies with a full-file hash</label>}
              <label><input type="checkbox" checked={makeProxies} onChange={(event) => onMakeProxies(event.target.checked)} /> Make missing proxies after import</label>
            </div>

            {plan && (
              <details
                className="import-file-ledger"
                open={ledgerOpen}
                onToggle={(event) => setLedgerOpen((event.target as HTMLDetailsElement).open)}
              >
                <summary>Preview every file</summary>
                <FileLedger plan={plan} listRef={ledgerListRef} previews={previews} onVisible={requestPreviews} />
              </details>
            )}
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

function evidenceLabel(evidence: ImportSequenceBoundaryEvidence) {
  switch (evidence.kind) {
    case "volumeChange": return "Volume";
    case "counterReset": return "Counter";
    case "folderReset": return "Folder";
    case "prefixChange": return "Prefix";
    case "largeDiscontinuity": return "Time gap";
  }
}

function evidenceDescription(evidence: ImportSequenceBoundaryEvidence) {
  switch (evidence.kind) {
    case "volumeChange": return `card or volume identity differs (${evidence.from} → ${evidence.to})`;
    case "counterReset": return `filename counter restarts ${evidence.previous} → ${evidence.next}`;
    case "folderReset": return `folder resets ${evidence.from || "root"} → ${evidence.to || "root"}`;
    case "prefixChange": return `filename prefix changes ${evidence.from || "none"} → ${evidence.to || "none"}`;
    case "largeDiscontinuity": {
      const hours = evidence.gapSeconds / 3_600;
      return `${hours >= 24 ? `${(hours / 24).toFixed(hours % 24 === 0 ? 0 : 1)} day` : `${hours.toFixed(hours % 1 === 0 ? 0 : 1)} hour`} file timestamp gap`;
    }
  }
}

/**
 * `run.needsReview` is core's own verdict on whether this run's evidence
 * actively disagrees with the sequence it was compared against — the one
 * condition worth a look before the user confirms it in Chronology. A
 * `proposeNew` run can carry a `comparedExistingSequenceId` for entirely
 * routine reasons too (another incoming run already claimed that sequence,
 * or a boundary split this run off deliberately), so `comparedExistingSequenceId`
 * alone is not the signal — `needsReview` is. This must stay a read of that
 * field, not a re-derivation from `reason`, which is prose for humans and
 * can be reworded without notice.
 */
function sequenceRunKind(run: ImportSequenceRunPreview): "extend" | "attention" | "new" {
  if (run.decision === "extendExisting") return "extend";
  return run.needsReview ? "attention" : "new";
}

function SequenceRunList({ runs }: { runs: ImportSequenceRunPreview[] }) {
  return (
    <ol className="import-sequence-runs">
      {runs.map((run) => {
        const kind = sequenceRunKind(run);
        const first = run.firstRelativePath.split("/").at(-1);
        const last = run.lastRelativePath.split("/").at(-1);
        return (
          <li
            key={`${run.firstRelativePath}:${run.lastRelativePath}`}
            data-decision={run.decision}
            data-attention={kind === "attention" ? "true" : undefined}
          >
            <div className="import-sequence-evidence">
              {run.evidence.map((evidence, index) => (
                <span key={`${evidence.kind}:${index}`} title={evidenceDescription(evidence)}>{evidenceLabel(evidence)}</span>
              ))}
            </div>
            <div className="import-sequence-run-copy">
              <strong>
                {kind === "extend"
                  ? <>Extends “{run.matchedExistingSequenceLabel}” by {noun(run.clipCount, "clip")}</>
                  : <>Proposes a new sequence for {noun(run.clipCount, "clip")}</>}
              </strong>
              <small>{run.cameraKey} · {first}{first === last ? "" : ` → ${last}`}</small>
              <p>{run.reason}</p>
            </div>
            <em>{kind === "extend" ? "Existing sequence" : kind === "attention" ? "Review before confirming" : "New sequence"}</em>
          </li>
        );
      })}
    </ol>
  );
}

function SequencePreview({ preview }: { preview: NonNullable<ImportPreflight["sequencePreview"]> }) {
  const attentionRuns = preview.runs.filter((run) => sequenceRunKind(run) === "attention");

  const heading = (
    <div className="import-sequence-heading">
      <div>
        <span className="import-route-kicker">Chronology preflight</span>
        <strong>{noun(preview.proposedSequenceCount, "incoming card run")}</strong>
      </div>
      {attentionRuns.length > 0 && <span className="is-attention">Needs a look</span>}
    </div>
  );
  const footer = <footer>Suggestions only — importing never changes chronology until you confirm it in Chronology.</footer>;

  if (attentionRuns.length === 0) {
    // Nothing here needs a decision before import — collapse it to one line
    // so routine chronology bookkeeping never has to be read before copying.
    return (
      <section className="import-sequence-preview is-routine" aria-label="Capture sequence preview">
        <details>
          <summary>Chronology suggestions ready for later review</summary>
          {heading}
          <SequenceRunList runs={preview.runs} />
          {footer}
        </details>
      </section>
    );
  }

  return (
    <section className="import-sequence-preview is-attention" aria-label="Capture sequence preview">
      {heading}
      <p className="import-sequence-attention-note">
        {noun(attentionRuns.length, "sequence proposal")} {attentionRuns.length === 1 ? "differs" : "differ"} from an
        existing named sequence and {attentionRuns.length === 1 ? "is" : "are"} worth a look before you confirm{" "}
        {attentionRuns.length === 1 ? "it" : "them"} in Chronology — the rest can wait until after import.
      </p>
      <SequenceRunList runs={preview.runs} />
      {footer}
    </section>
  );
}
