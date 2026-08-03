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
- Every condition the user must know about is a durable typed diagnostic read
  through one surface. No failure is discoverable only from a log line.
- Camera identity resolves EXIF first, then a documented filename-pattern table,
  then an explicit unknown. A pattern never overrides EXIF, and a pattern match
  supplies the filename counter.
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
| 90 | [D01-T009](tasks/D01-T009-capture-sequence-management.md) | Complete capture-sequence management | — | ready |
| 150 | [D01-T015](tasks/D01-T015-calibration-surface-read-only.md) | Complete expanded calibration surface | — | ready |
| 160 | [D01-T016](tasks/D01-T016-calibration-editing-interactions.md) | Complete calibration editing and impact preview | D01-T015 | blocked |
| 170 | [D01-T017](tasks/D01-T017-chronology-cli-surface.md) | Finish chronology CLI contract coverage | — | ready |
| 180 | [D01-T018](tasks/D01-T018-antarctica-chronology-acceptance.md) | Antarctica chronology manual acceptance | D01-T016, D01-T017 | blocked |
| 210 | [D01-T021](tasks/D01-T021-low-resolution-preview-render.md) | Finish preview runtime and media verification | — | ready |
| 220 | [D01-T022](tasks/D01-T022-deterministic-otio-export.md) | Finish OTIO golden and cross-process verification | — | ready |
| 230 | [D01-T023](tasks/D01-T023-edit-plan-and-export-ui.md) | Complete edit-plan and export desktop UI | D01-T021, D01-T022 | blocked |
| 240 | [D01-T024](tasks/D01-T024-resolve-import-acceptance.md) | DaVinci Resolve 20/21 import acceptance | D01-T022, D01-T023 | blocked |
| 250 | [D01-T029](tasks/D01-T029-desktop-chronology-edit-plan-regression-suite.md) | Desktop chronology/edit-plan regression suite | D01-T015, D01-T016, D01-T023 | blocked |
| 260 | [D01-T030](tasks/D01-T030-edit-plan-cli-and-diagnostics.md) | Edit-plan CLI and diagnostics | D01-T021, D01-T022 | blocked |
| 270 | [D01-T025](tasks/D01-T025-documentation-and-reconciliation.md) | Documentation and planning reconciliation | D01-T018, D01-T024, D01-T029, D01-T030 | blocked |

Completed packets are deleted from this active directory once their acceptance
surface is delivered; Git history and cycle evidence remain the record. Eighteen
completed packets were archived this way during the 2026-08-03 completion
review.

## Status, 2026-08-03 completion review

The branch provides a strong domain foundation and the full repository gate is
green, but the desktop MVP is not complete. Five bounded tasks are immediately
ready; later workflow and manual gates remain dependency-blocked.

| Finding | Current evidence | Packet |
| --- | --- | --- |
| Capture-sequence proposals cover only counter resets and lack reassignment/apply-proposals. | `chronology/api.rs` split proposal implementation and dedicated test inventory. | D01-T009 |
| The calibration drawer is a summary/form, not the promised lane/time/phase calibration view, and most mutation scopes have no UI. | Rendered browser review at 1440×900 and 980×680 plus component/source audit. | D01-T015, D01-T016 |
| Chronology CLI commands exist without process-level JSON contracts. | `apps/cli/src/chronology_commands.rs`; no matching CLI workflow tests. | D01-T017 |
| Preview generation lacks real-media acceptance, output probing, cooperative desktop cancellation, `spawn_blocking`, and shared ETA estimation. | `editplan/preview.rs` and synchronous Tauri adapter review. | D01-T021 |
| OTIO output lacks committed golden and separate-process determinism evidence. | `tests/edit_plan.rs` covers in-process stability only. | D01-T022 |
| The edit-plan drawer cannot reorder/trim/remove, play preview, show staleness/history, or confirm overwrite. | Rendered browser review and `EditPlanPanel.tsx` audit. | D01-T023 |
| New desktop workflows have no component regression tests. | Desktop Vitest suite contains review order/keymap tests only. | D01-T029 |
| Agent automation cannot inspect or mutate edit plans through stable JSON. | No `edit-plan` CLI command family. | D01-T030 |

The review fixed two contained integration defects directly: chronology writes
now refresh the parent review asset cache, and edit-plan activation/export reject
a mismatched project/revision pair before waking sync. Canonical implementation
checkpoints were also updated to schema 12 and the actual partial workflow state.

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
