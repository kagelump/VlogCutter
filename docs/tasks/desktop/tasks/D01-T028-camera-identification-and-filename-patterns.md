# D01-T028: Camera identification and filename patterns

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T028",
  "epic_id": "D01",
  "title": "Camera identification and filename patterns",
  "slug": "camera-identification-and-filename-patterns",
  "status": "draft",
  "task_class": "implementation",
  "depends_on": ["D01-T007", "D01-T027"],
  "principal_invariant": "A clip whose EXIF does not name a camera is identified from its filename pattern when a known pattern matches, and that pattern also supplies the filename counter, so a camera with a suffixed filename is neither anonymous nor counterless.",
  "central_falsification": "Ingest DJI Pocket 3, Insta360, and Sony clips with EXIF make and model stripped, and confirm each is identified as its own camera with a non-NULL, monotonically increasing counter; an anonymous camera or a NULL counter falsifies the identification.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": ["filename -> camera identity + filename counter"],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "docs/tasks/desktop/DIAGNOSTICS_MODEL.md", "PRD.md", "AGENTS.md"],
    "independent_tester": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md"],
    "reviewer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "docs/tasks/desktop/DIAGNOSTICS_MODEL.md", "PRD.md", "AGENTS.md"],
    "integrator": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "docs/tasks/server/SCHEMA.md"]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/desktop/tasks/D01-T028-camera-identification-and-filename-patterns.md",
      "docs/tasks/desktop/CHRONOLOGY_MODEL.md",
      "docs/tasks/desktop/CAMERA_PATTERNS.md"
    ],
    "implementer": [
      "core/rcdvlog-core/src/cameras/mod.rs",
      "core/rcdvlog-core/src/cameras/patterns.rs",
      "core/rcdvlog-core/tests/camera_identification.rs",
      "core/rcdvlog-core/tests/fixtures/cameras/**"
    ],
    "independent_tester": [
      "core/rcdvlog-core/tests/camera_identification_independent.rs",
      "core/rcdvlog-core/tests/fixtures/cameras-independent/**"
    ],
    "integrator": [
      "docs/tasks/server/SCHEMA.md",
      "docs/tasks/desktop/EPIC.md",
      "docs/task-cycles/D01-T028/**"
    ]
  },
  "forbidden_paths": ["apps/**", "packages/**", "core/rcdvlog-server/**", "core/rcdvlog-core/src/chronology/solver.rs", ".github/**", "PRD.md"],
  "verification_commands": [
    "cargo test -p rcdvlog-core --test camera_identification",
    "cargo test -p rcdvlog-core --test chronology_migration",
    "cargo test --workspace",
    "cargo clippy --workspace --all-targets -- -D warnings",
    "cargo fmt --all -- --check"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T028\\].*\\| complete \\|"
    },
    {
      "path": "docs/tasks/desktop/CAMERA_PATTERNS.md",
      "regex": "(?s)## Pattern table.*counter_group"
    },
    {
      "path": "docs/tasks/desktop/CHRONOLOGY_MODEL.md",
      "regex": "pattern-supplied counter"
    }
  ]
}
-->

## Goal

Identify a camera from its filename when EXIF cannot, and take the filename
counter from the same pattern — so that a camera whose files carry a trailing
suffix is neither anonymous nor excluded from every counter-ranged correction.

Two defects motivate this packet, both found against real footage rather than
fixtures.

**Cameras go anonymous.** `exif_camera` in `core/rcdvlog-core/src/lib.rs` reads
EXIF Make and Model and falls back to the literal string `"Unknown camera"`. The
smoke test recorded all 420 DJI Pocket 3 clips resolving to that fallback.
Migration 9 groups capture sequences by `assets.camera`, so every unidentified
camera in a project collapses into one sequence — precisely the collision that
scoping filename identity to a sequence exists to prevent (`PRD.md` §8.2: two
cards legitimately both contain `C0001.MP4`).

**Counters go NULL.** `parse_filename_counter` takes the trailing digit run of the
filename stem. DJI Pocket 3 names files `DJI_20251221092058_0001_D.MP4`, whose
stem ends in `_D`, so there is no trailing digit run and the counter is NULL. Under
the D01-T007 contract a NULL counter "is never inside any bounded range" and a
split "moves assets with `filename_counter >= at_counter`, assets with a NULL
counter stay in the source sequence." A camera named this way therefore cannot be
targeted by any counter-ranged clock epoch and cannot be split by counter at all.
On a three-camera corpus of 621 clips, 420 of them — 68% — are affected.

