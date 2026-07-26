# Task 6: Representation Store, Media Jobs, and Streaming

**Status:** Implemented (automated acceptance complete; manual Chrome/Ubuntu acceptance pending)
**Depends on:** Tasks 4 and 5
**Unblocks:** Tasks 12, 14, 15, 16, and 18

## Objective

Implement globally reusable thumbnail/proxy representations, durable server media handlers, atomic validation, and Chrome-compatible authenticated byte-range delivery.

## User outcome

The server can generate current review media from NAS originals overnight, reuse a verified result across projects, and stream proxies to Chrome without exposing or altering originals.

## Scope

- Add representation and representation-location tables/services.
- Define deterministic representation identity from verified content and versioned recipe.
- Add durable quality-1 thumbnail and H.264 720p proxy handlers.
- Preserve current orientation, scale, codec, audio, and validation contracts.
- Add derived-root path layout and optional server-local staging/cache.
- Atomically commit output only after tool and media validation.
- Register existing compatible project-local outputs as node-local locations when evidence is sufficient.
- Add internal media metadata and content endpoints with full HTTP range semantics; Task 9 adds public authentication.
- Add representation request, reuse, retry, cancellation, and missing/obsolete query services.

## Out of scope

- Serving original files to the browser
- Hardware acceleration
- New proxy quality recipes
- Contact sheets beyond quality-1 thumbnail
- Automatic Mac cache download
- Web playback UI
- Bulk offline preparation or automatic eviction

## Representation identity

Require a verified full source content hash before advertising an output as globally reusable. Identity includes:

- Full source content hash and size
- Representation kind
- Recipe ID and version
- All output-affecting settings
- Relevant orientation contract
- Model/version for future analysis representations, though none are implemented here

The initial keys remain:

- `proxy.h264-720p.orientation-v3`
- `thumbnail.q1.orientation-v2`

## Design requirements

1. Originals are the only proxy input and are opened read-only.
2. Thumbnail input may prefer original and then a verified compatible proxy, matching current behavior.
3. Proxy generation overrides input display rotation, disables autorotation, and validates 0-degree output display rotation.
4. Proxy output remains H.264 `libx264`, at most 1280 pixels wide, CRF 23, `veryfast`, `yuv420p`, AAC 128 kb/s, and fast-start.
5. Thumbnail remains JPEG at 20% duration and at most 480 pixels wide.
6. Temporary output never becomes a manifest location before successful ffprobe/image validation and atomic rename.
7. A canceled/failed job cleans only its own known temp artifact and never removes a prior valid output.
8. Deterministic paths are rooted beneath the configured derived root and cannot be supplied by clients.
9. Concurrent requests for one representation share idempotent work.
10. Existing current-version output is reused only if it exists and still passes required identity/validation checks.
11. Range responses support `HEAD`, valid/invalid `Range`, `206`, `416`, ETag, content length/type, and streaming backpressure.
12. Rust does not buffer an entire proxy in memory.
13. Filesystem paths and credentials do not appear in media URLs.
14. Derived-root outage yields explicit retryable state and does not fall back to writing beside originals.

## Likely implementation areas

- `core/rcdvlog-core` reusable media recipe and validation primitives
- `core/rcdvlog-server` representation repository, path policy, and job handlers
- `apps/server` internal representation/media routes
- Control-catalog migrations
- FFmpeg/ffprobe integration tests

## Acceptance criteria

- [x] Two projects referencing one verified media object reuse one representation record and blob.
- [x] Current proxy and thumbnail recipes produce outputs matching existing orientation and size tests.
- [x] Obsolete v1/v2 proxy paths are not misreported as current v3.
- [x] A failed, canceled, or invalid output never replaces a prior valid representation.
- [x] Server restart after file commit but before job response reconciles the valid output idempotently.
- [x] Concurrent duplicate requests result in one active media job.
- [x] Derived output always remains beneath the configured derived root.
- [x] No output or temp file appears in an original root.
- [x] Full and partial HTTP content responses return correct headers and bytes.
- [x] Invalid/multiple/unsatisfiable ranges return the documented stable response without reading arbitrary files.
- [x] Generated MP4 delivery has Chrome-compatible single-range semantics; a real Chrome smoke test remains manual acceptance.
- [x] Missing originals produce `media_offline`; missing derived root produces a distinct actionable condition.
- [x] Successful work records weighted rate observations; failures do not.

## Verification

Run core/server tests and real FFmpeg integration coverage. Add HTTP range tests against a known byte fixture plus a generated MP4. Use a filesystem-read-only original fixture and assert its complete tree remains unchanged.

## Handoff notes

Publish representation query/request/content services without embedding auth decisions. Task 9 protects routes, Task 12 transfers outputs to the Mac, and Task 15 supplies the review UI.
