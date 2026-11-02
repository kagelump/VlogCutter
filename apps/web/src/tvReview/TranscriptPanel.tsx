import { useEffect, useState } from "react";
import type {
  BrowserProjectClient,
  ReviewTranscriptionPreflight,
  ReviewTranscriptionRequest,
  ReviewTranscriptionRun,
  ReviewUtterance,
} from "@rcdvlog/project-client";
import { StatusBadge } from "@rcdvlog/review-ui";
import { normalizeError } from "../errors";

export const TRANSCRIPTION_CODE_COPY: Record<string, string> = {
  review_transcription_session_not_finalized: "Finish (stop and upload) the recording before transcribing it.",
  review_transcription_no_audio: "This session has no stored audio.",
  review_transcription_local_model_unavailable: "The speech models are not installed on this server. Install them with `rcdvlog models install`, or choose an authorized cloud model.",
  review_transcription_cloud_not_authorized: "Confirm that this private recording may be sent to the selected provider.",
  review_transcription_budget_required: "Set a maximum spend for this cloud transcription.",
  review_transcription_provider_unavailable: "No OpenRouter credential is configured on this server.",
  review_transcription_budget_exhausted: "The authorized spend ceiling was reached. Raise it and retry to continue from where it stopped.",
  review_transcription_dispatch_outcome_unknown: "A cloud request's response was lost. It is counted as spent and will not be resent.",
  review_transcription_audio_changed: "The recording changed after this run started.",
  review_transcription_cancelled: "Cancelled. Completed work is kept; retry to resume.",
  review_transcription_partial: "Some parts could not be transcribed. Completed parts are kept.",
  review_transcription_failed: "Transcription failed. The recording is untouched.",
  diarization_unavailable: "Speakers are not identified automatically; every line is “unknown speaker” until you correct it.",
  tv_dialogue_not_separated: "TV dialogue is not separated from the room: film lines may appear as utterances.",
  recording_has_gaps: "The recording has gaps; they are skipped, never filled.",
  budget_below_worst_case: "Your ceiling is below the worst case; the run may stop early and can be resumed.",
};

const FLAG_COPY: Record<string, string> = {
  lowConfidence: "low confidence",
  probabilityEvidenceUnavailable: "no confidence evidence",
  unexpectedLanguage: "unexpected language",
  languageUnknown: "language unknown",
  nearRecordingGap: "near a recording gap",
};

function copy(code: string) { return TRANSCRIPTION_CODE_COPY[code] ?? code; }

export function formatStamp(seconds: number) {
  const whole = Math.max(0, seconds);
  const m = Math.floor(whole / 60);
  const s = whole - m * 60;
  return `${m}:${s.toFixed(1).padStart(4, "0")}`;
}

function stateBadge(run: ReviewTranscriptionRun) {
  const tone = run.state === "completed" ? "positive" : run.state === "failed" ? "critical" : run.state === "noSpeech" ? "neutral" : "warning";
  const label = { queued: "Queued", running: "Transcribing…", completed: "Transcribed", noSpeech: "No speech detected", partial: "Partially transcribed", failed: "Failed", cancelled: "Cancelled" }[run.state];
  return <StatusBadge label={label} tone={tone} />;
}

export function UtteranceRow({ utterance, showRaw }: { utterance: ReviewUtterance; showRaw: boolean }) {
  const projected = utterance.displayProjection && utterance.displayText !== utterance.text;
  return <li className={utterance.suppressed ? "tv-utterance suppressed" : "tv-utterance"} data-language={utterance.language}>
    <span className="tv-stamp">{formatStamp(utterance.startSeconds)}–{formatStamp(utterance.endSeconds)}</span>
    <span className="tv-speaker" title="Speaker labels come only from evidence or your correction">{utterance.speaker === "unknown" ? "Speaker ?" : `Speaker ${utterance.speaker}`}</span>
    <span className="tv-language">{utterance.language || "?"}</span>
    <span className="tv-text" lang={utterance.language === "zh" ? "zh-Hant" : utterance.language || undefined}>
      {utterance.suppressed ? <em>Quality-gated (kept as evidence): </em> : null}
      {showRaw ? utterance.text : utterance.displayText}
      {projected && !showRaw ? <small className="tv-raw"> raw: {utterance.text}</small> : null}
    </span>
    {utterance.flags.length ? <span className="tv-flags">{utterance.flags.map((flag) => FLAG_COPY[flag] ?? flag.replace("qualityGate:", "quality gate: ")).join(" · ")}</span> : null}
  </li>;
}