## Principal invariant

This task is correct when a clip whose EXIF does not name a camera is identified
from its filename pattern when a known pattern matches, and that pattern also
supplies the filename counter, so a camera with a suffixed filename is neither
anonymous nor counterless.

## Inputs and outputs

Inputs:

- `exif_camera` and the `"Unknown camera"` fallback in
  `core/rcdvlog-core/src/lib.rs`.
- `parse_filename_counter` in `core/rcdvlog-core/src/chronology/mod.rs` and its
  frozen rule in `docs/tasks/desktop/CHRONOLOGY_MODEL.md`.
- The three filename shapes confirmed against real footage, supplied by the
  product owner:

  | Camera | Example | Shape |
  | --- | --- | --- |
  | DJI Pocket 3 | `DJI_20251221092058_0001_D.MP4` | `DJI_` + 14-digit timestamp + `_` + 4-digit counter + `_D` |
  | Insta360 Ultra Go | `VID_20251216_134946_113.mp4` | `VID_` + 8-digit date + `_` + 6-digit time + `_` + counter |
  | Sony A7C | `C0684.MP4` | `C` + 4-digit counter |

- `DiagnosticRecord` and `record_diagnostic` from D01-T027.

Outputs:

- `core/rcdvlog-core/src/cameras/patterns.rs` — the pattern table.
- `core/rcdvlog-core/src/cameras/mod.rs` — `identify_camera` and
  `pattern_counter`.
- Migration 11, backfilling `asset_capture_sequence.filename_counter` where it is
  NULL and a pattern now supplies one.
- Two new diagnostic kinds wired.
- `docs/tasks/desktop/CAMERA_PATTERNS.md` — the pattern table as a documented,
  extensible contract.
- An amendment to `docs/tasks/desktop/CHRONOLOGY_MODEL.md` recording the
  pattern-supplied counter rule.

## Decision authority

This packet owns the decisions below and is the sole authority for them.

### Decision 1 — EXIF wins, pattern is fallback, anonymous is last

Resolution order is EXIF Make/Model, then the filename pattern table, then the
`"Unknown camera"` literal.

| Alternative | Verdict |
| --- | --- |
| EXIF → pattern → literal | **Chosen.** EXIF is the camera's own claim about itself and is authoritative when present. A pattern is inference and must never override a fact. |
| Pattern → EXIF | Rejected. A pattern would silently override correct EXIF for any camera whose naming happens to collide. |
| Pattern only | Rejected. Discards the reliable signal that already works for the Sony body. |
| Prompt the user per unrecognized camera at ingest | Rejected here. Ingest must not block on a modal; the unidentified case emits a diagnostic instead, and naming is D01-T009's user-driven relabel. |

Acceptance criterion: a clip with valid EXIF Make/Model is identified from EXIF
even when its filename also matches a pattern, and the pattern is not consulted.

### Decision 2 — The pattern supplies the counter; the trailing-digit rule is the fallback

When a pattern matches, the counter is the pattern's declared `counter_group`.
When no pattern matches, the D01-T007 trailing-digit-run rule applies unchanged.

This amends the frozen contract in `CHRONOLOGY_MODEL.md`. `CHRONOLOGY_MODEL.md`
states that a change to its contract is a new task with a new migration; this is
that task, and the contract editor records the amendment as part of this packet.

| Alternative | Verdict |
| --- | --- |
| Pattern-supplied counter, trailing-digit fallback | **Chosen.** Fixes the 68% NULL case without changing behaviour for any filename that already parses. |
| Widen the trailing-digit rule to "longest digit run" | Rejected. `DJI_20251221092058_0001_D` would yield the 14-digit timestamp, which is not a counter and would order clips by a value that already exists as a time. |
| Widen to "last digit run before a 1–2 character suffix" | Rejected. An untargeted heuristic that silently changes counters for cameras nobody has tested against. |
| Leave NULL and rely on open-ended epochs only | Rejected. It removes counter-ranged correction — a launch-critical capability in `PRD.md` §21.2 — for the largest camera in the corpus. |

Acceptance criterion: `DJI_20251221092058_0001_D.MP4` yields 1;
`C0684.MP4` yields 684; `VID_20251216_134946_113.mp4` yields 113; every example
in the `CHRONOLOGY_MODEL.md` trailing-digit list yields its documented value
unchanged.

### Decision 3 — Weak patterns identify but declare themselves

