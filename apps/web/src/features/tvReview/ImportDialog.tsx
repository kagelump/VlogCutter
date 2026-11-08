import { useMemo, useState, type ChangeEvent } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { HISTORICAL_REVIEW_ACKNOWLEDGEMENT, type ReviewTargetPreflight } from "@rcdvlog/project-client";
import { keys, useClient } from "../../app/client";
import { ERROR_COPY } from "../../lib/errors";
import { formatClock, formatDateTime, shortDigest } from "../../lib/format";
import { Button, Checkbox, Dialog, ErrorMessage, Facts, Field, Icon, Notice } from "../../ui";
import { isHistoricalReview, unwrapHandoff } from "./youtube";
import styles from "./TvReview.module.css";

const MAX_HANDOFF_BYTES = 16 * 1024 * 1024 + 64 * 1024;

type Parsed = { handoff: Record<string, unknown>; historical: boolean } | { error: string } | null;

function parse(text: string): Parsed {
  if (!text.trim()) return null;
  let value: unknown;
  try { value = JSON.parse(text); } catch { return { error: "This isn’t valid JSON. Paste the export output or choose its file." }; }
  const handoff = unwrapHandoff(value);
  if (!handoff) return { error: "This JSON isn’t a review export." };
  return { handoff, historical: isHistoricalReview(handoff) };
}

export function ImportDialog({ open, projectId, onClose, onImported }: {
  open: boolean;
  projectId: string;
  onClose: () => void;
  onImported: (reviewTargetId: string) => void;
}) {
  const client = useClient();
  const queryClient = useQueryClient();
  const [text, setText] = useState("");
  const [issuer, setIssuer] = useState("");
  const [acknowledged, setAcknowledged] = useState(false);
  const [fileError, setFileError] = useState<string | null>(null);
  const parsed = useMemo(() => parse(text), [text]);
  const ok = parsed && "handoff" in parsed ? parsed : null;
  // Detected from the export itself, never chosen: a current export can't be weakened into a historical one.
  const historical = ok?.historical ?? false;

  function body() {
    if (!ok) throw new Error("no handoff");
    const declaredIssuer = issuer.trim() || null;
    return { ok, declaredIssuer };
  }

  const check = useMutation({
    mutationFn: async () => {
      const { ok: value, declaredIssuer } = body();
      return value.historical
        ? client.preflightHistoricalReviewTarget(projectId, { review: value.handoff, acknowledgement: HISTORICAL_REVIEW_ACKNOWLEDGEMENT, declaredIssuer })
        : client.preflightReviewTarget(projectId, { handoff: value.handoff, declaredIssuer });
    },
  });

  const commit = useMutation({
    mutationFn: async () => {
      const { ok: value, declaredIssuer } = body();
      return value.historical
        ? client.importHistoricalReviewTarget(projectId, { review: value.handoff, acknowledgement: HISTORICAL_REVIEW_ACKNOWLEDGEMENT, declaredIssuer })
        : client.importReviewTarget(projectId, { handoff: value.handoff, declaredIssuer });
    },
    onSuccess: (result) => {
      void queryClient.invalidateQueries({ queryKey: keys.targets(projectId) });
      reset();
      onImported(result.reviewTarget.reviewTargetId);
    },
  });

  function edited() { check.reset(); commit.reset(); }
  function reset() { setText(""); setIssuer(""); setAcknowledged(false); setFileError(null); check.reset(); commit.reset(); }
  function close() { reset(); onClose(); }

  async function chooseFile(event: ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (!file) return;
    if (file.size > MAX_HANDOFF_BYTES) { setFileError("That file is larger than the 16 MB limit."); return; }
    setFileError(null);
    setText(await file.text());
    edited();
  }

  const preflight = check.data;
  const canCheck = Boolean(ok) && (!historical || acknowledged);

  return (
    <Dialog
      open={open}
      onClose={close}
      wide
      title="Import a cut"
      footer={<>
        <Button onClick={close}>Cancel</Button>
        {preflight?.importable ? (
          <Button variant="primary" busy={commit.isPending} onClick={() => commit.mutate()}>{importLabel(preflight)}</Button>
        ) : (
          <Button variant="primary" busy={check.isPending} disabled={!canCheck} onClick={() => check.mutate()}>Check</Button>
        )}
      </>}
    >
      <p className={styles.muted}>Only the export’s metadata is sent. The movie, originals and catalog stay on the Mac.</p>

      <div className={styles.fileRow}>
        <label className={styles.filePick}>
          <Icon name="upload" size={16} />
          Choose export file
          <input type="file" accept="application/json,.json" onChange={(event) => void chooseFile(event)} className="visually-hidden" />
        </label>
        <span className={styles.muted}>or paste it below</span>
      </div>

      <Field label="Export JSON">
        {(id) => (
          <textarea
            id={id}
            rows={6}
            value={text}
            spellCheck={false}
            className={styles.code}
            placeholder='{"ok": true, "command": "edit-plan.review-target-export", …}'
            onChange={(event) => { setText(event.target.value); edited(); }}
          />
        )}
      </Field>
      {fileError ? <Notice tone="critical">{fileError}</Notice> : null}
      {parsed && "error" in parsed ? <Notice tone="critical">{parsed.error}</Notice> : null}

      <Field label="Exported from" hint="Optional. A name for the Mac, for your records. It isn’t verified.">
        {(id) => <input id={id} value={issuer} maxLength={64} placeholder="Travel MacBook" onChange={(event) => { setIssuer(event.target.value); edited(); }} />}
      </Field>

      {historical ? (
        <Notice tone="warning" title="Historical review.">
          This movie was rendered earlier. The Mac confirmed the file is unchanged and matches its cut list, but not that today’s sources still match or that it can be re-rendered.
          <div className={styles.ackRow}>
            <Checkbox checked={acknowledged} onChange={(value) => { setAcknowledged(value); edited(); }}>
              I understand this review covers the rendered movie only and can’t be used to render, edit, activate or upload anything.
            </Checkbox>
          </div>
        </Notice>
      ) : null}

      <ErrorMessage error={check.error ?? commit.error} />
      {preflight ? <PreflightResult preflight={preflight} /> : null}
    </Dialog>
  );
}

