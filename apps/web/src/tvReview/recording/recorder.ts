// Browser capture orchestration for TV voice review (#222).
//
// getUserMedia (processing disabled) → AudioWorklet (sample-clock tagged) →
// PcmChunker → durable outbox → ChunkUploader. The page never plays, controls
// or observes YouTube; the manual cue is the reviewer's own declaration.
import type { BrowserProjectClient, CreateReviewSessionRequest, RecordingEndReason, ReviewSessionDetail, ReviewTargetDetail } from "@rcdvlog/project-client";
import { PcmChunker, chunkFramesFor, sha256Hex, type PcmChunk } from "./chunker";
import { OutboxFullError, type ChunkOutbox, type OutboxEvent, type OutboxSession } from "./outbox";
import { ChunkUploader, type UploadStatus } from "./uploader";
import { captureCalibration } from "./clock";

export const WORKLET_URL = "/pcm-capture-worklet.js";
/** How often a native-frame / performance-clock bracket is recorded. */
export const CLOCK_SAMPLE_INTERVAL_MS = 5_000;
/** How long stop waits for the worklet to acknowledge its final delivered frame. */
export const STOP_ACK_TIMEOUT_MS = 1_000;
const RENDER_QUANTUM_FRAMES = 128;

type EventKind = OutboxEvent["kind"];
interface WorkletStopped { type: "stopped"; frame?: number; firstFrame?: number | null; finalFrame?: number | null }

export interface CaptureStatus {
  sampleRate: number;
  elapsedSeconds: number;
  peak: number;
  capturedFrames: number;
  lostFrames: number;
  discontinuities: number;
  /** Cues persisted locally; `upload.pendingEvents` says how many await the server. */
  cues: number;
  /** Events that could not even be stored locally (and are therefore lost). */
  unsavedEvents: number;
  upload: UploadStatus;
}

export function randomId(prefix: string): string {
  const bytes = crypto.getRandomValues(new Uint8Array(12));
  return `${prefix}_${Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("")}`;
}

export function microphoneConstraints(deviceId: string | null): MediaStreamConstraints {
  return {
    audio: {
      ...(deviceId ? { deviceId: { exact: deviceId } } : {}),
      echoCancellation: false,
      noiseSuppression: false,
      autoGainControl: false,
      channelCount: { ideal: 1 },
    },
    video: false,
  };
}

export async function listMicrophones(): Promise<MediaDeviceInfo[]> {
  const devices = await navigator.mediaDevices.enumerateDevices();
  return devices.filter((device) => device.kind === "audioinput");
}

function userAgentFamily(): string | null {
  const brands = (navigator as Navigator & { userAgentData?: { brands?: Array<{ brand: string }> } }).userAgentData?.brands ?? [];
  if (brands.some((b) => /chrom/i.test(b.brand))) return "chromium";
  return /firefox/i.test(navigator.userAgent) ? "firefox" : /safari/i.test(navigator.userAgent) ? "safari" : null;
}

export class VoiceRecording {
  private context!: AudioContext;
  private node!: AudioWorkletNode;
  private chunker!: PcmChunker;
  private uploader!: ChunkUploader;
  private sampleRate = 48_000;
  private startedAt = 0;
  private finished = false;
  private stopping: Promise<ReviewSessionDetail | null> | null = null;
  private stopAck: ((message: WorkletStopped | null) => void) | null = null;
  private clockEpoch = 0;
  private clockTimer: ReturnType<typeof setInterval> | null = null;
  private probe: { id: string; sentMs: number } | null = null;
  private probeTimer: ReturnType<typeof setInterval> | null = null;
  private writes: Promise<void> = Promise.resolve();
  private meta!: OutboxSession;
  private eventOrder = 0;
  session!: ReviewSessionDetail;
  status: CaptureStatus = {
    sampleRate: 48_000, elapsedSeconds: 0, peak: 0, capturedFrames: 0, lostFrames: 0,
    discontinuities: 0, cues: 0, unsavedEvents: 0,
    upload: { pendingChunks: 0, uploadedChunks: 0, pendingEvents: 0, lastReceiptAt: null, retrying: false, blockedCode: null },
  };

  private constructor(
    private readonly client: BrowserProjectClient,
    private readonly outbox: ChunkOutbox,
    private readonly projectId: string,
    private readonly stream: MediaStream,
    private readonly onStatus: (status: CaptureStatus) => void,
    private readonly onEnded: (reason: RecordingEndReason) => void,
  ) {}

