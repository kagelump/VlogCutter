# Desktop MVP Implementation Plan

> **For agentic workers:** this document is the **intermediate decomposition
> artifact** produced by `superpowers:writing-plans` and compiled into
> `docs/tasks/desktop/EPIC.md` by `writing-bounded-task-packets`. The bounded
> task packets under `docs/tasks/desktop/tasks/` carry implementation
> authority. Execute work with `executing-bounded-task-cycle` against the epic,
> not by reading this file top to bottom. This file exists to record *why* the
> slicing is what it is, and the exact interfaces that cross task boundaries.

**Goal:** Bring the macOS desktop app to the PRD Section 21 MVP scope contract by
first putting the existing code under CI and real native acceptance, then
finishing chronology as a genuine user workflow, then adding the smallest
complete editing handoff.

**Architecture:** All new domain state is operation-backed in `rcdvlog-core`
exactly like notes, labels, removal, and rotation are today — new SQLite tables
behind migration 9+, new `operation_type` constants dispatched from
`apply_project_operation`, and derived values recomputed by a pure solver rather
than stored as authority. The desktop is a thin Tauri command layer over that
core, the CLI is a second consumer of the same commands, and OTIO export is a
pure-Rust deterministic writer with no Python dependency.

**Tech Stack:** Rust 2024 (rusqlite bundled, chrono/chrono-tz, blake3, tokio),
Tauri 2 + React 19 + TypeScript + Vite, vitest + @testing-library/react,
FFmpeg/ffprobe child processes, GitHub Actions (ubuntu-latest + macos-latest),
pnpm 10 workspaces.

## Global Constraints

These are repository invariants copied from `AGENTS.md` §2 and `PRD.md` §21/§25.
Every task's requirements implicitly include this section.

- Original media is immutable. No operation may write into a source media tree.
- No operation may mutate source metadata to record a chronology correction.
- SQLite is canonical local state. Never hand-edit SQLite to achieve product
  behavior; add a domain operation plus a migration.
- Existing catalogs migrate forward. Delete-and-rebuild is not a migration
  strategy.
- All project mutations are typed operations with HLC logical time, an aggregate
  type/id, a payload version, and a base revision. Chronology is no exception.
- The desktop never waits on a network round trip to commit a local edit.
- Absolute paths are node-local; protocol state uses logical root IDs plus
  relative paths.
- Chronology changes are never applied automatically without user confirmation
  (`PRD.md` §21.4).
- Filename identity must include capture sequence, because two cards reuse the
  same counter (`PRD.md` §8.2).
- Tests use `tempfile` temporary directories. Never point destructive test setup
  at `~/Movies/VideoProjects`, `/Volumes`, or the repository root.
- Child-process arguments stay structured via `Command`; never build shell
  strings from media paths.
- Paths with spaces and non-ASCII filenames must survive every code path.
- Per-file failures surface without aborting an otherwise recoverable batch.
- Deferred, do not build: fine-cut editing, transitions, effects, color, audio
  mixing, general-purpose timeline editing, multi-user editing origins,
  automatic visual clock detection, transcription, VLM summaries, embeddings.

### Decisions frozen by the product owner on 2026-07-29

1. **CI:** GitHub Actions, `ubuntu-latest` for the full headless gate plus
   `macos-latest` running `tauri build` so the native desktop bundle is proven
   to compile on every change.
2. **Real-footage validation:** the Antarctica trip footage is the P1 chronology
   release gate. Its path is supplied by the product owner at gate time; the
   packet stays `blocked` until then. Synthetic multi-camera/multi-timezone
   corpora are development fixtures, never release evidence.
3. **Resolve:** DaVinci Resolve 20/21 is installed and usable on the target Mac,
   so OTIO import is a required manual acceptance packet with a pass/fail
   checklist, not an outstanding risk row.
4. **OTIO:** a pure-Rust deterministic OTIO JSON writer with golden-file tests.
   No Python runtime dependency is added to the desktop app.

---

## Current state assessment

Established by direct inspection of the repository on 2026-07-29.

