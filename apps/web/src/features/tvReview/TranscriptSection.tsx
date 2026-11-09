import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { ReviewSessionState, ReviewTranscriptionRequest, ReviewTranscriptionRun } from "@rcdvlog/project-client";
import { keys, useClient } from "../../app/client";
import { describeCode, describeError } from "../../lib/errors";
import { formatDateTime, formatStamp, plural } from "../../lib/format";
import { Badge, Button, Checkbox, Disclosure, Field, Loading, Notice, Panel, Segmented, type Tone } from "../../ui";
import { LiveTranscriptPanel, useLiveTranscript } from "./LiveTranscript";
import { UtteranceLine } from "./TranscriptLine";
import styles from "./TvReview.module.css";

const STATE: Record<ReviewTranscriptionRun["state"], { label: string; tone: Tone }> = {
  queued: { label: "Queued", tone: "warning" },
  running: { label: "Transcribing…", tone: "warning" },
  completed: { label: "Done", tone: "positive" },
  noSpeech: { label: "No speech", tone: "neutral" },
  partial: { label: "Partly done", tone: "warning" },
  failed: { label: "Failed", tone: "critical" },
  cancelled: { label: "Cancelled", tone: "neutral" },
};

export function TranscriptSection({ projectId, sessionId, sessionState, onOpenReview, pollMs = 2_000 }: {
  projectId: string;
  sessionId: string;
  sessionState: ReviewSessionState;
  onOpenReview: (runId: string) => void;
  pollMs?: number;
}) {
  const client = useClient();
  const queryClient = useQueryClient();
  const live = useLiveTranscript(projectId, sessionId, pollMs);
  const runs = useQuery({
    queryKey: keys.transcriptions(projectId, sessionId),
    queryFn: () => client.listReviewTranscriptions(projectId, sessionId),
    refetchInterval: (query) => live.data?.enabled && ["live", "refining"].includes(live.data.state) || query.state.data?.items.some((run) => run.state === "queued" || run.state === "running") ? pollMs : false,
  });
  const items = runs.data?.items ?? [];
  const active = items.find((run) => run.state === "queued" || run.state === "running");
  const [chosenRunId, setChosenRunId] = useState<string | null>(null);
  // Show what's useful: the run in progress, else the newest run with text, else the newest attempt.
  const shown = items.find((run) => run.runId === chosenRunId)
    ?? active
    ?? items.find((run) => run.utterances.length > 0)
    ?? items[0];
  const cancel = useMutation({
    mutationFn: (runId: string) => client.cancelReviewTranscription(projectId, runId),
    onSettled: () => queryClient.invalidateQueries({ queryKey: keys.transcriptions(projectId, sessionId) }),
  });
  const finalized = sessionState === "completed" || sessionState === "incomplete";

  const draft = live.data?.enabled && live.data.state !== "stopped" && shown?.state !== "completed" && shown?.state !== "noSpeech"
    ? <LiveTranscriptPanel data={live.data} projectId={projectId} recording={!finalized} /> : null;
  if (!finalized) {
    if (draft) return draft;
    return <Panel title="Transcript"><p className={styles.muted}>Available once the recording has finished uploading.</p></Panel>;
  }

  const hasTranscript = Boolean(shown?.utterances.length);
  return (
    <>
      {draft}
      {runs.isPending ? <Loading /> : null}
      {shown ? (
        <Panel
          title={draft ? "Full transcription" : "Transcript"}
          description={`${shown.provider} · ${shown.model} · ${formatDateTime(shown.createdAt)}`}
          actions={<div className={styles.inlineActions}>
            <Badge tone={STATE[shown.state].tone}>{STATE[shown.state].label}</Badge>
            {active && active.runId === shown.runId ? <Button size="sm" busy={cancel.isPending} onClick={() => cancel.mutate(active.runId)}>Cancel</Button> : null}
            {hasTranscript ? <Button size="sm" variant="primary" onClick={() => onOpenReview(shown.runId)}>Review notes</Button> : null}
          </div>}
        >
          <RunBody run={shown} />
        </Panel>
      ) : null}
      {cancel.error ? <Notice tone="critical">{describeError(cancel.error)}</Notice> : null}
      {items.length > 1 ? (
        <Disclosure title="All attempts" meta={items.length}>
          <ul className={styles.attempts}>
            {items.map((run) => (
              <li key={run.runId}>
                <button type="button" aria-current={run.runId === shown?.runId} onClick={() => setChosenRunId(run.runId)}>
                  <span>{formatDateTime(run.createdAt)} · {run.provider} · {run.model}</span>
                  <span className={styles.muted}>{run.utterances.length ? plural(run.utterances.length, "line") : run.errorCode ? describeCode(run.errorCode) : ""}</span>
                  <Badge tone={STATE[run.state].tone}>{STATE[run.state].label}</Badge>
                </button>
              </li>
            ))}
          </ul>
        </Disclosure>
      ) : null}
      {shown ? (
        <Disclosure title="Transcribe again"><TranscribeForm projectId={projectId} sessionId={sessionId} busy={Boolean(active)} onStarted={() => setChosenRunId(null)} /></Disclosure>
      ) : runs.isSuccess ? (
        <Panel title="Transcribe" description="Turn the recording into text so you can review what was said."><TranscribeForm projectId={projectId} sessionId={sessionId} busy={Boolean(active)} onStarted={() => setChosenRunId(null)} /></Panel>
      ) : null}
    </>
  );
}

