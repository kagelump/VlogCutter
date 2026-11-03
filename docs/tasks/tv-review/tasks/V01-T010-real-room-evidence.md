# V01-T010: Real-room RC6 v2 evidence run

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "V01-T010",
  "epic_id": "V01",
  "title": "Real-room RC6 v2 evidence run",
  "slug": "real-room-evidence",
  "status": "ready",
  "task_class": "qa",
  "depends_on": [
    "V01-T006",
    "V01-T008",
    "V01-T009"
  ],
  "principal_invariant": "On the RC6 real recording, v2 matches at least 6 windows, all at offset −0.96±0.08 s, and maps at least 20 of 27 utterances, while every negative control yields zero matches and zero segments.",
  "central_falsification": "Run the §10 negative controls (wrong movie window, reversed mic, different movie) and the seek splice through the built CLI; any match or segment in a negative control falsifies the claim.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "built rcdvlog CLI -> real RC6 recording and movie"
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
      "docs/tasks/tv-review/tasks/V01-T010-real-room-evidence.md"
    ],
    "implementer": [],
    "independent_tester": [
      "../data/editorial-review/rc6-passive-sync-v2/**",
      "docs/task-cycles/V01-T010/**"
    ],
    "integrator": [
      "docs/tasks/tv-review/EPIC.md"
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
    "cargo fmt --all -- --check"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/tv-review/EPIC.md",
      "regex": "\\| \\[V01-T010\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Produce the §10 evidence with the real hardware recording. This is a test-only packet with no production changes.

## Principal invariant

This task is correct when on the RC6 real recording, v2 matches at least 6 windows, all at offset −0.96±0.08 s, and maps at least 20 of 27 utterances, while every negative control yields zero matches and zero segments.

## Inputs and outputs

Inputs: `../data/editorial-review/rc6-transcription-2026-11-02/recording-48k.wav`, `local-transcript.json`, `../data/rough-cut6/deliverables/antarctica-rough-cut-6/movie.mp4`, and the old run.py as a template. Outputs: the v2 folder with run.py, JSON receipts and REPORT.md.

## Frozen decisions

§10 expectations and thresholds (0.5, 0.25, 6.0). The mask is built from transcript utterances padded ±0.25 s. The source and old evidence folder are read-only. Also apply all of ARCHITECTURE.md §2.

## Owned paths

- Contract editor: this packet only.
- Implementer: none (no production changes).
- Independent tester: `../data/editorial-review/rc6-passive-sync-v2/**`, `docs/task-cycles/V01-T010/**`.
- Integrator: `docs/tasks/tv-review/EPIC.md`.
- Forbidden paths always win.

## Required interface

CLI only: soundtrack-session-plan/run/evidence/consensus/sync.

## Acceptance oracle

§10 parts 1–4 expectations, verbatim.

## Independent test charter

Claim to falsify: Run the §10 negative controls (wrong movie window, reversed mic, different movie) and the seek splice through the built CLI; any match or segment in a negative control falsifies the claim. You are the tester. Also re-run `diagnose_probe.py` logic against the v2 matches to confirm the offsets independently. Required failure behaviour: wrong or ambiguous evidence must stay unmatched, ambiguous or unknown, and must never produce an anchor. Preservation: existing soundtrack, session and sync tests stay green. Evidence: command output and test names.

## Review charter

Risk: unacknowledged threshold tuning or cherry-picked controls. Ask: were all controls run and reported, including failures? Approval: APPROVE / REQUEST_CHANGES / BLOCK.

## Non-goals

- Claiming timing accuracy.
- Updating the web session or catalog.

## Stop conditions

Stop and escalate when:

- a change would alter the serialized bytes or digest of any existing v1 object (§2);
- you need a production path not listed under implementer ownership, a new crate dependency, or a SQLite migration;
- a frozen decision in ARCHITECTURE.md seems wrong or insufficient. Report it and do not redesign;
- the independent oracle would have to be weakened, or thresholds changed, to make tests pass;
- the work no longer fits one focused context.
- part 2 misses or any negative control matches. Record the numbers and stop; do not tune.

## Commands

Run these from `vlogcutter/`; all must pass:

```sh
cargo test -p rcdvlog-core tv_soundtrack
cargo test -p rcdvlog-core tv_review_sync
cargo clippy -p rcdvlog-core --all-targets -- -D warnings
cargo fmt --all -- --check
```

## Handoff

List changed paths, every command run with its complete result, compatibility effects (v1 digests unchanged?), known limitations, and any stop condition hit. The implementer does not approve the task.
