# Travel Ingest Architecture

**Status:** Accepted design for implementation planning
**Date:** 2026-07-27
**Scope:** Phone-carried SD-card ingest, server-side archive, and travel annotation
**Depends on:** [Companion Server Architecture](../server/ARCHITECTURE.md)
**Target deployment:** One iPhone, one Ubuntu home server, one Mac, one user, Tailscale

## 1. Executive summary

Today RCD VlogCutter assumes the Mac is present when footage arrives. The desktop ingests
from the card, generates representations, and the server assists afterward. That assumption
fails on a trip taken without a laptop.

Travel ingest inverts which node is primary without inverting ownership:

| | Current model | Travel model |
| --- | --- | --- |
| Phone | Not a participant | Courier and annotation terminal |
| Server | Assists after the fact | Ingest, archive, compute, active authority |
| Desktop | Ingests and reviews | Resumes the richer workflow on return |

The phone reads an attached SD card, streams originals to the home server over Tailscale,
and annotates footage while the remaining bytes are still uploading. The server verifies
each arrival, commits it into a writable NAS archive root, indexes it into the existing
global media library, and generates thumbnails and a small mobile proxy. The desktop later
adopts the trip project and continues normally.

The phone is a courier and a review terminal. It is never a third archive.

## 2. Accepted product decisions

These were confirmed during design and take precedence over older statements:

1. **The phone may create projects.** The companion-server invariant "project creation
   remains a desktop action" is amended. See section 9.
2. **The phone is an iPhone running an Expo custom development build.** Not Expo Go, and
   not a browser. See section 8.
3. **Uploading is a head start, not a backup.** Cards are carried home regardless. No user
   interface ever presents a "safe to format" state. See section 3.
4. **The phone reviews footage through a new low-bandwidth mobile proxy**, not the existing
   720p review proxy. See section 7.
5. **Remote access is Tailscale only.** The server's loopback-only listener is unchanged.
   See section 10.
6. **Original bytes are written only to a dedicated writable archive root.** Existing NAS
   scan roots remain immutable and read-only. See section 4.

## 3. The durability bar, stated plainly

This pipeline is deliberately **not** a backup system.

Uploading footage from a hotel is a head start on ingest and a useful off-site copy. It is
not permission to reuse the card. The user carries every card home, and the desktop
reconciles card contents against the archive on return (section 12).

This decision is load-bearing for the whole design, because it removes the requirement that
would otherwise dominate it. A system that licenses card formatting must prove durability
before releasing that state, which means NAS-side re-verification, an audit surface, and a
failure mode that loses footage when it is wrong. None of that is needed here.

What is retained regardless:

- The phone computes a full-file BLAKE3 while reading, because it reads every byte anyway.
- The server verifies size and hash before committing to the archive.
- Transfer states distinguish `received_unverified`, `verified`, and `archived`.

Those states are kept because ingest triggers off `archived` and because diagnostics must
be able to say where a file actually is — not because any of them license destruction.

**Consequence for implementers:** a verification failure is an inconvenience to retry, not
a data-loss incident. Handle it with a clear error and a retryable object. Do not build
escalation, alerting, or recovery machinery proportionate to a backup system.

## 4. The archive root

Original bytes need a writable destination. The existing storage model already expresses
this without modification.

`StorageRoot` carries an `immutable_original` flag, and `library.rs` enforces that a root
marked `immutable_original` accepts only `StorageAccessMode::ReadOnly` bindings. The
archive root is therefore a *new logical storage root* with `immutable_original: false`,
bound `ReadWrite` on the server node and (optionally) `ReadOnly` on the Mac.

Existing NAS scan roots are untouched and remain read-only at both the application and
deployment boundary.

### 4.1 Write discipline

The archive root is writable only along the create-new-file path. The server exposes no
API that renames, overwrites, truncates, or deletes anything beneath it. Once a file is
committed it is treated exactly like any other immutable original.

Commit is temp-file-in-the-same-directory, `fsync`, then atomic rename. A partial file is
never visible under its final name, and a failed commit leaves only a known temp path that
the server owns and can clean up.

