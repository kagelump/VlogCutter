import { StatusBadge } from "@rcdvlog/review-ui";
import type { SyncStatus } from "../types";
import { syncStateLabel } from "../review/format";

/**
 * The sync-status diagnostics (spec §14). Reads a `SyncStatus` and lays out
 * state, pending counts, any unresolved conflicts side by side, and the last
 * error. It handles its own backdrop Escape — the convention four modals
 * share — and offers one action, which the shell wires to `sync_now`.
 */
export function SyncDiagnosticsModal({
  status,
  onSyncNow,
  onClose,
}: {
  status: SyncStatus;
  onSyncNow(): void;
  onClose(): void;
}) {
  return (
    <div className="modal-backdrop" role="presentation">
      <section className="modal sync-diagnostics-modal" role="dialog" aria-modal="true" aria-labelledby="sync-diag-title">
        <span className="eyebrow">Sync status</span>
        <div className="settings-modal-heading">
          <h2 id="sync-diag-title">{syncStateLabel(status.state)}</h2>
          <button className="text-button" onClick={onClose}>Close</button>
        </div>
        <div className="sync-diag-grid">
          <div className="sync-diag-row">
            <span className="sync-diag-label">State</span>
            <StatusBadge
              className={`sync-state-badge sync-state-${status.state}`}
              label={syncStateLabel(status.state)}
              tone={status.state === "synced" ? "positive" : status.state === "attention" ? "warning" : "neutral"}
            />
          </div>
          {status.lastSyncAt && (
            <div className="sync-diag-row">
              <span className="sync-diag-label">Last sync</span>
              <span>{new Date(status.lastSyncAt).toLocaleString()}</span>
            </div>
          )}
          {status.activeEndpoint && (
            <div className="sync-diag-row">
              <span className="sync-diag-label">Endpoint</span>
              <span className="endpoint-tag">{status.activeEndpoint}</span>
            </div>
          )}
          <div className="sync-diag-row">
            <span className="sync-diag-label">Pending (local → server)</span>
            <span>{status.localPendingOperations}</span>
          </div>
          <div className="sync-diag-row">
            <span className="sync-diag-label">Pending (server → local)</span>
            <span>{status.remotePendingOperations}</span>
          </div>
          {status.conflictCount > 0 && (
            <div className="sync-diag-section">
              <div className="sync-diag-row sync-diag-warn">
                <span className="sync-diag-label">Conflicts</span>
                <span>{status.conflictCount} unresolved</span>
              </div>
              {status.conflictDetails?.map((detail) => (
                <div key={detail.conflictId} className="sync-diag-conflict">
                  <div className="sync-diag-conflict-header">
                    <span className="sync-diag-label">{detail.conflictType.replace(/_/g, ' ')}</span>
                    <span className="sync-diag-label">on {detail.aggregateId}</span>
                  </div>
                  <div className="sync-diag-conflict-sides">
                    {detail.incomingOperationSummary && (
                      <div className="sync-diag-conflict-side">
                        <span className="sync-diag-side-label">Incoming (conflicting)</span>
                        <div className="sync-diag-conflict-detail">
                          <span>Node: {detail.incomingOperationSummary.originNodeId}</span>
                          {detail.incomingOperationSummary.camera && (
                            <span>Camera: {detail.incomingOperationSummary.camera}</span>
                          )}
                          {detail.incomingOperationSummary.startFilename && (
                            <span>File: {detail.incomingOperationSummary.startFilename}</span>
                          )}
                          {detail.incomingOperationSummary.offsetSeconds != null && (
                            <span>Offset: {detail.incomingOperationSummary.offsetSeconds}s</span>
                          )}
                          {detail.incomingOperationSummary.label && (
                            <span>Label: {detail.incomingOperationSummary.label}</span>
                          )}
                        </div>
                      </div>
                    )}
                    {detail.activeOperationSummary && (
                      <div className="sync-diag-conflict-side">
                        <span className="sync-diag-side-label">Active (currently applied)</span>
                        <div className="sync-diag-conflict-detail">
                          <span>Node: {detail.activeOperationSummary.originNodeId}</span>
                          {detail.activeOperationSummary.camera && (
                            <span>Camera: {detail.activeOperationSummary.camera}</span>
                          )}
                          {detail.activeOperationSummary.startFilename && (
                            <span>File: {detail.activeOperationSummary.startFilename}</span>
                          )}
                          {detail.activeOperationSummary.offsetSeconds != null && (
                            <span>Offset: {detail.activeOperationSummary.offsetSeconds}s</span>
                          )}
                          {detail.activeOperationSummary.label && (
                            <span>Label: {detail.activeOperationSummary.label}</span>
                          )}
                        </div>
                      </div>
                    )}
                  </div>
                  <div className="sync-diag-conflict-meta">
                    <span>Expected revision: {detail.expectedBaseRevision ?? "—"} → Actual: {detail.actualRevision}</span>
                  </div>
                </div>
              ))}
            </div>
          )}
          {status.lastError && (
            <div className="sync-diag-row sync-diag-error">
              <span className="sync-diag-label">Last error</span>
              <span>{status.lastError}</span>
            </div>
          )}
          {status.backoffUntil && (
            <div className="sync-diag-row">
              <span className="sync-diag-label">Next retry</span>
              <span>{new Date(status.backoffUntil).toLocaleString()}</span>
            </div>
          )}
        </div>
        <div className="sync-diag-actions">
          <button className="button secondary" onClick={onSyncNow}>Sync now</button>
        </div>
      </section>
    </div>
  );
}
