# Epic: Travel Ingest

**Status:** Planned
**Architecture:** [ARCHITECTURE.md](ARCHITECTURE.md)
**Owner:** RCD VlogCutter
**Target:** One iPhone, one Ubuntu server, one Mac, one user, Tailscale
**Builds on:** [Companion Server Epic](../server/EPIC.md)

## Outcome

Deliver a travel workflow in which a phone acts as SD-card courier and annotation terminal
while the home server ingests, verifies, archives, and processes originals. The user can
travel without a laptop, archive footage from hotels across several unreliable nights, and
annotate clips while their originals are still uploading.

The phone is a courier and a review terminal. It is never a third archive, and uploading is
never treated as permission to reuse a card.

## User-visible capabilities

At epic completion, the user can:

1. Pair an iPhone to the home server with a revocable device credential over Tailscale.
2. Create a trip project from the phone, without a Mac present.
3. Attach an SD card and inventory it in minutes.
4. See every clip on the card as a project asset — on the phone, web, and desktop —
   before any bytes have uploaded.
5. Write notes, labels, and removals against those clips immediately, online or offline.
6. Upload originals with an explicit priority policy rather than an all-or-nothing job.
7. Pause, resume, and continue uploads across nights, networks, and app restarts without
   retransmitting committed data.
8. Remove and reinsert a card, or shoot more footage onto a partially uploaded card,
   without losing progress or duplicating work.
9. Watch thumbnails and small mobile proxies appear as individual clips finish arriving.
10. Review those proxies on the phone, including offline from cache.
11. See where every clip actually is: on the card, staged, verified, archived, or ingested.
12. Return home, adopt the trip project on the desktop, and ingest whatever never uploaded
    without re-transferring what did.
13. Find every travel note intact and attached to media that now resolves.

## In scope

- Writable archive storage root with a create-only write path
- Resumable chunked transfer protocol with per-chunk and full-file verification
- Server-side staging on local SSD with capacity guards
- Atomic archive commit and direct entry into the verified media library
- Content dedupe against the existing global library
- New low-bandwidth mobile proxy recipe
- Server-origin project creation and desktop adoption of a server-created project
- Provisional project assets and the media link operation
- Expo custom-development-build iPhone application
- Card identity, inventory, and durable transfer queue with priority policies
- Phone offline annotation through the existing sync protocol
- Phone media cache with a bounded size budget
- Unlinked-asset presentation on desktop, web, and phone
- Travel acceptance over a bandwidth-limited network with real media

## Out of scope

- Any "safe to format" state or card-reuse licensing
- Full duplication of card contents into phone storage
- iOS background upload daemons
- Browser-based SD-card upload
- Public internet exposure of transfer endpoints
- Project deletion on any surface
- Archive rename, overwrite, deletion, or cleanup policy
- Library-wide mobile proxy generation
- Transcription, scene extraction, visual summaries, chronology suggestion, duplicate
  detection beyond exact content hash, and other compute-first enrichment
- Android, multiple phones, multiple users, multiple servers
- Trip context, chapters, events, scripts, edit plans, and Resolve export

## Architectural constraints

- The global 64 MB HTTP body limit is unchanged; chunk routes carry their own per-route limit.
- The server's loopback-only listener is unchanged; remote access is Tailscale Serve.
- Existing NAS scan roots remain `immutable_original` and read-only.
- The archive root is writable only along the create-new-file path.
- Partial uploads never appear beneath the archive root.
- Staging never lives on the NAS.
- The chunk ledger, not the staging file, is the restart-time source of truth.
- `cardRelativePath` is untrusted input and is containment-checked after canonicalization.
- Provisional assets and link operations are ordinary project operations, not a parallel model.
- `projectAssetId` is allocated by the phone at inventory time and never changes.
- The link operation is emittable by the server *and* the desktop.
- The phone never becomes an authority for original bytes.

## Delivery plan

### Server-side transfer and archive

| Task | Deliverable | Depends on |
| --- | --- | --- |
| [T1](TASK_T1_writable_archive_root_and_commit_path.md) | Writable archive root and atomic commit path | server 3, 5 |
| [T2](TASK_T2_resumable_transfer_protocol.md) | Resumable transfer sessions, chunking, and verification | T1, server 4 |
| [T3](TASK_T3_archive_ingest_and_dedupe.md) | Archive-to-library ingest and content dedupe | T2, server 5 |
| [T4](TASK_T4_mobile_proxy_recipe.md) | Low-bandwidth mobile proxy recipe | server 6 |

### Project model

| Task | Deliverable | Depends on |
| --- | --- | --- |
| [T5](TASK_T5_server_origin_projects_and_desktop_adoption.md) | Server-origin project creation and desktop adoption | server 7, 8, 10 |

### Phone application

| Task | Deliverable | Depends on |
| --- | --- | --- |
| [T6](TASK_T6_mobile_app_foundation.md) | Mobile app foundation, pairing, sync, and offline notes | server 9, T5 |
| [T7](TASK_T7_card_inventory_and_provisional_assets.md) | Card inventory and provisional assets | T2, T5, T6 |
| [T8](TASK_T8_transfer_queue_and_phone_review.md) | Transfer queue, priority policies, and phone review | T3, T4, T7 |

### Acceptance

