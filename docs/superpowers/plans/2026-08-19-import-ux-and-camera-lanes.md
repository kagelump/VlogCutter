# Plan: Smart ingest UX, camera/card chronology hierarchy, and project lifecycle

One task per GitHub issue. Each task ends in its own commit on `main`, verified
locally (no GitHub Actions — the owner is out of quota), and closes its issue.

Source of truth for each task's requirements is the GitHub issue body. Read it
with `gh issue view <N>` before starting; the task text below restates the
essentials and adds repository-specific direction.

## Global Constraints

These bind every task. They come from `AGENTS.md` §2, §10, §12, §15, §16.

- **Originals are immutable.** Nothing in this plan may delete, move, rename or
  rewrite source media or archive copies. Project-local caches and catalog rows
  are the only removable state.
- **Canonical behavior lives in the owning Rust crate** (`core/rcdvlog-core`,
  `core/rcdvlog-server`). Desktop/Tauri/CLI/web are thin adapters and must not
  grow competing ingest, chronology, storage or job logic. React code calls the
  exported `api` object in `apps/desktop/src/api.ts`; no scattered `invoke(...)`.
- **Chronology changes are explicit.** Never silently apply an inferred clock,
  timezone or sequence correction. Proposals carry evidence; the user confirms.
- **Preflight does not mutate the catalog or the project.**
- **DTO parity.** A new field or command must land in Rust (`serde` camelCase),
  the Tauri command registration, `apps/desktop/src/types.ts`,
  `apps/desktop/src/api.ts`, and the CUJ harness fixture
  (`apps/desktop/src/test/harness/state.ts`) together. Settings additions need a
  Serde default so older settings files stay valid.
- **Tests are the gate, run locally.** For each task run the narrowest useful
  suites plus the required broader check from the AGENTS.md §12 table for the
  areas touched. At minimum for desktop UI work:
  `pnpm --dir apps/desktop check` and `pnpm test:desktop`. For Rust work:
  `cargo test -p rcdvlog-core` (or the owning crate) and
  `cargo fmt --all -- --check`. Coverage thresholds in
  `apps/desktop/vitest.config.ts` may be raised, never lowered.
- **Do not present CUJ/jsdom evidence as proof of media, native, persistence or
  import behavior.** That tier fakes the IPC destination. State plainly what was
  verified and at which tier, and name manual acceptance that was not run.
- **The layout invariants are enforced by tests**: three-row shell, right rail
  holds only human-authored content, notes field geometry is fixed, minimum
  window 980×680, default 1440×900.
- **Keyboard and focus**: any new control needs an accessible name, visible
  focus, and keyboard reachability. Global shortcuts return early inside text
  inputs. Update the `?` keyboard map when shortcuts change.
- **Commit style**: one commit per task on `main`, scoped to that task, with the
  repository's prose commit-message style (subject in the imperative, body
  explaining the why and the evidence). End the body with `[skip ci]` on its own
  line — GitHub Actions quota is exhausted and verification is local.
- **Working tree discipline**: inspect `git status` first, preserve unrelated
  user work, never point destructive setup at `~/Movies/VideoProjects`,
  `/Volumes`, or the repository root. Tests use `tempfile`/temp dirs.
- The real project at the path in `RCDVLOG_ACCEPTANCE_MEDIA_ROOT` (an existing
  local project catalog) may be read for acceptance, never mutated
  destructively. Never write its absolute path into Git.

## Task 1 — #64 Simplify Smart ingest preflight into an "all clear / needs attention" decision

Read `gh issue view 64` in full. Surface: `apps/desktop/src/components/ImportDialog.tsx`.

Replace the four-counter header and always-expanded chronology prose with a
decision-oriented, progressive-disclosure preflight:

- Lead with the outcome: a clear `Ready to import` state plus only the numbers
  that matter (e.g. `8 new clips · 8 proxies will be made`); zero-value counters
  and bookkeeping counts move behind a details disclosure.
- Camera-first title hierarchy: `Sony — Volume Untitled` with the clip count on
  its own line. A generic volume label such as `Untitled` must not be the
  primary identity.
- Routine sequence proposals collapse to a concise line that says they can be
  reviewed after import (`Chronology suggestions ready for later review`). A
  sequence condition that genuinely needs attention stays elevated and is
  visually distinguishable from a routine suggestion.
