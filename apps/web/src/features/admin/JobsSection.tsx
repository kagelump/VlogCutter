import { useState, type FormEvent } from "react";
import { useQuery } from "@tanstack/react-query";
import type { AdminJob, RepresentationBatchResult } from "@rcdvlog/project-client";
import { keys, useClient } from "../../app/client";
import { humanize } from "../../lib/errors";
import { formatEta, formatRelative } from "../../lib/format";
import { Button, Dialog, ErrorMessage, Field, Loading, Notice } from "../../ui";
import { ADMIN_POLL_MS, Row, Rows, Section, StateBadge, idempotencyKey, useAdminAction } from "./shared";
import styles from "./Admin.module.css";

const ACTIVE = new Set(["queued", "running", "retry_wait"]);
const COLLAPSED_COUNT = 10;

const JOB_NAMES: Record<string, string> = {
  "backup.create": "Backup",
  "representation.generate": "Preview",
  "review_session.transcribe": "Transcription",
  "scan.root": "Scan",
  "scan.quick_fingerprint": "Scan: quick fingerprint",
  "scan.full_hash": "Scan: full hash",
  "scan.media_probe": "Scan: media probe",
  "scan.finalizer": "Scan: finish",
};

/** `backup.create.v1` → "Backup". Unknown types fall back to a readable form. */
export function jobName(jobType: string) {
  const base = jobType.replace(/\.v\d+$/, "");
  return JOB_NAMES[base] ?? humanize(base.replaceAll(".", " "));
}

export function JobsSection() {
  const client = useClient();
  const jobs = useQuery({ queryKey: [...keys.admin, "jobs"], queryFn: () => client.listAdminJobs(50), refetchInterval: ADMIN_POLL_MS });
  const [requesting, setRequesting] = useState(false);
  const [showAll, setShowAll] = useState(false);
  const items = jobs.data?.items ?? [];
  const visible = showAll ? items : items.slice(0, COLLAPSED_COUNT);
  const active = items.filter((job) => ACTIVE.has(job.state)).length;

  return (
    <Section
      id="jobs"
      title="Jobs"
      description={active ? `${active} running or queued. Outputs go to the server’s preview storage.` : "Background work like making previews. Outputs go to the server’s preview storage."}
      actions={<Button icon="plus" onClick={() => setRequesting(true)}>Make previews</Button>}
    >
      {jobs.isPending ? <Loading /> : null}
      <ErrorMessage error={jobs.error} />
      {jobs.data ? <Rows empty="No jobs yet.">{visible.map((job) => <JobRow key={job.definition.jobId} job={job} />)}</Rows> : null}
      {items.length > COLLAPSED_COUNT ? (
        <div><Button size="sm" variant="ghost" onClick={() => setShowAll(!showAll)}>{showAll ? "Show fewer" : `Show all ${items.length}`}</Button></div>
      ) : null}
      <PreviewBatchDialog open={requesting} onClose={() => setRequesting(false)} />
    </Section>
  );
}

function JobRow({ job }: { job: AdminJob }) {
  const client = useClient();
  const cancel = useAdminAction(() => client.cancelAdminJob(job.definition.jobId));
  const retry = useAdminAction(() => client.retryAdminJob(job.definition.jobId));
  const percent = job.totalItems ? Math.min(100, (job.completedItems / job.totalItems) * 100) : 0;
  const running = ACTIVE.has(job.state);
  const meta = [
    `${job.completedItems}/${job.totalItems} done`,
    job.failedItems ? `${job.failedItems} failed` : null,
    running ? `ETA ${formatEta(job.estimate?.estimatedRemainingSeconds)}` : null,
    job.attemptCount > 1 ? `attempt ${job.attemptCount}` : null,
    formatRelative(job.updatedAt),
  ].filter(Boolean).join(" · ");

  return (
    <Row
      title={jobName(job.definition.jobType)}
      meta={<span title={job.definition.jobId}>{meta}{job.error ? ` · ${job.error.code}` : ""}</span>}
      status={<StateBadge state={job.state} />}
      actions={<>
        {running ? <Button size="sm" variant="danger" busy={cancel.isPending} onClick={() => cancel.mutate()}>Cancel</Button> : null}
        {job.state === "failed" && job.retryable ? <Button size="sm" busy={retry.isPending} onClick={() => retry.mutate()}>Retry</Button> : null}
      </>}
    >
      {running && job.totalItems ? <div className={styles.progress} role="progressbar" aria-valuenow={Math.round(percent)} aria-valuemin={0} aria-valuemax={100}><span style={{ width: `${percent}%` }} /></div> : null}
      <ErrorMessage error={cancel.error ?? retry.error} />
    </Row>
  );
}

function PreviewBatchDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const client = useClient();
  const [ids, setIds] = useState("");
  const [result, setResult] = useState<RepresentationBatchResult | null>(null);
  const request = useAdminAction((mediaObjectIds: string[]) => client.requestAdminRepresentationBatch({ mediaObjectIds, kind: "proxy", idempotencyKey: idempotencyKey() }));
  const list = ids.split(/[\s,]+/).filter(Boolean);

  function submit(event: FormEvent) {
    event.preventDefault();
    if (list.length) request.mutate(list, { onSuccess: setResult });
  }

  function close() { setResult(null); setIds(""); request.reset(); onClose(); }

  return (
    <Dialog
      open={open}
      onClose={close}
      title="Make previews"
      footer={<>
        <Button onClick={close}>{result ? "Done" : "Cancel"}</Button>
        {!result ? <Button type="submit" form="preview-batch" variant="primary" busy={request.isPending} disabled={!list.length}>Queue {list.length || ""} previews</Button> : null}
      </>}
    >
      <form id="preview-batch" className={styles.formGrid} onSubmit={submit}>
        <Field label="Media IDs" hint="One per line. Only files with a verified identity can get a preview.">
          {(id) => <textarea id={id} rows={6} value={ids} onChange={(event) => { setIds(event.target.value); setResult(null); }} placeholder="media_…" spellCheck={false} />}
        </Field>
        {result ? (
          <Notice tone={result.failedCount || result.skippedCount ? "warning" : "neutral"}>
            {result.queuedCount} queued · {result.reusedCount} already existed · {result.succeededCount} done · {result.failedCount} failed · {result.skippedCount} skipped
            {result.skipped.length ? ` (${[...new Set(result.skipped.map((item) => humanize(item.reason).toLowerCase()))].join(", ")})` : ""}
          </Notice>
        ) : null}
        <ErrorMessage error={request.error} />
      </form>
    </Dialog>
  );
}
