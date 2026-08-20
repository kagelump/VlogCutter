# Desktop MVP architecture contract

Authority for epic `D01`. Where this document and a task packet disagree, this
document wins and the packet is a defect. Where this document and `PRD.md`
disagree, `PRD.md` §21 (MVP Scope Contract) and §25.1 (Accepted MVP decisions)
win and this document is a defect.

## 1. What this epic is correcting

The optional companion server epic (`docs/tasks/server/EPIC.md`, 20/20 complete)
and the travel ingest epic are further along than the desktop application they
exist to serve. The desktop is the product; the server is optional. This epic
rebalances that by putting the existing desktop under real CI and real native
acceptance, finishing chronology as a workflow a person can actually use, and
adding the smallest editing handoff that completes the MVP loop.

Chronology is the project's core differentiator. It is sequenced first, before
any editing work, and it is validated against real trip footage.

## 2. Frozen constraints

These are inherited from `AGENTS.md` §2 and are not renegotiable inside this
epic.

- Original media is immutable. No task writes into a source media tree.
- **No chronology correction ever mutates source metadata.** Corrections are
  separate persisted facts (`PRD.md` §9.2).
- SQLite is canonical local state. Product behavior is never achieved by
  hand-editing SQLite; it is achieved by a domain operation plus a migration.
- Existing catalogs migrate forward. Delete-and-rebuild is not a migration.
- Every project mutation is a typed operation carrying HLC logical time, an
  aggregate type and id, a payload version, and a base revision. Chronology and
  edit-plan state are ordinary operations, not a parallel model.
- The desktop never waits on a network round trip to commit a local edit.
- Absolute paths are node-local. Protocol state uses logical root IDs plus
  relative paths.
- No chronology change is applied without explicit user confirmation
  (`PRD.md` §21.4).
- The Vite browser mock is never evidence that persistence, media processing, or
  native behavior works.

## 3. Chronology architecture

### 3.1 Separation of hypothesis from derivation

Persisted chronology state is only ever *evidence*:

| Persisted fact | Meaning |
| --- | --- |
| Raw camera timestamp | What the camera claimed. Never edited. |
| Capture sequence | A likely monotonic filename chain, scoped to one card. |
| Clock epoch | One offset model over a contiguous counter range in one sequence. |
| Clip time exception | A single-clip override that breaks its sequence. |
| Trip phase | Where the user physically was, with a timezone. |
| Event-order constraint | Ordering knowledge independent of exact time. |

Trip time, local display time, confidence, evidence, uncertainty, and total
order are **derived**. They are recomputed by a pure solver from the facts above
and never edited directly. `assets.best_time` and `assets.offset_seconds` remain
in the schema as projections of the solution for query performance; they are not
authority and must never be written by anything other than solution projection.

### 3.2 Solver contract

The solver is a pure function. It performs no I/O, reads no wall clock, and
consults no environment. Given identical facts it produces an identical
solution, including identical evidence ordering.

Determinism is a hard requirement, not a quality goal, because the review queue,
the CLI, and OTIO export must all agree on order. The realistic way determinism
breaks in Rust is incidental reliance on `HashMap` iteration order; the solver
uses ordered collections or explicit sorts on every path that reaches output.

Ordering uses a total `OrderKey` with no ties: trip time, then capture sequence
id, then filename counter, then filename, then asset id. Two distinct assets can
never compare equal.

### 3.3 Capture-sequence identity

Filename identity is scoped to its capture sequence. Two SD cards from the same
camera routinely both contain `C0001.MP4`; treating filename as project-unique
is a correctness bug, not a simplification. Sequence boundaries come from
different volume, counter reset, folder reset, prefix change, large
discontinuity, or explicit user split (`PRD.md` §8.2).

### 3.4 Timezone authority

Local display time derives from the trip phase covering the asset's trip time.
Camera metadata timezone is evidence about the camera, not about where the user
was, and is never the display authority. Transit phases carry origin and
destination timezones and a dual display mode (`PRD.md` §10.2).

