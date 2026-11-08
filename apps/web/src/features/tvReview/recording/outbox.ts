// Durable local buffer between capture and upload. Every chunk and capture
// event is written here before any upload attempt, so a network drop, tab close
// or restart leaves it recoverable. Bounded: when the byte cap, event cap or
// browser quota is reached, `put` throws `OutboxFullError` and the caller
// records the loss as a gap instead of evicting evidence.

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
  /** Evidence storage failed; recovery must not silently certify completion. */
  eventStorageFailed?: boolean;
  endReason: "stopped" | "deviceLost" | "interrupted" | "cancelled" | null;
  updatedAt: string;
}

/**
 * A capture event (cue, discontinuity, clock sample...) queued with its stable
 * id before any post, so a failed or interrupted post is retried with the
 * audio rather than lost. The server replays an identical id idempotently.
 */
export interface OutboxEvent {
  sessionId: string;
  eventId: string;
  /** Local creation order, so events drain in the order they happened. */
  order: number;
  kind: "manualSyncCue" | "captureDiscontinuity" | "localBufferOverflow" | "deviceLost" | "pageInterrupted" | "clockSample";
  audioFrame: number;
  performanceMs: number | null;
  declaredMovieSeconds: number | null;
  detail: Record<string, unknown>;
}

export interface ChunkOutbox {
  putChunk(chunk: OutboxChunk): Promise<void>;
  putEvent(event: OutboxEvent): Promise<void>;
  events(sessionId: string): Promise<OutboxEvent[]>;
  deleteEvent(sessionId: string, eventId: string): Promise<void>;
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
export const OUTBOX_MAX_EVENTS = 10_000;
const OUTBOX_MAX_EVENT_BYTES = 4 * 1024;

function validateEvent(event: OutboxEvent) {
  if (new TextEncoder().encode(JSON.stringify(event)).byteLength > OUTBOX_MAX_EVENT_BYTES) {
    throw new OutboxFullError();
  }
}

export class MemoryOutbox implements ChunkOutbox {
  private readonly chunkMap = new Map<string, OutboxChunk>();
  private readonly eventMap = new Map<string, OutboxEvent>();
  private readonly sessionMap = new Map<string, OutboxSession>();
  constructor(private readonly maxBytes = OUTBOX_MAX_BYTES) {}

  async putEvent(event: OutboxEvent) {
    validateEvent(event);
    const key = `${event.sessionId}:${event.eventId}`;
    if (!this.eventMap.has(key) && this.eventMap.size >= OUTBOX_MAX_EVENTS) throw new OutboxFullError();
    this.eventMap.set(key, structuredClone(event));
  }
  async events(sessionId: string) {
    return [...this.eventMap.values()]
      .filter((event) => event.sessionId === sessionId)
      .sort((a, b) => a.order - b.order);
  }
  async deleteEvent(sessionId: string, eventId: string) { this.eventMap.delete(`${sessionId}:${eventId}`); }
  async putChunk(chunk: OutboxChunk) {
    if ((await this.pendingBytes()) + chunk.bytes.byteLength > this.maxBytes) throw new OutboxFullError();
    this.chunkMap.set(`${chunk.sessionId}:${chunk.sequence}`, chunk);
  }
  async chunks(sessionId: string) {
    return [...this.chunkMap.values()]
      .filter((chunk) => chunk.sessionId === sessionId)
      .sort((a, b) => a.sequence - b.sequence);
  }
  async deleteChunk(sessionId: string, sequence: number) { this.chunkMap.delete(`${sessionId}:${sequence}`); }
  async putSession(session: OutboxSession) { this.sessionMap.set(session.sessionId, { ...session }); }
  async sessions() { return [...this.sessionMap.values()]; }
  async deleteSession(sessionId: string) {
    this.sessionMap.delete(sessionId);
    for (const [key, event] of this.eventMap) if (event.sessionId === sessionId) this.eventMap.delete(key);
  }
  async pendingBytes() { let total = 0; for (const chunk of this.chunkMap.values()) total += chunk.bytes.byteLength; return total; }
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

export class IndexedDbOutbox implements ChunkOutbox {
  private db: Promise<IDBDatabase> | null = null;
  constructor(private readonly maxBytes = OUTBOX_MAX_BYTES) {}

  private open(): Promise<IDBDatabase> {
    this.db ??= new Promise((resolve, reject) => {
      const request = indexedDB.open(DB_NAME, 2);
      request.onupgradeneeded = (event) => {
        const db = request.result;
        if (event.oldVersion < 1) {
          const chunks = db.createObjectStore("chunks", { keyPath: ["sessionId", "sequence"] });
          chunks.createIndex("bySession", "sessionId");
          db.createObjectStore("sessions", { keyPath: "sessionId" });
        }
        if (event.oldVersion < 2) {
          const events = db.createObjectStore("events", { keyPath: ["sessionId", "eventId"] });
          events.createIndex("bySession", "sessionId");
        }
      };
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    return this.db;
  }

  private async transaction(mode: IDBTransactionMode) {
    const tx = (await this.open()).transaction(["chunks", "sessions", "events"], mode);
    const done = new Promise<void>((resolve, reject) => {
      tx.oncomplete = () => resolve();
      tx.onabort = () => reject(tx.error);
      tx.onerror = () => reject(tx.error);
    });
    return { chunks: tx.objectStore("chunks"), sessions: tx.objectStore("sessions"), events: tx.objectStore("events"), done };
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
      const session = all.find((candidate) => candidate.sessionId === chunk.sessionId);
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

  async putEvent(event: OutboxEvent) {
    validateEvent(event);
    const tx = await this.transaction("readwrite");
    const existing = await promisify(tx.events.get([event.sessionId, event.eventId]));
    const count = await promisify(tx.events.count());
    if (!existing && count >= OUTBOX_MAX_EVENTS) {
      tx.events.transaction.abort();
      await tx.done.catch(() => undefined);
      throw new OutboxFullError();
    }
    tx.events.put(event);
    await tx.done;
  }

  async events(sessionId: string) {
    const tx = await this.transaction("readonly");
    const rows = await promisify(tx.events.index("bySession").getAll(IDBKeyRange.only(sessionId))) as OutboxEvent[];
    await tx.done;
    return rows.sort((a, b) => a.order - b.order);
  }

  async deleteEvent(sessionId: string, eventId: string) {
    const tx = await this.transaction("readwrite");
    tx.events.delete([sessionId, eventId]);
    await tx.done;
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
    tx.events.index("bySession").openKeyCursor(IDBKeyRange.only(sessionId)).onsuccess = (event) => {
      const cursor = (event.target as IDBRequest<IDBCursor | null>).result;
      if (cursor) { tx.events.delete(cursor.primaryKey); cursor.continue(); }
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
