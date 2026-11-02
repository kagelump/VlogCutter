# V01-T005: Speech mask weights in matching

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "V01-T005",
  "epic_id": "V01",
  "title": "Speech mask weights in matching",
  "slug": "speech-mask",
  "status": "ready",
  "task_class": "implementation",
  "depends_on": [
    "V01-T001"
  ],
  "principal_invariant": "Frames inside the session speech mask contribute nothing to any similarity score for that query, and the mask is part of the query's cache identity.",
  "central_falsification": "Run the same query twice with different loud noise inside the masked span: the reports' similarities must be equal; then change only the mask and prove the cache key and mask_digest change.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "SoundtrackSessionSpec.speech_mask -> match_pcm frame weights"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/tv-review/ARCHITECTURE.md",
      "docs/tasks/tv-review/EPIC.md"
    ],
    "implementer": [
      "docs/tasks/tv-review/ARCHITECTURE.md",
      "AGENTS.md",
      "docs/tv-soundtrack-matching.md",
      "docs/tv-review-local-session.md",
      "docs/tv-review-sync.md"
    ],
    "independent_tester": [
      "docs/tasks/tv-review/ARCHITECTURE.md",
      "docs/tv-soundtrack-matching.md"
    ],
    "reviewer": [
      "docs/tasks/tv-review/ARCHITECTURE.md",
      "AGENTS.md",
      "docs/tv-soundtrack-matching.md"
    ],
    "integrator": [
      "docs/tasks/tv-review/EPIC.md",
      "docs/tasks/tv-review/ARCHITECTURE.md",
      "AGENTS.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/tv-review/tasks/V01-T005-speech-mask.md"
    ],
    "implementer": [
      "core/rcdvlog-core/src/tv_soundtrack.rs",
      "core/rcdvlog-core/src/tv_soundtrack_tests.rs",
      "core/rcdvlog-core/src/tv_soundtrack_session.rs",
      "core/rcdvlog-core/src/tv_soundtrack_session_tests.rs"
    ],
    "independent_tester": [
      "core/rcdvlog-core/tests/v01_t005_mask_independent.rs"
    ],
    "integrator": [
      "docs/tasks/tv-review/EPIC.md",
      "docs/task-cycles/V01-T005/**"
    ]
  },
  "forbidden_paths": [
    "apps/desktop/**",
    "apps/web/**",
    "apps/server/**",
    "packages/**",
    "core/rcdvlog-server/**",
    "core/rcdvlog-sync/**",
    "core/rcdvlog-core/src/chronology/**",
    "core/rcdvlog-core/src/editplan/**",
    "../data/chronology-audit/**",
    "../data/editorial-review/rc6-passive-sync-2026-11-03/**",
    "../source/**"
  ],
  "verification_commands": [
    "cargo test -p rcdvlog-core tv_soundtrack",
    "cargo test -p rcdvlog-core tv_review_sync",
    "cargo clippy -p rcdvlog-core --all-targets -- -D warnings",
    "cargo fmt --all -- --check",
    "cargo test -p rcdvlog-core --test v01_t005_mask_independent"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/tv-review/EPIC.md",
      "regex": "\\| \\[V01-T005\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Implement the §6 mask: per-frame weights in `similarity`, the `masked_insufficient` rule and `mask_digest`, with session plumbing from `speech_mask` to the weights.

## Principal invariant

This task is correct when frames inside the session speech mask contribute nothing to any similarity score for that query, and the mask is part of the query's cache identity.

## Inputs and outputs

Inputs: speech_mask intervals on the capture clock. Outputs: weighted scores, mask_digest, cache key including the mask.

## Frozen decisions

§6 thresholds (2 s total, 0.5 s per third); weights in [0,1]; an empty mask gives exactly v1 bytes. Also apply all of ARCHITECTURE.md §2.

## Owned paths

- Contract editor: this packet only.
- Implementer: `core/rcdvlog-core/src/tv_soundtrack.rs`, `core/rcdvlog-core/src/tv_soundtrack_tests.rs`, `core/rcdvlog-core/src/tv_soundtrack_session.rs`, `core/rcdvlog-core/src/tv_soundtrack_session_tests.rs`.
- Independent tester: `core/rcdvlog-core/tests/v01_t005_mask_independent.rs`.
- Integrator: `docs/tasks/tv-review/EPIC.md`, `docs/task-cycles/V01-T005/**`.
- Forbidden paths always win.

## Required interface

`match_pcm` gains an optional weights parameter. Update all callers; no other public signature changes.

## Acceptance oracle

(1) Masked-noise invariance. (2) Reference audio plus loud speech-like bursts in 60% of frames: unmatched without the mask, matched with it (distinctive policy on). (3) A query almost fully masked gives masked_insufficient. (4) No mask gives bytes identical to main.

## Independent test charter

Claim to falsify: Run the same query twice with different loud noise inside the masked span: the reports' similarities must be equal; then change only the mask and prove the cache key and mask_digest change. Generate speech-like interference differently from the implementer's (e.g. amplitude-modulated formant tones). Build the mask through SoundtrackSessionSpec JSON, not by calling match_pcm directly. Required failure behaviour: wrong or ambiguous evidence must stay unmatched, ambiguous or unknown, and must never produce an anchor. Preservation: existing soundtrack, session and sync tests stay green. Evidence: command output and test names.

## Review charter

Risk: weights applied to only one side of the cosine; mask/frame index misalignment by WARMUP or HOP. Ask: are frame boundaries converted from capture samples correctly at 48 kHz → 8 kHz? Approval: APPROVE / REQUEST_CHANGES / BLOCK.

## Non-goals

- Gap-placed windows (T006).
- Reading transcripts in core.

## Stop conditions

Stop and escalate when:

- a change would alter the serialized bytes or digest of any existing v1 object (§2);
- you need a production path not listed under implementer ownership, a new crate dependency, or a SQLite migration;
- a frozen decision in ARCHITECTURE.md seems wrong or insufficient. Report it and do not redesign;
- the independent oracle would have to be weakened, or thresholds changed, to make tests pass;
- the work no longer fits one focused context.

## Commands

Run these from `vlogcutter/`; all must pass:

```sh
cargo test -p rcdvlog-core tv_soundtrack
cargo test -p rcdvlog-core tv_review_sync
cargo clippy -p rcdvlog-core --all-targets -- -D warnings
cargo fmt --all -- --check
cargo test -p rcdvlog-core --test v01_t005_mask_independent
```

## Handoff

List changed paths, every command run with its complete result, compatibility effects (v1 digests unchanged?), known limitations, and any stop condition hit. The implementer does not approve the task.
