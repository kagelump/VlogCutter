// Drains one session's durable outbox to the server in sequence order.
// Transient failures back off and retry; the chunk stays on disk until the
// server returns a receipt. Permanent refusals stop the drain and are surfaced,
// never retried blindly. Identical retries are safe: the server treats them as
// duplicates and returns the original receipt.
import { ProjectClientError, type BrowserProjectClient } from "@rcdvlog/project-client";
import type { ChunkOutbox } from "./outbox";

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
  "review_session_chunk_conflict",
  "review_session_chunk_corrupt",
  "review_session_limit_exceeded",
  "review_session_not_recording",
  "idempotency_conflict",
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
      const pending = await this.outbox.chunks(this.sessionId);
      const events = await this.outbox.events(this.sessionId);
      this.update({ pendingChunks: pending.length, pendingEvents:events.length });
      if (events.length) {
        const event = events[0].event;
        try {
          const receipt = await this.client.recordReviewSessionEvent(this.projectId,this.sessionId,event);
          if (receipt.eventId!==event.eventId || receipt.kind!==event.kind || receipt.audioFrame!==event.audioFrame || receipt.declaredMovieSeconds!==(event.declaredMovieSeconds??null)) {
            this.update({blockedCode:"review_session_receipt_mismatch"}); return;
          }
          await this.outbox.deleteEvent(this.sessionId,event.eventId);
          this.update({pendingEvents:events.length-1,retrying:false});
          delay=500;
        } catch (error) {
          if (isPermanentUploadError(error)) { this.update({blockedCode:error instanceof ProjectClientError?error.code:"unexpected_error",retrying:false}); return; }
          this.update({retrying:true}); await this.sleep(delay); delay=Math.min(delay*2,10_000);
        }
        continue;
      }
      if (!pending.length) { this.update({ retrying: false }); return; }
      const chunk = pending[0];
      try {
        const receipt = await this.client.putReviewSessionChunk(this.projectId, this.sessionId, {
          sequence: chunk.sequence,
          startFrame: chunk.startFrame,
          frameCount: chunk.frameCount,
          sha256: chunk.sha256,
          captureEndMs: chunk.captureEndMs,
        }, chunk.bytes);
        if (receipt.sha256 !== chunk.sha256 || receipt.sequence !== chunk.sequence) {
          this.update({ blockedCode: "review_session_receipt_mismatch" });
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