### 4.2 Layout

```text
<archiveRoot>/<tripSlug>/<cardSessionSlug>/<cardRelativePath>
```

- `tripSlug` derives from the trip project (`2026-08-iceland`).
- `cardSessionSlug` is `<cardLabel>-<cardSessionShortId>`, so a card reinserted later
  resumes into the same directory, while a reformatted-and-reused card gets a new identity
  and therefore a new directory. DCIM numbering wraparound cannot collide across sessions.
- `cardRelativePath` preserves the camera's own folder structure
  (`DCIM/100MSDCF/C0012.MP4`), which keeps camera-native sidecars and clip groupings intact.

### 4.3 Path safety

`cardRelativePath` originates on the phone and is therefore untrusted input. Before any
filesystem use it must be rejected if it is absolute, contains `..` or empty segments,
contains control characters or a NUL, or resolves outside the archive root after
canonicalization. Containment is re-checked after canonicalization, not only before.

## 5. Transfer protocol

Original-media transfer is a separate concern from project synchronization and must not be
retrofitted onto it. Operations are small, ordered, and replicated; media chunks are large,
unordered, and disposable until verified.

### 5.1 Body limits

`apps/server/src/lib.rs` applies a global 64 MB `DefaultBodyLimit`. That limit is correct
for control APIs and **stays exactly as it is**. Chunk routes receive their own per-route
limit derived from the configured chunk size (default 8 MiB, route limit chunk size plus
1 MiB of headroom).

Raising the global limit to accommodate media would weaken every other endpoint. Do not do
it.

### 5.2 Endpoints

