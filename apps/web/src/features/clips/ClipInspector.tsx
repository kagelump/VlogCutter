import { forwardRef, useEffect, useImperativeHandle, useRef, useState, type FormEvent } from "react";
import type { ProjectReviewAsset } from "@rcdvlog/project-client";
import { useClient } from "../../app/client";
import { formatClock, formatDateTime, plural } from "../../lib/format";
import { Badge, Button, ErrorMessage, Icon, Notice } from "../../ui";
import { useAssetEdit } from "./data";
import styles from "./Clips.module.css";

export interface InspectorHandle {
  focusNote(): void;
  focusLabel(): void;
  toggleRemoved(): void;
}

export const ClipInspector = forwardRef<InspectorHandle, { projectId: string; asset: ProjectReviewAsset }>(function ClipInspector({ projectId, asset }, ref) {
  const client = useClient();
  const edit = useAssetEdit(projectId);
  const [noteDraft, setNoteDraft] = useState<string | null>(null);
  const [labelDraft, setLabelDraft] = useState("");
  const [staleNote, setStaleNote] = useState(false);
  const noteRef = useRef<HTMLTextAreaElement>(null);
  const labelRef = useRef<HTMLInputElement>(null);
  const saving = edit.isPending;
  const id = asset.projectAssetId;

  // A draft belongs to one clip; switching clips drops it.
  useEffect(() => { setNoteDraft(null); setLabelDraft(""); setStaleNote(false); edit.reset(); }, [id]);

  const note = noteDraft ?? asset.activeNote ?? "";
  const noteDirty = noteDraft !== null && noteDraft !== (asset.activeNote ?? "");

  function run(optimistic: (value: ProjectReviewAsset) => ProjectReviewAsset, command: (base: number) => ReturnType<typeof client.setReviewRemoved>, onDone?: () => void) {
    if (saving) return;
    edit.mutate({ asset, optimistic, command }, {
      onSuccess: (result) => { setStaleNote(result.staleBaseRevision); onDone?.(); },
    });
  }

  function toggleRemoved() {
    run((value) => ({ ...value, removed: !value.removed }), (base) => client.setReviewRemoved(projectId, id, !asset.removed, base));
  }

  function saveNote(event?: FormEvent) {
    event?.preventDefault();
    if (!noteDirty) return;
    const text = noteDraft ?? "";
    run((value) => ({ ...value, activeNote: text || null }), (base) => client.createReviewNote(projectId, id, text, base), () => setNoteDraft(null));
  }

  function addLabel(event: FormEvent) {
    event.preventDefault();
    const label = labelDraft.trim();
    if (!label || asset.labels.includes(label)) { setLabelDraft(""); return; }
    setLabelDraft("");
    run((value) => ({ ...value, labels: [...value.labels, label].sort() }), (base) => client.addReviewLabel(projectId, id, label, base));
  }

  function removeLabel(label: string) {
    run((value) => ({ ...value, labels: value.labels.filter((item) => item !== label) }), (base) => client.removeReviewLabel(projectId, id, label, base));
  }

  function rotate() {
    const next = (asset.rotationDegrees + 90) % 360;
    run((value) => ({ ...value, rotationDegrees: next }), (base) => client.setReviewRotation(projectId, id, next, base));
  }

  useImperativeHandle(ref, () => ({
    focusNote: () => noteRef.current?.focus(),
    focusLabel: () => labelRef.current?.focus(),
    toggleRemoved,
  }));

  const facts = [asset.camera || "Unknown camera", formatClock(asset.durationSeconds), asset.bestTime ? formatDateTime(asset.bestTime) : null].filter(Boolean);

  return (
    <div className={styles.inspector}>
      <header className={styles.clipHeader}>
        <h2 className={styles.clipName}>{asset.filename}</h2>
        <p className={styles.clipFacts}>{facts.join(" · ")}</p>
        {asset.description ? <p className={styles.clipDescription}>{asset.description}</p> : null}
        <div className={styles.clipBadges}>
          {asset.removed ? <Badge tone="critical">Removed</Badge> : null}
          {asset.mediaVerificationState === "mismatch" ? <Badge tone="critical">Identity mismatch</Badge> : null}
          <span className={styles.saveState} role="status">
            {saving ? "Saving…" : asset.unresolvedConflictCount ? null : edit.isSuccess ? <><Icon name="check" size={14} />Saved</> : null}
          </span>
        </div>
      </header>

      <ErrorMessage error={edit.error} />
      {staleNote ? <Notice tone="warning">Someone else changed this note first. Both versions are kept; the newest is shown.</Notice> : null}
      {asset.unresolvedConflictCount ? <Notice tone="warning">{plural(asset.unresolvedConflictCount, "unresolved edit conflict")} on this clip.</Notice> : null}

      <form className={styles.section} onSubmit={saveNote}>
        <label className={styles.sectionLabel} htmlFor="clip-note">Note <kbd>N</kbd></label>
        <textarea
          id="clip-note"
          ref={noteRef}
          rows={3}
          value={note}
          placeholder="What should happen with this clip?"
          onChange={(event) => setNoteDraft(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.shiftKey && !event.metaKey && !event.ctrlKey && !event.nativeEvent.isComposing) { event.preventDefault(); saveNote(); }
            if (event.key === "Escape") { event.preventDefault(); setNoteDraft(null); event.currentTarget.blur(); }
          }}
          aria-keyshortcuts="N"
        />
        {noteDirty ? (
          <div className={styles.rowEnd}>
            <Button size="sm" variant="ghost" onClick={() => setNoteDraft(null)}>Discard</Button>
            <Button size="sm" variant="primary" type="submit" busy={saving}>Save note</Button>
          </div>
        ) : null}
      </form>

      <div className={styles.section}>
        <span className={styles.sectionLabel} id="clip-labels">Labels <kbd>L</kbd></span>
        {asset.labels.length ? (
          <ul className={styles.chips} aria-labelledby="clip-labels">
            {asset.labels.map((label) => (
              <li key={label}>
                <button type="button" className={styles.chip} disabled={saving} onClick={() => removeLabel(label)} aria-label={`Remove label ${label}`}>
                  {label}<Icon name="close" size={12} />
                </button>
              </li>
            ))}
          </ul>
        ) : null}
        <form className={styles.labelForm} onSubmit={addLabel}>
          <input ref={labelRef} value={labelDraft} onChange={(event) => setLabelDraft(event.target.value)} placeholder="Add a label" aria-label="Add label" aria-keyshortcuts="L" onKeyDown={(event) => { if (event.key === "Escape") event.currentTarget.blur(); }} />
          <Button type="submit" disabled={saving || !labelDraft.trim()}>Add</Button>
        </form>
      </div>

      <div className={styles.actions}>
        <Button variant={asset.removed ? "primary" : "secondary"} disabled={saving} onClick={toggleRemoved} aria-keyshortcuts="R">
          {asset.removed ? "Restore clip" : "Remove clip"}
        </Button>
        <Button icon="rotate" disabled={saving} onClick={rotate} title="Rotate 90° clockwise">
          {asset.rotationDegrees ? `Rotated ${asset.rotationDegrees}°` : "Rotate"}
        </Button>
      </div>
    </div>
  );
});
