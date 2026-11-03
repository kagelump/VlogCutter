# V01-T002: Report best peaks for every searched window

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "V01-T002",
  "epic_id": "V01",
  "title": "Report best peaks for every searched window",
  "slug": "peak-diagnostics",
  "status": "ready",
  "task_class": "implementation",
  "depends_on": [
    "V01-T001"
  ],
  "principal_invariant": "Every report and every completed session query that reached the search exposes its top peaks with movie_seconds computed per §3, while state, reason and candidates stay identical to before.",
  "central_falsification": "Bury a known reference excerpt at a known movie time under noise so it stays unmatched, run it through the session plan run_batch/evidence_page, and check that best_peaks[0].movie_seconds is within 0.016 s of the true time.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "match_pcm report -> session evidence_page merge"
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
      "docs/tasks/tv-review/tasks/V01-T002-peak-diagnostics.md"
    ],
    "implementer": [
      "core/rcdvlog-core/src/tv_soundtrack.rs",
      "core/rcdvlog-core/src/tv_soundtrack_tests.rs",
      "core/rcdvlog-core/src/tv_soundtrack_session.rs",
      "core/rcdvlog-core/src/tv_soundtrack_session_tests.rs"
    ],
    "independent_tester": [
      "core/rcdvlog-core/tests/v01_t002_peaks_independent.rs"
    ],
    "integrator": [
      "docs/tasks/tv-review/EPIC.md",
      "docs/task-cycles/V01-T002/**"
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
    "cargo test -p rcdvlog-core --test v01_t002_peaks_independent"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/tv-review/EPIC.md",
      "regex": "\\| \\[V01-T002\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Fill `best_peaks` (§3) in `match_pcm` for matched, ambiguous and unmatched reports, and merge them across references in `SessionQueryEvidence`. This removes the blind spot that hid the RC6 near-miss.

## Principal invariant

This task is correct when every report and every completed session query that reached the search exposes its top peaks with movie_seconds computed per §3, while state, reason and candidates stay identical to before.

## Inputs and outputs

Inputs: existing peak search in `match_pcm`. Outputs: `best_peaks` in reports and in session query evidence.

## Frozen decisions

§3 formula for movie_seconds; maximum of 4 peaks; 0.256 s merge distance; sort order. Quiet and insufficient-variation reports keep best_peaks empty, which leaves their bytes unchanged. Also apply all of ARCHITECTURE.md §2.

## Owned paths

- Contract editor: this packet only.
- Implementer: `core/rcdvlog-core/src/tv_soundtrack.rs`, `core/rcdvlog-core/src/tv_soundtrack_tests.rs`, `core/rcdvlog-core/src/tv_soundtrack_session.rs`, `core/rcdvlog-core/src/tv_soundtrack_session_tests.rs`.
- Independent tester: `core/rcdvlog-core/tests/v01_t002_peaks_independent.rs`.
- Integrator: `docs/tasks/tv-review/EPIC.md`, `docs/task-cycles/V01-T002/**`.
- Forbidden paths always win.

## Required interface

No new public functions. Fields from T001 become populated.

## Acceptance oracle

(1) A synthetic unmatched query has best_peaks with the correct position. (2) A matched query's best_peaks[0] equals candidates[0]'s offset. (3) The session merge dedupes the same movie position seen by two overlapping references. (4) A quiet query has empty best_peaks.

## Independent test charter

Claim to falsify: Bury a known reference excerpt at a known movie time under noise so it stays unmatched, run it through the session plan run_batch/evidence_page, and check that best_peaks[0].movie_seconds is within 0.016 s of the true time. Use a different synthetic generator from the implementer's (e.g. a chirp train plus pink noise at a different seed scheme). Exercise the session path end-to-end with a real WAV file in a tempdir, through run_batch and evidence_page. Required failure behaviour: wrong or ambiguous evidence must stay unmatched, ambiguous or unknown and must never be Matched. Amended 2026-11-03 (user decision): with `distinctive` unset, v1 ambiguous clusters still emit conflicting-alternative anchors, as the existing v1 test `distant_repeated_soundtrack_stays_ambiguous` requires (ARCHITECTURE §2). The "no anchor for ambiguous" rule applies only on the §5 distinctive path (T004). Preservation: existing soundtrack, session and sync tests stay green. Evidence: command output and test names.

## Review charter

Risk: off-by-half-window time errors and silent state changes. Ask: is movie_seconds the query centre (not its start)? Does any state or reason differ from main on the existing tests? Approval: APPROVE / REQUEST_CHANGES / BLOCK.

## Non-goals

- Changing acceptance thresholds.
- Global margin (T004).

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
cargo test -p rcdvlog-core --test v01_t002_peaks_independent
```

## Handoff

List changed paths, every command run with its complete result, compatibility effects (v1 digests unchanged?), known limitations, and any stop condition hit. The implementer does not approve the task.
