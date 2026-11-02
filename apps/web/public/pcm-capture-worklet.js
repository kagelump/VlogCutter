// RCD VlogCutter TV voice review (#222): untouched microphone capture.
//
// Runs on the audio rendering thread. Copies each input quantum verbatim and
// tags it with `currentFrame`, the AudioContext's monotonic sample clock, so
// the main thread can place every sample on the recording clock and detect
// skipped frames instead of assuming continuity. No gain, filtering or
// resampling happens here. Quanta are batched to keep message traffic low.
const BATCH_FRAMES = 4096;

class PcmCaptureProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();
    this.channels = Math.max(1, Math.min(2, options?.processorOptions?.channels ?? 1));
    this.pending = [];
    this.pendingFrames = 0;
    this.batchStart = null;
    this.expected = null;
    this.stopped = false;
    this.firstFrame = null;
    this.probe = null;
    this.port.onmessage = (event) => {
      if (event.data?.type === "flush") this.flush();
      if (event.data?.type === "clockProbe") this.probe = event.data.probeId;
      if (event.data?.type === "stop") {
        // Deliver the partial batch, then acknowledge the frame after the last
        // one delivered. Same-port FIFO means the main thread sees every final
        // audio batch before this acknowledgement.
        this.flush();
        this.stopped = true;
        this.port.postMessage({
          type: "stopped",
          frame: this.expected,
          firstFrame: this.firstFrame,
          finalFrame: this.expected,
        });
      }
    };
  }

  flush() {
    if (!this.pendingFrames) return;
    const out = [];
    for (let c = 0; c < this.channels; c += 1) {
      const joined = new Float32Array(this.pendingFrames);
      let offset = 0;
      for (const block of this.pending) { joined.set(block[c], offset); offset += block[c].length; }
      out.push(joined);
    }
    this.port.postMessage({ type: "audio", frame: this.batchStart, frames: this.pendingFrames, channels: out }, out.map((c) => c.buffer));
    this.pending = [];
    this.pendingFrames = 0;
    this.batchStart = null;
  }

  process(inputs) {
    if (this.stopped) return false;
    const input = inputs[0];
    if (!input || input.length === 0 || input[0].length === 0) return true;
    const frame = currentFrame;
    const length = input[0].length;
    if (this.firstFrame === null) this.firstFrame = frame;
    if (this.probe !== null) {
      this.port.postMessage({ type: "clockSample", probeId: this.probe, frame, firstFrame: this.firstFrame });
      this.probe = null;
    }
    if (this.expected !== null && frame !== this.expected) {
      this.flush();
      this.port.postMessage({ type: "discontinuity", expectedFrame: this.expected, actualFrame: frame });
    }
    if (this.batchStart === null) this.batchStart = frame;
    const block = [];
    for (let c = 0; c < this.channels; c += 1) {
      // A mono source on a stereo node is duplicated by the graph; keep what it delivers.
      block.push(new Float32Array(input[Math.min(c, input.length - 1)]));
    }
    this.pending.push(block);
    this.pendingFrames += length;
    this.expected = frame + length;
    if (this.pendingFrames >= BATCH_FRAMES) this.flush();
    return true;
  }
}

registerProcessor("rcdvlog-pcm-capture", PcmCaptureProcessor);