| Area | State | Consequence for this plan |
| --- | --- | --- |
| `.github/` | Does not exist. Zero CI. | Nothing prevents a broken `main`. P0 blocker. |
| `apps/desktop/src/App.tsx` | 2,103 lines, zero tests. `package.json` has no `test` script and no vitest dependency. | The largest and most-changed file in the product is completely unprotected. P1 adds a whole calibration surface to it. Needs a testability seam before P1 UI work. |
| `apps/desktop/src-tauri/src/` | 5 tests total across `lib.rs` (3) and `keychain.rs` (2). `server_client.rs`, `sync_manager.rs`, `main.rs` have none. | Tauri command layer is effectively untested. |
| `apps/web` | vitest + playwright + testing-library configured, `shell.spec.ts` e2e. | The *web* surface is better tested than the desktop one. This is the imbalance to correct. |
| Chronology persistence | One table, `clock_events(project_id, camera, start_filename, offset_seconds, label)`. One operation type, `clockEventCreated`, aggregate type `clock`, aggregate id = camera string. | No capture sequences, no timezones, no trip phases, no clip exceptions, no event-order constraints, no contradiction model, no confidence, no evidence. Chronology is a flat per-camera offset, not the PRD §9–§11 model. |
| Derived time | `assets.best_time` and `assets.offset_seconds` are stored columns recomputed by `materialize_clock`. | Derived values are persisted as authority. The solver contract has to separate hypotheses from derived output (`PRD.md` §9.1). |
| Capture sequence | Not modeled anywhere. `assets.camera` is a free-text string. | Counter reuse across two SD cards silently collides today. |
| Migrations | 8 applied, registered in `PROJECT_CATALOG_MIGRATIONS` in `core/rcdvlog-core/src/migrations.rs:85`. | Chronology model lands as migration 9; edit plan as migration 10. |
| Edit plan / preview / OTIO | No matches for `otio`, `OpenTimelineIO`, `edit_plan`, or `editPlan` anywhere in `core/`, `apps/`, or `packages/`. | P2 is greenfield. |
| Server epic S01 | 20/20 packets complete. | This is the imbalance the product owner identified: the optional companion server is finished while the desktop MVP is not. |

## File structure

### New files

```text
.github/workflows/ci.yml                            # ubuntu gate + macOS tauri build
.github/workflows/README.md                         # what each job proves, what it cannot prove
docs/tasks/desktop/ARCHITECTURE.md                  # architecture contract for this epic
docs/tasks/desktop/CHRONOLOGY_MODEL.md              # frozen chronology contract (output of D01-T007)
docs/tasks/desktop/EDIT_PLAN_MODEL.md               # frozen edit-plan contract (output of D01-T016)
docs/tasks/desktop/EPIC.md                          # the queue
docs/tasks/desktop/tasks/*.md                       # the packets

core/rcdvlog-core/src/chronology/mod.rs             # public chronology surface
core/rcdvlog-core/src/chronology/model.rs           # persisted fact types
core/rcdvlog-core/src/chronology/solver.rs          # pure facts -> derived order/time/evidence
core/rcdvlog-core/src/chronology/sequences.rs       # capture-sequence proposal + edit
core/rcdvlog-core/src/chronology/phases.rs          # trip phases + timezone derivation
core/rcdvlog-core/src/chronology/contradictions.rs  # constraint conflict detection
core/rcdvlog-core/tests/chronology_solver.rs        # determinism + ordering
core/rcdvlog-core/tests/chronology_migration.rs     # migration 9 order-preservation
core/rcdvlog-core/tests/fixtures/chronology/*.json  # multi-camera multi-timezone corpus

core/rcdvlog-core/src/editplan/mod.rs               # public edit-plan surface
core/rcdvlog-core/src/editplan/model.rs             # plan, revision, operation types
core/rcdvlog-core/src/editplan/validate.rs          # pre-render validation
core/rcdvlog-core/src/editplan/preview.rs           # FFmpeg low-res preview job
core/rcdvlog-core/src/editplan/otio.rs              # deterministic OTIO JSON writer
core/rcdvlog-core/tests/editplan_validation.rs
core/rcdvlog-core/tests/otio_golden.rs
core/rcdvlog-core/tests/fixtures/otio/*.otio        # golden expected output

apps/desktop/src/review/order.ts                    # extracted pure review-queue logic
apps/desktop/src/review/keymap.ts                   # extracted pure keyboard map
apps/desktop/src/review/order.test.ts
apps/desktop/src/review/keymap.test.ts
apps/desktop/src/calibration/CalibrationView.tsx    # expanded calibration surface
apps/desktop/src/calibration/lanes.ts               # pure lane layout math
apps/desktop/src/calibration/lanes.test.ts
apps/desktop/src/calibration/impact.ts              # pure before/after impact diff
apps/desktop/src/calibration/impact.test.ts
apps/desktop/src/editplan/EditPlanView.tsx
apps/desktop/vitest.config.ts
apps/desktop/src/test/setup.ts

docs/desktop/chronology.md                          # user-facing chronology docs
docs/desktop/edit-plan-and-export.md
docs/desktop/smoke-test.md                          # the repeatable native checklist
```