- Every attention warning (collisions, unreadable, blocked destination) gets a
  direct action that opens/focuses the affected rows in the file ledger
  (e.g. `Review 3 files`). No dead-end "needs attention".
- Destination, byte cost/headroom, verification choice, proxy choice and the
  Import button stay primary. Import stays one-click when preflight is safe; do
  not add a chronology confirmation gate.

Acceptance: component/CUJ coverage pins the all-clear case, the attention case,
and the generic-volume camera-first title. The summary must read correctly for
all-new, all-known, mixed, relink-only and missing-proxy combinations.

## Task 2 — #65 Rich file previews in Smart ingest

Read `gh issue view 65` in full. Surfaces: `ImportDialog.tsx` (`FileLedger`),
`apps/desktop/src/types.ts` (`PreflightItem`, `ImportRunReport.files`), and the
preflight owner in `core/rcdvlog-core`.

Every candidate row shows a thumbnail (or explicit placeholder), the
capture/best-available datetime, the duration, the filename, and the current
classification/warning. Constraints:

- Preflight still must not mutate the catalog or project. A transient
  import-preview cache/session is acceptable; normal project Q1 cache entries
  for assets that do not exist yet are not.
- Reuse metadata already probed during classification instead of re-running
  ffprobe/ExifTool.
- Bounded/lazy extraction: opening the dialog on a several-hundred-clip card
  must not eagerly decode every file. Visible-row or bounded-worker extraction.
- A failed thumbnail or missing metadata degrades that row only and never turns
  a valid import into a blocked import.
- The post-import file ledger reuses the same row presentation.

Acceptance: component/CUJ coverage for a normal row, an unknown-metadata row,
and a warning row.

## Task 3 — #56 Finish capture-sequence boundary detection, ingest preview, and per-clip reassignment

Read `gh issue view 56` in full. Surfaces: `core/rcdvlog-core/src/preflight.rs`
and the chronology modules; the desktop chronology drawer/workspace.

First establish what already exists — `ImportSequenceBoundaryEvidence` in
`types.ts` already names volumeChange, counterReset, folderReset, prefixChange
and largeDiscontinuity, so part of the detection work may have landed since the
issue was filed. Report precisely what was already done and implement the rest:

1. Detectors in core preflight for volume identity, folder path, filename
   prefix and timestamp discontinuity, each producing a proposal **with
   evidence**, never silently applied.
