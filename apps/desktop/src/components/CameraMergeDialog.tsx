import type { CameraMergePreview } from "../types";

interface CameraMergeDialogProps {
  sourceCamera: string;
  destinationCameras: string[];
  destinationCamera: string;
  preview: CameraMergePreview | null;
  busy: boolean;
  error: string;
  onDestinationChange(camera: string): void;
  onConfirm(): void;
  onCancel(): void;
}

export function CameraMergeDialog({
  sourceCamera,
  destinationCameras,
  destinationCamera,
  preview,
  busy,
  error,
  onDestinationChange,
  onConfirm,
  onCancel,
}: CameraMergeDialogProps) {
  return (
    <div className="modal-backdrop camera-merge-backdrop" role="presentation">
      <section
        className="modal camera-merge-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="camera-merge-title"
      >
        <p className="camera-merge-eyebrow">Camera identity repair</p>
        <h2 id="camera-merge-title">Merge camera lanes</h2>
        <p className="section-note">Choose the name this physical camera should use everywhere.</p>

        <div className="camera-merge-route" aria-label={`Merge ${sourceCamera} into ${destinationCamera || "another camera"}`}>
          <span>{sourceCamera}</span>
          <b aria-hidden="true">→</b>
          <span>{destinationCamera || "Choose a camera"}</span>
        </div>

        <label className="field-label">Merge with…
          <select
            autoFocus
            value={destinationCamera}
            disabled={busy}
            onChange={(event) => onDestinationChange(event.target.value)}
          >
            <option value="">Choose a camera…</option>
            {destinationCameras.map((camera) => <option key={camera} value={camera}>{camera}</option>)}
          </select>
        </label>

        {preview && (
          <div className="camera-merge-preview" aria-live="polite">
            <dl>
              <div><dt>Clips moving</dt><dd>{preview.affectedClipCount}</dd></div>
              <div><dt>Cards moving</dt><dd>{preview.affectedCaptureSequenceCount}</dd></div>
              <div><dt>Cards already there</dt><dd>{preview.destinationCaptureSequenceCount}</dd></div>
            </dl>
            <p>All {preview.affectedCaptureSequenceCount + preview.destinationCaptureSequenceCount} cards stay separate. Their boundaries, counters, and clock corrections keep the same identities.</p>
            {preview.affectedMappingCount > 0 && <p>{preview.affectedMappingCount} saved ingest {preview.affectedMappingCount === 1 ? "mapping" : "mappings"} will follow the merged camera.</p>}
          </div>
        )}

        {error && <p className="context-modal-error" role="alert">{error}</p>}

        <div className="modal-actions">
          <button className="button ghost" disabled={busy} onClick={onCancel}>Cancel</button>
          <button className="button primary" disabled={busy || !preview} onClick={onConfirm}>
            {busy ? "Merging…" : "Merge camera"}
          </button>
        </div>
      </section>
    </div>
  );
}