export function TranscriptPanel({ client, projectId, sessionId, sessionState, pollMs = 2000 }: { client: BrowserProjectClient; projectId: string; sessionId: string; sessionState: string; pollMs?: number }) {
  const [backend, setBackend] = useState<"local" | "openrouter">("local");
  const [budget, setBudget] = useState("0.50");
  const [acknowledged, setAcknowledged] = useState(false);
  const [zdr, setZdr] = useState(true);
  const [preflight, setPreflight] = useState<ReviewTranscriptionPreflight | null>(null);
  const [runs, setRuns] = useState<ReviewTranscriptionRun[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [showRaw, setShowRaw] = useState(false);

  const request = (): ReviewTranscriptionRequest => backend === "local"
    ? { backend: "local" }
    : { backend: "openrouter", model: "openai/whisper-1", authorizedMaxCostUsd: Number(budget) || null, cloudUploadAcknowledged: acknowledged, zeroDataRetention: zdr };

  const refresh = () => client.listReviewTranscriptions(projectId, sessionId).then((page) => setRuns(page.items)).catch(() => undefined);
  useEffect(() => { void refresh(); }, [client, projectId, sessionId]);
  const active = runs.find((run) => run.state === "queued" || run.state === "running");
  useEffect(() => {
    if (!active) return;
    const timer = window.setInterval(() => { void refresh(); }, pollMs);
    return () => window.clearInterval(timer);
  }, [active?.runId, pollMs]);
  useEffect(() => { setPreflight(null); }, [backend, budget, acknowledged, zdr]);

  async function check() {
    setBusy(true); setError(null);
    try { setPreflight(await client.preflightReviewTranscription(projectId, sessionId, request())); }
    catch (reason) { setError(copy(normalizeError(reason).code)); }
    finally { setBusy(false); }
  }

  async function start() {
    setBusy(true); setError(null);
    try { await client.startReviewTranscription(projectId, sessionId, request()); await refresh(); }
    catch (reason) { setError(copy(normalizeError(reason).code)); }
    finally { setBusy(false); }
  }

  async function cancel(runId: string) {
    try { await client.cancelReviewTranscription(projectId, runId); await refresh(); }
    catch (reason) { setError(copy(normalizeError(reason).code)); }
  }

  const latest = runs[0];
  const finalized = sessionState === "completed" || sessionState === "incomplete";
  return <section className="tv-transcript" aria-label="Transcript">
    <h2 className="tv-section-title">Transcript</h2>
    {!finalized ? <p className="help">Transcription is available once the recording is finalized.</p> : <form className="tv-transcribe" onSubmit={(event) => { event.preventDefault(); void check(); }}>
      <fieldset>
        <legend>Where to transcribe</legend>
        <label><input type="radio" name="tv-backend" checked={backend === "local"} onChange={() => setBackend("local")} /> On this server (Whisper, offline)</label>
        <label><input type="radio" name="tv-backend" checked={backend === "openrouter"} onChange={() => setBackend("openrouter")} /> OpenRouter cloud (openai/whisper-1)</label>
      </fieldset>
      {backend === "openrouter" ? <div className="tv-cloud">
        <label htmlFor="tv-budget">Maximum spend (USD)</label>
        <input id="tv-budget" inputMode="decimal" value={budget} onChange={(event) => setBudget(event.target.value)} />
        <label><input type="checkbox" checked={acknowledged} onChange={(event) => setAcknowledged(event.target.checked)} /> Send this private recording to OpenRouter for transcription.</label>
        <label><input type="checkbox" checked={zdr} onChange={(event) => setZdr(event.target.checked)} /> Require zero data retention.</label>
        <p className="help">Provider data collection is always denied. There is no automatic fallback to another provider or model.</p>
      </div> : null}
      <div className="tv-actions"><button className="secondary" disabled={busy}>Check</button>
        {preflight?.ready ? <button type="button" disabled={busy || Boolean(active)} onClick={() => void start()}>{preflight.existingRunId ? "Resume / reuse transcription" : "Transcribe"}</button> : null}</div>
      {preflight ? <div className="tv-preflight" role="status">
        {preflight.blockers.map((code) => <p key={code} className="inline-error">{copy(code)}</p>)}
        {preflight.estimate ? <p className="help">{preflight.estimate.receivedAudioSeconds.toFixed(0)} s of recorded audio in {preflight.estimate.segmentCount} segment{preflight.estimate.segmentCount === 1 ? "" : "s"}.{preflight.estimate.maxCostUsd !== null ? ` Worst-case cost $${preflight.estimate.maxCostUsd.toFixed(4)} (silence is never sent).` : " No cloud cost."}</p> : null}
        <ul className="tv-limitations">{preflight.warnings.map((code) => <li key={code}>{copy(code)}</li>)}</ul>
      </div> : null}
    </form>}
    {error ? <p className="inline-error" role="alert">{error}</p> : null}
    {latest ? <div className="tv-run">
      <div className="tv-heading-actions">{stateBadge(latest)}<span className="help">{latest.provider} · {latest.model} · attempt {latest.attemptCount}</span>
        {active ? <button className="secondary" onClick={() => void cancel(active.runId)}>Cancel</button> : null}
        <label className="tv-raw-toggle"><input type="checkbox" checked={showRaw} onChange={(event) => setShowRaw(event.target.checked)} /> Show raw text</label></div>
      {latest.errorCode ? <p className="inline-error">{copy(latest.errorCode)}</p> : null}
      {latest.spend.dispatchedWindows || latest.spend.authorizedMaxUsd !== null ? <p className="help">Spend: ${latest.spend.knownCostUsd.toFixed(4)} reported{latest.spend.unknownCostWindows ? `, ${latest.spend.unknownCostWindows} request(s) with unknown cost (held at $${latest.spend.reservedUsd.toFixed(4)})` : ""}{latest.spend.authorizedMaxUsd !== null ? ` of $${latest.spend.authorizedMaxUsd.toFixed(2)} authorized` : ""}.</p> : null}
      {latest.segments.filter((s) => s.state === "failed").map((s) => <p key={s.segmentIndex} className="help">{formatStamp(s.startSeconds)}–{formatStamp(s.endSeconds)} not transcribed: {copy(s.errorCode ?? "review_transcription_failed")}</p>)}
      <p className="help">{latest.diarization.reason}</p>
      {latest.utterances.length ? <ol className="tv-utterances">{latest.utterances.map((u) => <UtteranceRow key={u.utteranceId} utterance={u} showRaw={showRaw} />)}</ol>
        : latest.state === "noSpeech" ? <p className="help">No speech was detected; nothing was sent to a model.</p> : null}
    </div> : null}
  </section>;
}
