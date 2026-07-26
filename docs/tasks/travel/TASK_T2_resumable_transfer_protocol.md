# Task T2: Resumable Transfer Sessions, Chunking, and Verification

**Status:** Planned
**Depends on:** T1, companion-server Task 4
**Unblocks:** T3, T7

## Objective

Implement the original-media transfer service: card sessions, inventory declaration,
resumable chunk upload, restart-safe progress, and end-to-end verification before archive
commit.

## User outcome

An upload interrupted by hotel wifi, a removed card, a phone restart, or a server restart
resumes the next night without retransmitting a single byte that already arrived safely.

## Scope

- `/api/v1/transfer/` route group authenticated by device credential.
- Card session creation and resume, idempotent on card identity.
- Paged inventory declaration.
- Resume query returning needed objects with committed chunk ranges.
- Chunk upload with per-chunk hash verification and idempotent re-upload.
- Sparse staging file with positioned writes on the server SSD.
- Chunk ledger persistence and restart recovery.
- Completion: gap check, size check, full-file BLAKE3, then archive commit through T1.
- Object abandonment and staging reclamation.
- Per-route body limits sized to the configured chunk size.
- Transfer state machine and diagnostics.

## Out of scope

- Media library registration and dedupe (T3)
- Representation generation (T4)
- Project assets and operations (T7)
- Client-side queue policy (T8)
- Any "safe to format" state
- Raising the global HTTP body limit

## Endpoints

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/api/v1/transfer/cards` | Create or resume a card session |
| `POST` | `/api/v1/transfer/cards/{cardSessionId}/objects` | Declare inventory in bounded pages |
| `GET` | `/api/v1/transfer/cards/{cardSessionId}/objects?cursor=` | Needed objects with committed ranges |
| `PUT` | `/api/v1/transfer/objects/{objectId}/chunks/{index}` | Upload one chunk as raw bytes |
| `POST` | `/api/v1/transfer/objects/{objectId}/complete` | Declare full-file hash and size |
| `POST` | `/api/v1/transfer/objects/{objectId}/abandon` | Cancel and reclaim staging |

## Card identity

Derived from the volume UUID or label combined with a digest of the first N
`(relativePath, size)` pairs in a stable sort order. `POST /cards` is idempotent on this
identity so a phone that loses local state recovers by re-inventorying.

Declaring inventory for an existing card session adds newly discovered objects and leaves
existing objects' progress untouched. Shooting more footage onto a partially uploaded card
is an expected flow, not an error.

## Transfer state machine

```text
discovered → queued → uploading → received_unverified → verified → archived
                                              ↘ failed / cancelled / superseded
```

`ingested` and `proxy_ready` are appended by T3 and T4. States are distinct because ingest
triggers off `archived` and because diagnostics must report where a file actually is. No
state licenses card reuse.

## Chunking and resume

- Chunk size is fixed per object at declare time; index maps to offset arithmetically.
- Chunks are written at their computed offset into a sparse staging file.
- The **chunk ledger** stores one row per committed chunk: index, byte length, BLAKE3.
- The ledger, not the staging file, is the restart-time source of truth.
- Committed indices are coalesced into ranges in the resume response.

Idempotency:

| Situation | Result |
| --- | --- |
| Re-upload of a committed chunk, hash matches | `200`, no write |
| Re-upload of a committed chunk, hash differs | `409`, no write |
| Chunk whose body length disagrees with its index position | `400`, no write |
| Interrupted chunk | Never committed; retried whole |

## Design requirements

1. The global 64 MB `DefaultBodyLimit` is unchanged. Chunk routes carry their own limit of
   configured chunk size plus headroom.
2. A chunk is committed to the ledger only after its bytes are durably written.
3. Ledger and staging file are reconciled on startup; a ledger row whose bytes cannot be
   confirmed is dropped rather than trusted.
4. Completion requires a gap-free ledger whose total equals the declared size.
5. Full-file BLAKE3 is computed by the server over the staged file and compared to the
   phone's declaration. A mismatch discards staging and leaves the object retryable from
   scratch.
6. The archive commit path from T1 is called only with a fully verified staged file.
7. Staging capacity below the floor refuses new sessions and new objects while existing
   transfers continue to drain.
8. Abandonment reclaims staging promptly and is idempotent.
9. All transfer state is server-side and survives restart; the phone's local state is a
   cache that can always be rebuilt from the resume query.
10. Errors use stable structured codes distinguishing retryable from terminal.
11. Credentials never appear in a URL, query string, or log line.
12. Transfer progress must not block or delay project operation synchronization.

## Likely implementation areas

- `core/rcdvlog-server/src/transfer.rs` (new) sessions, ledger, state machine, staging
- `core/rcdvlog-server/src/config.rs` chunk size, capacity floors, staging root
- `apps/server/src/lib.rs` transfer routes and per-route body limits
- `packages/project-client` typed transfer contracts
- Fault-injection and restart integration tests

## Acceptance criteria

- [ ] A multi-chunk upload completes, verifies, and commits to the archive.
- [ ] An upload interrupted mid-chunk resumes without retransmitting committed chunks.
- [ ] A server restart mid-transfer preserves every committed chunk and no more.
- [ ] A ledger row whose bytes cannot be confirmed on startup is dropped, not trusted.
- [ ] Re-uploading a committed chunk with a matching hash is a no-op `200`.
- [ ] Re-uploading a committed chunk with a differing hash returns `409` and writes nothing.
- [ ] A gap in the ledger prevents completion with a clear error.
- [ ] A declared-size mismatch prevents completion.
- [ ] A full-file hash mismatch discards staging and leaves the object retryable.
- [ ] A partial file never appears beneath the archive root.
- [ ] No staging file is written to a NAS mount.
- [ ] `POST /cards` with the same card identity resumes rather than duplicating.
- [ ] Re-inventorying a card with new footage adds objects and preserves existing progress.
- [ ] Non-transfer routes still enforce the 64 MB limit.
- [ ] A chunk body exceeding the per-route limit is rejected without buffering the whole body.
- [ ] Below the capacity floor, new sessions are refused while existing transfers drain.
- [ ] Abandonment reclaims staging and is idempotent.
- [ ] Transfer requests without a valid device credential are rejected.
- [ ] Project operation sync continues normally while a large transfer runs.

## Verification

Test against a fault-injecting HTTP layer that drops connections mid-body, truncates
bodies, delays, and returns 5xx — exercising the real chunk route rather than a mock. Add a
property test asserting that arbitrary interleavings of interruption, restart, and resume
converge to byte-identical output. Kill the server process between chunks to validate
ledger-driven recovery. Use generated media large enough to require many chunks.

## Handoff notes

T3 hooks the transition from `archived` to `ingested`. Expose a commit hook rather than
having the transfer module call into the library directly, so the two remain testable
apart. T8's queue depends on the resume query returning coalesced ranges cheaply for cards
with thousands of objects — paginate it and index accordingly.
