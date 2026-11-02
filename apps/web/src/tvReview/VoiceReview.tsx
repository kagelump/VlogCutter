import { useEffect, useRef, useState } from "react";
import {
  ProjectClientError,
  type BrowserProjectClient,
  type RecordingEndReason,
  type ReviewSessionDetail,
  type ReviewSessionSummary,
  type ReviewTargetDetail,
} from "@rcdvlog/project-client";
import { StatePanel, StatusBadge } from "@rcdvlog/review-ui";
import { normalizeError } from "../errors";
import { TranscriptPanel } from "./TranscriptPanel";
import { defaultOutbox, type ChunkOutbox, type OutboxSession } from "./recording/outbox";
import {
  VoiceRecording,
  finalizeWhenDrained,
  listMicrophones,
  microphoneConstraints,
  pendingRecoveries,
  type CaptureStatus,
} from "./recording/recorder";

/** Browser capabilities, injectable so component tests need no audio stack. */
export interface VoiceDeps {
  outbox: ChunkOutbox;
  getUserMedia(constraints: MediaStreamConstraints): Promise<MediaStream>;
  listMicrophones(): Promise<MediaDeviceInfo[]>;
  startRecording(options: Parameters<typeof VoiceRecording.start>[0]): Promise<Pick<VoiceRecording, "session" | "cue" | "stop" | "pageHidden">>;
}

let browserDeps: VoiceDeps | null = null;
export function browserVoiceDeps(): VoiceDeps {
  browserDeps ??= {
    outbox: defaultOutbox(),
    getUserMedia: (constraints) => navigator.mediaDevices.getUserMedia(constraints),
    listMicrophones,
    startRecording: (options) => VoiceRecording.start(options),
  };
  return browserDeps;
}

export const SESSION_CODE_COPY: Record<string, string> = {
  review_session_busy: "The server is handling other recordings. Uploads will retry automatically.",
  review_session_not_recording: "This session is already finalized.",
  review_session_chunk_conflict: "A chunk conflicts with audio the server already holds. The local copy is kept; nothing was overwritten.",
  review_session_chunk_corrupt: "A chunk failed its integrity check. The local copy is kept.",
  review_session_limit_exceeded: "This session reached its size or duration limit.",
  review_session_audio_unavailable: "Audio download is only offered for a completed session; incomplete audio is never padded.",
  review_target_verification_expired: "The review target's verification has expired. Re-export it on the owning Mac before recording.",
  review_target_artifact_unavailable: "The review target's movie is not known to be present on its owning host.",
  low_disk_space: "The server is low on disk space. Audio stays buffered in this browser until space is available.",
  idempotency_conflict: "This request conflicts with an earlier one.",
};

function describe(error: ProjectClientError) {
  return SESSION_CODE_COPY[error.code] ?? error.message;
}

function sessionPath(projectId: string, targetId: string, sessionId?: string) {
  const base = `/tv-review/${encodeURIComponent(projectId)}/${encodeURIComponent(targetId)}`;
  return sessionId ? `${base}/sessions/${encodeURIComponent(sessionId)}` : `${base}/record`;
}

function stateBadge(session: ReviewSessionSummary) {
  if (session.state === "completed") return <StatusBadge label="Completed" tone="positive" />;
  if (session.state === "recording") return <StatusBadge label={session.abandoned ? "Recording · abandoned" : "Recording / uploading"} tone="warning" />;
  if (session.state === "incomplete") return <StatusBadge label="Incomplete · gaps recorded" tone="warning" />;
  if (session.state === "corrupt") return <StatusBadge label="Corrupt evidence" tone="critical" />;
  return <StatusBadge label="Deleted" tone="neutral" />;
}

export function formatClock(seconds: number) {
  const whole = Math.max(0, Math.floor(seconds));
  const h = Math.floor(whole / 3600);
  const m = Math.floor((whole % 3600) / 60);
  const s = whole % 60;
  return `${h ? `${h}:` : ""}${String(m).padStart(h ? 2 : 1, "0")}:${String(s).padStart(2, "0")}`;
}

