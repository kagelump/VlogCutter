import { useState } from "react";
import { useNavigate, useParams } from "react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { ReviewSessionDetail } from "@rcdvlog/project-client";
import { keys, useClient } from "../../app/client";
import { paths } from "../../app/paths";
import { Page } from "../../app/Shell";
import { ERROR_COPY, humanize } from "../../lib/errors";
import { formatClock, formatDateTime, plural } from "../../lib/format";
import { useDocumentTitle } from "../../lib/hooks";
import { ButtonAnchor, Button, ConfirmDialog, Disclosure, ErrorMessage, Facts, Loading, Notice, PageHeader, Stats } from "../../ui";
import { useProjectId } from "../projects/ProjectLayout";
import { SessionBadge } from "./shared";
import { TranscriptSection } from "./TranscriptSection";
import styles from "./TvReview.module.css";

export function SessionPage() {
  const projectId = useProjectId();
  const { targetId = "", sessionId = "" } = useParams();
  const client = useClient();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [confirming, setConfirming] = useState(false);
  const session = useQuery({
    queryKey: keys.session(projectId, sessionId),
    queryFn: () => client.getReviewSession(projectId, sessionId),
    refetchInterval: (query) => query.state.data?.state === "recording" ? 5_000 : false,
  });
  const remove = useMutation({
    mutationFn: () => client.deleteReviewSession(projectId, sessionId),
    onSuccess: (detail) => {
      queryClient.setQueryData(keys.session(projectId, sessionId), detail);
      void queryClient.invalidateQueries({ queryKey: keys.sessions(projectId, targetId) });
      setConfirming(false);
    },
  });
  const data = session.data;
  useDocumentTitle(data ? `Recording ${formatDateTime(data.createdAt)}` : "Recording");

  if (session.isPending) return <Page><Loading /></Page>;
  if (!data) return <Page><ErrorMessage error={session.error} onRetry={() => void session.refetch()} /></Page>;

  const cues = data.events.filter((event) => event.kind === "manualSyncCue");
  const gaps = data.coverage.gaps;

  return (
    <Page>
      <PageHeader
        back={{ to: paths.target(projectId, targetId), label: "Cut" }}
        title={formatDateTime(data.createdAt)}
        titleExtra={<SessionBadge session={data} />}
        subtitle="Recorded TV review"
        actions={data.state === "completed" ? (
          <ButtonAnchor icon="download" href={client.reviewSessionAudioUrl(projectId, sessionId)} download>Download WAV</ButtonAnchor>
        ) : null}
      />

      <div className={styles.columns}>
        <div className={styles.mainColumn}>
          <Stats label="Recording summary" items={[
            { label: "Length", value: formatClock(data.coverage.receivedSeconds) },
            { label: "Start marks", value: cues.length, detail: cues.length ? `first at ${formatClock(cues[0].audioSeconds)}` : "none" },
            { label: "Gaps", value: data.state === "recording" ? "—" : gaps.length, detail: gaps.length ? "skipped, never filled" : undefined },
            { label: "Ended", value: data.endReason ? humanize(data.endReason) : "Still open" },
          ]} />
          {data.state === "deleted" ? (
            <Notice>Audio deleted {formatDateTime(data.deletedAt)}. This record is kept so the deletion stays visible.</Notice>
          ) : (
            <TranscriptSection
              projectId={projectId}
              sessionId={sessionId}
              sessionState={data.state}
              onOpenReview={(runId) => navigate(paths.workspace(projectId, targetId, sessionId, runId))}
            />
          )}
        </div>

        <aside className={styles.sideColumn}>
          <Disclosure title="Recording details"><SessionFacts session={data} /></Disclosure>
          {data.state !== "deleted" ? (
            <div className={styles.dangerZone}>
              {data.state === "incomplete" || data.state === "corrupt" ? <p className={styles.muted}>{ERROR_COPY.review_session_audio_unavailable}</p> : null}
              <Button variant="danger" icon="trash" onClick={() => setConfirming(true)}>Delete audio</Button>
            </div>
          ) : null}
        </aside>
      </div>

      <ConfirmDialog
        open={confirming}
        title="Delete this recording’s audio?"
        confirmLabel="Delete audio"
        busy={remove.isPending}
        onClose={() => setConfirming(false)}
        onConfirm={() => remove.mutate()}
      >
        The audio is permanently removed from the server. A record that it existed is kept.
        <ErrorMessage error={remove.error} />
      </ConfirmDialog>
    </Page>
  );
}

function SessionFacts({ session }: { session: ReviewSessionDetail }) {
  const rate = session.sampleRate;
  const clockSamples = session.events.filter((event) => event.kind === "clockSample");
  const playbackSamples = clockSamples.filter((event) => event.detail?.method === "playerObservation");
  const epochs = new Set(clockSamples.map((event) => Number(event.detail?.clockEpoch ?? 0))).size;
  const cues = session.events.filter((event) => event.kind === "manualSyncCue");
  const health = session.events.filter((event) => event.kind !== "manualSyncCue" && event.kind !== "clockSample");
  const tailUnknown = session.events.some((event) => event.kind === "pageInterrupted" && event.detail?.captureTailStatus === "unknown");
  const onOff = (value: boolean | null) => value === null ? "unknown" : value ? "on" : "off";
  return (
    <Facts items={[
      ["Audio", `${session.sampleRate.toLocaleString()} Hz ${session.channels === 1 ? "mono" : "stereo"} · ${plural(session.coverage.chunkCount, "chunk")}`],
      ["Processing", `echo cancellation ${onOff(session.capture.echoCancellation)} · noise suppression ${onOff(session.capture.noiseSuppression)} · auto gain ${onOff(session.capture.autoGainControl)}`],
      ["Clock", clockSamples.length ? `${clockSamples.length} sample/performance clock pairs across ${plural(epochs, "clock epoch")}` : "No clock pairs"],
      playbackSamples.length ? ["Playback", `${plural(playbackSamples.length, "YouTube position observation")} saved · automatic comment placement is not yet available`] : null,
      ["Gaps", session.coverage.gaps.length ? session.coverage.gaps.map((gap) => `${formatClock(gap.startFrame / rate)}–${formatClock(gap.endFrame / rate)}`).join(", ") : session.state === "recording" ? "Computed when finished" : "None"],
      cues.length ? ["Start marks", cues.map((cue) => `${formatClock(cue.audioSeconds)} (your mark, not measured sync)`).join(", ")] : null,
      health.length ? ["Capture events", health.map((event) => `${formatClock(event.audioSeconds)} ${humanize(event.kind).toLowerCase()}`).join(", ")] : null,
      tailUnknown ? ["Ending", "The final sample count wasn’t confirmed; the end of the audio may be short."] : null,
      ["Consent", session.consent.additionalSpeakersDisclosed ? "Everyone present agreed · second speaker disclosed" : "Everyone present agreed · one speaker"],
      ["Cut", <span title={session.targetDigest}>{session.revisionId}</span>],
      ["Storage", `${session.storage.location} ${session.storage.retention} ${session.storage.backup}`],
      ["Finished", formatDateTime(session.finalizedAt)],
    ]} />
  );
}