  static async start(options: {
    client: BrowserProjectClient;
    outbox: ChunkOutbox;
    projectId: string;
    target: ReviewTargetDetail;
    stream: MediaStream;
    additionalSpeakersDisclosed: boolean;
    onStatus: (status: CaptureStatus) => void;
    onEnded: (reason: RecordingEndReason) => void;
  }): Promise<VoiceRecording> {
    const recording = new VoiceRecording(options.client, options.outbox, options.projectId, options.stream, options.onStatus, options.onEnded);
    await recording.begin(options.target, options.additionalSpeakersDisclosed);
    return recording;
  }

  private async begin(target: ReviewTargetDetail, additionalSpeakersDisclosed: boolean) {
    const track = this.stream.getAudioTracks()[0];
    if (!track) throw new Error("The microphone stream has no audio track.");
    const settings = track.getSettings();
    try {
      this.context = new AudioContext({ sampleRate: 48_000, latencyHint: "interactive" });
    } catch {
      this.context = new AudioContext({ latencyHint: "interactive" });
    }
    this.sampleRate = this.context.sampleRate;
    this.status = { ...this.status, sampleRate: this.sampleRate };
    const channels = Math.max(1, Math.min(2, settings.channelCount ?? 1));
    await this.context.audioWorklet.addModule(WORKLET_URL);
    const request: CreateReviewSessionRequest = {
      reviewTargetId: target.reviewTargetId,
      clientSessionKey: randomId("key"),
      consent: { allPresentAcknowledged: true, additionalSpeakersDisclosed },
      audio: { sampleRate: this.sampleRate, channels, encoding: "pcm_s16le" },
      capture: {
        echoCancellation: settings.echoCancellation ?? null,
        noiseSuppression: settings.noiseSuppression ?? null,
        autoGainControl: settings.autoGainControl ?? null,
        clock: "audioWorkletCurrentFrame",
        baseLatencySeconds: Number.isFinite(this.context.baseLatency) ? this.context.baseLatency : null,
        userAgentFamily: userAgentFamily(),
      },
      clientStartedPerformanceMs: performance.now(),
    };
    this.session = await this.client.createReviewSession(this.projectId, request);
    this.meta = { sessionId: this.session.sessionId, projectId: this.projectId, reviewTargetId: target.reviewTargetId, capturedFrames: 0, ended: false, endReason: null, updatedAt: new Date().toISOString() };
    await this.outbox.putSession(this.meta);
    this.uploader = new ChunkUploader(this.client, this.outbox, this.projectId, this.session.sessionId, (upload) => this.publish({ upload }));
    this.chunker = new PcmChunker(channels, chunkFramesFor(this.sampleRate, channels), (chunk) => { this.writes = this.writes.then(() => this.store(chunk)); });

    const source = this.context.createMediaStreamSource(this.stream);
    this.node = new AudioWorkletNode(this.context, "rcdvlog-pcm-capture", {
      numberOfInputs: 1,
      numberOfOutputs: 1,
      channelCount: channels,
      channelCountMode: "explicit",
      processorOptions: { channels },
    });
    const mute = this.context.createGain();
    mute.gain.value = 0;
    source.connect(this.node).connect(mute).connect(this.context.destination);
    this.node.port.onmessage = (event: MessageEvent) => this.onWorklet(event.data);
    track.addEventListener("ended", () => void this.deviceLost());
    this.context.addEventListener("statechange", () => void this.onContextState());
    this.startedAt = performance.now();
    if (this.context.state === "suspended") await this.context.resume();
    this.clockTimer = setInterval(() => void this.sampleClock("interval"), CLOCK_SAMPLE_INTERVAL_MS);
    this.probeTimer = setInterval(() => this.requestCalibration(), CLOCK_SAMPLE_INTERVAL_MS);
    this.requestCalibration();
  }

  /**
   * A suspended or interrupted context stops its sample clock while wall time
   * continues, so sample/performance pairs from either side of the pause must
   * never be fitted together: each resume starts a new explicit clock epoch.
   */
  private async onContextState() {
    if (this.finished) return;
    if (this.context.state === "running") {
      this.clockEpoch += 1;
      await this.sampleClock("resumed");
      this.requestCalibration();
    } else {
      await this.queueEvent("clockSample", this.contextFrame(), { clockEpoch: this.clockEpoch, transition: this.context.state, method: "statechange", uncertaintyMs: null }, null, null);
    }
  }

