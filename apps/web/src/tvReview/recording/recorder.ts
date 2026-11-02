// Browser capture orchestration for TV voice review (#222).
//
// getUserMedia (processing disabled) → AudioWorklet (sample-clock tagged) →
// PcmChunker → durable outbox → ChunkUploader. The page never plays, controls
// or observes YouTube; the manual cue is the reviewer's own declaration.
import type { BrowserProjectClient, CreateReviewSessionRequest, RecordingEndReason, ReviewSessionDetail, ReviewTargetDetail, ReviewSessionEventRequest } from "@rcdvlog/project-client";
import { PcmChunker, chunkFramesFor, sha256Hex, type PcmChunk } from "./chunker";
import { OutboxFullError, type ChunkOutbox, type OutboxSession } from "./outbox";
import { ChunkUploader, type UploadStatus } from "./uploader";
import { captureCalibration } from "./clock";

export const WORKLET_URL = "/pcm-capture-worklet.js";

export interface CaptureStatus {
  sampleRate: number;
  elapsedSeconds: number;
  peak: number;
  capturedFrames: number;
  lostFrames: number;
  discontinuities: number;
  cues: number;
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
  private stopping: Promise<ReviewSessionDetail|null> | null = null;
  private eventWrites: Promise<void> = Promise.resolve();
  private stopAck: ((valid: boolean) => void) | null = null;
  private captureOrigin: number | null = null;
  private clockEpoch = 0;
  private epochCalibrated = false;
  private epochFirstSample = 0;
  private probe: { id: string; sentMs: number } | null = null;
  private probeTimer: ReturnType<typeof setInterval> | null = null;
  private writes: Promise<void> = Promise.resolve();
  private meta!: OutboxSession;
  session!: ReviewSessionDetail;
  status: CaptureStatus = { sampleRate: 48_000, elapsedSeconds: 0, peak: 0, capturedFrames: 0, lostFrames: 0, discontinuities: 0, cues: 0, upload: { pendingChunks: 0, pendingEvents: 0, uploadedChunks: 0, lastReceiptAt: null, retrying: false, blockedCode: null } };

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
    this.startedAt = performance.now();
    this.node.port.onmessage = (event: MessageEvent) => this.onWorklet(event.data);
    track.addEventListener("ended", () => void this.deviceLost());
    source.connect(this.node).connect(mute).connect(this.context.destination);
    this.context.onstatechange = () => {
      if (this.finished) return;
      if (this.context.state === "suspended") {
        const origin=this.captureOrigin??this.chunker.originFrame;
        const sample=origin===null?this.chunker.capturedEndFrame:Math.max(0,Math.round(this.context.currentTime*this.sampleRate)-origin);
        void this.event("captureDiscontinuity",sample,{cause:"audioContextSuspended",clockEpoch:this.clockEpoch});
        this.clockEpoch+=1; this.epochFirstSample=sample; this.epochCalibrated=false; this.probe=null;
      } else if (this.context.state === "running") this.requestCalibration();
    };
    if (this.context.state === "suspended") await this.context.resume();
    this.requestCalibration();
    this.probeTimer=setInterval(()=>this.requestCalibration(),30_000);
  }

  private publish(patch: Partial<CaptureStatus>) {
    if (this.meta?.eventStorageFailed) patch={...patch,upload:{...(patch.upload??this.status.upload),blockedCode:"review_session_event_storage_failed"}};
    this.status = { ...this.status, ...patch };
    this.onStatus(this.status);
  }

  private requestCalibration() {
    if (this.finished || this.context.state!=="running") return;
    if (this.probe && performance.now()-this.probe.sentMs<1000) return;
    this.probe={id:randomId("probe"),sentMs:performance.now()};
    this.node.port.postMessage({type:"clockProbe",probeId:this.probe.id});
  }

  private onWorklet(message: { type: string; frame?: number; firstFrame?: number | null; finalFrame?: number | null; probeId?: string; channels?: Float32Array[]; expectedFrame?: number; actualFrame?: number }) {
    if (message.type==="stopped") {
      const origin=this.chunker.originFrame??message.firstFrame??null;
      const end=origin===null?0:(message.finalFrame??origin)-origin;
      this.stopAck?.(Number.isSafeInteger(end)&&end===this.chunker.capturedEndFrame);
      return;
    }
    if (this.finished && message.type !== "audio") return;
    const now = performance.now();
    if (message.type==="clockSample" && this.probe && this.probe.id===message.probeId && message.frame!==undefined && message.firstFrame!=null) {
      this.captureOrigin=message.firstFrame;
      const firstSample=this.clockEpoch>0&&!this.epochCalibrated?message.frame-message.firstFrame:this.epochFirstSample;
      const calibration=captureCalibration({clockId:`mic_epoch_${this.clockEpoch}`,sampleRate:this.sampleRate,firstSample,absoluteFrame:message.frame,originFrame:message.firstFrame,epochMs:this.startedAt,sentMs:this.probe.sentMs,receivedMs:now});
      this.probe=null;
      if (calibration) {
        if (this.clockEpoch>0&&!this.epochCalibrated) void this.event("captureDiscontinuity",calibration.firstSample,{cause:"audioContextResumed",clockId:calibration.clockId});
        this.epochFirstSample=calibration.firstSample; this.epochCalibrated=true;
        void this.event("clockSample",calibration.sampleIndex,{...calibration});
      }
    } else if (message.type === "audio" && message.channels && message.frame !== undefined) {
      this.chunker.push(message.frame, message.channels, now);
    } else if (message.type === "discontinuity" && message.expectedFrame !== undefined && message.actualFrame !== undefined) {
      this.chunker.discontinuity(now);
      const origin = this.chunker.originFrame ?? message.expectedFrame;
      void this.event("captureDiscontinuity", message.expectedFrame - origin, { missingFrames: message.actualFrame - message.expectedFrame });
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
      await this.event("localBufferOverflow", chunk.startFrame, { lostFrames: chunk.frameCount, sequence: chunk.sequence, reason: error instanceof OutboxFullError ? "outboxFull" : "storageError" });
    }
  }

  private event(kind: ReviewSessionEventRequest["kind"], audioFrame: number, detail: Record<string, unknown>, declaredMovieSeconds: number | null = null): Promise<boolean> {
    const event: ReviewSessionEventRequest={eventId:randomId("evt"),kind,audioFrame:Math.max(0,Math.round(audioFrame)),performanceMs:performance.now(),declaredMovieSeconds,detail};
    const stored=this.eventWrites.then(async()=>{
      await this.outbox.putEvent({sessionId:this.session.sessionId,event});
      void this.uploader.kick();
      return true;
    }).catch(async()=>{
      this.meta={...this.meta,eventStorageFailed:true};
      await this.outbox.putSession(this.meta).catch(()=>undefined);
      this.publish({upload:{...this.status.upload,blockedCode:"review_session_event_storage_failed"}});
      return false;
    });
    this.eventWrites=stored.then(()=>undefined);
    return stored;
  }

  /** The reviewer's declaration that "now" is a known movie time (default: first frame). */
  async cue(declaredMovieSeconds = 0): Promise<void> {
    if (this.finished) return;
    const origin = this.captureOrigin??this.chunker.originFrame;
    if (origin===null) return;
    // currentTime is the rendering clock at the next quantum, closer to the
    // keypress than the last delivered batch.
    const frame = Math.round(this.context.currentTime * this.sampleRate) - origin;
    if (await this.event("manualSyncCue", frame, { method: "audioContextCurrentTime", clockId:`mic_epoch_${this.clockEpoch}`, outputLatencySeconds: (this.context as AudioContext & { outputLatency?: number }).outputLatency ?? null }, declaredMovieSeconds)) this.publish({cues:this.status.cues+1});
  }

  private async deviceLost() {
    if (this.finished) return;
    await this.event("deviceLost", this.chunker.capturedEndFrame, {});
    this.onEnded("deviceLost");
    await this.stop("deviceLost");
  }

  /**
   * Stop capture, persist the tail, and try to upload and finalize. Returns the
   * finalized session, or null when chunks remain queued for later recovery.
   */
  stop(reason: RecordingEndReason = "stopped", drainTimeoutMs = 30_000, flushTimeoutMs = 3_000): Promise<ReviewSessionDetail | null> {
    this.stopping??=this.stopInternal(reason,drainTimeoutMs,flushTimeoutMs);
    return this.stopping;
  }
  private async stopInternal(reason: RecordingEndReason, drainTimeoutMs: number, flushTimeoutMs: number): Promise<ReviewSessionDetail|null> {
    if (!this.finished) {
      this.finished = true;
      if (this.probeTimer!==null) clearInterval(this.probeTimer);
      const acknowledged=await new Promise<boolean>((resolve)=>{
        const timer=setTimeout(()=>{this.stopAck=null;resolve(false);},flushTimeoutMs);
        this.stopAck=(valid)=>{clearTimeout(timer);this.stopAck=null;resolve(valid);};
        this.node.port.postMessage({type:"stop"});
      });
      if (!acknowledged) {
        await this.event("pageInterrupted",this.chunker.capturedEndFrame,{captureTailStatus:"unknown",reason:"workletStopUnacknowledged",originalEndReason:reason});
        reason="interrupted";
      }
      this.chunker.flush(performance.now());
      await this.writes;
      await this.eventWrites;
      this.stream.getTracks().forEach((track) => track.stop());
      await this.context.close().catch(() => undefined);
      this.meta = { ...this.meta, capturedFrames: Math.max(this.meta.capturedFrames, this.chunker.capturedEndFrame), ended: true, endReason: reason, updatedAt: new Date().toISOString() };
      await this.outbox.putSession(this.meta);
    }
    return finalizeWhenDrained(this.client, this.outbox, this.meta, this.uploader, drainTimeoutMs);
  }

  /** Best-effort marker for an abrupt page exit; recovery finalizes as interrupted. */
  pageHidden() {
    if (!this.finished) void this.event("pageInterrupted", this.chunker.capturedEndFrame, { visibility: document.visibilityState });
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
    await outbox.putEvent({sessionId:meta.sessionId,event:{eventId:"recovery_tail",kind:"pageInterrupted",audioFrame:meta.capturedFrames,performanceMs:null,declaredMovieSeconds:null,detail:{captureTailStatus:"unknown",reason:"interruptedBeforeAcknowledgedStop"}}});
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
