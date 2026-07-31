# D01: Desktop MVP

<!-- bounded-epic-metadata
{
  "schema_version": 1,
  "epic_id": "D01",
  "title": "Desktop MVP",
  "base_branch": "main",
  "task_branch_template": "codex/{task_id_lower}-{slug}",
  "tasks_per_cycle": 1,
  "merge_requires_user_authorization": true
}
-->

## Outcome

Bring the macOS desktop application to the `PRD.md` §21 MVP scope contract, in
priority order: stabilize what already exists behind real CI and real native
acceptance, finish chronology as a workflow a person can complete without
editing the database by hand, then add the smallest editing handoff that closes
the loop.

The epic succeeds when one real trip project — multi-camera, multi-timezone —
goes from mounted folders through ingest, review, chronology correction,
annotation, edit plan, low-resolution preview, and a deterministic OTIO export
that opens correctly in DaVinci Resolve, with no hand-editing of SQLite or
project JSON at any point.

Chronology is the differentiator and is sequenced first. Editing is deliberately
the smallest thing that can be called complete.

## Why this epic exists

The optional companion server epic is 20/20 complete and the travel ingest epic
is underway, while the desktop application those exist to serve has no CI, no
test runner for its 2,103-line UI file, and a chronology model consisting of one
flat table of per-camera filename offsets. This epic rebalances that.

## Frozen context

- Product contract: `PRD.md` §8–§12 (capture, chronology, trip context, clock
  anchors, queue stability), §21 (MVP scope contract), §25.1 (accepted
  decisions)
- Architecture contract: `docs/tasks/desktop/ARCHITECTURE.md`
- Repository conventions: `AGENTS.md` §2, §12, §14, §15, §16
- Chronology model contract: `docs/tasks/desktop/CHRONOLOGY_MODEL.md`
  (published by D01-T007)
- Edit-plan model contract: `docs/tasks/desktop/EDIT_PLAN_MODEL.md`
  (published by D01-T019)
- Decomposition rationale: `docs/superpowers/plans/2026-07-29-desktop-mvp.md`

### Architectural constraints

- Original media is immutable; no task writes into a source media tree.
- No chronology correction mutates source metadata.
- Chronology and edit-plan state are ordinary typed operations with HLC time,
  aggregate type/id, payload version, and base revision — not a parallel model.
- Persisted chronology state is evidence only. Trip time, display time,
  confidence, evidence, and total order are derived by a pure solver.
- The chronology solver performs no I/O and reads no wall clock; identical facts
  produce an identical solution including identical evidence ordering.
- Ordering uses a total `OrderKey` with no ties.
- Filename identity is scoped to its capture sequence; two cards reuse counters.
- Local display time derives from the covering trip phase, never from camera
  metadata timezone.
- Hard contradictions block apply and name the conflicting evidence.
- No chronology change is applied without explicit user confirmation.
- Editing scope is exhaustively: one video track, linked source audio, hard
  cuts, source in/out ranges, ordered operations, low-resolution preview,
  deterministic OTIO export. No track, transition, or effect field exists — not
  even as reserved schema.
- OTIO export is a pure-Rust writer. No Python runtime dependency is added.
- An edit-plan revision can never silently replace a newer one.
- Existing catalogs migrate forward; migration 9 must be order-preserving.
- The Vite browser mock is never evidence for persistence, media, or native
  behavior.
- `AGENTS.md` §15 applies: no reformatting of `lib.rs`, `operations.rs`, or
  `App.tsx` beyond the additive edits a packet owns.

## Queue