  /** Recording-relative frame of the context clock right now (0 before the first audio). */
  private contextFrame(): number {
    const origin = this.chunker.originFrame;
    return origin === null ? 0 : Math.round(this.context.currentTime * this.sampleRate) - origin;
  }

  /**
   * Record a paired (microphone sample index, performance.now) observation
   * with its declared uncertainty. getOutputTimestamp pairs both clocks in one
   * read; the fallback reads them separately and declares a wider bound.
   */
  async sampleClock(reason: "first" | "interval" | "resumed" | "stop"): Promise<void> {
    const origin = this.chunker?.originFrame ?? null;
    if (origin === null || this.context.state !== "running") return;
    const quantumMs = (RENDER_QUANTUM_FRAMES / this.sampleRate) * 1000;
    const stamp = typeof this.context.getOutputTimestamp === "function" ? this.context.getOutputTimestamp() : null;
    let contextSeconds: number;
    let performanceMs: number;
    let method: string;
    let uncertaintyMs: number;
    if (stamp && stamp.contextTime && stamp.performanceTime) {
      contextSeconds = stamp.contextTime;
      performanceMs = stamp.performanceTime;
      method = "getOutputTimestamp";
      uncertaintyMs = quantumMs;
    } else {
      contextSeconds = this.context.currentTime;
      performanceMs = performance.now();
      method = "currentTime";
      uncertaintyMs = 2 * quantumMs;
    }
    const sampleIndex = Math.round(contextSeconds * this.sampleRate) - origin;
    if (sampleIndex < 0) return;
    await this.queueEvent("clockSample", sampleIndex, { clockEpoch: this.clockEpoch, reason, method, uncertaintyMs, sampleRate: this.sampleRate }, null, performanceMs);
  }

  /**
   * Ask the audio thread for its native `currentFrame`. The worklet response
   * brackets that observation with the send/receive performance times; packet
   * arrival is never treated as sample zero.
   */
  private requestCalibration() {
    if (this.finished || this.context.state !== "running") return;
    if (this.probe && performance.now() - this.probe.sentMs < 1_000) return;
    this.probe = { id: randomId("probe"), sentMs: performance.now() };
    this.node.port.postMessage({ type: "clockProbe", probeId: this.probe.id });
  }

  private publish(patch: Partial<CaptureStatus>) {
    this.status = { ...this.status, ...patch };
    this.onStatus(this.status);
  }

  private onWorklet(message: { type: string; frame?: number; firstFrame?: number | null; finalFrame?: number | null; probeId?: string; channels?: Float32Array[]; expectedFrame?: number; actualFrame?: number }) {
    if (message.type === "stopped") {
      this.stopAck?.(message as WorkletStopped);
      return;
    }
    if (this.finished && message.type !== "audio") return;
    const now = performance.now();
    if (message.type === "clockSample" && this.probe && this.probe.id === message.probeId && message.frame !== undefined && message.firstFrame != null) {
      const origin = this.chunker.originFrame ?? message.firstFrame;
      const calibration = captureCalibration({
        clockId: `mic_epoch_${this.clockEpoch}`,
        sampleRate: this.sampleRate,
        firstSample: 0,
        absoluteFrame: message.frame,
        originFrame: origin,
        epochMs: this.startedAt,
        sentMs: this.probe.sentMs,
        receivedMs: now,
      });
      this.probe = null;
      if (calibration) {
        const uncertaintyMs = calibration.monotonicToleranceNs / 1_000_000;
        void this.queueEvent("clockSample", calibration.sampleIndex, { ...calibration, clockEpoch: this.clockEpoch, uncertaintyMs }, null, now);
      }
    } else if (message.type === "audio" && message.channels && message.frame !== undefined) {
      const first = this.chunker.originFrame === null;
      this.chunker.push(message.frame, message.channels, now);
      if (first) {
        void this.sampleClock("first");
        this.requestCalibration();
      }
    } else if (message.type === "discontinuity" && message.expectedFrame !== undefined && message.actualFrame !== undefined) {
      this.chunker.discontinuity(now);
      const origin = this.chunker.originFrame ?? message.expectedFrame;
      void this.queueEvent("captureDiscontinuity", message.expectedFrame - origin, { missingFrames: message.actualFrame - message.expectedFrame, clockEpoch: this.clockEpoch });
      this.publish({ discontinuities: this.status.discontinuities + 1 });
    }
    this.publish({ elapsedSeconds: (now - this.startedAt) / 1000, peak: this.chunker.peak, capturedFrames: this.chunker.capturedEndFrame });
  }