| Task | Deliverable | Depends on |
| --- | --- | --- |
| [T9](TASK_T9_travel_acceptance.md) | End-to-end travel acceptance | T8 |

T1–T4 are server-only and can proceed before the phone application exists. T5 is the one
task blocked on unbuilt companion-server work.

## Dependencies on unbuilt companion-server work

This is accepted and recorded so it is not discovered late.

| Needed | Task | Actual status |
| --- | --- | --- |
| Desktop pairing, root mapping, baseline adoption | [server 10](../server/TASK_10_desktop_pairing_root_mapping_and_baseline.md) | **Not implemented — blocks T5** |
| Web library and project browsing | [server 14](../server/TASK_14_web_library_project_browse_and_add.md) | Not implemented; affects web fallback only |
| Web proxy review and annotation | [server 15](../server/TASK_15_web_proxy_review_and_annotations.md) | Not implemented; affects web fallback only |
| Authentication and secure transport | [server 9](../server/TASK_9_authentication_and_secure_transports.md) | Core implemented; task file marker is stale |

Several companion-server task files carry stale `Status:` markers. Trust the code and git
history over the markers until server Task 20 reconciles them.

## Milestones

### Milestone T-A: The server can receive and archive footage

Tasks T1–T3 complete.

Exit criteria:

- A writable archive root exists and scan roots are provably untouched.
- A large file uploads in chunks, survives interruption and server restart, and resumes
  without retransmitting committed data.
- A verified file commits atomically into the archive and enters the media library already
  verified, with no second hashing pass.
- Re-uploading identical content dedupes instead of duplicating.

### Milestone T-B: A project can begin without a Mac

Tasks T4–T5 complete.

Exit criteria:

- A project created on the server adopts cleanly onto the desktop and is thereafter
  indistinguishable from a desktop-created project.
- The mobile proxy recipe produces correctly oriented output validated against real-camera
  fixtures.

### Milestone T-C: The phone is a working courier

Tasks T6–T8 complete.

Exit criteria:

- The phone pairs, syncs, and annotates offline.
- A card inventories into provisional assets that are annotatable before any upload.
- Notes written against a provisional asset survive the link operation unchanged.
- The queue honors priority policies and survives card removal, network loss, and app restart.
- Arriving clips become reviewable on the phone through mobile proxies.

### Milestone T-D: A real trip works

Task T9 complete.

Exit criteria:

- A multi-night, bandwidth-limited, interrupted upload of real camera media completes.
- Return-home reconciliation links card-ingested media to waiting provisional assets.
- No original is written, renamed, or deleted anywhere outside the archive root.

## Cross-cutting acceptance criteria

- [ ] No test or implementation writes, renames, or deletes anything under a scan root.
- [ ] No partial file ever appears beneath the archive root.
- [ ] No staging file is ever written to the NAS.
- [ ] The global 64 MB body limit is unchanged for non-transfer routes.
- [ ] The server listener remains loopback-only.
- [ ] An adversarial `cardRelativePath` cannot escape the archive root.
- [ ] A committed chunk is never silently overwritten by a differing re-upload.
- [ ] Server restart mid-transfer loses no committed chunk.
- [ ] `projectAssetId` is stable from inventory through linking.
- [ ] Notes on an unlinked asset survive linking with no rewrite.
- [ ] The desktop can link a locally ingested media object to a waiting provisional asset.
- [ ] Desktop, web, and phone all render an unlinked asset coherently.
- [ ] No user-facing surface presents a "safe to format" state.
- [ ] Transfer endpoints require a device credential and are unreachable off the tailnet.
- [ ] Existing desktop offline behavior, keyboard contract, and project behavior do not regress.
- [ ] A trip whose uploads never finished still yields a complete desktop project on return.

## Testing gates

```bash
pnpm --dir apps/desktop build
pnpm --dir apps/web build
pnpm --dir apps/mobile typecheck
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm test:integration
```

`pnpm --dir apps/mobile typecheck` is introduced by T6 along with the application itself.
As with the companion-server epic, exact script names may be introduced by the task that
owns them, and the final documentation must describe the checked-in commands exactly.

Manual acceptance uses a real SD card, a real reader, real camera media, and a
bandwidth-limited network. Transfer behavior cannot be validated on a fast local link.

## Primary risks and mitigations

| Risk | Mitigation |
| --- | --- |
| iOS suspends the app and stalls overnight uploads | Resume protocol makes this cost time, not data; keep-awake mode; honest expectations in the UI |
| Hotel bandwidth makes a full card impractical | Priority policies and per-night continuation; the workflow never depends on completing a card |
| Provisional assets confuse existing surfaces | Explicit unlinked-asset presentation required on all three surfaces |
| Writable archive root weakens immutability | Separate logical root, create-only path, no rename/delete/overwrite API |
| Mobile proxy orientation regression | Validated against the same real-camera fixtures as the existing proxy |
| Staging fills the server SSD | Capacity floor refuses new work while draining existing transfers |
| T5 blocked on unbuilt server Task 10 | T1–T4 are independent and can proceed first |
| Transfer treated as a backup system | Section 3 of the architecture states the durability bar; no state licenses card reuse |

## Definition of done

This epic is complete only when all nine task files satisfy their acceptance criteria and
T9 records evidence from a real multi-night trip simulation using real camera media over a
constrained network. A single small file uploaded over a fast local link does not
constitute completion.
