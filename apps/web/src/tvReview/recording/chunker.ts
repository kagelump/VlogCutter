// Pure PCM chunking on the microphone's sample clock. No timers, no I/O.
//
// Absolute AudioContext frames are rebased so the first captured frame is
// recording frame 0. A reported discontinuity closes the current chunk and the
// next chunk starts at the true later frame, so the server sees a gap rather
// than silently stitched audio.

export interface PcmChunk {
  sequence: number;
  startFrame: number;
  frameCount: number;
  bytes: Uint8Array;
  captureEndMs: number;
}

export const MAX_CHUNK_BYTES = 2 * 1024 * 1024;

export function chunkFramesFor(sampleRate: number, channels: number, seconds = 2): number {
  const byLimit = Math.floor((MAX_CHUNK_BYTES - 1024) / (2 * channels));
  return Math.max(128, Math.min(Math.round(sampleRate * seconds), byLimit));
}

/** Float [-1, 1] to 16-bit PCM with symmetric clamping. */
export function floatToInt16(sample: number): number {
  const clamped = Math.max(-1, Math.min(1, Number.isFinite(sample) ? sample : 0));
  return clamped < 0 ? Math.round(clamped * 0x8000) : Math.round(clamped * 0x7fff);
}

export class PcmChunker {
  private origin: number | null = null;
  private sequence = 0;
  private buffer: Int16Array;
  private filled = 0;
  private chunkStart = 0;
  private nextFrame = 0;
  peak = 0;

  constructor(
    private readonly channels: number,
    private readonly chunkFrames: number,
    private readonly emit: (chunk: PcmChunk) => void,
  ) {
    this.buffer = new Int16Array(chunkFrames * channels);
  }

  /** Recording-relative frame just after the last captured sample. */
  get capturedEndFrame(): number { return this.nextFrame; }
  get nextSequence(): number { return this.sequence; }
  get originFrame(): number | null { return this.origin; }

  push(absoluteFrame: number, data: Float32Array[], nowMs: number): void {
    const frames = data[0]?.length ?? 0;
    if (!frames) return;
    if (this.origin === null) this.origin = absoluteFrame;
    const relative = absoluteFrame - this.origin;
    if (relative !== this.nextFrame) {
      // Either the worklet skipped frames or a caller pushed out of order.
      this.flush(nowMs);
      this.nextFrame = relative;
    }
    if (this.filled === 0) this.chunkStart = this.nextFrame;
    let peak = 0;
    for (let i = 0; i < frames; i += 1) {
      for (let c = 0; c < this.channels; c += 1) {
        const value = data[Math.min(c, data.length - 1)][i];
        const magnitude = Math.abs(value);
        if (magnitude > peak) peak = magnitude;
        this.buffer[this.filled * this.channels + c] = floatToInt16(value);
      }
      this.filled += 1;
      this.nextFrame += 1;
      if (this.filled === this.chunkFrames) this.flush(nowMs);
    }
    this.peak = Math.min(1, peak);
  }

  /** The worklet reports that frames were skipped between two quanta. */
  discontinuity(nowMs: number): void {
    this.flush(nowMs);
  }

  flush(nowMs: number): void {
    if (!this.filled) return;
    const samples = this.buffer.slice(0, this.filled * this.channels);
    const bytes = new Uint8Array(samples.length * 2);
    const view = new DataView(bytes.buffer);
    for (let i = 0; i < samples.length; i += 1) view.setInt16(i * 2, samples[i], true);
    this.emit({ sequence: this.sequence, startFrame: this.chunkStart, frameCount: this.filled, bytes, captureEndMs: nowMs });
    this.sequence += 1;
    this.filled = 0;
    this.chunkStart = this.nextFrame;
  }
}

export async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", bytes as unknown as ArrayBuffer);
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("");
}
