import type { ImportHistoryEntry, ImportRollbackPreview, ImportSessionDetail } from "../types";

export function ImportHistoryDialog({ sessions, detail, preview, error, busy, onOpen, onPreview, onUndo, onClose }: {
  sessions: ImportHistoryEntry[]; detail: ImportSessionDetail | null; preview: ImportRollbackPreview | null;
  error: string; busy: boolean;
  onOpen(id: string): void; onPreview(id: string): void; onUndo(id: string): void; onClose(): void;
}) {
  return <div className="modal-backdrop" role="presentation"><section className="modal" role="dialog" aria-modal="true" aria-labelledby="import-history-title">
    <header><p className="eyebrow">Project history</p><h2 id="import-history-title">Imports</h2></header>
    {!detail ? <ul>{sessions.map((session) => <li key={session.id}><button className="text-button" onClick={() => onOpen(session.id)}>{new Date(session.createdAt).toLocaleString()} · {session.imported} added · {session.cancelled ? "stopped" : "complete"}<small> {session.sourcePath}</small></button></li>)}</ul> : <>
      <p><strong>{detail.imported} clips added</strong> from {detail.sourcePath}</p><p>{detail.destinationMode === "archive_copy" ? `Copied to ${detail.destinationName}` : "Originals referenced in place"} · {detail.relinked} reconnected · {detail.skipped} left alone</p>
      <details open><summary>File ledger ({detail.files.length})</summary><ul>{detail.files.map((file) => <li key={file.path}>{file.filename} — {file.result}{file.error ? `: ${file.error}` : ""}</li>)}</ul></details>
      {detail.rolledBackAt ? <p>This session’s added memberships were removed.</p> : <>
        {!preview ? <button className="button secondary" onClick={() => onPreview(detail.id)}>Remove from project…</button> : <div className="context-modal-error"><p>Remove {preview.removableAssetCount} clip memberships? Original files, archive copies, annotations, and derived review records will be preserved.</p>{preview.modifiedClips.length > 0 && <p>{preview.modifiedClips.length} modified clip{preview.modifiedClips.length === 1 ? "" : "s"} will be hidden from this project with their preserved annotations and derived review records: {preview.modifiedClips.map((clip) => clip.filename).join(", ")}.</p>}<p>{preview.message}</p><button className="button primary" disabled={busy} onClick={() => onUndo(detail.id)}>Confirm remove from project</button></div>}</>}
    </>}
    {error && <p className="context-modal-error" role="alert">{error}</p>}
    <footer className="modal-actions"><button className="button secondary" onClick={onClose}>Done</button></footer>
  </section></div>;
}
