# D01-T005: Native macOS desktop smoke acceptance

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T005",
  "epic_id": "D01",
  "title": "Native macOS desktop smoke acceptance",
  "slug": "native-desktop-smoke-acceptance",
  "status": "complete",
  "task_class": "qa",
  "depends_on": ["D01-T004"],
  "principal_invariant": "Every step of the PRD section 21.1 journey that today's build claims to implement has a recorded pass or fail result from the real native macOS application, not from the Vite browser mock.",
  "central_falsification": "Re-run three passing rows and every failing row on a second launch from a clean projects root; a row whose result does not reproduce, or that turns out to have been exercised through the browser mock, falsifies the acceptance.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["PRD.md", "AGENTS.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "independent_tester": ["PRD.md", "AGENTS.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "reviewer": ["PRD.md", "AGENTS.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "integrator": ["docs/tasks/desktop/EPIC.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T005-native-desktop-smoke-acceptance.md"],
    "implementer": [],
    "independent_tester": ["docs/desktop/smoke-test.md", "docs/tasks/desktop/SMOKE_LEDGER.md"],
    "integrator": ["docs/task-cycles/D01-T005/**"]
  },
  "forbidden_paths": ["core/**", "apps/**", "packages/**", "scripts/**", ".github/**", "Cargo.toml", "Cargo.lock", "package.json", "PRD.md", "AGENTS.md"],
  "verification_commands": [
    "pnpm --dir apps/desktop tauri build",
    "cargo test -p rcdvlog-desktop"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T005\\].*\\| complete \\|"
    },
    {
      "path": "docs/desktop/smoke-test.md",
      "regex": "(?s)## Checklist.*Tauri asset-protocol playback"
    }
  ]
}
-->

## Goal

Run the real macOS desktop application through the currently implemented portion
of the `PRD.md` §21.1 user journey, record what actually works, and publish both
a repeatable checklist for future use and a defect ledger for D01-T006.

CI cannot do this. `AGENTS.md` §12 explicitly forbids treating headless tests as
evidence for macOS dialogs and filesystem permission prompts, Tauri
asset-protocol playback, sleep/wake, or source-volume disconnect and reconnect.
This packet is where those get exercised.

## Principal invariant

This task is correct when every step of the `PRD.md` §21.1 journey that today's
build claims to implement has a recorded pass or fail result obtained from the
real native macOS application.

## Inputs and outputs

Inputs:

- A release build from `pnpm --dir apps/desktop tauri build`.
- `PRD.md` §21.1, steps 1–14, and the implementation checkpoint at the top of
  `PRD.md` naming what is and is not implemented.
- A small real media folder: 10–30 clips, at least two distinct cameras, mixed
  H.264 and H.265, at least one clip with a non-ASCII filename and one with
  spaces in its path. Real camera files, not synthetic MPEG-4, per `AGENTS.md`
  §14.
- An external or disk-image volume that can be ejected and remounted, to exercise
  the offline-original path without touching a real NAS.

Outputs:

- `docs/desktop/smoke-test.md` — the repeatable checklist, written so a future
  run needs no reconstruction: exact preconditions, exact steps, exact expected
  observation per row.
- `docs/tasks/desktop/SMOKE_LEDGER.md` — this run's results: one row per
  checklist item with `pass`, `fail`, or `not_implemented`, plus an `SD-n` defect
  section for every `fail`.

## Frozen decisions

- **The native app only.** The Vite browser preview is not acceptable evidence
  for any row, per `AGENTS.md` §15. Every row is exercised in the built Tauri
  application.
- **A `not_implemented` row is not a defect.** The `PRD.md` checkpoint already
  states that ingest preview, capture-sequence editing, trip-context persistence,
  expanded calibration, edit planning, preview render, timeline export, and
  relink are absent. Record those as `not_implemented`; they are this epic's later
  packets, not bugs.