All under `/api/v1/transfer/`, authenticated by device credential.

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/cards` | Create or resume a card session; idempotent on card identity |
| `POST` | `/cards/{cardSessionId}/objects` | Declare inventory, in bounded pages |
| `GET` | `/cards/{cardSessionId}/objects?cursor=` | What the server still needs, with committed ranges |
| `PUT` | `/objects/{objectId}/chunks/{index}` | Upload one chunk; raw bytes |
| `POST` | `/objects/{objectId}/complete` | Declare full-file hash and size |
| `POST` | `/objects/{objectId}/abandon` | Release staging for an object the user cancelled |

### 5.3 Card identity

A card session is identified by the volume UUID or label combined with a digest of the
first N `(relativePath, size)` pairs in stable order. Reinserting the same card produces
the same identity and resumes; a reformatted card produces a different identity and starts
clean. `POST /cards` is idempotent on this identity, so a phone that loses its local state
recovers by re-inventorying.

Shooting more footage onto a partially uploaded card is expected. Re-inventory adds new
objects and leaves existing objects' progress untouched.

### 5.4 Chunking and resume

Chunk size is fixed per object at declare time, so chunk index maps to byte offset
arithmetically and no offset negotiation is needed. The server writes each chunk at its
computed offset into a sparse staging file.

Committed chunks are recorded in a **chunk ledger** — one row per committed chunk carrying
index, byte length, and chunk BLAKE3. A bitmap would be smaller, but the ledger's per-chunk
hashes are what make re-upload safe to distinguish from corruption, and at 8 MiB chunks a
50 GB card produces roughly 6,400 rows, which is negligible.

The ledger, not the staging file, is the restart-time source of truth.

Idempotency rules:

- Re-uploading a committed chunk whose hash matches is a no-op `200`.
- Re-uploading a committed chunk whose hash differs is `409`, never a silent overwrite.
- An interrupted chunk was never committed, so it is simply retried whole.

`GET /cards/{id}/objects` returns committed chunk indices coalesced into ranges so the
phone can resume without enumerating thousands of individual chunks.

### 5.5 Staging

Staging lives on the **server SSD**, under `<serverDataDir>/transfer/staging/<objectId>` —
never on the NAS. Partial files must never appear beneath the archive root, and staging
write amplification must never land on network storage.

If staging capacity falls below a configured floor, the server refuses new card sessions
and new objects with a clear structured error while existing transfers continue to drain.
Refusing new work is correct; interrupting committed work is not.

### 5.6 Completion

On `POST /complete` the server:

1. Confirms the ledger covers every chunk with no gaps and the total equals the declared size.
2. Computes the full-file BLAKE3 over the staged file and compares it to the phone's declaration.
3. On mismatch, marks the object `failed`, discards staging, and leaves it retryable from scratch.
4. On match, checks the hash against verified library content. If the archive already holds
   that content, it skips the write and attaches the existing media object. Otherwise it
   commits into the archive root atomically (section 4.1).
5. Registers a media object through the existing library identity path, **with the hash
   already verified**, so no second hashing pass is scheduled.
6. Enqueues thumbnail and mobile-proxy jobs on the existing durable job engine.

Step 5 matters: the scan pipeline's normal flow is quick fingerprint first, full hash later
in the background. A transfer-committed file arrives already fully verified and should
enter the library in the verified state directly.

## 6. Notes before bytes

This is the mechanism the entire "annotate while footage uploads" promise rests on.

The moment the phone finishes inventorying a card — before a single byte has uploaded — it
emits ordinary project operations creating **provisional project assets**, carrying
filename, size, card identity, capture sequence, file modification time, and camera folder.

These are not a special travel-only entity. They are project assets that do not yet have a
`mediaObjectId`. They synchronize to the server and the desktop immediately through the
existing operation protocol, and notes, labels, and removal apply to them normally.

When an upload later completes, the server emits a **link operation** binding that
`projectAssetId` to the newly registered `mediaObjectId`.

Notes written during the gap survive untouched, because they were always attached to the
`projectAssetId`, which the phone allocated at inventory time and which never changes. The
identity model already separates `projectAssetId` from `mediaObjectId`; this design uses
that separation across *time* rather than only across nodes.

Two consequences worth stating explicitly:

- **The link operation is not server-only.** The desktop must be able to emit it too, so
  that footage which never uploaded can be linked to a locally ingested media object on
  return (section 12).
- **Every surface must render an unlinked asset gracefully.** Desktop, web, and phone all
  need a coherent presentation for "this asset is known but its media has not arrived."
  This is a real UI requirement, not an edge case — during a trip it is the *common* state.

### 6.1 Resulting timeline

```text
21:00  Card inserted, inventory begins
21:03  Provisional assets visible on phone, web, and desktop; notes can be written
21:10  First thumbnails appear
21:20  First mobile proxies reviewable
21:30  Annotating earlier clips while later originals upload
02:00  Hotel wifi drops; queue pauses at last committed chunk
Next night: resume, no committed chunk retransmitted
```

## 7. Mobile proxy recipe

A new `RepresentationKind::MobileProxy` with recipe key
`mobile.h264-480p.orientation-v1`: roughly 480p, CRF 30, 64k mono AAC, `faststart`.
Approximately 10–20x smaller than the existing `proxy.h264-720p.orientation-v3`.

Two requirements:

- It must **reproduce the existing proxy's orientation behavior**, not merely copy its
  settings string. Orientation handling is the reason the existing recipe key carries an
  `orientation-v3` suffix, and getting it wrong produces silently sideways review footage.
- It is enqueued **by the transfer pipeline for freshly archived travel media only**. It is
  never a library-wide sweep. Generating mobile proxies across an entire NAS archive would
  saturate the server for no benefit.

Downstream bandwidth is a real constraint on hotel wifi and competes directly with uploads.
The phone requests mobile proxies on demand and by explicit prefetch policy, never
speculatively for an entire card.

## 8. The phone application

`apps/mobile`, Expo with a custom development build.

- **Local SQLite** holds the project replica, pending operations, and transfer queue state.
- **Streaming read path:** SD card file, through a bounded buffer, into chunk uploads.
  Never a full local duplicate. A full copy would demand enormous free phone storage and
  double all local I/O for no gain.
- **Media cache** for thumbnails and mobile proxies, with a hard user-visible size budget
  and LRU eviction.
- **Authentication** by device credential through the existing pairing flow
  (`POST /api/v1/auth/pairing`, `POST /api/v1/auth/devices/exchange`), stored in the iOS
  Keychain. The phone is an ordinary paired native device, not a new credential class.
- **Offline annotation.** Notes, labels, and removal queue locally and reconcile through
  the existing sync protocol. Project *creation* requires connectivity; annotation does not.
- **Queue policies:** everything, today only, smallest first, this folder only, pinned
  first. Pause, resume, reorder, and per-card progress.

### 8.1 iOS constraints, stated honestly

iOS security-scoped access to an external volume does not reliably survive backgrounding.
Long transfers are therefore a **foreground, screen-on, plugged-in activity** with
best-effort background continuation. This is a platform limitation, not something to be
engineered around, and the design must not depend on a background upload daemon.

Concretely:

- The app offers an explicit "keep awake while uploading" mode.
- Overnight uploads assume the phone is plugged in with the app foregrounded.
- If the volume disappears mid-transfer, the queue pauses cleanly and resumes on
  reinsertion via card identity. It does not fail the card session.
- Notifications report completion and failure so the user need not watch the screen.

This is the single largest risk in the design. It is a workflow constraint rather than a
correctness problem: the resume protocol means an interrupted night costs time, not data.

### 8.2 Why not the browser

Mobile Safari cannot sustain multi-hour transfers of hundreds of large files from an
attached volume across screen locks and suspension, and cannot hold external-file access
across interruptions. The web client remains valuable as an administrative and review
fallback, but the SD-card courier must be native.

## 9. Server-origin project creation

A trip that begins without a laptop has no desktop on which to create its project. The
companion-server invariant is amended:

> Projects may be created on the desktop or on the server. Each project has exactly one
> replica per node that has adopted it.

Project **deletion** remains out of scope on every surface.

Implementation:

- A new `project.create` operation with server origin, callable by an authenticated device
  credential. The server allocates the `projectId`, records the origin node, and creates
  its replica directly rather than adopting one from elsewhere.
- The desktop gains **Adopt server project** — the mirror image of the existing baseline
  import. It lists server projects with no local replica, presents a preview, then
  materializes a local catalog from the operation log.

After adoption the project is indistinguishable from a desktop-created one. Origin is
recorded for diagnostics, not for behavior.

## 10. Remote access

`bind_loopback` is unchanged and continues to reject non-loopback addresses. The phone
reaches the server through Tailscale Serve in front of the loopback listener, which is
exactly the remote route the companion-server epic already specifies.

Uploads never traverse the public internet and no new externally reachable surface is
created. For a single-user system, private-network access is a far smaller security and
operational burden than operating a public upload service, and the transfer endpoints —
which accept large writes — are precisely the ones that should not be publicly reachable.

## 11. Failure handling

| Failure | Behavior |
| --- | --- |
| Network drop mid-chunk | Chunk was never committed; retried whole |
| Server restart mid-transfer | Chunk ledger is truth; staging file re-verified against it |
| Card removed mid-transfer | Queue pauses; card session persists; resumes on reinsertion |
| Full-file hash mismatch | Object marked `failed`, staging discarded, retryable from scratch |
| Same file uploaded twice | Deduped by verified hash against the library; provisional asset links to the existing media object |
| Card re-inventoried | Same identity returns existing objects with committed ranges |
| More footage shot onto a partial card | New objects added; existing progress preserved |
| Staging disk pressure | New sessions and objects refused with a structured error; existing transfers drain |
| Phone loses local state | Re-pair, re-inventory; card identity recovers server-side progress |
| Trip ends with uploads incomplete | Remaining originals ingest from the card on return (section 12) |

## 12. Returning home

Because cards are never formatted, return is a reconciliation rather than a handoff.

1. The desktop adopts the trip project if it has not already (section 9).
2. Desktop ingest reads the card and computes content hashes as it normally does.
3. For each file whose hash already exists in the server library, the desktop links rather
   than re-copies. Nothing is transferred twice.
4. For each file that never uploaded, the desktop ingests it normally and emits a link
   operation binding it to the waiting provisional asset (section 6).
5. Notes written on the phone during the trip are already present, attached to assets that
   now have media.

Step 4 is why the link operation must not be server-only. Without it, a trip where the
uploads did not finish would strand notes on assets that can never gain media.

## 13. Testing strategy

- **Fault injection.** A throttled HTTP layer that drops, truncates, delays, and returns
  errors mid-chunk, exercised against real chunk upload rather than a mock.
- **Restart mid-transfer.** Kill the server between chunks and assert the ledger drives
  correct resume.
- **Resume convergence.** Property test: arbitrary interleavings of interruption and resume
  converge to byte-identical output.
- **Containment.** Adversarial `cardRelativePath` inputs cannot escape the archive root.
- **Read-only assertion.** No test writes, renames, or deletes anything under a scan root.
  This is an existing cross-cutting criterion and it continues to hold.
- **Orientation.** The mobile proxy recipe is validated against the same real-camera
  orientation fixtures as the existing proxy recipe, not only synthetic media.
- **Mobile.** Queue state-machine unit tests, contract tests against `project-client`, and
  manual acceptance with a real card, a real reader, and a bandwidth-limited network.

## 14. Out of scope

- Any "safe to format" state or card-reuse licensing (section 3)
- A full copy of card contents in phone storage (section 8)
- Background upload daemons on iOS (section 8.1)
- Browser-based SD-card upload (section 8.2)
- Public internet exposure of transfer endpoints (section 10)
- Project deletion on any surface (section 9)
- Archive rename, overwrite, deletion, or cleanup policy (section 4.1)
- Library-wide mobile proxy generation (section 7)
- Transcription, scene extraction, visual summaries, chronology suggestion, and other
  compute-first enrichment. These become attractive once transfer is reliable, but planning
  them now would be speculative.
- Android, multiple phones, multiple users, and multiple servers

## 15. Dependencies on unbuilt work

This design depends on companion-server tasks that are not yet implemented. This is
accepted, and is recorded here so it is not discovered late:

| Needed | Companion-server task | Status |
| --- | --- | --- |
| Desktop pairing and baseline adoption | Task 10 | **Not implemented** |
| Web library and project browsing | Task 14 | Not implemented |
| Web proxy review and annotation | Task 15 | Not implemented |
| Authentication and secure transport | Task 9 | Core implemented; task file marks it Planned |

Task 10 is the one genuine prerequisite gap: travel task T5 (server-origin project creation
and desktop adoption) builds directly on its pairing and baseline machinery.

Several companion-server task files carry stale `Status:` markers. Trust the code and the
git history over the markers until Task 20 reconciles them.

## 16. Risks

| Risk | Mitigation |
| --- | --- |
| iOS suspends the app and stalls overnight uploads | Resume protocol makes this cost time, not data; keep-awake mode; honest expectations in the UI |
| Hotel bandwidth makes a full card impractical | Priority policies and per-night continuation; the workflow never depends on completing a card |
| Provisional assets confuse existing surfaces | Explicit unlinked-asset presentation is a stated requirement on all three surfaces, not an afterthought |
| Transfer endpoints become an attack surface | Tailscale-only, device credential, loopback backend unchanged, per-route body limits |
| Writable archive root weakens the immutability invariant | Separate logical root; create-only write path; no rename/delete/overwrite API; scan roots untouched |
| Mobile proxy orientation regression | Validated against the same real-camera fixtures as the existing proxy recipe |
| Staging fills the server SSD | Capacity floor refuses new work while draining existing transfers |
| Travel work diverges from the desktop model | Provisional assets and link operations are ordinary project operations, not a parallel model |

## 17. Definition of done

Travel ingest is complete when the user can leave with a phone and an SD card, create a
trip project from the phone, inventory a card and immediately begin annotating, upload
originals across several unreliable nights without retransmitting committed data, review
arriving footage through mobile proxies, and return home to a desktop project whose notes
are intact and whose media resolves — whether it arrived over the network or from the card
in their pocket.

A demo that uploads one small file over a fast connection does not constitute completion.