### Modified files

```text
core/rcdvlog-core/src/migrations.rs      # migration 9 (chronology), 10 (edit plan)
core/rcdvlog-core/src/operations.rs      # new operation constants + materializers + dispatch
core/rcdvlog-core/src/lib.rs             # ProjectCatalog chronology/edit-plan methods
apps/desktop/src-tauri/src/lib.rs        # new Tauri commands, registered in the builder
apps/desktop/src/App.tsx                 # calibration entry point, extraction call sites
apps/desktop/src/api.ts                  # the only place invoke() is called
apps/desktop/src/types.ts                # DTO mirrors
apps/desktop/src/styles.css              # calibration + edit-plan styling
apps/desktop/package.json                # vitest, testing-library, test script
apps/cli/src/main.rs                     # chronology + edit-plan CLI subcommands
apps/cli/tests/cli_workflow.rs           # JSON envelope contract tests
package.json                             # test:desktop, ci aggregate scripts
AGENTS.md                                # §3 state table, §12 testing table
PRD.md                                   # implementation checkpoint section
```

### Why these boundaries

`operations.rs` is already 90 KB and `lib.rs` 103 KB. `AGENTS.md` §15 forbids
unilateral reformatting of those files, so chronology and edit-plan logic goes
into new focused modules under `core/rcdvlog-core/src/chronology/` and
`core/rcdvlog-core/src/editplan/`, and the existing giant files get only
additive edits: new operation constants, new dispatch arms, new `ProjectCatalog`
methods delegating into the new modules. This keeps each new unit small enough
to hold in context and keeps the diff reviewable.

The React extraction is deliberately minimal and behavior-preserving. Pure logic
(review ordering, keyboard mapping, lane layout, impact diffing) moves into
testable modules; rendering stays where it is. `App.tsx` is not rewritten.

## Task decomposition

Tasks are ordered. `→` marks a dependency.

### Priority 0 — stabilize what exists

**Task 1 — Verification baseline audit (qa, no production changes).**
Run every command in the `AGENTS.md` §12 gate on current `main` and record exact
output. Produce `docs/tasks/desktop/BASELINE_AUDIT.md`: one row per command with
verbatim pass/fail, failure counts, and whether the failure is environmental
(missing FFmpeg, missing Playwright browsers) or a real defect. This is the
external oracle for Task 2 and the enforceable command set for Task 3. Written as
qa class specifically so the ledger is produced by someone with no stake in it
being short.

**Task 2 — Fix the baseline defect ledger** → Task 1.
Resolve every row the audit classified as a real defect. Bounded by the ledger:
the ledger is the spec, and a defect requiring an architecture change is a stop
condition, not a redesign opportunity.

**Task 3 — Desktop frontend testability seam (foundation)** → Task 2.
Decide and implement the extraction boundary that makes desktop React logic
testable, and add the vitest harness `apps/desktop` currently lacks. Foundation
class because it chooses a boundary every later P1/P2 UI task builds on, and
that choice is not the leaf implementer's to make.

Extract, behavior-preserving:
- `review/order.ts` — the review queue ordering and next-item selection
  currently inline in `App.tsx` (`activeAssets`, `moveSelection`).
- `review/keymap.ts` — the keyboard dispatch table currently inline in the
  `useEffect` at `App.tsx:589`.

**Task 4 — GitHub Actions CI (integration)** → Task 3.
`ubuntu-latest`: `cargo fmt --all -- --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo test --workspace`, `pnpm check:frontend`,
`pnpm test:desktop`, `pnpm test:web`, `pnpm test:integration`,
`pnpm check:openapi`, `pnpm check:docs`, `pnpm check:deployment`.
`macos-latest`: `pnpm --dir apps/desktop tauri build` plus `cargo test -p
rcdvlog-desktop`. Integration class because CI files are a shared hot spot.
The workflow's enforced set must equal the audited-green set; anything
quarantined is an explicit tracked row, never a silent omission.

