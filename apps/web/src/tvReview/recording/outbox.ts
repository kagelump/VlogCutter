// Durable local buffer between capture and upload. Every chunk is written here
// before any upload attempt, so a network drop, tab close or restart leaves it
// recoverable. Bounded: when the cap or the browser quota is reached, `put`
// throws `OutboxFullError` and the caller records the lost frames as a gap.

export interface OutboxChunk {
  sessionId: string;
  sequence: number;
  startFrame: number;
  frameCount: number;
  sha256: string;
  captureEndMs: number;
  bytes: Uint8Array;
}

export interface OutboxSession {
  sessionId: string;
  projectId: string;
  reviewTargetId: string;
  /** Recording-relative end of the last chunk produced (stored or lost). */
  capturedFrames: number;
  ended: boolean;
  endReason: "stopped" | "deviceLost" | "interrupted" | "cancelled" | null;
  updatedAt: string;
}

export interface ChunkOutbox {
  putChunk(chunk: OutboxChunk): Promise<void>;
  chunks(sessionId: string): Promise<OutboxChunk[]>;
  deleteChunk(sessionId: string, sequence: number): Promise<void>;
  putSession(session: OutboxSession): Promise<void>;
  sessions(): Promise<OutboxSession[]>;
  deleteSession(sessionId: string): Promise<void>;
  pendingBytes(): Promise<number>;
}

export class OutboxFullError extends Error {
  constructor() { super("The local recording buffer is full."); this.name = "OutboxFullError"; }
}

export const OUTBOX_MAX_BYTES = 256 * 1024 * 1024;

export class MemoryOutbox implements ChunkOutbox {
  private readonly chunkMap = new Map<string, OutboxChunk>();
  private readonly sessionMap = new Map<string, OutboxSession>();
  constructor(private readonly maxBytes = OUTBOX_MAX_BYTES) {}
  async putChunk(chunk: OutboxChunk) {
    if ((await this.pendingBytes()) + chunk.bytes.byteLength > this.maxBytes) throw new OutboxFullError();
    this.chunkMap.set(`${chunk.sessionId}:${chunk.sequence}`, chunk);
  }
  async chunks(sessionId: string) { return [...this.chunkMap.values()].filter((c) => c.sessionId === sessionId).sort((a, b) => a.sequence - b.sequence); }
  async deleteChunk(sessionId: string, sequence: number) { this.chunkMap.delete(`${sessionId}:${sequence}`); }
  async putSession(session: OutboxSession) { this.sessionMap.set(session.sessionId, { ...session }); }
  async sessions() { return [...this.sessionMap.values()]; }
  async deleteSession(sessionId: string) { this.sessionMap.delete(sessionId); }
  async pendingBytes() { let total = 0; for (const c of this.chunkMap.values()) total += c.bytes.byteLength; return total; }
}

const DB_NAME = "rcdvlog-voice-review";

