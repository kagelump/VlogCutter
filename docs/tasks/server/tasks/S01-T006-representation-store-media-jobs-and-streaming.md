# S01-T006: Representation Store, Media Jobs, and Streaming

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "S01-T006",
  "epic_id": "S01",
  "title": "Representation Store, Media Jobs, and Streaming",
  "slug": "representation-store-media-jobs-and-streaming",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": [
    "S01-T004",
    "S01-T005"
  ],
  "principal_invariant": "Every representation is generated through a durable job, validated before commit, atomically committed, identified by verified input plus versioned recipe, and streamable through byte-range requests — and output that fails validation is never committed.",
  "central_falsification": "Enqueue a thumbnail job for verified media, verify the output is atomically committed under the correct derived-media path, verify byte-range seeking works without downloading the full file, then deliberately produce invalid output (zero-duration, wrong dimensions) and assert the commit is rejected.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Representation jobs → durable job engine (S01-T004)"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/REPRESENTATIONS_AND_STREAMING.md"
    ],
    "implementer": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/REPRESENTATIONS_AND_STREAMING.md"
    ],
    "independent_tester": [
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "reviewer": [
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "integrator": [
      "docs/tasks/server/ARCHITECTURE.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/server/tasks/S01-T006-representation-store-media-jobs-and-streaming.md"
    ],
    "implementer": [
      "core/rcdvlog-server/src/**",
      "apps/server/src/lib.rs"
    ],
    "independent_tester": [
      "core/rcdvlog-server/tests/**"
    ],
    "integrator": [
      "Cargo.toml"
    ]
  },
  "forbidden_paths": [
    "core/rcdvlog-core/src/catalog/**",
    "apps/desktop/**",
    "apps/web/src/**",
    "deploy/**"
  ],
  "verification_commands": [
    "cargo test -p rcdvlog-server",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/server/EPIC.md",
      "regex": "\\| 60 \\| \\[S01-T006\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Implement representation generation as durable jobs, validated output with atomic
commit, recipe-versioned identity, and authenticated byte-range media delivery.

## Principal invariant

This task is correct when every representation is generated through a durable
job, validated before commit, atomically committed, identified by verified input
plus versioned recipe, and streamable through byte-range requests — and output
that fails validation is never committed.

## Inputs and outputs

Inputs:
- S01-T004 durable job engine and child-process cancellation contract
- S01-T005 verified content identity

Outputs:
- Quality-1 thumbnail and H.264 720p proxy recipes as durable job handlers
- FFmpeg integration with the child-process cancellation contract
- Output validation: probe-able, non-zero duration, expected dimensions
- Atomic commit into derived-media storage
- Recipe-versioned representation identity
- Authenticated byte-range media delivery
- Representation manifest and availability tracking

## Frozen decisions

1. A representation is immutable and identified by verified input plus versioned recipe.
2. Output is validated before commit; invalid output is never committed.
3. Commit is atomic; a partial representation is never visible.
4. Recipes are versioned; changing settings requires a new recipe key.
5. Current recipes: quality-1 thumbnail and H.264 720p proxy with orientation handling.
6. FFmpeg concurrency is bounded (one proxy worker by default).
7. Byte-range requests support Chrome seeking without full download.
8. Representations are disposable; losing one is a regeneration, not a data loss.

## Owned paths

- **Implementer:** `core/rcdvlog-server/src/representations.rs`, `core/rcdvlog-server/src/media_jobs.rs`, `apps/server/src/lib.rs` (media routes)
- **Independent tester:** `core/rcdvlog-server/tests/` with real-camera orientation fixtures
- **Integrator:** workspace `Cargo.toml` (FFmpeg dependency)

## Required interface

- Representation enqueue API consumed by S01-T005 (post-scan) and S01-T014 (web)
- Media streaming endpoints with byte-range support
- Representation DTOs for project-client

## Acceptance oracle

1. Thumbnail and proxy recipes generate through durable jobs.
2. Output is validated (probe-able, non-zero, correct dimensions) before commit.
3. Invalid output is rejected and never committed.
4. Commit is atomic; partial representations are not visible.
5. Representations are identified by verified input plus versioned recipe.
6. Byte-range requests permit Chrome seeking without full download.
7. FFmpeg concurrency respects the configured bound.
8. Cancelling a running FFmpeg job terminates the child process.
9. Regenerating a deleted representation produces an identical result.

## Independent test charter

Try to falsify: "invalid output is never committed." Deliberately produce
zero-duration output, wrong-dimension output, and truncated output. Verify each
is rejected before commit and no partial file remains. Test orientation against
real-camera fixtures (Sony, DJI, iPhone) and assert output orientation through
`ffprobe`. Test byte-range seeking with Chrome-typical range headers. Test
cancellation by killing a running FFmpeg job mid-generation.

## Review charter

Risk domain: representation correctness and delivery. Questions: (1) Can invalid
output be committed? (2) Is orientation correct on real-camera sources? (3) Does
byte-range delivery work for Chrome seeking? (4) Is FFmpeg concurrency bounded?
Severity: P0 for invalid output committed or orientation broken on real cameras;
P1 for missing byte-range support or unbounded concurrency. Approval vocabulary:
`APPROVE`, `APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- Mobile proxy recipe (T01-T004)
- Library-wide representation backfill
- Hardware transcoding
- Adaptive bitrate or multiple quality tiers

## Stop conditions

Stop and escalate when:

- A recipe needs per-camera-model tuning rather than one settings key
- FFmpeg concurrency cannot be bounded at the job-engine level
- Byte-range delivery requires loading the full file into memory
- Representation storage needs to live on the NAS

## Commands

```sh
cargo test -p rcdvlog-server
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

Representation jobs and media streaming are consumed by S01-T012 (remote exchange),
S01-T014 (web library), S01-T015 (web review), and T01-T004 (mobile proxy recipe).
