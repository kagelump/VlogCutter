# Camera filename patterns — documented contract

Published by D01-T028
(`docs/tasks/desktop/tasks/D01-T028-camera-identification-and-filename-patterns.md`),
2026-08-03. The table lives in `core/rcdvlog-core/src/cameras/patterns.rs`; this
document is its human-readable contract and the place to record why each row
exists.

## Why this exists

Two defects, both found against real footage rather than fixtures:

- **Cameras went anonymous.** `exif_camera` reads EXIF Make and Model and fell
  back to the literal `"Unknown camera"`. The smoke run recorded all 420 DJI
  Pocket 3 clips resolving to that fallback. Migration 9 groups capture
  sequences by `assets.camera`, so every unidentified camera in a project
  collapsed into one sequence — precisely the collision that scoping filename
  identity to a sequence exists to prevent (`PRD.md` §8.2: two cards
  legitimately both contain `C0001.MP4`).
- **Counters went NULL.** The trailing-digit rule takes the last digit run of the
  stem. `DJI_20251221092058_0001_D.MP4` ends in `_D`, so its counter was NULL —
  and a NULL counter is never inside a bounded range and never moves in a
  counter split. On a three-camera corpus of 621 clips, 420 of them (68%) could
  not be targeted by any counter-ranged correction at all.

## Resolution order

**EXIF → filename pattern → `"Unknown camera"`.**

EXIF is the camera's own claim about itself and is authoritative when present. A
pattern is inference and never overrides a fact: a clip with valid EXIF is
identified from EXIF even when its filename also matches a pattern, and the
pattern is not consulted.

## The table

Evaluated in order; the first match wins. Strong patterns precede weak ones.

| Id | Label | Stem pattern | Counter group | Confidence |
| --- | --- | --- | ---: | --- |
| `dji-pocket-3` | DJI Pocket 3 | `^DJI_\d{14}_(\d{4})_D$` | 1 | strong |
| `insta360-ultra-go` | Insta360 Ultra Go | `^VID_\d{8}_\d{6}_(\d+)$` | 1 | strong |
| `sony-c-series` | Sony (C-series) | `^C(\d{4})$` | 1 | weak |

Worked examples:

| Filename | Camera | Counter |
| --- | --- | ---: |
| `DJI_20251221092058_0001_D.MP4` | DJI Pocket 3 | 1 |
| `VID_20251216_134946_113.mp4` | Insta360 Ultra Go | 113 |
| `C0684.MP4` | Sony (C-series) | 684 |
| `clip.mp4` | Unknown camera | *(none)* |

## Confidence

A **strong** pattern is specific enough that a match is a real identification.

A **weak** pattern identifies the camera *and* additionally records a
`camera.weak_pattern_match` diagnostic, so the user can confirm or relabel.
`C0684.MP4` is the motivating case: `C` plus four digits is used by Sony, by
Canon cinema bodies, and by others. It is good enough to group a card and
useless as a claim about the manufacturer. Declaring the weakness is honest —
suppressing the match would lose the counter, and asserting a manufacturer would
be a guess presented as a fact.

## The counter rule

When a pattern matches, the counter is that pattern's declared `counter_group`.
When no pattern matches, the D01-T007 trailing-digit-run rule applies unchanged.

Two rejected alternatives, recorded so they are not re-proposed:

- *Widen the trailing-digit rule to the longest digit run.*
  `DJI_20251221092058_0001_D` would yield the 14-digit timestamp, which is not a
  counter and would order clips by a value that already exists as a time.
- *Widen to "last digit run before a 1–2 character suffix".* An untargeted
  heuristic that silently changes counters for cameras nobody has tested
  against.

## Migration 11

Migration 11 fills `asset_capture_sequence.filename_counter` where it is NULL
and a pattern supplies one. It does **not** rewrite `assets.camera`, does not
create or merge capture sequences, and does not touch `assets.best_time` or
`assets.offset_seconds`.

Rewriting `assets.camera` retroactively would regroup capture sequences and
reorder an existing project, which `CHRONOLOGY_MODEL.md`'s migration rules
forbid. An existing catalog whose cameras are anonymous is repaired by the user
through D01-T009's split and relabel; re-ingesting into a fresh project gets
correct identity from the start.

Backfilling the counter is safe in a way that regrouping is not: migration 9
carries legacy clock events forward with `end_counter` NULL — open-ended — and
nothing emits a bounded epoch until D01-T010, so filling a NULL counter cannot
remove an asset from any epoch's coverage in any catalog that can exist today.

## Adding a camera

A new camera is a new row in `CAMERA_PATTERNS` and a new row in the table above.
The pattern is data, not scattered branches. Requirements for a new row:

1. An anchored expression over the **stem** (extension already stripped).
2. Exactly one capture group holding the counter, named by `counter_group`.
3. A confidence that is honest about what the shape actually proves.
4. At least one real filename from the camera, recorded in the worked-examples
   table.
5. Placement: strong patterns before weak ones, and specific before general.

A row that does not compile is skipped rather than panicking a user's ingest;
`every_pattern_in_the_table_compiles_and_has_its_counter_group` fails the build
first.

## Diagnostic kinds

| Kind | Severity | Subject | Dedupe key |
| --- | --- | --- | --- |
| `camera.unidentified` | `warning` | `Project` | `camera.unidentified:<containing folder>` |
| `camera.weak_pattern_match` | `info` | `CaptureSequence` | `camera.weak_pattern_match:<pattern_id>` |

Both follow `docs/tasks/desktop/DIAGNOSTICS_MODEL.md`.