- **A `fail` is a defect in something that claims to work.** That is the ledger
  D01-T006 fixes.
- **No source media is written to.** Enumerate read-only. The project catalog
  lives under the configured laptop-local projects root, never inside the source
  tree, per `AGENTS.md` §13.
- **Defect ids `SD-1`, `SD-2`, … are stable** once published and are referenced
  by D01-T006.
- Screenshots or screen recordings are the evidence format for visual and
  playback rows. A prose claim that playback worked is not evidence.

## Owned paths

Every production path is forbidden. A tester who fixes what they find has
destroyed the oracle, and the whole value of this packet is that its ledger was
produced by someone with no stake in it being short. The independent tester owns
the checklist and the ledger; the integrator owns cycle evidence.

## Required interface

The ledger's shape is fixed because D01-T006 consumes it:

```md
## Run

- Build: `<tauri build output path>`
- App version, commit SHA, macOS version, machine
- Source folder: `<path>`, clip count, cameras present
- Projects root: `<path>`

## Checklist results

| # | Row | Result | Evidence |
| ---: | --- | --- | --- |
| 1 | Create a project | pass | `screenshots/01-create.png` |

## Defects

### SD-1: <one-line summary>

- Row: `<checklist row>`
- Reproduction: numbered steps from a clean launch
- Observed / Expected
- Reproduces on relaunch: yes / no
- Evidence: `<file>`
```

## Acceptance oracle

Every row below is exercised in the native app and recorded. The expected
observation is stated so the result is not a matter of opinion.

1. **Create a project.** Named project appears in the library and survives an app
   relaunch.
2. **Set the projects root.** The chosen root is used for the catalog; the
   catalog file exists on disk at the expected path.
3. **Add a media folder.** Ingest completes; the reported asset count equals the
   number of media files in the folder. Non-ASCII and space-containing filenames
   are present and correctly displayed.
4. **Originals referenced in place.** No media byte is copied into the projects
   root. Verify by comparing the source folder's size and modification times
   before and after, and by confirming the catalog stores source paths.
5. **Thumbnails generated.** Every asset shows a thumbnail. Progress and
   estimated time remaining update during the batch.
6. **Proxies generated.** Every asset gets a review proxy. Interrupt the batch by
   quitting mid-run, relaunch, and confirm completed work was preserved and the
   batch resumes rather than restarting.
7. **Asset-protocol playback.** A proxy plays in the player with audio. Seek to
   the middle and confirm the frame updates. This is the row CI cannot reach.
8. **Rotation.** A portrait clip displays with the correct orientation; changing
   rotation persists across relaunch.
9. **Labels, notes, reversible removal.** All three persist across relaunch. A
   removed clip is recoverable, not destroyed.
10. **Keyboard-first review.** Next, previous, toggle removal, zoom, and centre
    selection all work from the keyboard without the mouse. Typing in the notes
    field does not trigger review shortcuts.
11. **Clock correction.** Applying a clock event to one camera at a filename
    boundary changes displayed times for that camera at and after the boundary,
    and for nothing else.
12. **Review position resume.** Quit while on a specific clip; relaunch and
    confirm the review position is preserved.
13. **Volume disconnect.** Eject the source volume with the app running. The app
    reports originals offline without crashing, and proxy review continues to
    work.
14. **Volume reconnect.** Remount. Originals are recognized as available again
    with no corruption and no duplicate assets created.
15. **Sleep and wake.** Sleep the machine with the app open mid-batch; wake and
    confirm the app is responsive and job state is coherent.
16. **Relaunch integrity.** After all of the above, relaunch and confirm project
    state, job state, labels, notes, removals, and rotation are all intact.

## Independent test charter

Falsify the claim that the ledger reflects the native application's real
behavior.

- Re-run three rows recorded `pass` and every row recorded `fail`, from a **fresh
  projects root** and a fresh launch, using your own media folder with different
  clips, different cameras, and a different non-ASCII filename than the
  implementer used.