function promisify<T>(request: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

function isQuotaError(error: unknown): boolean {
  return error instanceof DOMException && (error.name === "QuotaExceededError" || error.name === "NS_ERROR_DOM_QUOTA_REACHED");
}

/**
 * Chunk sizes are tracked per session in the small `sessions` store, updated in
 * the same transaction as the chunk, so the cap check never reads audio back.
 */
export class IndexedDbOutbox implements ChunkOutbox {
  private db: Promise<IDBDatabase> | null = null;
  constructor(private readonly maxBytes = OUTBOX_MAX_BYTES) {}

  private open(): Promise<IDBDatabase> {
    this.db ??= new Promise((resolve, reject) => {
      const request = indexedDB.open(DB_NAME, 1);
      request.onupgradeneeded = () => {
        const db = request.result;
        const chunks = db.createObjectStore("chunks", { keyPath: ["sessionId", "sequence"] });
        chunks.createIndex("bySession", "sessionId");
        db.createObjectStore("sessions", { keyPath: "sessionId" });
      };
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    return this.db;
  }

  private async transaction(mode: IDBTransactionMode) {
    const tx = (await this.open()).transaction(["chunks", "sessions"], mode);
    const done = new Promise<void>((resolve, reject) => {
      tx.oncomplete = () => resolve();
      tx.onabort = () => reject(tx.error);
      tx.onerror = () => reject(tx.error);
    });
    return { chunks: tx.objectStore("chunks"), sessions: tx.objectStore("sessions"), done };
  }

  async putChunk(chunk: OutboxChunk) {
    try {
      const tx = await this.transaction("readwrite");
      const all = await promisify(tx.sessions.getAll()) as StoredSession[];
      const total = all.reduce((sum, session) => sum + (session.pendingBytes ?? 0), 0);
      if (total + chunk.bytes.byteLength > this.maxBytes) {
        tx.chunks.transaction.abort();
        await tx.done.catch(() => undefined);
        throw new OutboxFullError();
      }
      const session = all.find((s) => s.sessionId === chunk.sessionId);
      if (!session) throw new Error("outbox session must be registered before its chunks");
      const bytes = chunk.bytes.buffer.slice(chunk.bytes.byteOffset, chunk.bytes.byteOffset + chunk.bytes.byteLength);
      tx.chunks.put({ ...chunk, bytes });
      tx.sessions.put({ ...session, pendingBytes: (session.pendingBytes ?? 0) + chunk.bytes.byteLength });
      await tx.done;
    } catch (error) {
      if (isQuotaError(error)) throw new OutboxFullError();
      throw error;
    }
  }

  async chunks(sessionId: string) {
    const tx = await this.transaction("readonly");
    const rows = await promisify(tx.chunks.index("bySession").getAll(IDBKeyRange.only(sessionId))) as Array<OutboxChunk & { bytes: ArrayBuffer }>;
    return rows.map((row) => ({ ...row, bytes: new Uint8Array(row.bytes) })).sort((a, b) => a.sequence - b.sequence);
  }

  async deleteChunk(sessionId: string, sequence: number) {
    const tx = await this.transaction("readwrite");
    const row = await promisify(tx.chunks.get([sessionId, sequence])) as { bytes: ArrayBuffer } | undefined;
    const session = await promisify(tx.sessions.get(sessionId)) as StoredSession | undefined;
    tx.chunks.delete([sessionId, sequence]);
    if (row && session) tx.sessions.put({ ...session, pendingBytes: Math.max(0, (session.pendingBytes ?? 0) - row.bytes.byteLength) });
    await tx.done;
  }

  async putSession(session: OutboxSession) {
    const tx = await this.transaction("readwrite");
    const existing = await promisify(tx.sessions.get(session.sessionId)) as StoredSession | undefined;
    tx.sessions.put({ ...session, pendingBytes: existing?.pendingBytes ?? 0 });
    await tx.done;
  }

  async sessions() {
    const tx = await this.transaction("readonly");
    const rows = await promisify(tx.sessions.getAll()) as StoredSession[];
    return rows.map(({ pendingBytes: _ignored, ...session }) => session);
  }

  async deleteSession(sessionId: string) {
    const tx = await this.transaction("readwrite");
    tx.sessions.delete(sessionId);
    tx.chunks.index("bySession").openKeyCursor(IDBKeyRange.only(sessionId)).onsuccess = (event) => {
      const cursor = (event.target as IDBRequest<IDBCursor | null>).result;
      if (cursor) { tx.chunks.delete(cursor.primaryKey); cursor.continue(); }
    };
    await tx.done;
  }

  async pendingBytes() {
    const tx = await this.transaction("readonly");
    const rows = await promisify(tx.sessions.getAll()) as StoredSession[];
    return rows.reduce((sum, session) => sum + (session.pendingBytes ?? 0), 0);
  }
}

type StoredSession = OutboxSession & { pendingBytes?: number };

export function defaultOutbox(): ChunkOutbox {
  return typeof indexedDB === "undefined" ? new MemoryOutbox() : new IndexedDbOutbox();
}