  private async store(chunk: PcmChunk) {
    const end = chunk.startFrame + chunk.frameCount;
    this.meta = { ...this.meta, capturedFrames: Math.max(this.meta.capturedFrames, end), updatedAt: new Date().toISOString() };
    try {
      await this.outbox.putSession(this.meta);
      await this.outbox.putChunk({ sessionId: this.session.sessionId, sequence: chunk.sequence, startFrame: chunk.startFrame, frameCount: chunk.frameCount, sha256: await sha256Hex(chunk.bytes), captureEndMs: chunk.captureEndMs, bytes: chunk.bytes });
      void this.uploader.kick();
    } catch (error) {
      // Audio that cannot be buffered is lost; account for it, never pretend.
      this.publish({ lostFrames: this.status.lostFrames + chunk.frameCount });
      await this.queueEvent("localBufferOverflow", chunk.startFrame, { lostFrames: chunk.frameCount, sequence: chunk.sequence, reason: error instanceof OutboxFullError ? "outboxFull" : "storageError" });
    }
  }

  /**
   * Persist an event with a stable id before any post. The uploader posts it
   * with the audio and retries it until the server acknowledges it; an
   * identical replay is idempotent on the server. Returns false only when the
   * event could not be stored locally, which is counted, never hidden.
   */
  private async queueEvent(kind: EventKind, audioFrame: number, detail: Record<string, unknown>, declaredMovieSeconds: number | null = null, performanceMs: number | null = performance.now()): Promise<boolean> {
    const event: OutboxEvent = {
      sessionId: this.session.sessionId,
      eventId: randomId("evt"),
      order: this.eventOrder++,
      kind,
      audioFrame: Math.max(0, Math.round(audioFrame)),
      performanceMs,
      declaredMovieSeconds,
      detail,
    };
    try {
      await this.outbox.putEvent(event);
    } catch {
      this.meta = { ...this.meta, eventStorageFailed: true };
      await this.outbox.putSession(this.meta).catch(() => undefined);
      this.publish({ unsavedEvents: this.status.unsavedEvents + 1, upload: { ...this.status.upload, blockedCode: "review_session_event_storage_failed" } });
      return false;
    }
    void this.uploader.kick();
    return true;
  }

  /** The reviewer's declaration that "now" is a known movie time (default: first frame). */
  async cue(declaredMovieSeconds = 0): Promise<boolean> {
    if (this.finished) return false;
    // currentTime is the rendering clock at the next quantum, closer to the
    // keypress than the last delivered batch.
    const stored = await this.queueEvent("manualSyncCue", this.contextFrame(), {
      method: "audioContextCurrentTime",
      clockEpoch: this.clockEpoch,
      outputLatencySeconds: (this.context as AudioContext & { outputLatency?: number }).outputLatency ?? null,
    }, declaredMovieSeconds);
    // Counted once durably queued; upload.pendingEvents shows server acceptance.
    if (stored) this.publish({ cues: this.status.cues + 1 });
    return stored;
  }

  private async deviceLost() {
    if (this.finished) return;
    await this.queueEvent("deviceLost", this.chunker.capturedEndFrame, { clockEpoch: this.clockEpoch });
    this.onEnded("deviceLost");
    await this.stop("deviceLost");
  }

  /**
   * Stop capture, persist the tail, and try to upload and finalize. Returns the
   * finalized session, or null when chunks remain queued for later recovery.
   */
  stop(reason: RecordingEndReason = "stopped", drainTimeoutMs = 30_000, flushTimeoutMs = STOP_ACK_TIMEOUT_MS): Promise<ReviewSessionDetail | null> {
    this.stopping ??= this.stopInternal(reason, drainTimeoutMs, flushTimeoutMs);
    return this.stopping;
  }

