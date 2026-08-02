# D01-T022: Deterministic OTIO export

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T022",
  "epic_id": "D01",
  "title": "Deterministic OTIO export",
  "slug": "deterministic-otio-export",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": ["D01-T020"],
  "principal_invariant": "Identical edit-plan input always produces byte-identical OpenTimelineIO output with stable clip and metadata identifiers.",
  "central_falsification": "Export the same plan repeatedly across separate processes, on different machines, and with the plan's rows inserted in different orders, then compare bytes against a committed golden file.",
  "architecture_decisions_frozen": false,
  "load_bearing_seams": ["edit plan revision -> OTIO document"],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/EDIT_PLAN_MODEL.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/desktop/EDIT_PLAN_MODEL.md", "PRD.md", "AGENTS.md"],
    "independent_tester": ["docs/tasks/desktop/EDIT_PLAN_MODEL.md", "PRD.md"],
    "reviewer": ["docs/tasks/desktop/EDIT_PLAN_MODEL.md", "PRD.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "integrator": ["docs/tasks/desktop/EDIT_PLAN_MODEL.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T022-deterministic-otio-export.md"],
    "implementer": [
      "core/rcdvlog-core/src/editplan/otio.rs",
      "core/rcdvlog-core/tests/otio_golden.rs",
      "core/rcdvlog-core/tests/fixtures/otio/**"
    ],
    "independent_tester": [
      "core/rcdvlog-core/tests/otio_determinism_independent.rs",
      "core/rcdvlog-core/tests/fixtures/otio-independent/**"
    ],
    "integrator": ["docs/tasks/desktop/EPIC.md", "docs/task-cycles/D01-T022/**"]
  },
  "forbidden_paths": ["apps/**", "packages/**", "core/rcdvlog-server/**", "core/rcdvlog-core/src/migrations.rs", "core/rcdvlog-core/src/chronology/**", ".github/**"],
  "verification_commands": [
    "cargo test -p rcdvlog-core --test otio_golden",
    "cargo test --workspace",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T022\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Write the OpenTimelineIO document a finishing editor imports, in pure Rust, with
byte-identical output for identical input.

## Principal invariant

This task is correct when identical edit-plan input always produces byte-identical
OpenTimelineIO output with stable clip and metadata identifiers.

## Inputs and outputs

Inputs:

- A validated `EditPlanRevision`.
- Per-asset source references: path, frame rate, and duration.

Outputs:

- An `.otio` JSON file: one timeline, one video track, one clip per plan operation, with
  source references and source in/out ranges.
- Golden fixture files committed to the repository.
- An export record through `record_edit_plan_export`, including the output digest.

## Frozen decisions

- **Pure Rust.** No Python, no `otiotool`, no shelling out to the reference
  implementation. `PRD.md` §21.3 forbids introducing a permanent Python dependency, and
  an export path is not sufficient reason to breach it. The accepted cost is that this
  repository owns schema fidelity, which is why D01-T024's Resolve import gate is
  required rather than optional.
- **Determinism is a hard requirement.** Key ordering in JSON objects is fixed and
  explicit. No `HashMap` reaches serialization. No timestamp, hostname, username,
  absolute temporary path, process id, or random value appears in the output.
- **Identifiers are derived from stable input.** A clip's identifier is a function of
  the plan revision id and the operation's position, so re-export produces the same
  identifiers and a finishing editor sees the same clips rather than new ones.
- **Rational time, not floats.** OTIO expresses time as a value and a rate. Convert
  source seconds to frames using the asset's frame rate with a documented, tested
  rounding rule. A float that round-trips differently across platforms is a
  determinism hazard.
- **Source references are the originals, not the proxies.** A finishing editor conforms
  against camera masters. The preview uses proxies; the export must not.
- **Existing exports are never silently replaced.** `PRD.md` §23 Milestone 6 requires
  preventing silent replacement of newer outputs; an export to an existing path is
  refused unless the caller supplies the digest it expects to overwrite.
- **One video track, hard cuts only.** No transition, no effect, no gap, no filler. The
  model cannot express them and the writer must not invent them.

## Owned paths

The implementer owns the OTIO writer, its golden test, and the golden fixtures. The
independent tester owns a separate fixture directory, because a golden file the
implementer wrote and the implementer's test compares against proves only
self-consistency.

## Required interface

```rust
pub struct OtioExportRequest {
    pub revision_id: String,
    pub output_path: String,
    /// The digest the caller expects to overwrite. `None` means the file must not exist.
    pub expected_existing_digest: Option<String>,
    pub timeline_name: String,
}

pub struct OtioExportResult {
    pub output_path: String,
    pub output_digest: String,
    pub clip_count: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum OtioExportError {
    #[error("output {path} already exists with digest {actual}")]
    OutputExists { path: String, actual: String },
    #[error("asset {asset_id} has no usable frame rate")]
    UnusableFrameRate { asset_id: String },
}

impl OtioExportError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::OutputExists { .. } => "otio_output_exists",
            Self::UnusableFrameRate { .. } => "otio_unusable_frame_rate",
        }
    }
}

/// Serializes to a byte-stable OTIO document. Pure: no clock, no environment.
pub fn otio_document(plan: &EditPlanRevision, assets: &[Asset], timeline_name: &str)
    -> Result<String>;

impl ProjectCatalog {
    pub fn export_otio(&self, request: &OtioExportRequest) -> Result<OtioExportResult>;
}
```

`otio_document` is separated from `export_otio` so determinism is testable without
touching the filesystem.

## Acceptance oracle

1. Exporting the same plan twice in one process produces identical bytes.
2. Exporting in two separate processes produces identical bytes.
3. Output matches a committed golden file byte for byte.
4. Inserting the plan's operation rows in a different order, then exporting, produces
   identical bytes — the writer must sort by position, not rely on row order.
5. Clip identifiers are identical across re-exports of the same revision, and different
   across different revisions.
6. Output contains exactly one timeline and one video track.
7. Clip count equals plan operation count, and clip order equals plan position order.
8. Each clip's source range converts the plan's seconds to frames at the asset's frame
   rate, with the rounding rule applied and tested at boundaries.
9. Source references point at original media paths, not proxy paths.
10. Output contains no timestamp, hostname, username, temporary path, process id, or
    random value: verified by a grep over the produced document for the current year,
    the hostname, and the username.
11. Exporting to an existing path without `expected_existing_digest` is refused with
    `otio_output_exists`.
12. Exporting to an existing path with the correct digest succeeds and replaces it.
13. Exporting to an existing path with a wrong digest is refused.
14. An asset with a zero or nonsensical frame rate is refused with
    `otio_unusable_frame_rate` before any file is written.
15. Non-ASCII filenames and paths with spaces are encoded correctly and survive a JSON
    round-trip.
16. An export record is written with the output digest, and history is append-only.

## Independent test charter

Falsify determinism and schema fidelity.

- Write your own golden files in `core/rcdvlog-core/tests/fixtures/otio-independent/`,
  derived from the OTIO schema rather than from the implementer's writer output. A
  golden file generated by the code under test asserts nothing.
- Export the same plan in separate processes, and if a second machine is available, on
  both, and compare bytes.
- Shuffle the plan's operation insertion order across 32 permutations and compare
  bytes each time.
- Parse the output with an independent JSON parser and assert structure: exactly one
  track, `OTIO_SCHEMA` fields present with expected versions, every clip carrying a
  media reference with a target URL, and every clip's `source_range` present with a
  value and a rate.
- Attack rounding: source ranges at 23.976, 25, 29.97, 30, 59.94, and 120 fps; a range
  whose seconds value falls exactly halfway between two frames; a range shorter than one
  frame.
- Attack the overwrite guard by exporting, modifying the file externally, then
  re-exporting with the original digest — this must be refused, since the digest no
  longer matches what is on disk.
- Grep the output for the current year, the hostname, the username, and `/tmp`.
- Confirm source references are originals by asserting no proxy cache path appears.

## Review charter

Risk domain: nondeterminism that makes a finishing editor see new clips on every
re-export, and schema drift that only surfaces at import time in Resolve.

Questions:

1. Is any `HashMap` or unspecified iteration order on the serialization path?
2. Is any environment-derived value — clock, hostname, username, temp path, pid,
   random — in the output?
3. Are clip identifiers derived purely from plan input?
4. Is the seconds-to-frames rounding rule documented and tested at boundaries?
5. Do source references point at originals rather than proxies?
6. Can an existing export be overwritten without the caller naming the digest?
7. Is any transition, gap, filler, or second track emitted?
8. Are the golden files hand-derived from the schema, or generated by the writer?

Severity rules: nondeterminism, an environment-derived value in the output, unstable
identifiers, proxy source references, a silent overwrite, or an emitted transition is
blocking. Key ordering aesthetics and file indentation are non-blocking provided they
are deterministic.

Smallest acceptable remedy: replace the unordered collection or remove the
environment-derived value, and add the cross-process comparison that exposed it.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Verifying the import in Resolve, which is D01-T024's manual gate. This packet cannot
  prove schema fidelity by itself, and must not claim to.
- Other interchange formats: AAF, EDL, FCPXML.
- Round-tripping OTIO back into the project.
- Preview rendering, which is D01-T021.
- Any UI or CLI surface.
- Transitions, effects, colour, or audio mixing.

## Stop conditions

Stop and escalate when:

- the OTIO schema requires a field that cannot be produced deterministically;
- byte-stability cannot be achieved because of floating-point serialization, which
  means the rational-time conversion needs a design decision;
- the schema appears to require a Python-generated construct, which would conflict with
  the pure-Rust decision and needs the product owner's call;
- an asset's frame rate is unavailable for real media, making frame conversion
  impossible;
- the overwrite guard cannot be implemented without a race between the digest check and
  the write.

## Commands

```sh
cargo test -p rcdvlog-core --test otio_golden
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Then, to prove cross-process determinism and cleanliness:

```sh
cargo run -q --example export_otio_fixture > /tmp/otio-a.otio
cargo run -q --example export_otio_fixture > /tmp/otio-b.otio
cmp /tmp/otio-a.otio /tmp/otio-b.otio && echo "byte identical across processes"
grep -nE "$(date +%Y)|$(hostname)|$(whoami)|/tmp/" /tmp/otio-a.otio && echo "ENVIRONMENT LEAK" || echo "no environment leak"
```

Expected result: `cmp` reports identical files and the grep finds no environment leak.

## Handoff

Report the document structure produced, the determinism evidence including cross-process
and shuffled-insertion results, the golden file comparison, the rounding rule with its
boundary test results, the overwrite-guard behavior for all three cases, and the
environment-leak grep output. State explicitly that Resolve import fidelity is not
proven by this packet and remains D01-T024's gate. List any stop condition encountered.
The implementer does not approve this task.

## Promotion note

`draft` until D01-T019 publishes `docs/tasks/desktop/EDIT_PLAN_MODEL.md` and D01-T020
lands validation. The contract editor confirms `record_edit_plan_export` and the
`Asset` frame-rate field are available before promoting.