Each pattern carries `strong` or `weak` confidence. A weak pattern still resolves
a camera, and additionally records a `camera.weak_pattern_match` diagnostic so the
user can confirm or relabel.

`C0684.MP4` is the motivating case: `C` plus four digits is used by Sony, by Canon
cinema bodies, and by others. It is good enough to group a card and useless as a
claim about the manufacturer. Declaring the weakness is honest; suppressing the
match would lose the counter, and asserting a manufacturer would be a guess
presented as a fact.

Acceptance criterion: a weak match produces the camera identity, the counter, and
exactly one diagnostic per project naming the pattern.

### Decision 4 — Migration 11 backfills counters only, never `assets.camera`

Migration 11 fills `asset_capture_sequence.filename_counter` where it is NULL and
a pattern supplies one. It does not rewrite `assets.camera`, does not create or
merge capture sequences, and does not touch `assets.best_time` or
`assets.offset_seconds`.

Rewriting `assets.camera` retroactively would regroup capture sequences and
reorder an existing project, which is exactly what `CHRONOLOGY_MODEL.md`'s
migration rules forbid. An existing catalog whose cameras are anonymous is
repaired by the user through D01-T009's split and relabel, where a filename-prefix
change is already boundary evidence that separates `DJI_`, `VID_`, and `C`
sequences. Re-ingesting into a fresh project also gets correct identity from the
start.

Backfilling the counter is safe in a way that regrouping is not: nothing emits a
bounded `clockEpochSet` until D01-T010, and migration 9 carries legacy clock
events forward with `end_counter` NULL — open-ended. Filling a NULL counter
therefore cannot remove an asset from any epoch's coverage in any catalog that can
exist today. That window closes once D01-T010 ships, which is why this packet is
sequenced before it.

Acceptance criterion: migration 11 leaves `assets.camera`, `assets.best_time`,
`assets.offset_seconds`, `capture_sequences`, and `clock_epochs` byte-identical,
and changes only NULL counters.

## Frozen decisions

Inherited, not open for this packet to revisit:

- Original media is immutable; identification reads filenames and EXIF and writes
  nothing to a source tree.
- Existing catalogs migrate forward. Delete-and-rebuild is not a migration.
- No chronology change is applied without explicit user confirmation
  (`PRD.md` §21.4). Identification proposes a camera name; it never merges or
  splits a sequence on its own.
- `AGENTS.md` §15: additive edits only to `lib.rs`. New logic lives in the new
  `cameras` module.
- The pattern table is data, not scattered branches. A new camera is a table row.

## Owned paths

The implementer owns the new `cameras` module, migration 11, and its tests. The
independent tester owns a separate test file and fixture directory. The contract
editor owns the pattern documentation and the `CHRONOLOGY_MODEL.md` amendment,
because amending a frozen contract is never the implementer's call. The integrator
owns the schema document of record and the epic row.

Every surface is forbidden. Showing the user which camera was inferred and letting
them correct it is D01-T009 and D01-T015.

## Required interface

```rust
// core/rcdvlog-core/src/cameras/patterns.rs

pub enum PatternConfidence { Strong, Weak }

pub struct CameraPattern {
    /// Stable slug, e.g. `dji-pocket-3`. Never displayed.
    pub id: &'static str,
    /// Human-readable camera name written into `assets.camera`.
    pub label: &'static str,
    /// Anchored regular expression over the filename stem.
    pub stem_pattern: &'static str,
    /// 1-based capture group holding the counter.
    pub counter_group: usize,
    pub confidence: PatternConfidence,
}

/// Evaluated in order; the first match wins. Strong patterns precede weak ones.
pub const CAMERA_PATTERNS: &[CameraPattern] = &[
    CameraPattern {
        id: "dji-pocket-3",
        label: "DJI Pocket 3",
        stem_pattern: r"^DJI_\d{14}_(\d{4})_D$",
        counter_group: 1,
        confidence: PatternConfidence::Strong,
    },
    CameraPattern {
        id: "insta360-ultra-go",
        label: "Insta360 Ultra Go",
        stem_pattern: r"^VID_\d{8}_\d{6}_(\d+)$",
        counter_group: 1,
        confidence: PatternConfidence::Strong,
    },
    CameraPattern {
        id: "sony-c-series",
        label: "Sony (C-series)",
        stem_pattern: r"^C(\d{4})$",
        counter_group: 1,
        confidence: PatternConfidence::Weak,
    },
];
```