**Task 5 — Native macOS desktop smoke acceptance (qa, no production changes)** → Task 4.
Build and run the real Tauri app — not the Vite browser mock, which `AGENTS.md`
§15 explicitly forbids as persistence evidence — and execute a written CUJ
checklist: create project, ingest a small real folder, thumbnails and proxies,
asset-protocol playback, labels/notes/reversible removal, a clock correction,
quit and relaunch with review position intact, source-volume disconnect and
reconnect. Record pass/fail and evidence per row in
`docs/desktop/smoke-test.md` plus a defect ledger.

**Task 6 — Fix the native smoke defect ledger** → Task 5.
Same bounded-by-ledger shape as Task 2.

### Priority 1 — chronology as a real user workflow

**Task 7 — Chronology domain model and operation vocabulary (foundation)** → Task 6.
The load-bearing decision of the whole epic. Freezes and implements:

Tables (migration 9):

```sql
capture_sequences(id, project_id, camera_key, label, volume_hint,
                  filename_prefix, start_counter, end_counter, created_at)
asset_capture_sequence(asset_id, capture_sequence_id, filename_counter)
clock_epochs(id, project_id, capture_sequence_id, start_counter, end_counter,
             offset_seconds, confidence, evidence_json, label, created_at)
clip_time_exceptions(asset_id, project_id, trip_time, reason, created_at)
trip_phases(id, project_id, label, phase_kind, start_trip_time, end_trip_time,
            timezone, origin_timezone, destination_timezone, display_mode,
            created_at)
event_order_constraints(id, project_id, earlier_asset_id, later_asset_id,
                        strength, source, created_at)
```

Operation constants added to `core/rcdvlog-core/src/operations.rs`:

```rust
pub const CAPTURE_SEQUENCE_DECLARED: &str = "captureSequenceDeclared";
pub const CAPTURE_SEQUENCE_SPLIT: &str = "captureSequenceSplit";
pub const CAPTURE_SEQUENCE_MERGED: &str = "captureSequenceMerged";
pub const CLOCK_EPOCH_SET: &str = "clockEpochSet";
pub const CLOCK_EPOCH_CLEARED: &str = "clockEpochCleared";
pub const CLIP_TIME_EXCEPTION_SET: &str = "clipTimeExceptionSet";
pub const CLIP_TIME_EXCEPTION_CLEARED: &str = "clipTimeExceptionCleared";
pub const TRIP_PHASE_SET: &str = "tripPhaseSet";
pub const TRIP_PHASE_REMOVED: &str = "tripPhaseRemoved";
pub const EVENT_ORDER_CONSTRAINT_SET: &str = "eventOrderConstraintSet";
pub const EVENT_ORDER_CONSTRAINT_CLEARED: &str = "eventOrderConstraintCleared";
```

Aggregate types: `captureSequence`, `clockEpoch`, `tripPhase`,
`eventOrderConstraint`; clip exceptions ride the existing `projectAsset`
aggregate so they inherit its revision guard.

Forward migration: every existing `clock_events` row becomes a
`capture_sequences` row (one per distinct `camera`, counters derived from
observed filenames) plus a `clock_epochs` row whose `start_counter` is the
counter parsed from `start_filename` and whose `end_counter` is NULL. The
principal invariant is that this migration is **order-preserving**: for every
existing project, the total review order after migration is identical to the
order before it.

Forward compatibility: an older peer receiving one of these operation types
currently hits `bail!("unsupported operation type: {value}")` at
`operations.rs:1424`. This task must decide and implement the quarantine policy
so an unknown chronology operation is retained and reported rather than failing
the sync batch.

**Task 8 — Deterministic chronology solver** → Task 7.
Pure function, no I/O, no clock reads:

```rust
pub fn solve(facts: &ChronologyFacts) -> ChronologySolution;

pub struct ChronologySolution {
    pub assets: Vec<AssetChronology>,   // in stable total order
    pub contradictions: Vec<Contradiction>,
}
pub struct AssetChronology {
    pub asset_id: String,
    pub raw_time: Option<DateTime<Utc>>,
    pub trip_time: Option<DateTime<Utc>>,
    pub display_time: Option<DisplayTime>,
    pub confidence: f32,
    pub evidence: Vec<Evidence>,        // ordered, deduplicated
    pub uncertainty_seconds: u32,
    pub order_key: OrderKey,            // total, deterministic tie-break
}
```

