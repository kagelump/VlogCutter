import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { ReviewSessionState, ReviewTranscriptionRequest, ReviewTranscriptionRun, ReviewUtterance } from "@rcdvlog/project-client";
import { keys, useClient } from "../../app/client";
import { describeCode, describeError } from "../../lib/errors";
import { formatDateTime, formatStamp, plural } from "../../lib/format";
import { Badge, Button, Checkbox, Disclosure, Field, Loading, Notice, Panel, Segmented, type Tone } from "../../ui";
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

const FLAG_COPY: Record<string, string> = {
  lowConfidence: "low confidence",
  probabilityEvidenceUnavailable: "no confidence data",
  unexpectedLanguage: "unexpected language",
  languageUnknown: "language unknown",
  nearRecordingGap: "near a gap",
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
  const runs = useQuery({
    queryKey: keys.transcriptions(projectId, sessionId),
    queryFn: () => client.listReviewTranscriptions(projectId, sessionId),
    refetchInterval: (query) => query.state.data?.items.some((run) => run.state === "queued" || run.state === "running") ? pollMs : false,
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

  if (!finalized) {
    return <Panel title="Transcript"><p className={styles.muted}>Available once the recording has finished uploading.</p></Panel>;
  }

  const hasTranscript = Boolean(shown?.utterances.length);
  return (
    <>
      {runs.isPending ? <Loading /> : null}
      {shown ? (
        <Panel
          title="Transcript"
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
        <Disclosure title="Transcribe again"><TranscribeForm projectId={projectId} sessionId={sessionId} busy={Boolean(active)} /></Disclosure>
      ) : runs.isSuccess ? (
        <Panel title="Transcribe" description="Turn the recording into text so you can review what was said."><TranscribeForm projectId={projectId} sessionId={sessionId} busy={Boolean(active)} /></Panel>
      ) : null}
    </>
  );
}

function RunBody({ run }: { run: ReviewTranscriptionRun }) {
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
      {spend.dispatchedWindows || spend.authorizedMaxUsd !== null ? (
        <p className={styles.muted}>
          Cost ${spend.knownCostUsd.toFixed(4)}
          {spend.unknownCostWindows ? ` + ${plural(spend.unknownCostWindows, "request")} of unknown cost (up to $${spend.reservedUsd.toFixed(4)})` : ""}
          {spend.authorizedMaxUsd !== null ? ` of $${spend.authorizedMaxUsd.toFixed(2)} limit` : ""}
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
            {run.utterances.map((utterance) => <UtteranceLine key={utterance.utteranceId} utterance={utterance} showRaw={showRaw} />)}
          </ol>
        </>
      ) : null}
    </div>
  );
}

function UtteranceLine({ utterance, showRaw }: { utterance: ReviewUtterance; showRaw: boolean }) {
  return (
    <li className={styles.utterance} data-suppressed={utterance.suppressed || undefined}>
      <span className={styles.stamp}>{formatStamp(utterance.startSeconds)}</span>
      <span className={styles.speaker}>{utterance.speaker === "unknown" ? "?" : utterance.speaker}</span>
      <span className={styles.utteranceText} lang={utterance.language === "zh" ? "zh-Hant" : utterance.language || undefined}>
        {utterance.suppressed ? <em className={styles.muted}>Low quality: </em> : null}
        {showRaw ? utterance.text : utterance.displayText}
        {utterance.flags.length ? <small className={styles.flags}>{utterance.flags.map((flag) => FLAG_COPY[flag] ?? flag.replace("qualityGate:", "")).join(" · ")}</small> : null}
      </span>
    </li>
  );
}

function TranscribeForm({ projectId, sessionId, busy }: { projectId: string; sessionId: string; busy: boolean }) {
  const client = useClient();
  const queryClient = useQueryClient();
  const [backend, setBackend] = useState<"local" | "openrouter">("local");
  const [budget, setBudget] = useState("0.50");
  const [acknowledged, setAcknowledged] = useState(false);
  const [zdr, setZdr] = useState(true);
  const request = (): ReviewTranscriptionRequest => backend === "local"
    ? { backend: "local" }
    : { backend: "openrouter", model: "openai/whisper-1", authorizedMaxCostUsd: Number(budget) || null, cloudUploadAcknowledged: acknowledged, zeroDataRetention: zdr };

  const check = useMutation({ mutationFn: () => client.preflightReviewTranscription(projectId, sessionId, request()) });
  const start = useMutation({
    mutationFn: () => client.startReviewTranscription(projectId, sessionId, request()),
    onSuccess: () => { check.reset(); void queryClient.invalidateQueries({ queryKey: keys.transcriptions(projectId, sessionId) }); },
  });
  useEffect(() => { check.reset(); }, [backend, budget, acknowledged, zdr]);

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
          <Field label="Spending limit (USD)">
            {(id) => <input id={id} inputMode="decimal" value={budget} onChange={(event) => setBudget(event.target.value)} />}
          </Field>
          <Checkbox checked={acknowledged} onChange={setAcknowledged}>Send this recording to OpenRouter (openai/whisper-1).</Checkbox>
          <Checkbox checked={zdr} onChange={setZdr}>Require zero data retention.</Checkbox>
          <p className={styles.hintSmall}>Providers may never train on it. There’s no fallback to another provider or model.</p>
        </div>
      )}

      {preflight ? (
        <div className={styles.runBody} role="status">
          {preflight.blockers.map((code) => <Notice key={code} tone="critical">{describeCode(code)}</Notice>)}
          {preflight.estimate ? (
            <p className={styles.muted}>
              {formatStamp(preflight.estimate.receivedAudioSeconds)} of audio in {plural(preflight.estimate.segmentCount, "segment")}.
              {preflight.estimate.maxCostUsd !== null ? ` At most $${preflight.estimate.maxCostUsd.toFixed(4)}; silence isn’t sent.` : " Free."}
            </p>
          ) : null}
          {preflight.warnings.length ? <ul className={styles.bullets}>{preflight.warnings.map((code) => <li key={code}>{describeCode(code)}</li>)}</ul> : null}
        </div>
      ) : null}
      {check.error ?? start.error ? <Notice tone="critical">{describeError(check.error ?? start.error)}</Notice> : null}

      <div className={styles.inlineActions}>
        {preflight?.ready ? (
          <Button variant="primary" busy={start.isPending} disabled={busy} onClick={() => start.mutate()}>{preflight.existingRunId ? "Resume transcription" : "Transcribe"}</Button>
        ) : (
          <Button type="submit" busy={check.isPending}>Check</Button>
        )}
      </div>
    </form>
  );
}
