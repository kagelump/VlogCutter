// Drains one session's durable outbox (queued capture events, then audio
// chunks in sequence order) to the server. Transient failures back off and
// retry; the local row stays until the server returns a matching receipt.
// Permanent refusals stop the drain and are surfaced, never retried blindly.
import { ProjectClientError, type BrowserProjectClient } from "@rcdvlog/project-client";
import type { ChunkOutbox, OutboxEvent } from "./outbox";

export interface UploadStatus {
  pendingChunks: number;
  pendingEvents: number;
  uploadedChunks: number;
  lastReceiptAt: string | null;
  retrying: boolean;
  /** Stable server code that stopped the drain, if any. */
  blockedCode: string | null;
}

const PERMANENT = new Set([
  "idempotency_conflict",
  "review_session_receipt_mismatch",
  "review_session_chunk_conflict",
  "review_session_chunk_corrupt",
  "review_session_limit_exceeded",
  "review_session_not_recording",
  "not_found",
  "project_replica_not_found",
  "invalid_input",
  "invalid_query",
]);

export function isPermanentUploadError(error: unknown): boolean {
  if (!(error instanceof ProjectClientError)) return false;
  if (error.kind === "session_expired" || error.kind === "csrf_failure" || error.kind === "credential_revoked") return true;
  return PERMANENT.has(error.code);
}

function eventRequest(event: OutboxEvent) {
  return {
    eventId: event.eventId,
    kind: event.kind,
    audioFrame: event.audioFrame,
    performanceMs: event.performanceMs,
    declaredMovieSeconds: event.declaredMovieSeconds,
    detail: event.detail,
  };
}

/**
 * Key-order-independent JSON for comparing a stored detail with its receipt:
 * the server re-serializes `detail` with sorted object keys, so insertion
 * order is not part of an event's identity.
 */
export function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (value && typeof value === "object") {
    const record = value as Record<string, unknown>;
    return `{${Object.keys(record).filter((key) => record[key] !== undefined).sort().map((key) => `${JSON.stringify(key)}:${canonicalJson(record[key])}`).join(",")}}`;
  }
  return JSON.stringify(value ?? null);
}

export function sameEvent(left: OutboxEvent, receipt: Awaited<ReturnType<BrowserProjectClient["recordReviewSessionEvent"]>>): boolean {
  return receipt.eventId === left.eventId
    && receipt.kind === left.kind
    && receipt.audioFrame === left.audioFrame
    && receipt.performanceMs === (left.performanceMs ?? null)
    && receipt.declaredMovieSeconds === (left.declaredMovieSeconds ?? null)
    && canonicalJson(receipt.detail ?? null) === canonicalJson(left.detail ?? null);
}

export class ChunkUploader {
  private running: Promise<void> | null = null;
  private stopped = false;
  status: UploadStatus = { pendingChunks: 0, pendingEvents: 0, uploadedChunks: 0, lastReceiptAt: null, retrying: false, blockedCode: null };

  constructor(
    private readonly client: BrowserProjectClient,
    private readonly outbox: ChunkOutbox,
    private readonly projectId: string,
    private readonly sessionId: string,
    private readonly onStatus: (status: UploadStatus) => void = () => undefined,
    private readonly sleep: (ms: number) => Promise<void> = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
  ) {}

  /** Start a drain if none is running; resolves when the outbox is empty or blocked. */
  kick(): Promise<void> {
    this.running ??= this.drain().finally(() => { this.running = null; });
    return this.running;
  }

  stop() { this.stopped = true; }

  private update(patch: Partial<UploadStatus>) {
    this.status = { ...this.status, ...patch };
    this.onStatus(this.status);
  }

  private async drain(): Promise<void> {
    let delay = 500;
    for (;;) {
      if (this.stopped || this.status.blockedCode) return;
      const events = await this.outbox.events(this.sessionId);
      const pending = await this.outbox.chunks(this.sessionId);
      this.update({ pendingChunks: pending.length, pendingEvents: events.length });
      if (!pending.length && !events.length) { this.update({ retrying: false }); return; }
      try {
        if (events.length) {
          const event = events[0];
          const receipt = await this.client.recordReviewSessionEvent(this.projectId, this.sessionId, eventRequest(event));
          if (!sameEvent(event, receipt)) {
            this.update({ blockedCode: "review_session_receipt_mismatch", retrying: false });
            return;
          }
          await this.outbox.deleteEvent(this.sessionId, event.eventId);
          delay = 500;
          this.update({ pendingEvents: events.length - 1, retrying: false });
          continue;
        }
        const chunk = pending[0];
        const receipt = await this.client.putReviewSessionChunk(this.projectId, this.sessionId, {
          sequence: chunk.sequence,
          startFrame: chunk.startFrame,
          frameCount: chunk.frameCount,
          sha256: chunk.sha256,
          captureEndMs: chunk.captureEndMs,
        }, chunk.bytes);
        if (receipt.sha256 !== chunk.sha256
          || receipt.sequence !== chunk.sequence
          || receipt.startFrame !== chunk.startFrame
          || receipt.frameCount !== chunk.frameCount) {
          this.update({ blockedCode: "review_session_receipt_mismatch", retrying: false });
          return;
        }
        await this.outbox.deleteChunk(this.sessionId, chunk.sequence);
        delay = 500;
        this.update({ uploadedChunks: this.status.uploadedChunks + 1, lastReceiptAt: receipt.receivedAt, retrying: false, pendingChunks: pending.length - 1 });
      } catch (error) {
        if (isPermanentUploadError(error)) {
          this.update({ blockedCode: error instanceof ProjectClientError ? error.code : "unexpected_error", retrying: false });
          return;
        }
        this.update({ retrying: true });
        await this.sleep(delay);
        delay = Math.min(delay * 2, 10_000);
      }
    }
  }
}