- For each `fail`, confirm it reproduces. A non-reproducing `SD-n` is either a
  flake — which must be recorded as such, since intermittent failures are worse
  than deterministic ones — or a bad row.
- Attack the evidence itself: confirm no row was exercised in the Vite mock. Check
  that the recorded build path is a `.app` bundle, and that screenshots show the
  native window chrome rather than a browser.
- Verify row 4 independently by hashing the source folder's file list and sizes
  before and after ingest. Byte-level immutability of originals is the
  repository's foundational invariant and deserves its own check.
- Verify row 11 by listing displayed times for every asset of both cameras before
  and after the clock event, and confirming no asset outside the declared range
  moved.
- Verify row 13 by ejecting during an active proxy batch, not while idle, which is
  the harder and more realistic case.
- Confirm no file under the source media folder was created, modified, or deleted.

## Review charter

Risk domain: acceptance theatre. The failure mode is a ledger that records passes
for rows that were skipped, exercised in the browser mock, or judged by
impression.

Questions:

1. Does every `pass` row have concrete evidence, and does that evidence show the
   native application?
2. Is any row recorded `not_implemented` that the `PRD.md` checkpoint claims is
   implemented? That is a defect being misfiled as absence.
3. Is any row recorded `pass` where the expected observation in this packet was
   not actually checked?
4. Were originals provably untouched?
5. Is the checklist in `docs/desktop/smoke-test.md` genuinely repeatable by
   someone who was not present, with exact preconditions?
6. Are defect reproductions written from a clean launch, or do they assume the
   tester's accumulated state?

Severity rules: a `pass` without evidence, a row exercised in the browser mock, a
defect misfiled as `not_implemented`, or any modification to source media is
blocking. Missing wall times and screenshot quality are non-blocking.

Smallest acceptable remedy: re-run the specific row in the native app and attach
evidence.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Fixing any defect found. That is D01-T006.
- Scale acceptance over thousands of assets. That is separate periodic manual
  acceptance per `AGENTS.md` §13, not this smoke test.
- Exercising chronology beyond today's flat clock-event behavior.
- Exercising edit plan, preview render, or export, which do not exist yet.
- Server pairing, sync, or the Chrome client.
- Performance measurement or thermal behavior.

## Stop conditions

Stop and escalate when:

- `tauri build` fails, which means D01-T004's `macos` job was not actually green
  and the epic has a contradiction to resolve;
- the app crashes in a way that loses committed project state — record it
  immediately as a blocking defect and stop rather than continuing to accumulate
  rows on a corrupted catalog;
- macOS denies a filesystem permission and the prompt does not appear, so the
  user must intervene;
- the defect count exceeds eight, which means D01-T006 is too large for one
  packet and the contract editor must split it;
- a row cannot be exercised without media the user has not provided;
- any operation appears to have written into the source media tree — stop
  everything and report, since that breaches the repository's core invariant.

## Commands

```sh
pnpm --dir apps/desktop tauri build
cargo test -p rcdvlog-desktop
```

Then, before and after ingest, to prove originals were untouched:

```sh
find "<source folder>" -type f -exec stat -f "%N %z %m" {} \; | sort > /tmp/before.txt
find "<source folder>" -type f -exec stat -f "%N %z %m" {} \; | sort > /tmp/after.txt
diff /tmp/before.txt /tmp/after.txt && echo "originals untouched"
```

Expected result: the build produces a `.app` bundle, `cargo test -p
rcdvlog-desktop` passes, and the `diff` reports no differences.

## Handoff

Report the build path and commit SHA, the machine and macOS version, the media
folder used with its clip and camera counts, the full checklist results table,
the `SD-n` defect list with its count, and the evidence file list. State
explicitly that no row was exercised in the browser mock and that source media
was untouched, with the `diff` output as evidence. List any stop condition
encountered. The tester does not approve this task and does not fix what it
finds.