function importLabel(preflight: ReviewTargetPreflight) {
  if (preflight.outcome === "duplicate") return "Open existing";
  if (preflight.outcome === "refresh") return "Update verification";
  return "Import";
}

function PreflightResult({ preflight }: { preflight: ReviewTargetPreflight }) {
  const problems = preflight.checks.filter((item) => item.status !== "pass");
  const passed = preflight.checks.length - problems.length;
  const preview = preflight.preview;
  return (
    <section className={styles.preflight} aria-label="Check result">
      <p className={styles.preflightTitle} data-ok={preflight.importable || undefined}>
        <Icon name={preflight.importable ? "check" : "alert"} size={16} />
        {preflightTitle(preflight)}
      </p>
      {preview ? (
        <Facts items={[
          ["Type", preview.contract === "historicalRenderedArtifactReviewV1" ? "Historical (integrity only)" : "Current (fully verified)"],
          ["Revision", <span title={preview.revisionId}>{preview.revisionId}</span>],
          ["Length", `${formatClock(preview.durationSeconds)} · ${preview.cutCount.toLocaleString()} shots`],
          ["Verified", formatDateTime(preview.verifiedAt)],
          ["YouTube", preview.youtubeVideoId ?? "None"],
          ["Digest", <code title={preview.targetDigest}>{shortDigest(preview.targetDigest)}</code>],
        ]} />
      ) : null}
      {problems.length ? (
        <ul className={styles.checks}>
          {problems.map((item) => (
            <li key={item.name} data-status={item.status}>
              <Icon name="alert" size={14} />
              <span><strong>{item.name}:</strong> {(item.code && ERROR_COPY[item.code]) || item.detail}</span>
            </li>
          ))}
        </ul>
      ) : null}
      <p className={styles.muted}>{passed} of {preflight.checks.length} checks passed.{preflight.importable ? "" : " Nothing is imported until every check passes."}</p>
    </section>
  );
}

function preflightTitle(preflight: ReviewTargetPreflight) {
  switch (preflight.outcome) {
    case "new": return "Ready to import.";
    case "duplicate": return "Already imported.";
    case "refresh": return "Already imported. This adds a newer verification.";
    case "conflict": return "A different version of this movie was already imported.";
    default: return "This export can’t be imported.";
  }
}
