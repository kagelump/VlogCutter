# Task 12: Remote Representation Exchange and Mac Requests

**Status:** Planned
**Depends on:** Tasks 6 and 11
**Unblocks:** Task 19

## Objective

Bridge representation availability between nodes by allowing explicit server-to-Mac proxy download and durable server-side requests for media work that can only run on the Mac.

## User outcome

A remote-only project asset can become reviewable on the Mac by downloading its server proxy, while a web request for Mac-only footage can wait safely until the Mac reconnects and the original becomes available.

## Scope

- Add explicit desktop command to request/download a compatible server representation.
- Transfer proxy/thumbnail bytes through authenticated streaming with resumable or restart-safe behavior.
- Verify representation identity, length, checksum, media contract, and source association before local registration.
- Atomically commit downloads into the project-local cache.
- Add server `targetNode=mac` request state and delivery through synchronization.
- Add desktop acceptance/decline/offline execution adapter using current Rust media operations.
- Report completed Mac representation manifests back to the server.
- Deduplicate requests when compatible work/output already exists on either node.
- Surface request origin, execution node, waiting reason, progress when available, and final result.

## Out of scope

- Original file transfer
- Bulk “Prepare offline” selection and space planning
- General durable Mac job scheduler
- Server-initiated network connection to the Mac
- Competitive job claiming or silent reassignment
- Automatic proxy cache eviction

## Desktop proxy download

1. Query representation manifest and compatibility.
2. Reserve a known temp path beneath the project cache.
3. Stream authenticated bytes without putting credentials in the URL.
4. If resumable transfer is implemented, bind ranges to immutable ETag/representation ID.
5. Verify expected size/hash and ffprobe orientation contract.
6. Atomically rename to the current local cache path.
7. Register the local representation location and publish its manifest.

Partial/invalid files never become playback candidates.

## Mac-targeted request

1. Server stores a durable pending request with stable idempotency key.
2. Mac receives it during ordinary reconciliation.
3. Mac reports whether required original and tools are available.
4. User-initiated requests may auto-accept according to explicit settings; otherwise show a clear pending action.
5. Existing core media operation executes locally with current progress behavior.
6. Mac publishes success manifest or structured failure.
7. Server marks request complete, retryable, declined, or canceled.

## Design requirements

1. Operations/manifests synchronize separately from blob bytes.
2. The server never sends originals through this feature.
3. Download targets are deterministic, project-local, and containment-checked.
4. A source/recipe/checksum mismatch deletes only the known temp download and surfaces an error.
5. Repeated download or request commands are idempotent.
6. Credentials use headers or native request APIs, never query parameters persisted in media elements/logs.
7. Network interruption leaves a retryable temp artifact or removes it safely; it never replaces a valid proxy.
8. The Mac checks original availability at execution time rather than trusting stale server observations.
9. A canceled server request cannot later be completed by an old Mac attempt without explicit reconciliation.
10. Current one-worker Mac media bounds remain in force.
11. Desktop playback continues to prefer a valid local proxy, then available original.
12. Remote-only state clearly distinguishes missing local media from missing everywhere.

## Likely implementation areas

- `core/rcdvlog-sync` node-targeted request DTOs/status
- `core/rcdvlog-server` request persistence and representation download metadata
- `apps/server` authenticated streaming/request routes
- `apps/desktop/src-tauri` native authenticated download and execution adapter
- `apps/desktop` availability/action/progress UI
- Media and network interruption tests

## Acceptance criteria

- [ ] A server-only current proxy downloads, validates, atomically commits, and becomes the desktop playback source.
- [ ] A duplicate download reuses the valid local output.
- [ ] Interrupted, truncated, wrong-ETag, wrong-hash, and invalid-orientation downloads never become available assets.
- [ ] Credentials and filesystem paths are absent from persisted URLs and logs.
- [ ] A Mac-targeted request remains pending while the Mac is offline.
- [ ] On reconnect, an available Mac original can produce and report a compatible representation.
- [ ] An offline/missing original returns a retryable reason without being transferred.
- [ ] Cancel and decline states survive server/Mac restart and reconcile correctly.
- [ ] Existing compatible work on another node prevents unnecessary duplicate generation.
- [ ] Project annotations continue syncing while a large proxy downloads.
- [ ] No original file is uploaded, changed, renamed, or deleted.

## Verification

Use generated media and a throttled/fault-injected HTTP server to test interruption and retry. Run real Tauri acceptance because browser mock transport cannot validate Keychain credentials, native downloads, asset protocol playback, or local cache permissions.

## Handoff notes

Task 19 must exercise remote-only asset adoption, server proxy download, and one Mac-only pending request across an actual desktop/server connection.