function RunBody({ run }: { run: ReviewTranscriptionRun }) {
  const client = useClient();
  const workspace = useQuery({
    queryKey: keys.workspace(run.projectId, run.runId),
    queryFn: () => client.getReviewWorkspace(run.projectId, run.runId),
    enabled: run.utterances.length > 0,
    refetchInterval: run.state === "running" ? 2_000 : false,
  });
  const target = useQuery({
    queryKey: keys.target(run.projectId, run.reviewTargetId),
    queryFn: () => client.getReviewTarget(run.projectId, run.reviewTargetId),
    enabled: run.utterances.length > 0,
  });
  const context = workspace.data?.runId === run.runId && workspace.data.sessionId === run.sessionId
    && workspace.data.revisionId === run.revisionId ? workspace.data : undefined;
  const timedLines = new Map(context?.utterances.map((line) => [line.machine.utteranceId, line.movieTime]));
  const [showRaw, setShowRaw] = useState(false);
  const projected = run.utterances.some((utterance) => utterance.displayProjection && utterance.displayText !== utterance.text);
  const failedSegments = run.segments.filter((segment) => segment.state === "failed");
  const spend = run.spend;
  return (
    <div className={styles.runBody}>
      {run.errorCode ? <Notice tone="critical">{describeCode(run.errorCode)}</Notice> : null}
      {failedSegments.map((segment) => (
        <Notice key={segment.segmentIndex} tone="warning">{formatStamp(segment.startSeconds)}–{formatStamp(segment.endSeconds)} wasn’t transcribed: {describeCode(segment.errorCode ?? "review_transcription_failed")}</Notice>
      ))}
      {(run.providerErrors ?? []).map((error) => <Notice key={`${error.segmentIndex}:${error.windowIndex}`} tone="critical"
        title={`OpenRouter${error.httpStatus ? ` · HTTP ${error.httpStatus}` : ""}${error.code && error.code !== String(error.httpStatus) ? ` · ${error.code}` : ""}`}>
        {error.message}
        {error.generationId ? <small className={styles.code}> Request: {error.generationId}</small> : null}
      </Notice>)}
      {run.backend === "openrouter" && !["queued", "running"].includes(run.state) && (spend.dispatchedWindows > 0 || spend.unknownCostWindows > 0) ? (
        <p className={styles.muted}>
          {spend.unknownCostWindows && spend.knownCostUsd === 0 ? "Cost unavailable." : `Cost $${spend.knownCostUsd.toFixed(4)}.`}
          {spend.unknownCostWindows ? ` OpenRouter did not report the cost of ${plural(spend.unknownCostWindows, "request")}.` : " Reported by OpenRouter."}
        </p>
      ) : null}
      {run.state === "noSpeech" ? <p className={styles.muted}>No speech was found, so nothing was sent to a model.</p> : null}
      {run.utterances.length ? (
        <>
          <div className={styles.transcriptBar}>
            <span className={styles.muted}>{plural(run.utterances.length, "line")} · {run.diarization.status === "unavailable" ? "speakers not identified" : run.diarization.reason}</span>
            {projected ? <Checkbox checked={showRaw} onChange={setShowRaw}>Show original script</Checkbox> : null}
          </div>
          <ol className={styles.utterances}>
            {run.utterances.map((utterance) => {
              const movieTime = timedLines.get(utterance.utteranceId);
              const seconds = movieTime?.movieSeconds;
              const cut = seconds !== undefined && Number.isFinite(seconds) && seconds >= 0
                ? context?.cuts.find((shot) => shot.movieInSeconds <= seconds && seconds < shot.movieOutSeconds) : undefined;
              return <UtteranceLine key={utterance.utteranceId} utterance={utterance} showRaw={showRaw}
                movieTime={movieTime && Number.isFinite(movieTime.movieSeconds) && movieTime.movieSeconds >= 0 ? movieTime : undefined}
                thumbnail={cut ? client.projectAssetThumbnailUrl(run.projectId, cut.assetId) : undefined}
                shotNumber={cut ? cut.position + 1 : undefined}
                videoId={target.data?.youtube.videoId ?? undefined} timingLoading={workspace.isPending} />;
            })}
          </ol>
        </>
      ) : null}
    </div>
  );
}

