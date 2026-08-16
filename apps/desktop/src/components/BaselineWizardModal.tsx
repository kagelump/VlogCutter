import type { BaselinePreviewResult, PathAnalysis, RootMappingEntry } from "../types";

export type BaselineStep = "root_mapping" | "preview" | "confirming" | "complete" | "error";

/**
 * The pair-project-with-server (baseline) wizard (spec §14). Maps absolute media
 * paths to storage roots, previews the server-side baseline, uploads it, and
 * reports completion or an error the user can retry.
 *
 * Like the pairing wizard it receives its step and data as flat props; the shell
 * owns the step transitions and the reset. `onCancel` is the shared reset that
 * Cancel and backdrop Escape both call; `onDone` is the distinct success reset.
 */
export function BaselineWizardModal({
  step,
  error,
  pathAnalysis,
  rootMappings,
  preview,
  busy,
  onPreview,
  onBack,
  onConfirm,
  onRetry,
  onDone,
  onCancel,
}: {
  step: BaselineStep;
  error: string | null;
  pathAnalysis: PathAnalysis | null;
  rootMappings: RootMappingEntry[];
  preview: BaselinePreviewResult | null;
  busy: boolean;
  onPreview(): void;
  onBack(): void;
  onConfirm(): void;
  onRetry(): void;
  onDone(): void;
  onCancel(): void;
}) {
  return (
    <div className="modal-backdrop pairing-backdrop" role="presentation">
      <section className="modal baseline-modal" role="dialog" aria-modal="true" aria-labelledby="baseline-title">
        <span className="eyebrow">Pair project with server</span>
        <div className="settings-modal-heading">
          <h2 id="baseline-title">
            {step === "root_mapping" && "Step 1: Map media paths"}
            {step === "preview" && "Step 2: Review baseline preview"}
            {step === "confirming" && "Uploading baseline…"}
            {step === "complete" && "Project paired!"}
            {step === "error" && "Pairing issue"}
          </h2>
          <button className="text-button" onClick={onCancel}>Cancel <kbd>Esc</kbd></button>
        </div>

        {error && <p className="error-message" role="alert">{error}</p>}

        {step === "root_mapping" && pathAnalysis && (
          <div className="wizard-step">
            <p>Your project has <strong>{pathAnalysis.absolutePathCount}</strong> assets with absolute paths. Map each source folder to a logical storage root that the server can recognize.</p>
            {pathAnalysis.commonPrefixes.length === 0 ? (
              <p className="quiet">No absolute-path assets found — skipping root mapping.</p>
            ) : (
              <div className="root-mapping-table">
                <div className="root-mapping-header">
                  <span>Local path</span>
                  <span>Assets</span>
                  <span>Storage root ID</span>
                </div>
                {rootMappings.map((mapping, i) => (
                  <div className="root-mapping-row" key={i}>
                    <span className="mono" title={mapping.localPrefix}>{mapping.localPrefix}</span>
                    <span>{pathAnalysis.commonPrefixes[i]?.assetCount ?? "?"}</span>
                    <span className="mono">{mapping.storageRootId}</span>
                  </div>
                ))}
              </div>
            )}
            <p className="quiet">No original media will be uploaded during baseline pairing.</p>
            <div className="modal-actions">
              <button className="button secondary" onClick={onCancel}>Cancel</button>
              <button className="button primary" disabled={busy} onClick={onPreview}>Preview baseline</button>
            </div>
          </div>
        )}

        {step === "preview" && preview && (
          <div className="wizard-step">
            <p>Server preview of <strong>{preview.preview.projectName}</strong>:</p>
            <div className="baseline-stats">
              <div className="baseline-stat">
                <span className="baseline-stat-value">{preview.preview.assetCount}</span>
                <span className="baseline-stat-label">Assets</span>
              </div>
              <div className="baseline-stat">
                <span className="baseline-stat-value">{preview.preview.operationCount}</span>
                <span className="baseline-stat-label">Operations</span>
              </div>
              <div className="baseline-stat">
                <span className="baseline-stat-value">{preview.preview.conflictCount}</span>
                <span className="baseline-stat-label">Conflicts</span>
              </div>
              <div className="baseline-stat">
                <span className="baseline-stat-value">{preview.preview.rootMappingCount}</span>
                <span className="baseline-stat-label">Root mappings</span>
              </div>
              <div className="baseline-stat">
                <span className="baseline-stat-value">{preview.preview.linkedMediaCount}</span>
                <span className="baseline-stat-label">Linked media</span>
              </div>
            </div>
            <p className="quiet">Baseline digest: <span className="mono">{preview.digest.slice(0, 16)}…</span></p>
            <p className="quiet">This operation will NOT upload any original media files. Only project metadata and manifests are transferred.</p>
            <div className="modal-actions">
              <button className="button secondary" onClick={onBack}>Back</button>
              <button className="button primary" disabled={busy} onClick={onConfirm}>Confirm &amp; upload baseline</button>
            </div>
          </div>
        )}

        {step === "confirming" && (
          <div className="wizard-step">
            <p>Uploading baseline to server…</p>
          </div>
        )}

        {step === "complete" && (
          <div className="wizard-step">
            <p className="success">Project is now paired with the server!</p>
            <p>The server has a replica of your project. Background sync will begin automatically.</p>
            <div className="modal-actions">
              <button className="button primary" autoFocus onClick={onDone}>Done</button>
            </div>
          </div>
        )}

        {step === "error" && (
          <div className="wizard-step">
            <p>The baseline operation did not complete. You can retry safely — no data has been lost.</p>
            <div className="modal-actions">
              <button className="button secondary" onClick={onCancel}>Abandon</button>
              <button className="button primary" onClick={onRetry}>Retry</button>
            </div>
          </div>
        )}
      </section>
    </div>
  );
}
