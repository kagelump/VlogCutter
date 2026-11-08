import { ProjectClientError, type BrowserProjectClient } from "@rcdvlog/project-client";
import { PcmChunker, chunkFramesFor, floatToInt16, sha256Hex, type PcmChunk } from "./chunker";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { MemoryOutbox, OutboxFullError, type OutboxEvent, type OutboxSession } from "./outbox";
import { ChunkUploader, canonicalJson, isPermanentUploadError, sameEvent } from "./uploader";
import { finalizeWhenDrained } from "./recorder";

function ramp(frames: number, offset = 0): Float32Array {
  return Float32Array.from({ length: frames }, (_, i) => ((i + offset) % 100) / 200);
}

test("float samples convert with symmetric clamping", () => {
  expect(floatToInt16(0)).toBe(0);
  expect(floatToInt16(1)).toBe(32767);
  expect(floatToInt16(-1)).toBe(-32768);
  expect(floatToInt16(4)).toBe(32767);
  expect(floatToInt16(Number.NaN)).toBe(0);
});

test("chunk size respects the server's 2 MiB bound", () => {
  expect(chunkFramesFor(48_000, 1)).toBe(96_000);
  expect(chunkFramesFor(96_000, 2, 10) * 4).toBeLessThanOrEqual(2 * 1024 * 1024);
});

test("frames are rebased to the first captured sample and chunked contiguously", () => {
  const chunks: PcmChunk[] = [];
  const chunker = new PcmChunker(1, 1000, (chunk) => chunks.push(chunk));
  chunker.push(5_000, [ramp(600)], 1);
  chunker.push(5_600, [ramp(600, 600)], 2);
  chunker.flush(3);
  expect(chunks.map((c) => [c.sequence, c.startFrame, c.frameCount])).toEqual([[0, 0, 1000], [1, 1000, 200]]);
  expect(chunks[0].bytes.byteLength).toBe(2000);
  expect(new DataView(chunks[0].bytes.buffer).getInt16(2, true)).toBe(floatToInt16(1 / 200));
  expect(chunker.capturedEndFrame).toBe(1200);
});

test("a skipped-frame discontinuity becomes a gap, never stitched audio", () => {
  const chunks: PcmChunk[] = [];
  const chunker = new PcmChunker(1, 1000, (chunk) => chunks.push(chunk));
  chunker.push(0, [ramp(300)], 1);
  chunker.discontinuity(2);
  chunker.push(800, [ramp(300)], 3);
  chunker.flush(4);
  expect(chunks.map((c) => [c.startFrame, c.frameCount])).toEqual([[0, 300], [800, 300]]);
});

test("stereo is interleaved", () => {
  const chunks: PcmChunk[] = [];
  const chunker = new PcmChunker(2, 4, (chunk) => chunks.push(chunk));
  chunker.push(0, [Float32Array.of(0.5, 0.5, 0.5, 0.5), Float32Array.of(-0.5, -0.5, -0.5, -0.5)], 1);
  const view = new DataView(chunks[0].bytes.buffer);
  expect([view.getInt16(0, true), view.getInt16(2, true)]).toEqual([floatToInt16(0.5), floatToInt16(-0.5)]);
});