An asset no phase covers displays in UTC. That is deliberate: the host's
timezone says where the editor is sitting today, which is not evidence about
where the footage was shot, and a confident wrong local clock is worse than an
honest UTC one. What the UI owes that state is a way out, not a guess (#72):

- The Story Ribbon's selected-position readout renders ordinal, wall clock, and
  zone as three independently laid-out parts. The clock and the zone are only
  meaningful together, so neither ever truncates; the ordinal yields first, then
  the date, and the ribbon's zoom slider yields before any of them.
- The zone is a control. A covered clip opens its phase; an uncovered one opens
  the range flow, so the scope of the answer is named before it is given.
- The project's own phases may *propose* a zone for an uncovered range, labelled
  with the phase it came from. A proposal is offered, never prefilled, and
  nothing is stored until the user saves a phase.

Display timezone and camera clock correction stay separate concepts. Changing
the former edits a phase and cannot reorder clips or alter `tripTime`; only a
clock epoch does that.

### 3.5 Contradictions

Hard contradictions block apply and name every conflicting piece of evidence.
Soft contradictions report without blocking. A contradiction is reported against
the user's evidence, never resolved silently in the user's favor, and never
resolved by discarding a fact.

### 3.6 Review-queue stability

A chronology correction may reorder many clips. The intended next review item is
captured before the change and restored after it, so a correction never costs
the reviewer their place (`PRD.md` §12).

### 3.7 Forward compatibility of new operation types

`apply_project_operation` currently fails an unknown operation type outright
(`core/rcdvlog-core/src/operations.rs`, `bail!("unsupported operation type")`).
Adding chronology operation types therefore changes what an older peer does with
a newer desktop's operations. `D01-T007` owns this decision and must implement a
quarantine-and-report policy: an unknown operation is retained and surfaced, not
silently dropped, and not allowed to fail an entire sync batch.

## 4. Editing-handoff architecture

### 4.1 Accepted scope, exhaustively

The MVP editing handoff is **only**:

- one video track;
- linked source audio;
- hard cuts;
- source in/out ranges;
- ordered edit-plan operations;
- basic low-resolution preview;
- deterministic OTIO export;
- verified import into DaVinci Resolve 20/21.

There is no track field, no transition field, no effect field, no color, and no
audio mixing — not as unimplemented stubs, not as reserved schema columns, not
as commented-out code. `PRD.md` §20 names scope expansion into full editing as a
primary project risk; the defense is that the data model cannot express it.

Timeline order is the index of the operation in the ordered list. There is no
separate timeline-time field to fall out of sync with it.

### 4.2 Determinism of export

Identical plan input produces byte-identical OTIO output. This is verified with
golden files, not by re-reading the writer's own output through the writer.
Clip and metadata identifiers are derived from stable inputs, never from
allocation order, iteration order, timestamps, or random values.

### 4.3 OTIO implementation

A pure-Rust OTIO JSON writer. No Python runtime dependency is added to the
desktop application. `PRD.md` §21.3 forbids introducing a permanent Python or
model-provider dependency in anticipation of later work, and an export path is
not a good enough reason to breach it. The cost accepted in exchange is that
this repository owns OTIO schema fidelity, which is why the Resolve import
acceptance packet is required rather than optional.

### 4.4 Revision safety

An edit-plan revision can never silently replace a newer one. Mutations that can
replace an active artifact require an expected revision id, matching the
existing script-revision discipline. A plan derived from an older script
revision is visibly stale in both the UI and the CLI.

### 4.5 Validation precedes work

Asset existence, media availability, range bounds, overlap, unsupported
operation, and revision staleness are all checked before any render or export
begins. Each failure carries a stable machine-readable error code. Rendering
must never be the mechanism by which a user discovers their plan is invalid.

## 5. Verification architecture

### 5.1 CI topology

GitHub Actions, two jobs:

- `ubuntu-latest` runs the full headless gate: `cargo fmt`, `cargo clippy
  --workspace --all-targets -- -D warnings`, `cargo test --workspace`, the
  TypeScript checks, the desktop and web unit suites, the CLI process
  integration suite, and the OpenAPI/docs/deployment checks.
- `macos-latest` runs `pnpm --dir apps/desktop tauri build` and `cargo test -p
  rcdvlog-desktop`, so a change that breaks the native desktop bundle cannot
  reach `main` green.

CI enforces exactly the set of commands measured green in
`docs/tasks/desktop/BASELINE_AUDIT.md`. A check that cannot pass yet is an
explicit tracked row in that document. It is never silently omitted from the
workflow, because a green badge over an unenforced gate is worse than no badge.

### 5.2 What CI cannot prove

Per `AGENTS.md` §12, CI is not evidence for: DaVinci Resolve import, macOS
dialogs and filesystem permission prompts, native media playback,
sleep/wake, source-volume disconnect and reconnect, expo-scale NAS performance,
or target-laptop thermal behavior. Those require the manual acceptance packets
in this epic and are tracked as such.

### 5.3 Desktop frontend testability

`apps/desktop` has no test runner and `apps/desktop/src/App.tsx` is 2,103 lines
with zero tests, while `apps/web` has vitest, testing-library, and Playwright.
This epic corrects that imbalance before adding the calibration surface to the
same file.

The correction is deliberately narrow. Pure logic — review ordering, keyboard
mapping, lane layout, impact diffing — moves into small focused modules that can
be tested directly. Rendering stays where it is. `App.tsx` is not rewritten, and
no behavior changes as part of the extraction. `AGENTS.md` §15 forbids
reformatting large unrelated files, and a testability seam is not a licence to
restructure the application.

### 5.4 Real media over synthetic media

Synthetic fixtures are development tools. The chronology release gate is real
trip footage with real multi-camera, multi-timezone, date-line-crossing
structure, because that is the pattern the solver exists to handle and the
pattern synthetic corpora are least likely to reproduce faithfully.

## 6. Frozen decisions of record, 2026-07-29

| Decision | Value | Rationale |
| --- | --- | --- |
| CI provider and matrix | GitHub Actions; `ubuntu-latest` full gate plus `macos-latest` `tauri build` | The desktop is the product; a gate that never compiles it is not a gate. |
| Chronology release evidence | Antarctica trip footage, path supplied by the product owner at gate time | Real multi-timezone structure; synthetic corpora are not release evidence. |
| Resolve availability | Resolve 20/21 installed and usable | The OTIO import gate is a required manual acceptance packet, not an open risk. |
| OTIO implementation | Pure-Rust deterministic JSON writer | Avoids a permanent Python dependency; cost is owning schema fidelity, offset by the Resolve gate. |

## 7. Related epics

- `docs/tasks/server/EPIC.md` — optional companion server, complete.
- `docs/tasks/travel/EPIC.md` — travel ingest, in progress.

Neither is a prerequisite for this epic. Desktop MVP completion must not depend
on a paired server being present.