  private async stopInternal(reason: RecordingEndReason, drainTimeoutMs: number, flushTimeoutMs: number): Promise<ReviewSessionDetail | null> {
    if (!this.finished) {
      if (this.clockTimer) clearInterval(this.clockTimer);
      if (this.probeTimer) clearInterval(this.probeTimer);
      await this.sampleClock("stop");
      this.finished = true;
      // The worklet flushes its partial batch and then acknowledges the final
      // frame; same-port FIFO ordering guarantees the audio arrives first.
      // Without that acknowledgment the tail is unknown and must not be
      // certified as a clean stop.
      const acknowledged = await new Promise<WorkletStopped | null>((resolve) => {
        const timer = setTimeout(() => { this.stopAck = null; resolve(null); }, flushTimeoutMs);
        this.stopAck = (message) => { clearTimeout(timer); this.stopAck = null; resolve(message); };
        this.node.port.postMessage({ type: "stop" });
      });
      this.chunker.flush(performance.now());
      await this.writes;
      const captured = this.chunker.capturedEndFrame;
      let finalFrames = captured;
      if (!acknowledged) {
        finalFrames = Math.max(captured, this.contextFrame());
        await this.queueEvent("pageInterrupted", captured, { captureTailStatus: "unknown", reason: "workletStopUnacknowledged", originalEndReason: reason, clockEpoch: this.clockEpoch });
        this.publish({ discontinuities: this.status.discontinuities + 1 });
        reason = "interrupted";
      } else {
        const ackEnd = acknowledged.firstFrame != null && acknowledged.finalFrame != null
          ? acknowledged.finalFrame - acknowledged.firstFrame
          : acknowledged.frame != null && this.chunker.originFrame !== null
            ? acknowledged.frame - this.chunker.originFrame
            : null;
        if (ackEnd !== null && Number.isSafeInteger(ackEnd) && ackEnd >= captured) finalFrames = Math.max(captured, ackEnd);
      }
      this.stream.getTracks().forEach((track) => track.stop());
      await this.context.close().catch(() => undefined);
      this.meta = { ...this.meta, capturedFrames: Math.max(this.meta.capturedFrames, finalFrames), ended: true, endReason: reason, updatedAt: new Date().toISOString() };
      await this.outbox.putSession(this.meta);
    }
    return finalizeWhenDrained(this.client, this.outbox, this.meta, this.uploader, drainTimeoutMs);
  }

  /** Best-effort marker for an abrupt page exit; recovery finalizes as interrupted. */
  pageHidden() {
    if (!this.finished) void this.queueEvent("pageInterrupted", this.chunker.capturedEndFrame, { visibility: document.visibilityState, clockEpoch: this.clockEpoch });
  }
}

/** Upload what the outbox holds for a session, then finalize it on the server. */
export async function finalizeWhenDrained(
  client: BrowserProjectClient,
  outbox: ChunkOutbox,
  meta: OutboxSession,
  uploader: ChunkUploader = new ChunkUploader(client, outbox, meta.projectId, meta.sessionId),
  drainTimeoutMs = 30_000,
): Promise<ReviewSessionDetail | null> {
  if (!meta.ended) {
    // A restarted page cannot know the worklet's final frame. Do not certify
    // its last stored prefix as an acknowledged complete capture.
    await outbox.putEvent({ sessionId: meta.sessionId, eventId: "recovery_tail", order: -1, kind: "pageInterrupted", audioFrame: meta.capturedFrames, performanceMs: null, declaredMovieSeconds: null, detail: { captureTailStatus: "unknown", reason: "interruptedBeforeAcknowledgedStop" } });
  }
  const drained = await Promise.race([uploader.kick().then(() => true), new Promise<boolean>((resolve) => setTimeout(() => resolve(false), drainTimeoutMs))]);
  if (!drained || meta.eventStorageFailed || uploader.status.blockedCode || (await outbox.chunks(meta.sessionId)).length || (await outbox.events(meta.sessionId)).length) {
    uploader.stop();
    return null;
  }
  const endReason = meta.ended ? meta.endReason ?? "stopped" : "interrupted";
  try {
    const detail = await client.finalizeReviewSession(meta.projectId, meta.sessionId, { finalFrameCount: meta.capturedFrames, endReason });
    await outbox.deleteSession(meta.sessionId);
    return detail;
  } catch {
    const current = await client.getReviewSession(meta.projectId, meta.sessionId).catch(() => null);
    if (current && current.state !== "recording") {
      await outbox.deleteSession(meta.sessionId);
      return current;
    }
    return null;
  }
}

/** Sessions with locally buffered audio or no finalization yet (tab closed, network lost). */
export async function pendingRecoveries(outbox: ChunkOutbox, projectId: string): Promise<OutboxSession[]> {
  return (await outbox.sessions()).filter((session) => session.projectId === projectId);
}
