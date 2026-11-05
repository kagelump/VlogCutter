# V01: Passive soundtrack sync on real room audio

<!-- bounded-epic-metadata
{
  "schema_version": 1,
  "epic_id": "V01",
  "title": "Passive soundtrack sync on real room audio",
  "base_branch": "main",
  "task_branch_template": "claude/{task_id_lower}-{slug}",
  "tasks_per_cycle": 1,
  "merge_requires_user_authorization": true
}
-->

## Outcome

Make passive TV-soundtrack sync work on a real laptop mic in a room while the
editor talks. The RC6 session went from 0/31 matched windows to anchored
utterances, with no new false matches. Six fixes (ARCHITECTURE.md §3–§9):
peak diagnostics, distinctive acceptance (per reference and cross-reference),
a speech mask with gap-placed windows, recipe v2 weighting, offset consensus,
and consensus-gated interpolation. Every fix is off by default, and v1 replays
byte-identically.

## Frozen context

The subsequent source-audio eval work has a separate bounded delivery scope:
[source-audio MVP design](SOURCE_AUDIO_MVP_DESIGN.md) and
[implementation plan](SOURCE_AUDIO_MVP_PLAN.md). These preserve pooled recognition
improvements without claiming completion of the V01 real-room acceptance gate.

- Architecture contract: `docs/tasks/tv-review/ARCHITECTURE.md`
- Evidence: `../data/editorial-review/rc6-passive-sync-2026-11-03/REPORT.md`, `diagnose_probe.py`
- Existing behaviour: `docs/tv-soundtrack-matching.md`, `docs/tv-review-local-session.md`, `docs/tv-review-sync.md`

## Queue

| Order | Task | Title | Depends on | Status |
| ---: | --- | --- | --- | --- |
| 10 | [V01-T001](tasks/V01-T001-v2-contract-types.md) | Freeze v2 contract types with v1 byte compatibility | — | complete |
| 20 | [V01-T002](tasks/V01-T002-peak-diagnostics.md) | Report best peaks for every searched window | V01-T001 | complete |
| 30 | [V01-T003](tasks/V01-T003-distinctive-acceptance.md) | Distinctive-peak acceptance in match_pcm | V01-T002 | complete |
| 40 | [V01-T004](tasks/V01-T004-global-margin.md) | Cross-reference margin in session evidence | V01-T003 | complete |
| 50 | [V01-T005](tasks/V01-T005-speech-mask.md) | Speech mask weights in matching | V01-T001 | ready |
| 60 | [V01-T006](tasks/V01-T006-gap-windows.md) | Gap-placed query windows in session plans | V01-T005 | ready |
| 70 | [V01-T007](tasks/V01-T007-offset-consensus.md) | Offset consensus module and CLI | V01-T004 | complete |
| 80 | [V01-T008](tasks/V01-T008-consensus-interpolation.md) | Consensus-gated interpolation in the sync mapper | V01-T007 | complete |
| 90 | [V01-T009](tasks/V01-T009-recipe-v2.md) | Recipe v2: energy-weighted robust features | V01-T005 | ready |
| 100 | [V01-T010](tasks/V01-T010-real-room-evidence.md) | Real-room RC6 v2 evidence run | V01-T006, V01-T008, V01-T009 | ready |
| 110 | [V01-T011](tasks/V01-T011-docs-reconcile.md) | Docs and planning reconciliation | V01-T010 | ready |

## Model guidance

V01-T001 is foundation work. It carries the serde/digest compatibility risk, so
give it to a stronger model, or review it closely, before handing leaf packets
to a small model. V01-T010 is a QA packet whose stop condition must be honoured.

## Promotion rule

The epic is promoted only when every packet is complete and the V01-T010
evidence meets ARCHITECTURE.md §10. One task cycle never promotes unrelated work.