| Order | Task | Title | Depends on | Status |
| ---: | --- | --- | --- | --- |
| 10 | [D01-T001](tasks/D01-T001-verification-baseline-audit.md) | Verification baseline audit | — | complete |
| 20 | [D01-T002](tasks/D01-T002-fix-baseline-gate-defects.md) | Fix baseline gate defects | D01-T001 | complete |
| 30 | [D01-T003](tasks/D01-T003-desktop-frontend-testability-seam.md) | Desktop frontend testability seam | D01-T002 | complete |
| 40 | [D01-T004](tasks/D01-T004-github-actions-ci.md) | GitHub Actions CI with macOS native build | D01-T003 | complete |
| 50 | [D01-T005](tasks/D01-T005-native-desktop-smoke-acceptance.md) | Native macOS desktop smoke acceptance | D01-T004 | complete |
| 60 | [D01-T006](tasks/D01-T006-fix-native-smoke-defects.md) | Fix native smoke defects | D01-T005 | complete |
| 70 | [D01-T007](tasks/D01-T007-chronology-domain-model.md) | Chronology domain model and operation vocabulary | D01-T006 | ready |
| 80 | [D01-T008](tasks/D01-T008-deterministic-chronology-solver.md) | Deterministic chronology solver | D01-T007 | draft |
| 90 | [D01-T009](tasks/D01-T009-capture-sequence-management.md) | Capture-sequence management | D01-T008 | draft |
| 100 | [D01-T010](tasks/D01-T010-clock-epoch-ranges.md) | Clock epochs over capture-sequence ranges | D01-T009 | draft |
| 110 | [D01-T011](tasks/D01-T011-clip-time-exceptions.md) | Clip time exceptions | D01-T010 | draft |
| 120 | [D01-T012](tasks/D01-T012-trip-phases-and-timezone-context.md) | Trip phases and timezone context | D01-T008 | draft |
| 130 | [D01-T013](tasks/D01-T013-constraints-and-contradiction-reporting.md) | Event-order constraints and contradiction reporting | D01-T011, D01-T012 | draft |
| 140 | [D01-T014](tasks/D01-T014-chronology-explain-and-queue-stability.md) | Chronology explain and review-queue stability | D01-T013 | draft |
| 150 | [D01-T015](tasks/D01-T015-calibration-surface-read-only.md) | Expanded calibration surface, read-only | D01-T003, D01-T014 | draft |
| 160 | [D01-T016](tasks/D01-T016-calibration-editing-interactions.md) | Calibration editing interactions and impact preview | D01-T015 | draft |
| 170 | [D01-T017](tasks/D01-T017-chronology-cli-surface.md) | Chronology CLI surface | D01-T014 | draft |
| 180 | [D01-T018](tasks/D01-T018-antarctica-chronology-acceptance.md) | Antarctica footage chronology acceptance | D01-T016, D01-T017 | blocked |
| 190 | [D01-T019](tasks/D01-T019-edit-plan-model.md) | Edit-plan model and revision semantics | D01-T014 | ready |
| 200 | [D01-T020](tasks/D01-T020-edit-plan-validation.md) | Edit-plan validation | D01-T019 | draft |
| 210 | [D01-T021](tasks/D01-T021-low-resolution-preview-render.md) | Low-resolution preview render | D01-T020 | draft |
| 220 | [D01-T022](tasks/D01-T022-deterministic-otio-export.md) | Deterministic OTIO export | D01-T020 | draft |
| 230 | [D01-T023](tasks/D01-T023-edit-plan-and-export-ui.md) | Edit-plan and export desktop UI | D01-T021, D01-T022 | draft |
| 240 | [D01-T024](tasks/D01-T024-resolve-import-acceptance.md) | DaVinci Resolve 20/21 import acceptance | D01-T023 | draft |
| 250 | [D01-T025](tasks/D01-T025-documentation-and-reconciliation.md) | Documentation and planning reconciliation | D01-T018, D01-T024 | draft |

## Draft promotion protocol

Most of this queue is `draft` on purpose, and the executor must not promote a
row on its own.

| Rows | Blocked on | Promoted by |
| --- | --- | --- |
| ~~T002~~, T006 | Their path ownership is the set of files named in a defect ledger that does not exist yet. A fix packet cannot exhaustively own paths before the audit that finds the defects. | The contract editor, after T001 / T005 publishes its ledger. |
| T008–T017 | `architecture_decisions_frozen` would be a false claim: the chronology tables, operation vocabulary, and solver interface are frozen *by* T007, whose packet is the authority for them. | The contract editor, after T007 publishes `CHRONOLOGY_MODEL.md`. |
| T020–T023 | Same shape: frozen by T019's `EDIT_PLAN_MODEL.md`. | The contract editor, after T019 publishes it. |
| T024, T025 | Depend on the two contracts above plus the acceptance surface. | The contract editor, after T022 and T018 respectively. |
| T018 | The Antarctica footage path has not been supplied. | The product owner supplying the path; the contract editor then sets `ready`. |

Promotion means re-running the admission gate in
`.claude/skills/writing-bounded-task-packets/SKILL.md` §5 against the now-frozen
contract, not editing a status field.

## Promotion rule

The epic is promoted only when every required implementation, foundation, QA,
review, and release-gate packet is complete. One task cycle never promotes
unrelated work.

Two manual gates are load-bearing and cannot be satisfied by automation:
D01-T018 (real Antarctica footage chronology acceptance) and D01-T024 (DaVinci
Resolve 20/21 import). Neither a synthetic fixture, a passing headless suite,
nor a browser-mock walkthrough is evidence for either one.

## Cross-cutting verification

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm check:frontend
pnpm test:desktop
pnpm test:web
pnpm test:integration
pnpm --dir apps/desktop tauri build
```

## Relationship to other epics

`docs/tasks/server/EPIC.md` (complete) and `docs/tasks/travel/EPIC.md` (in
progress) are both optional relative to this one. No packet here may make
desktop MVP completion depend on a paired companion server being present.
