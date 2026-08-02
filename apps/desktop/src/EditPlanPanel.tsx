// The edit-plan and export surface (D01-T023).
//
// The smallest thing that can be called complete: build an ordered list of
// source ranges, see what would block it, watch a low-resolution preview
// render, and write a deterministic OTIO file a finishing editor can open.
//
// There is no track control, no transition control, and no effect control,
// because the model cannot express them. The UI must not offer what the model
// cannot store.

import { useCallback, useEffect, useState } from "react";
import { chronologyApi } from "./chronologyApi";
import type {
  EditPlanRevision,
  PreviewProgress,
  ValidationReport,
} from "./chronologyTypes";
import type { Asset } from "./types";

interface Props {
  projectId: string;
  /** Clips in the solver's order, so the plan starts in chronology order. */
  assets: Asset[];
}

function formatSeconds(value: number): string {
  const minutes = Math.floor(value / 60);
  const seconds = Math.floor(value % 60);
  return `${minutes}:${String(seconds).padStart(2, "0")}`;
}

export function EditPlanPanel({ projectId, assets }: Props) {
  const [plan, setPlan] = useState<EditPlanRevision | null>(null);
  const [report, setReport] = useState<ValidationReport>({ findings: [] });
  const [progress, setProgress] = useState<PreviewProgress | null>(null);
  const [previewPath, setPreviewPath] = useState<string | null>(null);
  const [exportPath, setExportPath] = useState("");
  const [status, setStatus] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const active = await chronologyApi.activeEditPlan(projectId);
      setPlan(active);
      setReport(active ? await chronologyApi.validateEditPlan(active.revisionId) : { findings: [] });
    } catch (cause) {
      setError(String(cause));
    }
  }, [projectId]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    let stop: (() => void) | undefined;
    void chronologyApi.onPreviewProgress(setProgress).then((unlisten) => {
      stop = unlisten;
    });
    return () => stop?.();
  }, []);

  /**
   * Seeds a plan from the review queue in the solver's order — which is the
   * point of doing chronology first.
   */
  const buildFromQueue = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const operations = assets
        .filter((asset) => !asset.removed && asset.durationSeconds > 0)
        .map((asset) => ({
          assetId: asset.id,
          sourceInSeconds: 0,
          sourceOutSeconds: asset.durationSeconds,
          rationale: "from the review queue",
        }));
      const created = await chronologyApi.createEditPlanRevision(
        projectId,
        operations,
        plan?.revisionId ?? null,
      );
      if (created) {
        await chronologyApi.activateEditPlanRevision(
          projectId,
          created.revisionId,
          plan?.revisionId ?? null,
        );
      }
      await refresh();
      setStatus(`Built a plan from ${operations.length} clips.`);
    } catch (cause) {
      // A stale-revision refusal is the expected outcome when someone else
      // changed the plan; say so rather than showing a raw error.
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  }, [assets, plan, projectId, refresh]);

  const renderPreview = useCallback(async () => {
    if (!plan) return;
    setBusy(true);
    setError(null);
    setStatus(null);
    try {
      const result = await chronologyApi.renderPreview(plan.revisionId);
      if (result) {
        setPreviewPath(result.outputPath);
        setStatus(
          result.failures.length > 0
            ? `Preview rendered ${result.clipCount} clips; ${result.failures.length} failed.`
            : `Preview rendered ${result.clipCount} clips.`,
        );
      }
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  }, [plan]);

  const exportOtio = useCallback(async () => {
    if (!plan || !exportPath.trim()) return;
    setBusy(true);
    setError(null);
    setStatus(null);
    try {
      const result = await chronologyApi.exportOtio(
        projectId,
        plan.revisionId,
        exportPath.trim(),
        "Trip",
        null,
      );
      if (result) {
        setStatus(`Exported ${result.clipCount} clips to ${result.outputPath}.`);
      }
    } catch (cause) {
      // An existing output is never silently replaced; the refusal names the
      // digest actually on disk.
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  }, [exportPath, plan, projectId]);

  const errors = report.findings.filter((finding) => finding.severity === "error");
  const warnings = report.findings.filter((finding) => finding.severity === "warning");
  const renderable = errors.length === 0;

  return (
    <section className="edit-plan-panel" aria-label="Edit plan">
      {error && (
        <p className="edit-plan-error" role="alert">
          {error}
        </p>
      )}
      {status && <p className="edit-plan-status">{status}</p>}

      {!plan ? (
        <div className="edit-plan-empty">
          <p>No edit plan yet.</p>
          <button type="button" disabled={busy} onClick={() => void buildFromQueue()}>
            Build one from the review queue
          </button>
        </div>
      ) : (
        <>
          <div className="edit-plan-summary">
            <span>{plan.operations.length} clips</span>
            <span>
              {formatSeconds(
                plan.operations.reduce(
                  (total, operation) =>
                    total +
                    Math.max(0, operation.sourceOutSeconds - operation.sourceInSeconds),
                  0,
                ),
              )}
            </span>
            <button type="button" disabled={busy} onClick={() => void buildFromQueue()}>
              Rebuild from the review queue
            </button>
          </div>

          {/* Errors block; warnings inform. The split is the whole point. */}
          {errors.length > 0 && (
            <div className="edit-plan-errors" role="alert">
              <h3>{errors.length} problem(s) block this plan</h3>
              <ul>
                {errors.map((finding, index) => (
                  <li key={`${finding.code}:${index}`} data-code={finding.code}>
                    {finding.message}
                  </li>
                ))}
              </ul>
            </div>
          )}
          {warnings.length > 0 && (
            <div className="edit-plan-warnings">
              <h3>{warnings.length} thing(s) worth knowing</h3>
              <ul>
                {warnings.map((finding, index) => (
                  <li key={`${finding.code}:${index}`} data-code={finding.code}>
                    {finding.message}
                  </li>
                ))}
              </ul>
            </div>
          )}

          <ol className="edit-plan-operations">
            {plan.operations.map((operation) => {
              const asset = assets.find((candidate) => candidate.id === operation.assetId);
              return (
                <li key={operation.operationId}>
                  <span>{asset?.filename ?? operation.assetId}</span>
                  <span>
                    {formatSeconds(operation.sourceInSeconds)}–
                    {formatSeconds(operation.sourceOutSeconds)}
                  </span>
                  {operation.rationale && <em>{operation.rationale}</em>}
                </li>
              );
            })}
          </ol>

          <div className="edit-plan-preview">
            <button type="button" disabled={busy || !renderable} onClick={() => void renderPreview()}>
              Render preview
            </button>
            {!renderable && <span>Fix the problems above first.</span>}
            {progress && progress.type === "clipCompleted" && (
              <span>
                Rendering clip {progress.position}
                {progress.estimateRemainingSeconds !== null &&
                  ` — about ${Math.round(progress.estimateRemainingSeconds)}s left`}
              </span>
            )}
            {progress && progress.type === "clipFailed" && (
              <span role="alert">Clip {progress.position} failed: {progress.message}</span>
            )}
            {previewPath && <span>Preview at {previewPath}</span>}
          </div>

          <div className="edit-plan-export">
            <label>
              Export to
              <input
                type="text"
                value={exportPath}
                onChange={(event) => setExportPath(event.target.value)}
                placeholder="/path/to/timeline.otio"
              />
            </label>
            <button
              type="button"
              disabled={busy || !renderable || !exportPath.trim()}
              onClick={() => void exportOtio()}
            >
              Export OTIO
            </button>
          </div>
        </>
      )}
    </section>
  );
}
