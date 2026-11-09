import { useCallback, useEffect, useRef, useState } from "react";
import { useBlocker, useNavigate, useParams, useSearchParams } from "react-router";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ProjectClientError, type RecordingEndReason } from "@rcdvlog/project-client";
import { keys, useClient } from "../../app/client";
import { paths } from "../../app/paths";
import { Page } from "../../app/Shell";
import { describeCode, describeError } from "../../lib/errors";
import { formatClock } from "../../lib/format";
import { useDocumentTitle, useHotkeys } from "../../lib/hooks";
import { Button, Checkbox, ConfirmDialog, ErrorMessage, Field, Kbd, Loading, Notice, PageHeader } from "../../ui";
import { useProjectId } from "../projects/ProjectLayout";
import { microphoneConstraints, type CaptureStatus } from "./recording/recorder";
import { PendingRecoveries, targetTitle, useVoiceDeps, type VoiceDeps } from "./shared";
import styles from "./TvReview.module.css";
import { LiveTranscriptPanel, useLiveTranscript } from "./LiveTranscript";
import { YouTubePlayer } from "./YouTubePlayer";

type Phase = "setup" | "starting" | "recording" | "stopping" | "queued";
type Recording = Awaited<ReturnType<VoiceDeps["startRecording"]>>;

export function RecordPage() {
  const projectId = useProjectId();
  const { targetId = "" } = useParams();
  const [searchParams] = useSearchParams();
  const playMode = searchParams.get("mode") === "play";
  const client = useClient();
  const deps = useVoiceDeps();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const target = useQuery({ queryKey: keys.target(projectId, targetId), queryFn: () => client.getReviewTarget(projectId, targetId) });
  const [liveEnabled, setLiveEnabled] = useState(true);
  const [liveError, setLiveError] = useState(false);
  const [stream, setStream] = useState<MediaStream | null>(null);
  const [microphones, setMicrophones] = useState<MediaDeviceInfo[]>([]);
  const [deviceId, setDeviceId] = useState("");
  const [micState, setMicState] = useState<"idle" | "requesting" | "denied" | "granted">("idle");
  const [phase, setPhase] = useState<Phase>("setup");
  const [status, setStatus] = useState<CaptureStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const recording = useRef<Recording | null>(null);
  const microphone = useRef<MediaStream | null>(null);
  const micRequest = useRef(0);
  const liveTranscript = useLiveTranscript(projectId, activeSessionId ?? "");
  useDocumentTitle(phase === "recording" ? `● ${formatClock(status?.elapsedSeconds ?? 0)}` : "Record review");

  const live = phase === "recording" || phase === "stopping";
  // Guard in-app navigation while capturing; our own redirect after Stop is allowed through.
  const leaving = useRef(false);
  const blocker = useBlocker(() => live && !leaving.current);

  useEffect(() => () => {
    micRequest.current += 1;
    microphone.current?.getTracks().forEach((track) => track.stop());
  }, []);
  useEffect(() => {
    const hidden = () => recording.current?.pageHidden();
    window.addEventListener("pagehide", hidden);
    return () => window.removeEventListener("pagehide", hidden);
  }, []);
  useEffect(() => {
    if (!live) return;
    const warn = (event: BeforeUnloadEvent) => { event.preventDefault(); };
    window.addEventListener("beforeunload", warn);
    return () => window.removeEventListener("beforeunload", warn);
  }, [live]);

  const cue = () => void recording.current?.cue(0);
  useHotkeys({ m: cue }, phase === "recording");

  const openMicrophone = useCallback(async (nextDevice = "") => {
    const request = ++micRequest.current;
    setError(null);
    setMicState("requesting");
    setStream(null);
    microphone.current?.getTracks().forEach((track) => track.stop());
    microphone.current = null;
    try {
      const next = await deps.getUserMedia(microphoneConstraints(nextDevice || null));
      if (request !== micRequest.current) { next.getTracks().forEach((track) => track.stop()); return; }
      microphone.current = next;
      setStream(next);
      setMicState("granted");
      const active = next.getAudioTracks()[0]?.getSettings().deviceId;
      if (active) setDeviceId(active);
      // Enumeration failure shouldn't disable a microphone that opened successfully.
      const devices = await deps.listMicrophones().catch(() => []);
      if (request === micRequest.current) setMicrophones(devices);
    } catch (reason) {
      if (request !== micRequest.current) return;
      const name = reason instanceof DOMException ? reason.name : "";
      setMicState(name === "NotAllowedError" || name === "SecurityError" ? "denied" : "idle");
      setError(name === "NotAllowedError"
        ? "Microphone access is blocked. Allow it in the browser’s site settings, then try again."
        : name === "NotFoundError" ? "No microphone found. Connect one and try again."
        : "The microphone couldn’t be opened. Check that no other app is using it.");
    }
  }, [deps]);

  useEffect(() => {
    if (target.data?.readiness.ready) void openMicrophone();
  }, [openMicrophone, target.data?.readiness.ready]);

  async function start() {
    if (!stream || !target.data || phase !== "setup") return;
    setLiveError(false);
    setPhase("starting");
    setError(null);
    try {
      const started = await deps.startRecording({
        client, outbox: deps.outbox, projectId, target: target.data, stream,
        additionalSpeakersDisclosed: false,
        onStatus: setStatus,
        onEnded: (reason: RecordingEndReason) => {
          if (reason === "deviceLost") setError("The microphone disconnected. Recording stopped; everything captured so far is kept.");
        },
      });
      recording.current = started;
      setActiveSessionId(started.session.sessionId);
      setPhase("recording");
      if (liveEnabled) {
        // Capture is already running. Transcription failure must never stop it.
        void client.enableLiveReviewTranscript(projectId, started.session.sessionId).then((data) => {
          queryClient.setQueryData(keys.liveTranscript(projectId, started.session.sessionId), data);
        }).catch(() => setLiveError(true));
      }
    } catch (reason) {
      setPhase("setup");
      setError(reason instanceof ProjectClientError ? describeError(reason) : "Recording couldn’t start in this browser.");
    }
  }

  async function stop() {
    const active = recording.current;
    if (!active) return;
    setPhase("stopping");
    const result = await active.stop("stopped");
    recording.current = null;
    setStream(null);
    void queryClient.invalidateQueries({ queryKey: keys.sessions(projectId, targetId) });
    if (result) { leaving.current = true; navigate(paths.session(projectId, targetId, result.sessionId)); }
    else setPhase("queued");
  }

  if (target.isPending) return <Page><Loading /></Page>;
  if (!target.data) return <Page><ErrorMessage error={target.error} onRetry={() => void target.refetch()} /></Page>;
  const detail = target.data;
  const blocked = !detail.readiness.ready;
  const upload = status?.upload;

  return (
    <Page>
      <div className={styles.recordLayout}>
      <div className={styles.recordMain}>
        <PageHeader
          back={live ? undefined : { to: paths.target(projectId, targetId), label: targetTitle(detail) }}
          title={playMode ? "Play + record review" : "Record review"}
          subtitle={playMode ? targetTitle(detail) : detail.youtube.videoId ? <>Play <a href={`https://www.youtube.com/watch?v=${encodeURIComponent(detail.youtube.videoId)}`} target="_blank" rel="noreferrer noopener">youtu.be/{detail.youtube.videoId}</a> on the TV.</> : "Play this cut on the TV."}
        />

        <PendingRecoveries projectId={projectId} exclude={activeSessionId} />
        {blocked ? <Notice tone="critical" title="Recording is blocked.">{detail.readiness.blockers.map(describeCode).join(" ")}</Notice> : null}
        {error ? <Notice tone="critical">{error}</Notice> : null}

        {live ? (
          <section className={styles.recorder} aria-label="Recorder">
            <div className={styles.recTime} role="status" aria-live="polite">
              <span className={styles.recDot} aria-hidden="true" />
              Recording <strong>{formatClock(status?.elapsedSeconds ?? 0)}</strong>
            </div>
            <Meter peak={status?.peak ?? 0} />
            <div className={styles.recButtons}>
              {!playMode ? <Button size="lg" onClick={cue} disabled={phase !== "recording"} aria-keyshortcuts="M">Mark start</Button> : null}
              <Button size="lg" variant="danger" busy={phase === "stopping"} onClick={() => void stop()}>{phase === "stopping" ? "Finishing upload…" : "Stop"}</Button>
            </div>
            {!playMode ? <p className={styles.muted}>Tap <strong>Mark start</strong> (or press <Kbd>M</Kbd>) when the first frame appears on the TV. It helps line up your comments with the cut, but is optional.</p> : null}
            {status ? (
              <ul className={styles.recStats}>
                <li><span>Uploaded</span>{upload!.uploadedChunks} chunks{upload!.pendingChunks ? `, ${upload!.pendingChunks} waiting` : ""}{upload!.retrying ? " · retrying" : ""}</li>
                <li><span>Start marks</span>{status.cues}{upload!.pendingEvents ? ` (${upload!.pendingEvents} waiting)` : ""}</li>
                {status.lostFrames ? <li data-warn><span>Lost</span>{(status.lostFrames / status.sampleRate).toFixed(1)}s couldn’t be buffered and is recorded as a gap</li> : null}
                {status.discontinuities ? <li data-warn><span>Glitches</span>{status.discontinuities}</li> : null}
                {status.unsavedEvents ? <li data-warn><span>Unsaved events</span>{status.unsavedEvents}</li> : null}
                {upload!.blockedCode ? <li data-warn><span>Upload</span>{describeCode(upload!.blockedCode)}</li> : null}
              </ul>
            ) : null}
          </section>
        ) : phase === "queued" ? (
          <Notice tone="warning" title="Recording stopped, upload unfinished.">Some audio is still in this browser. Use <strong>Finish upload</strong> above when the server is reachable.</Notice>
        ) : (
          <section className={styles.recorder} aria-label="Recorder setup">
            <div className={styles.recordToolbar}>
              <Button size="lg" variant="primary" icon="mic" busy={phase === "starting"} disabled={!stream || blocked || (playMode && !detail.youtube.videoId)} onClick={() => void start()}>Record</Button>
              {micState === "granted" ? (
                <Field label="Microphone">
                  {(id) => <select id={id} value={deviceId} onChange={(event) => { setDeviceId(event.target.value); void openMicrophone(event.target.value); }}>
                    {microphones.length ? microphones.map((mic, index) => <option key={mic.deviceId || index} value={mic.deviceId}>{mic.label || `Microphone ${index + 1}`}</option>) : <option value={deviceId}>Default microphone</option>}
                  </select>}
                </Field>
              ) : <Button busy={micState === "requesting"} onClick={() => void openMicrophone(deviceId)}>{micState === "denied" ? "Try again" : "Allow microphone"}</Button>}
            </div>
            <Checkbox checked={liveEnabled} onChange={setLiveEnabled}>Live transcription</Checkbox>
          </section>
        )}

        {playMode && detail.youtube.videoId ? <>
          <YouTubePlayer videoId={detail.youtube.videoId} recording={phase === "recording"} onObservation={(position, state, rate, time) => recording.current?.observePlayback(position, state, rate, time)} />
        </> : playMode ? <Notice tone="warning">This review has no YouTube video. Return to the cut and use Record review.</Notice> : null}
      </div>
      {liveEnabled ? <aside className={styles.recordTranscript} aria-label="Recording transcript">
        {!activeSessionId ? <Notice>Live transcript will appear here when you record.</Notice> : liveError ? <Notice tone="warning">Live transcription couldn’t start. Recording continues. <Button onClick={() => {
          void client.enableLiveReviewTranscript(projectId, activeSessionId).then((data) => {
            setLiveError(false); queryClient.setQueryData(keys.liveTranscript(projectId, activeSessionId), data);
          }).catch(() => setLiveError(true));
        }}>Retry live transcription</Button></Notice> : liveTranscript.data?.enabled ? <LiveTranscriptPanel data={liveTranscript.data} projectId={projectId} videoId={detail.youtube.videoId ?? undefined} recording /> : <Notice>Starting live transcription…</Notice>}
      </aside> : null}
      </div>

      <ConfirmDialog
        open={blocker.state === "blocked"}
        title="Leave while recording?"
        confirmLabel="Leave"
        onClose={() => blocker.reset?.()}
        onConfirm={() => blocker.proceed?.()}
      >
        Recording keeps running only while this page is open. Stop it first to make sure everything uploads.
      </ConfirmDialog>
    </Page>
  );
}

function Meter({ peak }: { peak: number }) {
  const value = Math.max(0, Math.min(1, peak));
  return (
    <div className={styles.meter} role="meter" aria-label="Input level" aria-valuemin={0} aria-valuemax={1} aria-valuenow={Number(value.toFixed(2))}>
      <span style={{ width: `${Math.round(value * 100)}%` }} />
    </div>
  );
}
