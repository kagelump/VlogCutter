# T01-T002: Resumable Transfer Sessions, Chunking, and Verification

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "T01-T002",
  "epic_id": "T01",
  "title": "Resumable Transfer Sessions, Chunking, and Verification",
  "slug": "resumable-transfer-protocol",
  "status": "ready",
  "task_class": "foundation",
  "depends_on": [
    "T01-T001"
  ],
  "principal_invariant": "An upload interrupted by network loss, app restart, card removal, or server restart resumes without retransmitting a single committed byte — the chunk ledger, not the staging file, is the restart-time source of truth.",
  "central_falsification": "Upload a multi-chunk file, kill the server between chunks, restart, query the resume endpoint, and assert every committed chunk is reported and no committed chunk is re-requested. Then kill mid-chunk and assert the interrupted chunk was never committed and must be retried whole.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Transfer routes → chunk ledger → archive commit path (T01-T001)",
    "Resume query → phone queue (T01-T008)"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ],
    "implementer": [
      "docs/tasks/travel/ARCHITECTURE.md",
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "independent_tester": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ],
    "reviewer": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ],
    "integrator": [
      "docs/tasks/travel/ARCHITECTURE.md",
      "docs/tasks/server/ARCHITECTURE.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/travel/tasks/T01-T002-resumable-transfer-protocol.md"
    ],
    "implementer": [
      "core/rcdvlog-server/src/transfer.rs",
      "core/rcdvlog-server/src/config.rs",
      "apps/server/src/lib.rs"
    ],
    "independent_tester": [
      "core/rcdvlog-server/tests/**",
      "apps/server/tests/**"
    ],
    "integrator": [
      "packages/project-client/**"
    ]
  },
  "forbidden_paths": [
    "core/rcdvlog-core/src/catalog/**",
    "apps/desktop/**",
    "apps/web/**",
    "apps/mobile/**"
  ],
  "verification_commands": [
    "cargo test -p rcdvlog-server",
    "cargo test -p rcdvlog-server --test integration",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/travel/EPIC.md",
      "regex": "\\| 20 \\| \\[T01-T002\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Implement the original-media transfer service: card sessions, inventory
declaration, resumable chunk upload, restart-safe progress, and end-to-end
verification before archive commit.

## Principal invariant

This task is correct when an upload interrupted by network loss, app restart,
card removal, or server restart resumes without retransmitting a single committed
byte — the chunk ledger, not the staging file, is the restart-time source of
truth.

## Inputs and outputs

Inputs:

- T01-T001 archive commit path
- S01-T004 durable job engine patterns (for event notifications)
- Existing HTTP listener and authentication infrastructure

Outputs:

- `/api/v1/transfer/` route group (6 endpoints) authenticated by device credential
- Card session create/resume, idempotent on card identity
- Paged inventory declaration
- Resume query returning committed chunk ranges
- Chunk upload with per-chunk BLAKE3 verification, idempotent re-upload
- Sparse staging file with positioned writes on server SSD
- Chunk ledger persistence and restart recovery
- Completion: gap check, size check, full-file BLAKE3, archive commit
- Object abandonment and staging reclamation
- Per-route body limits sized to configured chunk size

## Frozen decisions

1. The global 64 MB `DefaultBodyLimit` is unchanged. Chunk routes carry their own per-route limit.
2. A chunk is committed to the ledger only after its bytes are durably written.
3. Ledger and staging file reconciled on startup; unconfirmable ledger rows are dropped.
4. Completion requires gap-free ledger whose total equals declared size.
5. Full-file BLAKE3 computed by server over staged file, compared to phone declaration.
6. Archive commit path from T01-T001 called only with fully verified staged file.
7. Staging capacity below floor refuses new sessions/objects while existing transfers drain.
8. Abandonment reclaims staging promptly and is idempotent.
9. All transfer state is server-side and survives restart; phone state is a rebuildable cache.
10. Errors use stable structured codes distinguishing retryable from terminal.
11. Credentials never appear in URL, query string, or log line.
12. Transfer progress must not block or delay project operation synchronization.

## Owned paths

- **Implementer:** `core/rcdvlog-server/src/transfer.rs` (new: sessions, ledger, state machine, staging), `core/rcdvlog-server/src/config.rs` (chunk size, capacity floors, staging root), `apps/server/src/lib.rs` (transfer routes, per-route body limits)
- **Independent tester:** `core/rcdvlog-server/tests/`, `apps/server/tests/` with fault-injection HTTP layer
- **Integrator:** `packages/project-client/` (typed transfer contracts)

## Required interface

- Transfer routes and their typed contracts for phone consumption (T01-T008)
- Ingest hook for T01-T003 (transition from `archived` to `ingested`)
- Resume query with coalesced chunk ranges, paginated for large cards

## Acceptance oracle

1. A multi-chunk upload completes, verifies, and commits to the archive.
2. An upload interrupted mid-chunk resumes without retransmitting committed chunks.
3. A server restart mid-transfer preserves every committed chunk and no more.
4. A ledger row whose bytes cannot be confirmed on startup is dropped, not trusted.
5. Re-uploading a committed chunk with matching hash is a no-op `200`.
6. Re-uploading a committed chunk with differing hash returns `409` and writes nothing.
7. A gap in the ledger prevents completion with a clear error.
8. A declared-size mismatch prevents completion.
9. A full-file hash mismatch discards staging and leaves the object retryable.
10. A partial file never appears beneath the archive root.
11. No staging file is written to a NAS mount.
12. `POST /cards` with the same card identity resumes rather than duplicating.
13. Re-inventorying a card with new footage adds objects and preserves existing progress.
14. Non-transfer routes still enforce the 64 MB limit.
15. Transfer requests without a valid device credential are rejected.
16. Project operation sync continues normally while a large transfer runs.

## Independent test charter

Try to falsify: "the chunk ledger survives anything." Test against a
fault-injecting HTTP layer that drops connections mid-body, truncates bodies,
delays, and returns 5xx — exercising the real chunk route. Add a property test
asserting that arbitrary interleavings of interruption, restart, and resume
converge to byte-identical output. Kill the server process between chunks to
validate ledger-driven recovery. Use generated media large enough to require many
chunks. Verify that committed chunks are never re-requested and interrupted
chunks are retried whole.

## Review charter

Risk domain: data integrity and resume correctness. Questions: (1) Can committed
chunks be lost on restart? (2) Can a corrupted chunk be committed? (3) Can a
hash mismatch lead to a corrupted archive file? (4) Can the ledger and staging
file disagree? (5) Does the per-route body limit actually apply? Severity: P0 for
committed chunk loss, corrupted archive commit, or ledger inconsistency; P1 for
staging leak or incorrect resume ranges. Approval vocabulary: `APPROVE`,
`APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- Media library registration and dedupe (T01-T003)
- Representation generation (T01-T004)
- Project assets and operations (T01-T007)
- Client-side queue policy (T01-T008)
- Any "safe to format" state
- Raising the global HTTP body limit

## Stop conditions

Stop and escalate when:

- The per-route body limit doesn't actually override the global limit
- The chunk ledger needs coordination with project operation sync
- Staging capacity enforcement needs to interrupt committed work
- Transfer state depends on the phone retaining anything beyond a credential

## Commands

```sh
cargo test -p rcdvlog-server
cargo test -p rcdvlog-server --test integration
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

T01-T003 hooks the transition from `archived` to `ingested`. Expose a commit hook
rather than having the transfer module call into the library directly, so the two
remain testable apart. T01-T008's queue depends on the resume query returning
coalesced ranges cheaply for cards with thousands of objects — paginate it and
index accordingly.