2. Ingest preview states the sequence delta ("extends A7IV-seq-3 by 41 clips;
   proposes a new sequence for 12 clips after a counter reset") alongside the
   project-scoped delta.
3. Reassignment: move a clip (and optionally the clips following it) to another
   capture sequence, reachable from the chronology workspace or the queue.
   Build on the existing split/merge operations; it must be an explicit,
   operation-backed domain change with core tests.
4. Acceptance fixtures: synthetic card layouts for each boundary kind
   (counter rollover, reformat-reuse, folder split, prefix change, time gap) so
   detectors are testable without physical cards.

## Task 4 — #71 Select/override the camera identity during import and remember it

Read `gh issue view 71` in full.

Smart ingest shows the detected camera identity before import and lets the user
pick an existing project camera or deliberately create a new one. The chosen
identity feeds both asset ingest and capture-sequence preflight so the two
cannot disagree, and the sequence preview updates when it changes.

Persist the mapping **per project**, keyed on durable evidence (EXIF make/model,
folder pattern, filename prefix, card evidence) rather than an ephemeral mount
path, so a reformatted card from the same camera inherits the mapping while a
different camera sharing a generic volume label does not. Ambiguous evidence
must make the uncertainty visible rather than silently applying a remembered
mapping. The user can inspect, change and clear stored mappings.

Acceptance: CUJ coverage for wrong auto-detection → choose existing camera →
import → reinsert a matching new card → remembered choice, plus persistence
across app restart and a changed mount path.

## Task 5 — #69 One visible lane per physical camera

Read `gh issue view 69` in full. Surfaces: the chronology workspace's
`chronologyLanes()` (groups on `orderKey.captureSequenceId` today),
`CalibrationPanel.previewSplit()`, and `StoryRibbon.tsx` (groups on
`Asset.camera`).

Model: **camera = lane; card / capture sequence = a segment inside that lane.**
Group the chronology workspace's visible lanes by camera identity, and render
each capture sequence as a clearly marked, individually selectable run inside
its camera lane. Card boundaries stay visible and inspectable — they carry
filename counters, clock epochs and chronology evidence. Do not remove capture
sequences or merge their counter spaces to obtain one lane. Sequence-specific
operations (clock correction, split, reassign, merge) remain reachable from the
run inside the lane, and Review's Story Ribbon and the Chronology workspace use
compatible camera-versus-card terminology.

Acceptance: accepting a proposed "new card" split does not increase the visible
camera-lane count; sequence clock epochs still apply to their own sequence;
browser/CUJ coverage includes one camera with two cards plus a second camera.

## Task 6 — #70 Explicit camera merge

Read `gh issue view 70` in full.

Add a first-class `Merge with…` from a camera lane: choose a destination camera
identity from existing project cameras, preview the affected clip and
capture-sequence counts, confirm. Afterwards Review and Chronology both show one
physical camera.

This is a camera-identity merge, not a card/capture-sequence merge: capture
sequence IDs and card boundaries are preserved, and existing clock epochs and
sequence facts keep targeting their original sequence IDs. Fix the known
consistency hazard — today `rename_camera` updates `assets.camera` only and
leaves `capture_sequences.camera_key` behind, so Review and Chronology can
disagree. The merge must keep both coherent.

Acceptance: regression coverage for `Camera A` + `Camera A (wrong)` collapsing
to one camera that still holds both sets of cards, including the case where the
destination already has multiple capture sequences.

## Task 7 — #73 Show imported clips progressively

Read `gh issue view 73` in full. Surfaces: `core/rcdvlog-core/src/preflight.rs`
(`run_import` / `import_one`), `apps/desktop/src/App.tsx` (`onImportProgress`),
`apps/desktop/src/session/projectSession.ts`.

After each file's catalog commit succeeds, emit the committed asset (stable
asset id at minimum, full `Asset` if practical) and patch it into the open
project session so the Review queue and the Story Ribbon gain it immediately, in
correct solved order rather than appended by completion order. The useful
boundary is one successfully committed file — not a project reload per byte;
chronology may reconcile at a bounded cadence. The final completion reload stays
as a reconciliation safety net and must converge to the same state as a fresh
project open.

Nothing speculative: a file still copying or verifying is never shown, failed
and cancelled files leave no phantom rows, and already-committed clips survive a
later cancellation. Selection must stay stable — do not steal focus to the
newest clip.

Acceptance: CUJ coverage for `commit first item → row and ribbon visible →
import still running → later items appear`, plus a cancellation case.

## Task 8 — #67 Recover interrupted imports after drive disconnect

Read `gh issue view 67` in full. Surfaces: `docs/tasks/import/ARCHITECTURE.md`
§4.4/§5, `apps/desktop/src/App.tsx` (mount polling,
`dismissedCardSessionsRef`, `reviewMountedCard`), `TopBar.tsx`.

Treat source loss as a recoverable import state, not a generic error: stop the
affected work safely, keep the session/report state visible, and offer
`Reconnect source` / `Choose source…` / `Cancel import`. When the same card
session remounts, recognize it and offer Resume after a safe re-preflight.
Resume re-classifies source and destination — already committed and verified
files stay committed and are never recopied, and resume is never inferred from a
pathname alone. Destination root, verification choice, proxy preference and the
session identity all survive the interruption.

Fix the frontend recovery trap: the card session is added to the dismissed set
as soon as the user chooses to review it, while mount polling suppresses card
updates when the dialog is open, so the same card can fail to re-surface.

Add a re-entry point under the existing Add media / media menu
(`Resume interrupted import` / `Current import…`). `Review last import` is
history and is not a substitute. The global busy/operation state must clear or
transition correctly so unrelated review UI is not permanently disabled, and
Cancel stays safe and idempotent from the disconnected state.

Acceptance: CUJ coverage for disconnect → remount same session → resume, plus
rejection of a different card mounted at the same path.

## Task 9 — #66 Sticky "import from here" cutoff