/** Recovery banner: finish uploading sessions a closed tab or dropped network left behind. */
export function PendingRecoveries({ client, projectId, deps, exclude, completed }: { client: BrowserProjectClient; projectId: string; deps: VoiceDeps; exclude?: string | null; completed?: () => void }) {
  const [pending, setPending] = useState<OutboxSession[]>([]);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const refresh = () => pendingRecoveries(deps.outbox, projectId).then((items) => setPending(items.filter((item) => item.sessionId !== exclude))).catch(() => setPending([]));
  useEffect(() => { void refresh(); }, [projectId, exclude]);
  if (!pending.length) return message ? <p className="help" role="status">{message}</p> : null;
  async function resume() {
    setBusy(true);
    let finished = 0;
    for (const meta of pending) {
      const result = await finalizeWhenDrained(client, deps.outbox, meta, undefined, 60_000);
      if (result) finished += 1;
    }
    setBusy(false);
    setMessage(finished === pending.length ? "Buffered recordings uploaded and finalized." : "Some audio is still buffered in this browser; try again when the server is reachable.");
    await refresh();
    completed?.();
  }
  return <section className="tv-recovery" role="region" aria-label="Unfinished recordings">
    <p><strong>{pending.length === 1 ? "One recording" : `${pending.length} recordings`} still {pending.length === 1 ? "has" : "have"} audio buffered in this browser or {pending.length === 1 ? "is" : "are"} not finalized.</strong> Nothing is lost yet; upload it before clearing browser data.</p>
    <button disabled={busy} onClick={() => void resume()}>{busy ? "Uploading…" : "Resume upload and finalize"}</button>
  </section>;
}

export function SessionList({ client, projectId, targetId, navigate }: { client: BrowserProjectClient; projectId: string; targetId: string; navigate: (path: string) => void }) {
  const [sessions, setSessions] = useState<ReviewSessionSummary[] | null>(null);
  useEffect(() => { client.listReviewSessions(projectId, targetId, 50).then((page) => setSessions(page.items)).catch(() => setSessions([])); }, [client, projectId, targetId]);
  if (!sessions?.length) return <p className="help">No voice reviews recorded against this exact target yet.</p>;
  return <div className="project-list">{sessions.map((session) => <button key={session.sessionId} onClick={() => navigate(sessionPath(projectId, targetId, session.sessionId))}>
    <span><strong>{new Date(session.createdAt).toLocaleString()}</strong><small>{formatClock(session.receivedSeconds)} received · {session.manualCueCount} cue{session.manualCueCount === 1 ? "" : "s"}</small></span>
    {stateBadge(session)}
  </button>)}</div>;
}

type Phase =
  | { kind: "setup" }
  | { kind: "starting" }
  | { kind: "recording" }
  | { kind: "stopping" }
  | { kind: "queued"; sessionId: string };