`best_time` and `offset_seconds` become *projections* of the solution rather
than independently maintained authority.

**Task 9 — Capture-sequence management** → Task 8.
Proposal from camera identity, filename prefix, counter continuity, counter
reset, and volume change (`PRD.md` §8.2), plus user split/merge/reassign as
operations. Invariant: filename identity is scoped to its capture sequence, so
two cards using counter `C0001` never collide.

**Task 10 — Clock epochs over capture-sequence ranges** → Task 9.
Replaces today's per-camera-and-filename offset with a bounded
`[start_counter, end_counter]` range inside one capture sequence. Invariant:
applying one epoch changes derived time for assets inside its declared range and
for no asset outside it.

**Task 11 — Clip exceptions** → Task 10.
Per-clip override for the rare clip that breaks its sequence (`PRD.md` §9.4).
Invariant: an exception overrides sequence-derived time for exactly that clip,
and is always surfaced as explicit evidence because it weakens the monotonic
filename assumption.

**Task 12 — Trip phases and timezone context** → Task 8.
Location and transit phases with timezone, origin/destination timezone, and
display mode (`PRD.md` §10). Invariant: local display time derives from the
trip phase covering the asset's trip time, never from camera metadata timezone.

**Task 13 — Event-order constraints and contradiction reporting** → Task 11, Task 12.
Hard and soft before/after constraints plus cycle and interval-conflict
detection. Invariant: a hard contradiction blocks apply and names every
conflicting piece of evidence.

**Task 14 — Chronology explain and review-queue stability** → Task 13.
`chronology explain <asset>` returning time, confidence, evidence, and the
corrections that applied; plus capturing the intended next review item before a
reorder and resuming there afterward (`PRD.md` §12).

**Task 15 — Expanded calibration surface, read-only** → Task 14, Task 3.
Trip-context band, time ruler, camera/card lanes, filename blocks, raw versus
best-local timestamps, confidence and evidence panel. Read-only so a reviewer
can reject the visualization without rejecting the editing interactions.

**Task 16 — Calibration editing interactions** → Task 15.
Adjustment-scope selection, numeric offset control, drag, before/after
placement, impact preview, explicit apply/cancel. Invariant: no chronology
change reaches the catalog without an explicit user apply, and the impact
preview shown before apply matches the reorder that apply produces.

**Task 17 — Chronology CLI surface** → Task 14.
`chronology sequences|epochs|phases|exceptions|constraints|explain|status` with
the repository's stable JSON envelopes and machine-readable error codes.
Invariant: desktop and CLI produce the same total order for the same catalog.

**Task 18 — Antarctica footage chronology acceptance (qa, manual gate)** → Task 16, Task 17.
Blocked until the product owner supplies the footage path. Validates the real
multi-camera, multi-timezone, date-line-crossing pattern that synthetic
fixtures cannot represent.

### Priority 2 — smallest complete editing handoff

**Task 19 — Edit-plan model and revision semantics (foundation)** → Task 14.
Migration 10. Accepted scope only: one video track, linked source audio, hard
cuts, source in/out ranges, ordered operations. Invariant: an edit-plan revision
can never silently replace a newer one — mutations require an expected revision
id, matching the existing script-revision discipline.

**Task 20 — Edit-plan validation** → Task 19.
Asset existence, media availability, range bounds, overlap, unsupported
operation, and revision staleness, each with a stable error code. Invariant:
every invalid plan is rejected before any render or export work begins.

**Task 21 — Low-resolution preview render** → Task 20.
FFmpeg render over review proxies with cancellable, incrementally committed job
semantics. Invariant: preview duration and clip order match the plan within one
frame.

**Task 22 — Deterministic pure-Rust OTIO export** → Task 20.
Golden-file tested OTIO JSON writer with stable clip and metadata identifiers.
Invariant: identical plan input produces byte-identical OTIO output.

**Task 23 — Edit-plan and export desktop UI** → Task 21, Task 22, Task 3.
Build a plan from included clips with in/out ranges, render preview, export,
export history, and stale-artifact warnings.

