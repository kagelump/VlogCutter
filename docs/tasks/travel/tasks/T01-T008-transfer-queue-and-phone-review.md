# T01-T008: Transfer Queue, Priority Policies, and Phone Review

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "T01-T008",
  "epic_id": "T01",
  "title": "Transfer Queue, Priority Policies, and Phone Review",
  "slug": "transfer-queue-and-phone-review",
  "status": "ready",
  "task_class": "implementation",
  "depends_on": [
    "T01-T003",
    "T01-T004",
    "T01-T007"
  ],
  "principal_invariant": "The upload queue survives app restart, streams from the card without a full local copy, respects priority policies, and the phone's review surface plays arriving mobile proxies — including offline — while never presenting a 'safe to format' state.",
  "central_falsification": "Start a multi-file upload with a priority policy, pause mid-upload, force-quit the app, restart, and assert the queue state is intact with pause preserved. Resume, complete some files, verify they are reviewable through mobile proxies with the network disabled. Then remove the card and assert the queue pauses cleanly rather than failing.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Phone queue → chunk upload routes (T01-T002)"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ],
    "implementer": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ],
    "independent_tester": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ],
    "reviewer": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ],
    "integrator": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/travel/tasks/T01-T008-transfer-queue-and-phone-review.md"
    ],
    "implementer": [
      "apps/mobile/src/**",
      "packages/project-client/**",
      "core/rcdvlog-server/src/transfer.rs"
    ],
    "independent_tester": [
      "apps/mobile/__tests__/**"
    ],
    "integrator": []
  },
  "forbidden_paths": [
    "apps/desktop/**",
    "apps/web/src/**",
    "core/rcdvlog-core/src/catalog/**",
    "deploy/**"
  ],
  "verification_commands": [
    "pnpm --dir apps/mobile typecheck",
    "cargo test -p rcdvlog-server",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/travel/EPIC.md",
      "regex": "\\| 80 \\| \\[T01-T008\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Give the phone a durable upload queue with real priority policies, a streaming
read path that never duplicates the card, and a review surface that plays
arriving mobile proxies — including offline.

## Principal invariant

This task is correct when the upload queue survives app restart, streams from the
card without a full local copy, respects priority policies, and the phone's
review surface plays arriving mobile proxies — including offline — while never
presenting a "safe to format" state.

## Inputs and outputs

Inputs:

- T01-T002 transfer routes and resume query
- T01-T003 ingest state diagnostics
- T01-T004 mobile proxy recipe and streaming endpoints
- T01-T007 card inventory persistence

Outputs:

- Durable local upload queue backed by SQLite, surviving app restart
- Streaming read path: SD card file → bounded buffer → chunk upload (never full local copy)
- Priority policies: everything, today only, smallest first, this folder only, pinned first
- Pause, resume, reorder (does not restart in-flight object)
- Per-card and per-object progress, throughput, estimated remaining time
- Network transition and card removal handling
- Keep-awake mode, completion/failure notifications
- Mobile proxy and thumbnail playback with LRU-bounded media cache
- Transfer and ingest state surfacing from T01-T002 and T01-T003

## Frozen decisions

1. Queue state is durable in local SQLite and survives app restart, crash, and force-quit.
2. The server's resume query is authoritative; local queue state is a rebuildable cache.
3. Only one object uploads at a time by default, with concurrency configurable and bounded.
4. Chunk uploads retry with bounded exponential backoff, never spin against a hard failure.
5. Pause is immediate and leaves committed chunks intact.
6. Reordering does not restart the in-flight object.
7. The card is opened read-only; nothing is ever written to it.
8. Progress reflects committed chunks, not bytes optimistically pushed.
9. The media cache has a hard size budget with LRU eviction and visible current size.
10. Mobile proxies fetched on demand and by explicit prefetch, never speculatively for a whole card.
11. Cached proxies remain playable with no network.
12. No surface presents a "safe to format" state.
13. Transfer, ingest, and representation states are distinguishable in the UI.

## Owned paths

- **Implementer:** `apps/mobile/` (queue engine, streaming reader, playback, cache, notifications, native module surface for external-volume streaming and keep-awake), `packages/project-client/` (transfer and representation contracts), `core/rcdvlog-server/src/transfer.rs` (resume query paging and indexing for large cards)
- **Independent tester:** `apps/mobile/` physical-device acceptance
- **Integrator:** no new integration paths

## Required interface

- Transfer and representation typed contracts consumed by phone
- Server resume query (paginated, coalesced ranges) for queue rebuild
- Ingest state diagnostics consumed by phone UI

## Acceptance oracle

1. Queue state survives app restart, crash, and force-quit.
2. Local queue state can be rebuilt entirely from the server's resume query.
3. Uploads stream from the card without a full local copy.
4. Peak phone storage use during a large card upload stays within a small bounded budget.
5. The full-file hash is computed during the upload read, not in a separate pass.
6. Each priority policy orders the queue as specified.
7. A clip pinned from the review surface moves to the front without restarting the in-flight object.
8. Pause is immediate and preserves committed chunks.
9. Resume after pause retransmits no committed chunk.
10. A network transition mid-upload resumes without data loss.
11. Card removal pauses cleanly; reinsertion resumes via card identity.
12. Retries use bounded backoff and stop against terminal errors.
13. Progress reflects committed chunks only.
14. Completion and failure notifications fire with the app backgrounded.
15. Thumbnails and mobile proxies play, and seek, on the phone.
16. Cached proxies play with no network.
17. The media cache respects its size budget and evicts by LRU.
18. Proxies are never prefetched for an entire card automatically.
19. Transfer, ingest, and representation states are distinguishable in the UI.
20. No surface presents a "safe to format" state.
21. Nothing is written, renamed, or deleted on the SD card.

## Independent test charter

Try to falsify: "the queue is durable and streaming is efficient." Physical
device, real card, real reader, real camera media, bandwidth-limited network.
Test across actual network change (wifi to cellular), actual force-quit, actual
card removal. Verify card read-only with full checksum manifest before and after.
Measure peak phone storage during multi-gigabyte upload and assert against
declared budget. Verify no committed chunk is retransmitted after any
interruption class.

## Review charter

Risk domain: upload durability and card safety. Questions: (1) Can queue state be
lost on force-quit? (2) Is the card ever written to? (3) Is a full local copy
ever created? (4) Does progress reflect committed or optimistic bytes? (5) Is a
"safe to format" state ever presented? Severity: P0 for card writes, "safe to
format" presentation, or full local copy creation; P1 for queue state loss or
misleading progress. Approval vocabulary: `APPROVE`,
`APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- Any "safe to format" state
- Full duplication of card contents into phone storage
- Background upload daemons
- Automatic policy selection or learned prioritization
- Server-side remote diagnostics UI

## Stop conditions

Stop and escalate when:

- The streaming read path cannot avoid a full local copy on iOS
- Background continuation is required for correctness rather than best-effort
- The queue needs server-side state that isn't available from the resume query
- A "safe to format" state is needed by any UI flow

## Commands

```sh
pnpm --dir apps/mobile typecheck
cargo test -p rcdvlog-server
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

T01-T009 exercises this across multiple simulated nights. Anything this task
leaves as best-effort — particularly background continuation — must be documented
as such so T01-T009 tests the promised behavior rather than the hoped-for
behavior.