export function RecordView({ client, projectId, targetId, navigate, deps = browserVoiceDeps() }: { client: BrowserProjectClient; projectId: string; targetId: string; navigate: (path: string) => void; deps?: VoiceDeps }) {
  const [target, setTarget] = useState<ReviewTargetDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [consent, setConsent] = useState(false);
  const [secondSpeaker, setSecondSpeaker] = useState(false);
  const [stream, setStream] = useState<MediaStream | null>(null);
  const [microphones, setMicrophones] = useState<MediaDeviceInfo[]>([]);
  const [deviceId, setDeviceId] = useState<string>("");
  const [permission, setPermission] = useState<"unknown" | "denied" | "granted">("unknown");
  const [phase, setPhase] = useState<Phase>({ kind: "setup" });
  const [status, setStatus] = useState<CaptureStatus | null>(null);
  const recording = useRef<Awaited<ReturnType<VoiceDeps["startRecording"]>> | null>(null);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);

  useEffect(() => { client.getReviewTarget(projectId, targetId).then(setTarget).catch((reason) => setError(describe(normalizeError(reason)))); }, [client, projectId, targetId]);
  useEffect(() => () => { stream?.getTracks().forEach((track) => track.stop()); }, [stream]);
  useEffect(() => {
    const hidden = () => recording.current?.pageHidden();
    window.addEventListener("pagehide", hidden);
    return () => window.removeEventListener("pagehide", hidden);
  }, []);

  async function allowMicrophone(nextDevice = deviceId) {
    setError(null);
    try {
      stream?.getTracks().forEach((track) => track.stop());
      const next = await deps.getUserMedia(microphoneConstraints(nextDevice || null));
      setStream(next);
      setPermission("granted");
      const inputs = await deps.listMicrophones();
      setMicrophones(inputs);
      const active = next.getAudioTracks()[0]?.getSettings().deviceId;
      if (active) setDeviceId(active);
    } catch (reason) {
      const name = reason instanceof DOMException ? reason.name : "";
      setPermission(name === "NotAllowedError" || name === "SecurityError" ? "denied" : "unknown");
      setError(name === "NotAllowedError"
        ? "Microphone access was refused. Allow it from the address bar's site settings, then try again."
        : name === "NotFoundError" ? "No microphone was found. Connect one and try again."
        : "The microphone could not be opened. Check that no other app holds it, then try again.");
    }
  }

  async function record() {
    if (!stream || !target || !consent) return;
    setPhase({ kind: "starting" });
    setError(null);
    try {
      const started = await deps.startRecording({
        client, outbox: deps.outbox, projectId, target, stream,
        additionalSpeakersDisclosed: secondSpeaker,
        onStatus: setStatus,
        onEnded: (reason: RecordingEndReason) => { if (reason === "deviceLost") setError("The microphone was disconnected. Recording stopped; everything captured so far is kept."); },
      });
      recording.current = started;
      setActiveSessionId(started.session.sessionId);
      setPhase({ kind: "recording" });
    } catch (reason) {
      setPhase({ kind: "setup" });
      setError(reason instanceof ProjectClientError ? describe(reason) : "Recording could not start in this browser.");
    }
  }

  async function stop() {
    const active = recording.current;
    if (!active) return;
    setPhase({ kind: "stopping" });
    const result = await active.stop("stopped");
    recording.current = null;
    setStream(null);
    if (result) navigate(sessionPath(projectId, targetId, result.sessionId));
    else setPhase({ kind: "queued", sessionId: active.session.sessionId });
  }

  useEffect(() => {
    if (phase.kind !== "recording") return;
    const key = (event: KeyboardEvent) => {
      const tag = (event.target as HTMLElement | null)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
      if (event.key === "m" || event.key === "M") { event.preventDefault(); void recording.current?.cue(0); }
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [phase.kind]);

  if (!target) return <section className="workspace-page">{error ? <p className="inline-error" role="alert">{error}</p> : <StatePanel title="Loading review target" busy><p>Reading the immutable snapshot…</p></StatePanel>}</section>;
  const recordingNow = phase.kind === "recording" || phase.kind === "stopping";
  const blocked = !target.readiness.ready;
  return <section className="workspace-page tv-review-page">
    <div className="page-heading"><div><p className="eyebrow">Voice review / revision {target.revisionId}</p><h1>Record a TV review</h1></div>
      {recordingNow ? <span className="tv-recording-indicator" role="status" aria-live="polite">● Recording {formatClock(status?.elapsedSeconds ?? 0)}</span> : <button className="secondary" onClick={() => navigate(`/tv-review/${encodeURIComponent(projectId)}/${encodeURIComponent(targetId)}`)}>Back to target</button>}
    </div>
    <PendingRecoveries client={client} projectId={projectId} deps={deps} exclude={activeSessionId} />
    {blocked ? <p className="inline-error" role="alert">This target cannot start a new session: {target.readiness.blockers.map((code) => SESSION_CODE_COPY[code] ?? code).join(" ")}</p> : null}
    {error ? <p className="inline-error" role="alert">{error}</p> : null}
    <div className="tv-review-columns">
      <div className="tv-record-steps">
        <ol className="tv-steps">
          <li>Open <strong>{target.youtube.videoId ? `YouTube video ${target.youtube.videoId}` : "the review video"}</strong> on the TV yourself. This page never plays or controls it.</li>
          <li>Allow the microphone and pick the one nearest the conversation.</li>
          <li>Confirm consent, press <strong>Record</strong>, and talk normally while the TV plays.</li>
          <li>Press <strong>Mark video start</strong> (or <kbd>M</kbd>) the moment the first frame appears. It is optional and only declares what you saw.</li>
          <li>Press <strong>Stop</strong> when you are done.</li>
        </ol>
        <fieldset className="tv-consent" disabled={recordingNow}>
          <legend>Consent</legend>
          <label><input type="checkbox" checked={consent} onChange={(event) => setConsent(event.target.checked)} /> Everyone present knows this conversation is being recorded.</label>
          <label><input type="checkbox" checked={secondSpeaker} onChange={(event) => setSecondSpeaker(event.target.checked)} /> Another person will speak and has agreed to be recorded.</label>
        </fieldset>
        <p className="help">Audio is stored privately on this companion server with the session, bound to revision {target.revisionId} and preview digest {target.movie.blake3.slice(0, 12)}…. It is kept until you delete it, is not included in server backups, and is never sent anywhere else. Microphone processing (echo cancellation, noise suppression, gain) is switched off so the raw room sound is preserved.</p>
      </div>
      <aside className="tv-import" aria-label="Recorder">
        <p className="card-index">Microphone</p>
        {permission !== "granted" ? <>
          <p className="help">Your browser will ask for microphone access. Nothing is recorded until you press Record.</p>
          <button disabled={recordingNow} onClick={() => void allowMicrophone()}>{permission === "denied" ? "Try microphone again" : "Allow microphone"}</button>
        </> : <>
          <label htmlFor="tv-mic">Input device</label>
          <select id="tv-mic" value={deviceId} disabled={recordingNow} onChange={(event) => { setDeviceId(event.target.value); void allowMicrophone(event.target.value); }}>
            {microphones.map((mic, index) => <option key={mic.deviceId || index} value={mic.deviceId}>{mic.label || `Microphone ${index + 1}`}</option>)}
          </select>
        </>}
        <div className="tv-meter" aria-label="Input level" role="meter" aria-valuemin={0} aria-valuemax={1} aria-valuenow={status?.peak ?? 0}><i style={{ width: `${Math.round((status?.peak ?? 0) * 100)}%` }} /></div>
        <div className="tv-actions">
          {!recordingNow ? <button disabled={!stream || !consent || blocked || phase.kind === "starting"} onClick={() => void record()}>{phase.kind === "starting" ? "Starting…" : "● Record"}</button> : null}
          {phase.kind === "recording" ? <>
            <button className="secondary" onClick={() => void recording.current?.cue(0)} aria-keyshortcuts="M">Mark video start</button>
            <button onClick={() => void stop()}>■ Stop</button>
          </> : null}
          {phase.kind === "stopping" ? <button disabled>Finishing upload…</button> : null}
        </div>
        {status ? <dl className="tv-facts tv-recorder-facts">
          <dt>Elapsed</dt><dd>{formatClock(status.elapsedSeconds)}</dd>
          <dt>Uploaded</dt><dd>{status.upload.uploadedChunks} chunks{status.upload.pendingChunks ? ` · ${status.upload.pendingChunks} buffered` : ""}{status.upload.retrying ? " · retrying" : ""}</dd>
          <dt>Cues</dt><dd>{status.cues}{status.upload.pendingEvents ? ` · ${status.upload.pendingEvents} event${status.upload.pendingEvents === 1 ? "" : "s"} awaiting the server` : ""}</dd>
          {status.unsavedEvents ? <><dt>Events</dt><dd className="inline-error">{status.unsavedEvents} capture event{status.unsavedEvents === 1 ? "" : "s"} could not be saved locally</dd></> : null}
          {status.lostFrames ? <><dt>Lost</dt><dd>{(status.lostFrames / status.sampleRate).toFixed(1)} s could not be buffered (recorded as a gap)</dd></> : null}
          {status.discontinuities ? <><dt>Glitches</dt><dd>{status.discontinuities} capture discontinuities recorded</dd></> : null}
          {status.upload.blockedCode ? <><dt>Upload</dt><dd className="inline-error">{SESSION_CODE_COPY[status.upload.blockedCode] ?? status.upload.blockedCode}</dd></> : null}
        </dl> : null}
        {phase.kind === "queued" ? <p className="inline-error" role="alert">Recording stopped. Some audio is still buffered in this browser and the session is not finalized yet. Use “Resume upload and finalize” when the server is reachable.</p> : null}
      </aside>
    </div>
  </section>;
}

export function SessionDetailView({ client, projectId, targetId, sessionId, navigate }: { client: BrowserProjectClient; projectId: string; targetId: string; sessionId: string; navigate: (path: string) => void }) {
  const [session, setSession] = useState<ReviewSessionDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [confirming, setConfirming] = useState(false);
  useEffect(() => { client.getReviewSession(projectId, sessionId).then(setSession).catch((reason) => setError(describe(normalizeError(reason)))); }, [client, projectId, sessionId]);
  async function remove() {
    try { setSession(await client.deleteReviewSession(projectId, sessionId)); setConfirming(false); }
    catch (reason) { setError(describe(normalizeError(reason))); }
  }
  if (error) return <section className="workspace-page"><p className="inline-error" role="alert">{error}</p></section>;
  if (!session) return <section className="workspace-page"><StatePanel title="Loading voice review" busy><p>Reading session evidence…</p></StatePanel></section>;
  const rate = session.sampleRate;
  const cues = session.events.filter((event) => event.kind === "manualSyncCue");
  const clockSamples = session.events.filter((event) => event.kind === "clockSample");
  const clockEpochs = new Set(clockSamples.map((event) => Number(event.detail?.clockEpoch ?? 0))).size;
  const health = session.events.filter((event) => event.kind !== "manualSyncCue" && event.kind !== "clockSample");
  return <section className="workspace-page tv-review-page">
    <div className="page-heading"><div><p className="eyebrow">Voice review / revision {session.revisionId}</p><h1>Voice review session</h1></div>
      <div className="tv-heading-actions">{stateBadge(session)}<button className="secondary" onClick={() => navigate(`/tv-review/${encodeURIComponent(projectId)}/${encodeURIComponent(targetId)}`)}>Back to target</button></div></div>
    <div className="tv-review-columns">
      <div>
        <dl className="tv-facts">
          <dt>Recorded</dt><dd>{formatClock(session.coverage.receivedSeconds)} of audio received · {session.coverage.chunkCount} chunks · {session.sampleRate} Hz {session.channels === 1 ? "mono" : "stereo"} PCM</dd>
          <dt>Ended</dt><dd>{session.endReason ?? "not finalized"}{session.finalizedAt ? ` at ${session.finalizedAt}` : ""}</dd>
          <dt>Bound to</dt><dd>revision {session.revisionId} · target digest <code>{session.targetDigest.slice(0, 12)}…</code></dd>
          <dt>Consent</dt><dd>everyone present acknowledged{session.consent.additionalSpeakersDisclosed ? " · second speaker disclosed" : " · single speaker declared"}</dd>
          <dt>Processing</dt><dd>echo cancellation {String(session.capture.echoCancellation)} · noise suppression {String(session.capture.noiseSuppression)} · auto gain {String(session.capture.autoGainControl)}</dd>
          <dt>Clock</dt><dd>{clockSamples.length ? `${clockSamples.length} sample/performance clock pairs across ${clockEpochs} clock epoch${clockEpochs === 1 ? "" : "s"}` : "no clock pairs recorded"}</dd>
          <dt>Gaps</dt><dd>{session.coverage.gaps.length ? session.coverage.gaps.map((gap) => `${formatClock(gap.startFrame / rate)}–${formatClock(gap.endFrame / rate)}`).join(", ") + " (no audio exists for these spans)" : session.state === "recording" ? "not computed until finalized" : "none"}</dd>
        </dl>
        <h2 className="tv-section-title">Manual cues</h2>
        {cues.length ? <ul className="tv-limitations">{cues.map((cue) => <li key={cue.eventId}>{formatClock(cue.audioSeconds)} on the recording clock — declared movie {cue.declaredMovieSeconds ?? "?"} s (reviewer declaration, not measured sync)</li>)}</ul> : <p className="help">No manual cue. Automatic synchronization may still align this recording later.</p>}
        {health.length ? <><h2 className="tv-section-title">Capture events</h2><ul className="tv-limitations">{health.map((event) => <li key={event.eventId}>{formatClock(event.audioSeconds)} · {event.kind}</li>)}</ul></> : null}
        {session.state !== "deleted" ? <TranscriptPanel client={client} projectId={projectId} sessionId={sessionId} sessionState={session.state} onOpenReview={(runId) => navigate(`${sessionPath(projectId, targetId, sessionId)}/review/${encodeURIComponent(runId)}`)} /> : null}
      </div>
      <aside className="tv-import" aria-label="Evidence">
        <p className="card-index">Private evidence</p>
        <p className="help">{session.storage.location}</p>
        <p className="help">{session.storage.retention} {session.storage.backup}</p>
        {session.state === "completed" ? <a className="button-link" href={client.reviewSessionAudioUrl(projectId, sessionId)} download>Download WAV</a> : null}
        {session.state === "incomplete" || session.state === "corrupt" ? <p className="help">{SESSION_CODE_COPY.review_session_audio_unavailable}</p> : null}
        {session.state !== "deleted" ? (confirming
          ? <div className="tv-actions"><button className="danger" onClick={() => void remove()}>Delete audio permanently</button><button className="secondary" onClick={() => setConfirming(false)}>Keep</button></div>
          : <button className="secondary" onClick={() => setConfirming(true)}>Delete recording…</button>) : <p className="help">Audio deleted {session.deletedAt}. The session record remains so nothing is silently forgotten.</p>}
      </aside>
    </div>
  </section>;
}