```rust
// core/rcdvlog-core/src/cameras/mod.rs

pub enum CameraIdentitySource {
    Exif,
    FilenamePattern { pattern_id: &'static str, confidence: PatternConfidence },
    Unknown,
}

pub struct CameraIdentity {
    /// Written into `assets.camera`, exactly as the EXIF path writes today.
    pub camera: String,
    pub source: CameraIdentitySource,
}

/// `exif` is the already-resolved EXIF make and model, `None` when absent.
pub fn identify_camera(exif: Option<&str>, filename: &str) -> CameraIdentity;

/// The counter a matching pattern supplies, or `None` when no pattern matches.
/// Callers fall back to the D01-T007 trailing-digit rule on `None`.
pub fn pattern_counter(filename: &str) -> Option<u64>;
```

### Diagnostic kinds wired by this packet

| Kind | Severity | Subject | Dedupe key |
| --- | --- | --- | --- |
| `camera.unidentified` | `warning` | `Project` | `camera.unidentified:<containing folder>` |
| `camera.weak_pattern_match` | `info` | `CaptureSequence` | `camera.weak_pattern_match:<pattern_id>` |

### `CHRONOLOGY_MODEL.md` amendment

The contract editor replaces the counter bullet in the Capture sequences section
with the pattern-first rule, keeping every existing example and its documented
value, and adds `DJI_20251221092058_0001_D.MP4` → 1 as a worked example. The
amendment states that this is a pattern-supplied counter and names this packet as
its authority.

## Acceptance oracle

1. **EXIF wins.** A clip with EXIF Make `Sony` / Model `ILCE-7C` and a filename
   matching the DJI pattern is identified from EXIF, and `source` is `Exif`.
2. **DJI is identified and counted.** `DJI_20251221092058_0001_D.MP4` with no
   EXIF yields camera `DJI Pocket 3`, source `FilenamePattern`, counter 1.
   A run of 420 such names yields 420 distinct, increasing counters.
3. **Insta360 is identified and counted.** `VID_20251216_134946_113.mp4` yields
   `Insta360 Ultra Go` and counter 113.
4. **Sony is identified weakly.** `C0684.MP4` yields `Sony (C-series)`, counter
   684, confidence `Weak`, and exactly one `camera.weak_pattern_match`
   diagnostic for the project.
5. **No pattern, no invention.** `clip.mp4` with no EXIF yields
   `"Unknown camera"`, source `Unknown`, a `camera.unidentified` diagnostic, and
   a NULL counter via the unchanged trailing-digit rule.
6. **Every frozen example is unchanged.** `C0101.MP4` → 101, `DJI_0042.MP4` → 42,
   `clip.mp4` → NULL, `GX010234.MP4` → 10234, `IMG_1234 (1).MOV` → 1.
7. **Three cameras, three sequences.** Ingest all three folders with EXIF
   stripped into one project and confirm three capture sequences, each with the
   right assets, and no counter collision between the Sony `C0684` run and any
   other.
8. **Migration 11 backfills only NULLs.** Against a schema-10 catalog holding DJI
   assets: every NULL counter with a matching pattern is filled, no non-NULL
   counter changes, and `assets.camera`, `assets.best_time`,
   `assets.offset_seconds`, `capture_sequences`, and `clock_epochs` are
   byte-identical before and after.
9. **Migration 11 is order-preserving.** Capture the `Catalog::list_assets` order
   before and after; it is identical, with no first differing index.
10. **Ordering is unaffected where nothing changed.** For a catalog of only Sony
    and Insta360 assets — both of which already parsed — migration 11 is a no-op
    and `chronology_facts()` returns an identical `ChronologyFacts`.
11. **Patterns are anchored.** `NOTDJI_20251221092058_0001_D.MP4` and
    `DJI_20251221092058_0001_D_extra.MP4` do not match the DJI pattern.
12. **No panic on hostile input.** A 4,000-character filename, a non-ASCII digit
    run, an empty stem, and a counter group exceeding `u64` each yield a value or
    `None`, never a panic.

## Independent test charter

Falsify the claim that identification is correct, conservative, and
order-preserving.

- Build your own schema-10 catalogs by direct insert. Do not reuse the
  implementer's fixtures.
- Attack Decision 1: construct a clip whose EXIF and filename disagree and
  confirm EXIF wins. Then strip only the Model, leaving Make, and decide from the
  contract what should happen — if the contract does not say, that is a finding.
