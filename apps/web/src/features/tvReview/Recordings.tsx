import { useState } from "react";
import { Link } from "react-router";
import { useQuery } from "@tanstack/react-query";
import type { ReviewSessionSummary } from "@rcdvlog/project-client";
import { keys, useClient } from "../../app/client";
import { paths } from "../../app/paths";
import { formatClock, formatDateTime, plural } from "../../lib/format";
import { Badge, Button, ButtonLink, Dialog, ErrorMessage, Loading, Panel } from "../../ui";
import { SessionBadge } from "./shared";
import { TranscribeForm } from "./TranscriptSection";
import styles from "./TvReview.module.css";

/** Recordings of one cut, each with its transcript state and the next step: transcribe or review. */
export function Recordings({ projectId, targetId }: { projectId: string; targetId: string }) {
  const client = useClient();
  const sessions = useQuery({ queryKey: keys.sessions(projectId, targetId), queryFn: () => client.listReviewSessions(projectId, targetId, 50) });
  const [transcribing, setTranscribing] = useState<ReviewSessionSummary | null>(null);
  const items = (sessions.data?.items ?? []).filter((session) => session.state !== "deleted");

  return (
    <Panel title="Recordings">
      {sessions.isPending ? <Loading /> : null}
      <ErrorMessage error={sessions.error} />
      {sessions.isSuccess && !items.length ? <p className={styles.muted}>No recordings yet. Start one while this cut plays on the TV.</p> : null}
      {items.length ? (
        <ul className={styles.recordings}>
          {items.map((session) => (
            <RecordingRow key={session.sessionId} projectId={projectId} targetId={targetId} session={session} onTranscribe={() => setTranscribing(session)} />
          ))}
        </ul>
      ) : null}
      <Dialog open={Boolean(transcribing)} onClose={() => setTranscribing(null)} title="Transcribe recording">
        {transcribing ? (
          <TranscribeForm projectId={projectId} sessionId={transcribing.sessionId} busy={false} onStarted={() => setTranscribing(null)} />
        ) : null}
      </Dialog>
    </Panel>
  );
}

function RecordingRow({ projectId, targetId, session, onTranscribe }: {
  projectId: string;
  targetId: string;
  session: ReviewSessionSummary;
  onTranscribe: () => void;
}) {
  const client = useClient();
  const finalized = session.state === "completed" || session.state === "incomplete";
  const runs = useQuery({
    queryKey: keys.transcriptions(projectId, session.sessionId),
    queryFn: () => client.listReviewTranscriptions(projectId, session.sessionId),
    enabled: finalized,
    refetchInterval: (query) => query.state.data?.items.some((run) => run.state === "queued" || run.state === "running") ? 3_000 : false,
  });
  const items = runs.data?.items ?? [];
  const active = items.find((run) => run.state === "queued" || run.state === "running");
  const usable = items.find((run) => run.utterances.length > 0);
  const failed = !active && !usable && items[0] && (items[0].state === "failed" || items[0].state === "partial");

  const meta = [
    `${formatClock(session.receivedSeconds)} recorded`,
    session.manualCueCount ? plural(session.manualCueCount, "start mark") : null,
    usable ? plural(usable.utterances.length, "transcript line") : null,
  ].filter(Boolean).join(" · ");

  return (
    <li className={styles.recording}>
      <Link to={paths.session(projectId, targetId, session.sessionId)} className={styles.recordingMain}>
        <strong>{formatDateTime(session.createdAt)}</strong>
        <span>{meta}</span>
      </Link>
      <div className={styles.recordingActions}>
        {session.state !== "completed" ? <SessionBadge session={session} /> : null}
        {active ? <Badge tone="warning">Transcribing…</Badge> : null}
        {failed ? <Badge tone="critical">Transcription failed</Badge> : null}
        {usable && !active ? (
          <ButtonLink size="sm" variant="primary" to={paths.workspace(projectId, targetId, session.sessionId, usable.runId)}>Review notes</ButtonLink>
        ) : null}
        {finalized && !active && !usable && runs.isSuccess ? (
          <Button size="sm" variant={failed ? "secondary" : "primary"} onClick={onTranscribe}>{failed ? "Try again" : "Transcribe"}</Button>
        ) : null}
      </div>
    </li>
  );
}