Read `gh issue view 66` in full.

From the preflight file preview, a user marks a specific clip as
`Import from here` / `Ignore everything before this`. The dialog shows a
persistent cutoff marker on that exact row and how many earlier candidates are
excluded, using the row presentation built in Task 2 so the rule never becomes
an invisible filter.

Store a stable ordering boundary — capture timestamp with a deterministic
tie-breaker, or camera filename counter/prefix — scoped to the project, not a
transient absolute mount path, so it survives remounts and path changes. Import
considers only clips at or after the cutoff while still classifying known and
relinkable content correctly. Excluded files are not reported as failures or
warnings and are not copied, ingested or proxied; the preflight summary counts
`excluded by cutoff` separately from `already known`. The rule must not quietly
suppress an explicit relink/recovery of an asset that genuinely needs its
original repaired. The user can inspect, move and clear the cutoff from the UI.

Acceptance: CUJ coverage proving set → close and reopen → remembered → clear.

## Task 10 — #72 Fix the truncated timestamp and make the local display timezone editable

Read `gh issue view 72` in full. Surfaces: `StoryRibbon.tsx`
(`chronology-position` readout), `apps/desktop/src/styles.css`
(`.chronology-position { max-width: 180px; … }`),
`apps/desktop/src/chronology/geometry.ts` (`formatChronologyTime`),
`core/rcdvlog-core/src/chronology/phases.rs`.

Show the full selected position (`#32 · Aug 8, 09:21:23 · Europe/Madrid`) when
the window has room; the hard 180px clip goes away. Responsive behavior needs an
intentional priority order — never leave an ambiguous wall clock with the zone
truncated to `U…`.

Make the zone label clickable and keyboard-reachable to choose an IANA
timezone, with an explicit scope (e.g. the current trip phase/range). A covering
trip phase stays authoritative; a guess is visibly a guess until persisted; the
host/browser timezone is never used silently; the user can choose UTC again.
DST resolves through IANA rules, not a fixed numeric offset. **Display timezone
and camera clock correction stay separate concepts**: changing the display zone
must not reorder clips or change `tripTime`/camera correction facts.

Acceptance: browser-tier coverage reproduces the screenshot-length string and
proves the zone stays legible.

## Task 11 — #68 Story Ribbon / window overflow during import

Read `gh issue view 68` in full. Surfaces: `StoryRibbon.tsx`
(`story-scroll` / `story-canvas`), `styles.css` (fixed app-shell rows,
`.story-ribbon { overflow: hidden; }`), `TopBar.tsx` (import operation slot).

The app shell always fits the native viewport during every import stage
(scanning, copying, reading, proxies, complete, error). `story-scroll` stays the
sole horizontal scroll owner for the chronology canvas; no second page or shell
scrollbar appears. Top-bar import progress truncates or collapses before it can
widen the shell, including with long current-file names, with the ribbon
expanded to multiple camera lanes, and at max zoom.

Acceptance: a real-browser (`pnpm test:desktop:browser`) regression state that
renders the shell with active import progress and asserts
`scrollWidth <= clientWidth` for the body and app shell while the ribbon keeps a
wider internal scroll area. Check 1280×800, 1440×900 and a wide window.

## Task 12 — #16 Project deletion and cache lifecycle

Read `gh issue view 16` in full.

Add a project/storage lifecycle surface reachable from Settings **and** from the
opening project library modal:

- Show each project's catalog location and approximate cache/storage usage,
  broken down enough that the user understands what a cleanup removes.
- Reclaim disposable representations (proxies, thumbnails, other regenerated
  cache) with a clear preview, and explain which representations regenerate and
  what is temporarily unavailable afterwards.
- Delete a project through an explicit destructive confirmation that clearly
  distinguishes deleting VlogCutter project state from deleting media.

Hard limits: no lifecycle action deletes, moves, renames or rewrites originals
or archive copies. Cleanup works with offline/unavailable media — mounting
originals must never be a precondition for deleting local cache. Long-running
cleanup reports progress and per-item failures without corrupting the project.
The destructive path belongs in the owning Rust crate with core tests covering
missing files, offline media and partial failure.

Non-goals: automated eviction policy, deleting source/archive originals,
companion-server storage administration, edit-plan work.