/** Choose a backend/model, check availability, then start. Costs are informational after execution. */
export function TranscribeForm({ projectId, sessionId, busy, onStarted }: { projectId: string; sessionId: string; busy: boolean; onStarted?: () => void }) {
  const client = useClient();
  const queryClient = useQueryClient();
  const [backend, setBackend] = useState<"local" | "openrouter">("local");
  const [model, setModel] = useState("openai/whisper-1");
  const models = useQuery({ queryKey: keys.transcriptionModels(projectId), queryFn: () => client.listReviewTranscriptionModels(projectId), enabled: backend === "openrouter", staleTime: 300_000 });
  const choices = models.data?.items ?? [];
  const selectedModel = choices.some((choice) => choice.id === model) ? model : choices[0]?.id ?? "";
  const request = (): ReviewTranscriptionRequest => backend === "local"
    ? { backend: "local" }
    : { backend: "openrouter", model: selectedModel };

  const check = useMutation({ mutationFn: () => client.preflightReviewTranscription(projectId, sessionId, request()) });
  const start = useMutation({
    mutationFn: () => client.startReviewTranscription(projectId, sessionId, request()),
    onSuccess: () => { check.reset(); void queryClient.invalidateQueries({ queryKey: keys.transcriptions(projectId, sessionId) }); onStarted?.(); },
  });
  useEffect(() => { check.reset(); }, [backend, selectedModel]);

  const preflight = check.data;
  return (
    <form className={styles.transcribe} onSubmit={(event) => { event.preventDefault(); check.mutate(); }}>
      <Segmented
        label="Where to transcribe"
        value={backend}
        onChange={setBackend}
        options={[{ value: "local", label: "On this server" }, { value: "openrouter", label: "OpenRouter (cloud)" }]}
      />
      {backend === "local" ? (
        <p className={styles.muted}>Whisper, offline. Nothing leaves the server.</p>
      ) : (
        <div className={styles.cloud}>
          <Field label="OpenRouter model">
            {(id) => <select id={id} value={selectedModel} disabled={models.isPending || models.isError} onChange={(event) => setModel(event.target.value)}>
              {!choices.length ? <option value="">{models.isPending ? "Loading models…" : "No models available"}</option> : null}
              {choices.map((choice) => <option key={choice.id} value={choice.id}>{choice.name} ({choice.id})</option>)}
            </select>}
          </Field>
          {models.error ? <Notice tone="warning">{describeError(models.error)} <Button size="sm" onClick={() => void models.refetch()}>Retry model list</Button></Notice> : null}

        </div>
      )}

      {preflight ? (
        <div className={styles.runBody} role="status">
          {preflight.blockers.map((code) => <Notice key={code} tone="critical">{describeCode(code)}</Notice>)}
          {preflight.estimate ? (
            <p className={styles.muted}>
              {formatStamp(preflight.estimate.receivedAudioSeconds)} of audio in {plural(preflight.estimate.segmentCount, "segment")}.
              {backend === "local" ? " Free." : ""}
            </p>
          ) : null}
          {preflight.warnings.length ? <ul className={styles.bullets}>{preflight.warnings.map((code) => <li key={code}>{describeCode(code)}</li>)}</ul> : null}
        </div>
      ) : null}
      {check.error ?? start.error ? <Notice tone="critical">{describeError(check.error ?? start.error)}</Notice> : null}

      <div className={styles.inlineActions}>
        {preflight?.ready ? (
          <Button variant="primary" busy={start.isPending} disabled={busy || (backend === "openrouter" && (!selectedModel || models.isError))} onClick={() => start.mutate()}>{preflight.existingRunId ? "Resume transcription" : "Transcribe"}</Button>
        ) : (
          <Button type="submit" busy={check.isPending} disabled={backend === "openrouter" && (!selectedModel || models.isError)}>Check</Button>
        )}
      </div>
    </form>
  );
}