**Task 24 — Resolve 20/21 import acceptance (qa, manual gate)** → Task 22, Task 23.
Import the exported OTIO into the installed Resolve and verify source
references, in/out points, ordering, and clip names survive.

**Task 25 — Documentation and planning reconciliation (integration)** → Task 18, Task 24.
Update `PRD.md`'s checkpoint, `AGENTS.md` §3 and §12, and `docs/desktop/*` to
match what shipped, and close the epic's reconciliation checks.

## Interfaces that cross task boundaries

Recorded here because each packet's implementer sees only their own packet.

```rust
// Task 7 produces; Tasks 8-14, 17 consume.
pub struct ChronologyFacts {
    pub assets: Vec<AssetTimeHypothesis>,
    pub sequences: Vec<CaptureSequence>,
    pub epochs: Vec<ClockEpoch>,
    pub exceptions: Vec<ClipTimeException>,
    pub phases: Vec<TripPhase>,
    pub constraints: Vec<EventOrderConstraint>,
}
impl ProjectCatalog {
    pub fn chronology_facts(&self, project_id: &str) -> Result<ChronologyFacts>;
    pub fn chronology(&self, project_id: &str) -> Result<ChronologySolution>;
}

// Task 8 produces; Tasks 9-17 consume. Total order, no ties.
pub struct OrderKey {
    pub trip_time_millis: Option<i64>,
    pub capture_sequence_id: String,
    pub filename_counter: Option<u64>,
    pub filename: String,
    pub asset_id: String,
}

// Task 12 produces; Tasks 15, 16, 17 consume.
pub enum DisplayMode { Local, Dual, Utc }
pub struct DisplayTime {
    pub primary: String,             // RFC 3339 with offset
    pub secondary: Option<String>,   // populated when DisplayMode::Dual
    pub timezone: String,            // IANA id
    pub phase_id: Option<String>,
}

// Task 13 produces; Tasks 14, 15, 16, 17 consume.
pub struct Contradiction {
    pub kind: ContradictionKind,     // Cycle | IntervalConflict | ExceptionConflict
    pub strength: ConstraintStrength,// Hard blocks apply; Soft reports only
    pub asset_ids: Vec<String>,
    pub evidence: Vec<Evidence>,
    pub message: String,
}

// Task 19 produces; Tasks 20-24 consume.
pub struct EditPlanRevision {
    pub revision_id: String,
    pub project_id: String,
    pub script_revision_id: Option<String>,
    pub operations: Vec<EditPlanOperation>,  // ordered; index is timeline order
    pub created_at: String,
}
pub struct EditPlanOperation {
    pub operation_id: String,
    pub asset_id: String,
    pub source_in_seconds: f64,
    pub source_out_seconds: f64,
    pub rationale: String,
}
// One video track with linked source audio and hard cuts only. There is no
// track field, no transition field, and no effect field, by accepted scope.
```

TypeScript mirrors in `apps/desktop/src/types.ts` use the same names in
camelCase, matching the existing `Asset`/`AssetPatch` convention.

## Representative test design

The packets carry the full acceptance oracles. These examples establish the
*kind* of test expected, since test design is the most common weak point.

Migration order-preservation (Task 7) — the highest-risk claim in the epic:

```rust
#[test]
fn migration_9_preserves_existing_total_order() {
    let catalog = fixture_catalog_at_schema_8("two_cameras_one_clock_event");
    let before: Vec<String> = catalog.list_assets("project_test").unwrap()
        .iter().map(|asset| asset.id.clone()).collect();

    migrate_project_catalog(&mut catalog.connection().unwrap()).unwrap();

    let after: Vec<String> = catalog.list_assets("project_test").unwrap()
        .iter().map(|asset| asset.id.clone()).collect();
    assert_eq!(before, after, "migration 9 must not reorder existing projects");
}
```

Solver determinism under shuffled input (Task 8) — guards against accidental
reliance on HashMap iteration order, the realistic way determinism breaks:

```rust
#[test]
fn solver_order_is_independent_of_fact_insertion_order() {
    let facts = load_fixture("antarctica_pattern.json");
    let expected = solve(&facts).assets.iter()
        .map(|a| a.asset_id.clone()).collect::<Vec<_>>();

    for seed in 0..32u64 {
        let shuffled = shuffle_facts(&facts, seed);
        let actual = solve(&shuffled).assets.iter()
            .map(|a| a.asset_id.clone()).collect::<Vec<_>>();
        assert_eq!(expected, actual, "order changed for seed {seed}");
    }
}
```

