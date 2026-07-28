# T01-T003: Archive-to-Library Ingest and Content Dedupe

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "T01-T003",
  "epic_id": "T01",
  "title": "Archive-to-Library Ingest and Content Dedupe",
  "slug": "archive-ingest-and-dedupe",
  "status": "ready",
  "task_class": "implementation",
  "depends_on": [
    "T01-T002"
  ],
  "principal_invariant": "Every transfer-committed archive file enters the library directly in the verified state — without scheduling a redundant full-hash job — and content the library already holds is recognized rather than stored twice.",
  "central_falsification": "Upload a file with a known hash that already exists in the library. Verify the archive write is skipped, the existing media object is attached, and no full-hash job appears in the job queue. Then upload a novel file and verify no full-hash job is scheduled — it enters verified directly.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Transfer completion → library registration (S01-T003, S01-T005)"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ],
    "implementer": [
      "docs/tasks/travel/ARCHITECTURE.md",
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/STORAGE_LIBRARY.md"
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
      "docs/tasks/travel/tasks/T01-T003-archive-ingest-and-dedupe.md"
    ],
    "implementer": [
      "core/rcdvlog-server/src/transfer.rs",
      "core/rcdvlog-server/src/library.rs",
      "core/rcdvlog-server/src/media_facts.rs",
      "core/rcdvlog-server/src/representations.rs"
    ],
    "independent_tester": [
      "core/rcdvlog-server/tests/**"
    ],
    "integrator": []
  },
  "forbidden_paths": [
    "core/rcdvlog-core/src/catalog/**",
    "apps/desktop/**",
    "apps/web/**",
    "apps/mobile/**",
    "deploy/**"
  ],
  "verification_commands": [
    "cargo test -p rcdvlog-server",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/travel/EPIC.md",
      "regex": "\\| 30 \\| \\[T01-T003\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Register archived originals into the existing global media library as
already-verified media objects, deduplicate content that the library already
holds, and enqueue representation work — without scheduling a redundant hashing
pass.

## Principal invariant

This task is correct when every transfer-committed archive file enters the
library directly in the verified state — without scheduling a redundant full-hash
job — and content the library already holds is recognized rather than stored
twice.

## Inputs and outputs

Inputs:

- T01-T002 transfer completion hook
- S01-T005 scan pipeline library identity code
- S01-T006 representation job enqueue infrastructure
- Existing media-facts probe path

Outputs:

- Transfer object state transition from `archived` to `ingested`
- Verified-on-arrival library registration (no full-hash job scheduled)
- Storage object registration beneath the archive root
- Content dedupe: hash lookup against verified library content
- Metadata probing through existing media-facts path
- Thumbnail and mobile-proxy job enqueue per file as it lands
- Ingest outcome reporting through transfer diagnostics

## Frozen decisions

1. Ingest is idempotent; replaying it for an already-ingested object changes nothing.
2. Ingest runs after archive commit and never partially registers a file that failed to commit.
3. Registration reuses existing library identity code rather than reimplementing it.
4. No full-hash verification job is scheduled for transfer-ingested media.
5. Metadata probing failures mark the object ingested with a probe error, not failed.
6. Representation jobs are enqueued per file as it lands, not batched per card.
7. Representation job enqueue failures are retryable and do not roll back ingest.
8. A deduped object leaves no orphan file beneath the archive root.
9. Ingest errors are visible in transfer diagnostics with stable structured codes.
10. Ingest work is bounded so a large card cannot saturate the job engine and starve interactive work.

## Owned paths

- **Implementer:** `core/rcdvlog-server/src/transfer.rs` (ingest hook, state transition), `core/rcdvlog-server/src/library.rs` (verified-on-arrival registration), `core/rcdvlog-server/src/media_facts.rs` (probe invocation), `core/rcdvlog-server/src/representations.rs` (job enqueue)
- **Independent tester:** `core/rcdvlog-server/tests/` with known-hash generated media
- **Integrator:** no new integration paths

## Required interface

- Ingest hook consumed by T01-T002: `on_transfer_complete(object_id) -> IngestResult`
- Transfer diagnostics shape (consumed by T01-T008 for phone surfacing): differentiate "waiting to ingest", "ingested, probe failed", "ingested, representations pending"

## Acceptance oracle

1. A committed archive file registers as a media object in the verified state.
2. No full-hash verification job is scheduled for transfer-ingested media.
3. Metadata is probed and recorded through the existing media-facts path.
4. Replaying ingest for an ingested object is a no-op.
5. Uploading content the library already holds verified skips the archive write.
6. A deduped upload attaches the existing media object and leaves no orphan file.
7. Dedupe works when the existing copy is on an unavailable mount.
8. Thumbnail and mobile-proxy jobs are enqueued per file as it lands.
9. A probe failure marks the object ingested with a retryable probe error.
10. A job-enqueue failure does not roll back ingest and is retryable.
11. Ingesting a card's worth of files does not starve interactive server work.
12. No scan root is read, written, or re-hashed by this path.

## Independent test charter

Try to falsify: "transfer-ingested media never triggers a redundant hash job."
Use generated media with known hashes. Assert absence of full-hash jobs by
inspecting the job queue, not by timing. Test dedupe against a library seeded
through the ordinary scan path to prove the two paths agree on identity. Test
idempotency by replaying the ingest hook directly. Verify bounded concurrency by
enqueuing several hundred objects and observing that interactive requests remain
responsive.

## Review charter

Risk domain: library identity consistency and job engine load. Questions: (1) Is
a full-hash job ever scheduled for transfer-ingested media? (2) Can a dedupe
match leave an orphan file in the archive? (3) Does a probe failure prevent
ingest completion? (4) Can a large card saturate the job engine? Severity: P0 for
scheduling a redundant full-hash job on transfer-ingested media (wastes hours of
NAS I/O); P1 for orphan files, ingest blocking on probe failure, or job engine
saturation. Approval vocabulary: `APPROVE`, `APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- The mobile proxy recipe itself (T01-T004)
- Project asset creation and linking (T01-T007)
- Re-scanning or re-hashing existing scan roots
- Deletion or cleanup of duplicate archive files beyond declining to write them
- Perceptual or near-duplicate detection

## Stop conditions

Stop and escalate when:

- The verified-on-arrival path needs to duplicate the scan pipeline's full-hash job logic
- Dedupe requires access to project-scoped data
- Ingest batching is needed because per-file enqueue is too slow
- The transfer module needs to call library registration directly rather than through a hook

## Commands

```sh
cargo test -p rcdvlog-server
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

T01-T008 surfaces ingest state on the phone, so the diagnostics shape defined
here becomes a user-visible contract. Include enough detail to distinguish
"waiting to ingest", "ingested, probe failed", and "ingested, representations
pending".