test("sha256 matches a known vector", async () => {
  expect(await sha256Hex(new TextEncoder().encode("abc"))).toBe("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
});

test("the outbox is bounded and refuses rather than evicting", async () => {
  const outbox = new MemoryOutbox(10);
  await outbox.putChunk({ sessionId: "s", sequence: 0, startFrame: 0, frameCount: 4, sha256: "x", captureEndMs: 0, bytes: new Uint8Array(8) });
  await expect(outbox.putChunk({ sessionId: "s", sequence: 1, startFrame: 4, frameCount: 4, sha256: "y", captureEndMs: 0, bytes: new Uint8Array(8) })).rejects.toBeInstanceOf(OutboxFullError);
  expect(await outbox.pendingBytes()).toBe(8);
});

function uploadClient(put: BrowserProjectClient["putReviewSessionChunk"]): BrowserProjectClient {
  return {
    putReviewSessionChunk: put,
    recordReviewSessionEvent: vi.fn(async (_p: string, _s: string, event: { eventId: string; kind: string; audioFrame: number; performanceMs?: number | null; declaredMovieSeconds?: number | null; detail?: Record<string, unknown> | null }) => ({
      ...event,
      audioSeconds: event.audioFrame / 48_000,
      performanceMs: event.performanceMs ?? null,
      declaredMovieSeconds: event.declaredMovieSeconds ?? null,
      detail: event.detail ?? null,
      serverReceivedAt: "t",
    })),
    finalizeReviewSession: vi.fn().mockResolvedValue({ sessionId: "s", state: "completed" }),
    getReviewSession: vi.fn(),
  } as unknown as BrowserProjectClient;
}

async function seed(outbox: MemoryOutbox, count: number) {
  for (let sequence = 0; sequence < count; sequence += 1) {
    const bytes = new Uint8Array(4).fill(sequence);
    await outbox.putChunk({ sessionId: "s", sequence, startFrame: sequence * 2, frameCount: 2, sha256: await sha256Hex(bytes), captureEndMs: sequence, bytes });
  }
}

test("transient upload failures retry with backoff and keep chunks until receipted", async () => {
  const outbox = new MemoryOutbox();
  await seed(outbox, 2);
  let calls = 0;
  const put = vi.fn(async (_p: string, _s: string, chunk: { sequence: number; sha256: string }) => {
    calls += 1;
    if (calls === 1) throw new ProjectClientError("server_unavailable", "offline", "server_unavailable");
    if (calls === 2) throw new ProjectClientError("server_error", "busy", "review_session_busy", null, 503);
    return { sessionId: "s", sequence: chunk.sequence, startFrame: (chunk as { startFrame?: number }).startFrame ?? 0, frameCount: (chunk as { frameCount?: number }).frameCount ?? 2, sha256: chunk.sha256, receivedAt: "t", duplicate: calls === 4 };
  });
  const sleeps: number[] = [];
  const uploader = new ChunkUploader(uploadClient(put as never), outbox, "p", "s", undefined, async (ms) => { sleeps.push(ms); });
  await uploader.kick();
  expect(sleeps).toEqual([500, 1000]);
  expect(await outbox.chunks("s")).toEqual([]);
  expect(uploader.status.uploadedChunks).toBe(2);
});

test("a permanent refusal stops the drain and keeps the local copy", async () => {
  const outbox = new MemoryOutbox();
  await seed(outbox, 2);
  const put = vi.fn().mockRejectedValue(new ProjectClientError("server_error", "conflict", "review_session_chunk_conflict", null, 409));
  const uploader = new ChunkUploader(uploadClient(put), outbox, "p", "s", undefined, async () => undefined);
  await uploader.kick();
  expect(uploader.status.blockedCode).toBe("review_session_chunk_conflict");
  expect((await outbox.chunks("s")).length).toBe(2);
  expect(put).toHaveBeenCalledTimes(1);
  expect(isPermanentUploadError(new ProjectClientError("session_expired", "x", "session_invalid"))).toBe(true);
});

test("recovery uploads a closed tab's buffered audio and finalizes it as interrupted", async () => {
  const outbox = new MemoryOutbox();
  const meta: OutboxSession = { sessionId: "s", projectId: "p", reviewTargetId: "t", capturedFrames: 6, ended: false, endReason: null, updatedAt: "now" };
  await outbox.putSession(meta);
  await seed(outbox, 3);
  const put = vi.fn(async (_p: string, _s: string, chunk: { sequence: number; startFrame: number; frameCount: number; sha256: string }) => ({ sessionId: "s", sequence: chunk.sequence, startFrame: chunk.startFrame, frameCount: chunk.frameCount, sha256: chunk.sha256, receivedAt: "t", duplicate: false }));
  const client = uploadClient(put as never);
  const result = await finalizeWhenDrained(client, outbox, meta);
  expect(result?.state).toBe("completed");
  expect(client.finalizeReviewSession).toHaveBeenCalledWith("p", "s", { finalFrameCount: 6, endReason: "interrupted" });
  expect(await outbox.sessions()).toEqual([]);
});

test("recovery never finalizes while audio is still buffered", async () => {
  const outbox = new MemoryOutbox();
  const meta: OutboxSession = { sessionId: "s", projectId: "p", reviewTargetId: "t", capturedFrames: 2, ended: true, endReason: "stopped", updatedAt: "now" };
  await outbox.putSession(meta);
  await seed(outbox, 1);
  const client = uploadClient(vi.fn().mockRejectedValue(new ProjectClientError("server_error", "x", "review_session_chunk_corrupt", null, 400)));
  expect(await finalizeWhenDrained(client, outbox, meta)).toBeNull();
  expect(client.finalizeReviewSession).not.toHaveBeenCalled();
  expect((await outbox.sessions()).length).toBe(1);
});

function cueEvent(eventId: string, order: number): OutboxEvent {
  return { sessionId: "s", eventId, order, kind: "manualSyncCue", audioFrame: 4800, performanceMs: 1500, declaredMovieSeconds: 0, detail: { clockEpoch: 0 } };
}

test("queued events are retried with their stable id until the server acknowledges them", async () => {
  const outbox = new MemoryOutbox();
  await outbox.putEvent(cueEvent("evt_b", 1));
  await outbox.putEvent(cueEvent("evt_a", 0));
  const posted: string[] = [];
  let calls = 0;
  const record = vi.fn(async (_p: string, _s: string, event: { eventId: string }) => {
    calls += 1;
    if (calls === 1) throw new ProjectClientError("server_unavailable", "offline", "server_unavailable");
    posted.push(event.eventId);
    return event;
  });
  const client = { ...uploadClient(vi.fn()), recordReviewSessionEvent: record } as unknown as BrowserProjectClient;
  const sleeps: number[] = [];
  const uploader = new ChunkUploader(client, outbox, "p", "s", undefined, async (ms) => { sleeps.push(ms); });
  await uploader.kick();
  // The failed post kept the event; the retry reused the same id, in creation order.
  expect(posted).toEqual(["evt_a", "evt_b"]);
  expect(record.mock.calls[0][2]).toMatchObject({ eventId: "evt_a", declaredMovieSeconds: 0, performanceMs: 1500, detail: { clockEpoch: 0 } });
  expect(sleeps).toEqual([500]);
  expect(await outbox.events("s")).toEqual([]);
  expect(uploader.status.pendingEvents).toBe(0);
});

test("a session is never finalized while an event is still queued", async () => {
  const outbox = new MemoryOutbox();
  const meta: OutboxSession = { sessionId: "s", projectId: "p", reviewTargetId: "t", capturedFrames: 0, ended: true, endReason: "stopped", updatedAt: "now" };
  await outbox.putSession(meta);
  await outbox.putEvent(cueEvent("evt_conflict", 0));
  const client = { ...uploadClient(vi.fn()), recordReviewSessionEvent: vi.fn().mockRejectedValue(new ProjectClientError("server_error", "x", "idempotency_conflict", null, 409)) } as unknown as BrowserProjectClient;
  expect(await finalizeWhenDrained(client, outbox, meta)).toBeNull();
  expect(client.finalizeReviewSession).not.toHaveBeenCalled();
  expect((await outbox.events("s")).length).toBe(1);
  await outbox.deleteSession("s");
  expect(await outbox.events("s")).toEqual([]);
});

test("the capture worklet delivers its partial batch before acknowledging the final frame", () => {
  const source = readFileSync(resolve(__dirname, "../../../../public/pcm-capture-worklet.js"), "utf8");
  let Processor: new (options: unknown) => { port: { onmessage: (e: { data: unknown }) => void }; process: (inputs: Float32Array[][]) => boolean } = null as never;
  const sent: Array<{ type: string; frame?: number; frames?: number }> = [];
  class FakeProcessor { port = { onmessage: (_e: { data: unknown }) => undefined, postMessage: (m: { type: string }) => sent.push(m) }; }
  const scope = { AudioWorkletProcessor: FakeProcessor, registerProcessor: (_n: string, c: typeof Processor) => { Processor = c; }, currentFrame: 1000 };
  new Function("AudioWorkletProcessor", "registerProcessor", "scope", source.replace(/currentFrame/g, "scope.currentFrame"))(scope.AudioWorkletProcessor, scope.registerProcessor, scope);
  const node = new Processor({ processorOptions: { channels: 1 } });
  node.process([[new Float32Array(128)]]);
  scope.currentFrame = 1128;
  node.process([[new Float32Array(128)]]);
  node.port.onmessage({ data: { type: "stop" } });
  expect(sent.map((m) => m.type)).toEqual(["audio", "stopped"]);
  expect(sent[0]).toMatchObject({ frame: 1000, frames: 256 });
  expect(sent[1]).toMatchObject({ frame: 1256, firstFrame: 1000, finalFrame: 1256 });
  expect(node.process([[new Float32Array(128)]])).toBe(false);
});


test("a receipt that does not match a queued event stops the drain and keeps the event", async () => {
  const outbox = new MemoryOutbox();
  const event = cueEvent("evt_mismatch", 0);
  await outbox.putEvent(event);
  const record = vi.fn(async () => ({
    eventId: event.eventId,
    kind: event.kind,
    audioFrame: event.audioFrame + 1,
    audioSeconds: 0,
    performanceMs: event.performanceMs,
    declaredMovieSeconds: event.declaredMovieSeconds,
    detail: event.detail,
    serverReceivedAt: "t",
  }));
  const client = { ...uploadClient(vi.fn()), recordReviewSessionEvent: record } as unknown as BrowserProjectClient;
  const uploader = new ChunkUploader(client, outbox, "p", "s");
  await uploader.kick();
  expect(uploader.status.blockedCode).toBe("review_session_receipt_mismatch");
  expect(await outbox.events("s")).toHaveLength(1);
});

test("a chunk receipt that is not the exact upload stops the drain and keeps the chunk", async () => {
  const outbox = new MemoryOutbox();
  await seed(outbox, 1);
  const put = vi.fn(async (_p: string, _s: string, chunk: { sequence: number; sha256: string }) => ({
    sessionId: "s", sequence: chunk.sequence, startFrame: 0, frameCount: 2,
    sha256: "not-the-same", receivedAt: "t", duplicate: false,
  }));
  const uploader = new ChunkUploader(uploadClient(put as never), outbox, "p", "s");
  await uploader.kick();
  expect(uploader.status.blockedCode).toBe("review_session_receipt_mismatch");
  expect(await outbox.chunks("s")).toHaveLength(1);
});

test("cue and calibration events survive lost responses and reload recovery before finalize", async () => {
  const outbox = new MemoryOutbox();
  const meta: OutboxSession = { sessionId: "s", projectId: "p", reviewTargetId: "t", capturedFrames: 2, ended: true, endReason: "stopped", updatedAt: "now" };
  await outbox.putSession(meta);
  await seed(outbox, 1);
  const event: OutboxEvent = { sessionId: "s", eventId: "stable-cue", order: 0, kind: "manualSyncCue", audioFrame: 1, performanceMs: 10, declaredMovieSeconds: 0, detail: { clockEpoch: 0 } };
  await outbox.putEvent(event);
  const receipt = { eventId: event.eventId, kind: event.kind, audioFrame: event.audioFrame, audioSeconds: 1 / 48000, performanceMs: event.performanceMs, declaredMovieSeconds: event.declaredMovieSeconds, detail: event.detail, serverReceivedAt: "t" };
  const post = vi.fn()
    .mockRejectedValueOnce(new ProjectClientError("server_unavailable", "lost committed response", "server_unavailable"))
    .mockResolvedValue(receipt);
  const put = vi.fn(async (_p: string, _s: string, chunk: { sequence: number; sha256: string }) => ({ sessionId: "s", sequence: chunk.sequence, startFrame: 0, frameCount: 2, sha256: chunk.sha256, receivedAt: "t", duplicate: false }));
  const client = { ...uploadClient(put as never), recordReviewSessionEvent: post } as unknown as BrowserProjectClient;
  const uploader = new ChunkUploader(client, outbox, "p", "s", undefined, async () => undefined);
  await uploader.kick();
  expect(post).toHaveBeenCalledTimes(2);
  expect(post.mock.calls[0][2]).toEqual(post.mock.calls[1][2]);
  expect(await outbox.events("s")).toEqual([]);
  expect(post.mock.invocationCallOrder[1]).toBeLessThan(put.mock.invocationCallOrder[0]);
  expect(await finalizeWhenDrained(client, outbox, meta)).not.toBeNull();
  expect(client.finalizeReviewSession).toHaveBeenCalledWith("p", "s", { finalFrameCount: 2, endReason: "stopped" });
});

test("failed event storage cannot be finalized as a complete capture", async () => {
  const outbox = new MemoryOutbox();
  const meta: OutboxSession = { sessionId: "s", projectId: "p", reviewTargetId: "t", capturedFrames: 0, ended: true, endReason: "stopped", eventStorageFailed: true, updatedAt: "now" };
  await outbox.putSession(meta);
  const client = uploadClient(vi.fn());
  expect(await finalizeWhenDrained(client, outbox, meta)).toBeNull();
  expect(client.finalizeReviewSession).not.toHaveBeenCalled();
  expect((await outbox.sessions()).length).toBe(1);
});

test("an event receipt with the server's sorted detail keys is the same event, a changed value is not", () => {
  const event: OutboxEvent = { sessionId: "s", eventId: "evt_cal", order: 0, kind: "clockSample", audioFrame: 48000, performanceMs: 1000, declaredMovieSeconds: null,
    detail: { schema: "review-capture-calibration.v1", clockId: "mic_epoch_0", sampleRate: 48000, nested: { z: 1, a: [2, { y: 3, b: 4 }] }, clockEpoch: 0 } };
  // serde_json without preserve_order returns object keys sorted.
  const sorted = JSON.parse(JSON.stringify({ clockEpoch: 0, clockId: "mic_epoch_0", nested: { a: [2, { b: 4, y: 3 }], z: 1 }, sampleRate: 48000, schema: "review-capture-calibration.v1" }));
  const receipt = { eventId: "evt_cal", kind: "clockSample" as const, audioFrame: 48000, audioSeconds: 1, performanceMs: 1000, declaredMovieSeconds: null, serverReceivedAt: "t", detail: sorted };
  expect(sameEvent(event, receipt)).toBe(true);
  expect(sameEvent(event, { ...receipt, detail: { ...sorted, clockEpoch: 1 } })).toBe(false);
  expect(canonicalJson({ b: 1, a: undefined })).toBe(canonicalJson({ b: 1 }));
});