Epoch range containment (Task 10) — the invariant most likely to regress:

```rust
#[test]
fn epoch_changes_only_its_declared_counter_range() {
    let catalog = corpus_with_counters("sony_sd_a", 100..=200);
    let before = times_by_counter(&catalog);

    catalog.set_clock_epoch("sony_sd_a", 150, Some(160), -480 * 60).unwrap();

    let after = times_by_counter(&catalog);
    for counter in 100..=200 {
        if (150..=160).contains(&counter) {
            assert_ne!(before[&counter], after[&counter], "counter {counter} should shift");
        } else {
            assert_eq!(before[&counter], after[&counter], "counter {counter} must not move");
        }
    }
}
```

OTIO determinism (Task 22):

```rust
#[test]
fn identical_plans_export_byte_identical_otio() {
    let plan = load_fixture_plan("three_clip_hard_cuts.json");
    let first = export_otio(&plan).unwrap();
    let second = export_otio(&plan).unwrap();
    assert_eq!(first, second);
    assert_eq!(first, read_golden("three_clip_hard_cuts.otio"));
}
```

Review-queue stability (Task 14) — a behavioral claim, not a symbol check:

```rust
#[test]
fn reorder_resumes_at_the_captured_next_item() {
    let catalog = corpus_with_counters("sony_sd_a", 1..=50);
    let next = catalog.review_queue("project_test").unwrap().next_asset_id().unwrap();

    catalog.set_clock_epoch("sony_sd_a", 10, None, -3600).unwrap();

    assert_eq!(catalog.review_queue("project_test").unwrap().next_asset_id().unwrap(), next);
}
```

Extracted React logic (Task 3) — must assert behavior that a passing `tsc`
build does not catch:

```ts
it("keeps the selected clip when a reorder moves it", () => {
  const before = orderAssets(fixtureAssets, { removedVisible: false });
  const selected = before[3].id;
  const after = orderAssets(shiftClockEpoch(fixtureAssets, -3600), { removedVisible: false });
  expect(nextSelection(after, selected)).toBe(selected);
});
```

## Self-review

**Spec coverage.** Every item the product owner listed maps to a task: CI → 4;
native smoke test → 5; fix defects found → 2 and 6; persisted trip phases and
timezone context → 12; capture-sequence management → 9; clip exceptions → 11;
before/after placement → 13 (domain) and 16 (interaction); contradiction
reporting → 13; expanded calibration interface → 15 and 16; real
Antarctica footage validation → 18; one video track, linked source audio, hard
cuts, source in/out, ordered operations → 19; basic low-resolution preview →
21; deterministic OTIO export → 22; Resolve 20/21 import test → 24.

Two items were added that the product owner did not name but that the stated
goal requires. The desktop frontend testability seam (Task 3) is included
because "there's a lack of test" cannot be honestly addressed while the
2,103-line file that owns the entire UI has zero tests and no test runner
installed, and because P1 adds a large calibration surface to that same file.
The verification baseline audit (Task 1) is included because CI cannot enforce a
gate whose current pass/fail state nobody has measured.

**Placeholder scan.** No `TBD`, `TODO`, or "handle edge cases" steps. The one
genuinely unknown value in the epic — the Antarctica footage path — is not a
placeholder inside a ready packet; Task 18 is `blocked` on it by design, and
the product owner supplies it at gate time.

**Type consistency.** `ChronologyFacts`, `ChronologySolution`,
`AssetChronology`, `OrderKey`, `DisplayTime`, `DisplayMode`, `Contradiction`,
`EditPlanRevision`, and `EditPlanOperation` are each defined once above and
referenced by those exact names in every consuming task. `solve` takes
`&ChronologyFacts` and returns `ChronologySolution` everywhere it appears.
`set_clock_epoch(sequence_id, start_counter, end_counter, offset_seconds)` has
the same signature in the Task 10 and Task 14 examples.

## Compiled output

This plan is compiled into bounded task packets at `docs/tasks/desktop/EPIC.md`.
Execute with `executing-bounded-task-cycle`, one ready task per invocation.