- Attack Decision 2's blast radius: assemble every filename shape you can find in
  the repository's existing fixtures, compute the counter before and after this
  change, and report every filename whose counter moved. The claim under test is
  that only previously-NULL counters change.
- Attack the anchors: for each pattern, construct near-misses that differ by one
  character at the start, at the end, and in the digit-group width.
- Attack migration 11 on a catalog that already has a bounded epoch
  (`end_counter` non-NULL), constructed by direct insert even though no shipped
  code can create one yet. Confirm the implementation refuses or escalates rather
  than silently changing epoch coverage — this is the safety argument in
  Decision 4 and it must hold mechanically, not just by sequencing.
- Verify the 68% claim independently: take the three real filename shapes,
  compute counters under the pre-change rule, and confirm the DJI shape yields
  NULL. A packet premise that does not reproduce is the most valuable finding
  available here.
- Confirm no source media file was created, modified, or deleted, and that no
  test writes into a source tree.

## Review charter

Risk domain: a heuristic presented as a fact. The failure modes are a pattern that
overrides real EXIF, a pattern loose enough to claim a manufacturer it cannot
know, and a counter change that silently reorders an existing project.

Questions:

1. Can any pattern match a filename the product owner did not intend? Check every
   pattern for anchoring at both ends and for digit-group width.
2. Does any code path let a pattern override present, valid EXIF?
3. Does migration 11 change any counter that was not NULL, or any column outside
   `asset_capture_sequence.filename_counter`?
4. Is the Decision 4 safety argument enforced in code, or only asserted in prose?
   A bounded epoch inserted by hand must not be silently invalidated.
5. Is `Sony (C-series)` honest about what a `Cnnnn` filename can establish, or
   does the label overclaim a manufacturer?
6. Is the pattern table genuinely data-driven, so adding a camera is one row and
   no new branch?
7. Does the `CHRONOLOGY_MODEL.md` amendment preserve every previously documented
   example and its value?

Severity rules: a pattern overriding EXIF, a non-NULL counter changing, a
migration that reorders any project, or a silent invalidation of a bounded epoch
is blocking. Label wording and pattern-table ordering are non-blocking.

Smallest acceptable remedy: tighten the specific pattern or narrow the specific
migration predicate. Do not redesign the table.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- A UI for reviewing or correcting an inferred camera. D01-T009 and D01-T015.
- Rewriting `assets.camera` for already-ingested assets. Decision 4 excludes it.
- Detecting card boundaries or proposing sequences. D01-T009.
- Any pattern for a camera the product owner has not supplied a real filename
  for. Three patterns ship; the table exists so a fourth is a row, not a packet.
- Reading camera metadata timezone, which `CHRONOLOGY_MODEL.md` deliberately
  excludes.
- Full-file inspection, container probing, or any new dependency on ffprobe
  output for identity.

## Stop conditions

Stop and escalate when:

- a supplied filename shape cannot be expressed as an anchored pattern without
  matching filenames from a different camera;
- migration 11 encounters a `clock_epochs` row with a non-NULL `end_counter`,
  which means a bounded epoch exists and the Decision 4 safety argument no longer
  holds;
- the independent tester finds any previously non-NULL counter changing;
- amending `CHRONOLOGY_MODEL.md` turns out to require changing more than the
  counter rule, which means the contract editor must re-freeze the affected
  sections before implementation continues;
- more than the three supplied cameras are needed to make the acceptance oracle
  meaningful, which means the product owner must supply more real filenames.

## Commands

```sh
cargo test -p rcdvlog-core --test camera_identification
cargo test -p rcdvlog-core --test chronology_migration
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

The real-footage filename shapes are supplied by the product owner at invocation
and are not committed. To regenerate the fixture name lists from a mounted
corpus without recording its path in the repository:

```sh
ls "$FOOTAGE_ROOT"/DJIPocket3 | grep -v '^\._' | head -20
```

Expected result: all five commands exit 0, and the identification suite covers all
twelve acceptance oracle cases.

## Handoff

Report the pattern table as shipped, the acceptance oracle results case by case,
and the full list of filenames whose counter changed under migration 11 — which
must contain only previously-NULL entries. State explicitly whether any pattern
required loosening during implementation and why, since a loosened anchor is the
most likely way this packet does harm. Name the two diagnostic kinds for D01-T015
and D01-T017. Confirm the `CHRONOLOGY_MODEL.md` amendment preserves every prior
worked example.
