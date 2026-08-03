# D01-T021: Low-resolution preview render

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T021",
  "epic_id": "D01",
  "title": "Low-resolution preview render",
  "slug": "low-resolution-preview-render",
  "status": "ready",
  "task_class": "implementation",
  "depends_on": [],
  "principal_invariant": "A rendered preview contains the plan's clips, in the plan's order, with each clip's source range, and a total duration matching the plan within one frame.",
  "central_falsification": "Render a plan built from clips with distinguishable content, then probe the output and sample frames to confirm order, per-clip durations, and total duration against the plan computed independently.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": ["validated edit plan -> FFmpeg preview job"],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/EDIT_PLAN_MODEL.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/desktop/EDIT_PLAN_MODEL.md", "PRD.md", "AGENTS.md", "docs/batch-estimation.md"],
    "independent_tester": ["docs/tasks/desktop/EDIT_PLAN_MODEL.md", "PRD.md", "AGENTS.md"],
    "reviewer": ["docs/tasks/desktop/EDIT_PLAN_MODEL.md", "PRD.md", "AGENTS.md"],
    "integrator": ["docs/tasks/desktop/EDIT_PLAN_MODEL.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T021-low-resolution-preview-render.md"],
    "implementer": [
      "core/rcdvlog-core/src/editplan/preview.rs",
      "core/rcdvlog-core/tests/editplan_preview.rs"
    ],
    "independent_tester": ["core/rcdvlog-core/tests/editplan_preview_independent.rs"],
    "integrator": ["docs/tasks/desktop/EPIC.md", "docs/task-cycles/D01-T021/**"]
  },
  "forbidden_paths": ["apps/**", "packages/**", "core/rcdvlog-server/**", "core/rcdvlog-core/src/migrations.rs", "core/rcdvlog-core/src/chronology/**", ".github/**"],
  "verification_commands": [
    "cargo test -p rcdvlog-core --test editplan_preview",
    "cargo test --workspace",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T021\\].*\\| complete \\|"
    }
  ]
}
-->

## Current implementation checkpoint

The core renders per-clip H.264/AAC segments, concatenates successful segments,
publishes the final file atomically, and emits progress. Completion is not yet
established: there is no real-media preview test, output probe, cancellation
command, or interruption test; the Tauri command runs synchronously instead of
using `spawn_blocking`; and ETA is ad hoc rather than `batch-estimator`. Finish
those runtime and media-boundary requirements without changing the frozen preview
recipe silently.

## Goal

Render a watchable low-resolution preview of an edit plan through FFmpeg, so the user
can check their cut before handing it to a finishing editor.

## Principal invariant

This task is correct when a rendered preview contains the plan's clips, in the plan's
order, with each clip's source range, and a total duration matching the plan within
one frame.

## Inputs and outputs

Inputs:

- A validated `EditPlanRevision` whose `ValidationReport::renderable()` is true.
- Review proxies for its assets.
- The existing batch job and progress conventions, including `BatchEstimator` and
  per-project rate history described in `docs/batch-estimation.md`.

Outputs:

- A single preview file per plan revision, under the project's cache directory.
- Progress events, a worker count, and an estimated time remaining, following the
  existing batch conventions.
- Cancellation, and resumption of an interrupted render where practical.

## Frozen decisions

- Render from review proxies, not originals. This is a preview: `PRD.md` §23
  Milestone 6 asks for low resolution, and rendering from originals would be slow and
  would touch source media unnecessarily.
- Validation runs first. A plan that is not `renderable()` is refused before FFmpeg is
  invoked, and refusal is not a render failure.
- Output goes under the project cache directory, keyed by plan revision id, so a
  revision's preview is never confused with another's.
- Child-process arguments are structured through `Command` and never assembled as a
  shell string, per `AGENTS.md` §15. Media paths contain spaces and non-ASCII
  characters.
- Per-clip failures do not abort the batch silently: a failure is surfaced with the
  clip that caused it, per `AGENTS.md` §15.
- Cancellation leaves no partial file presented as complete. A cancelled render either
  leaves nothing or leaves a clearly incomplete artifact that is not recorded as a
  preview.
- The preview is not an export and is never handed to a finishing editor. It has no
  determinism requirement; D01-T022 owns determinism.
- Hard cuts only. No transition, no crossfade, no audio ramp — including none
  introduced incidentally by an FFmpeg filter default.

## Owned paths

The implementer owns the preview module and its tests. The plan model, validation, and
chronology are read-only here.

## Required interface

```rust
pub struct PreviewRequest {
    pub revision_id: String,
    pub max_height: u32,
}

pub struct PreviewResult {
    pub output_path: String,
    pub duration_seconds: f64,
    pub clip_count: u32,
    pub failures: Vec<PreviewFailure>,
}

pub struct PreviewFailure {
    pub position: u32,
    pub asset_id: String,
    pub message: String,
}

pub enum PreviewProgress {
    Started { clip_count: u32, worker_count: u32 },
    ClipCompleted { position: u32, elapsed_seconds: f64, estimate_remaining_seconds: Option<f64> },
    ClipFailed { position: u32, message: String },
    Finished { output_path: String, duration_seconds: f64 },
    Cancelled,
}

impl ProjectCatalog {
    pub fn render_preview(
        &self,
        request: &PreviewRequest,
        progress: &mut dyn FnMut(PreviewProgress),
        cancel: &dyn Fn() -> bool,
    ) -> Result<PreviewResult>;
}
```

`cancel` is polled between clips, which is the granularity at which a concat render
can stop cleanly.

## Acceptance oracle

1. A three-clip plan renders one output whose probed duration equals the sum of the
   three source ranges within one frame at the output frame rate.
2. Clip order in the output matches plan order, verified by sampling a frame from each
   clip's expected position and matching it to distinguishable source content.
3. Each clip's segment duration matches its `source_out_seconds - source_in_seconds`
   within one frame.
4. Audio is present and its duration matches the video's within one frame — linked
   source audio is part of the accepted scope.
5. A plan that is not `renderable()` is refused before any FFmpeg process starts,
   with the validation report returned.
6. Cancelling mid-render stops promptly and leaves no file recorded as a completed
   preview.
7. Re-rendering the same revision after a completed render replaces the output rather
   than accumulating files.
8. Interrupting the process and re-running preserves completed segment work where the
   implementation supports resumption, and otherwise restarts cleanly without leaving
   a corrupt artifact.
9. A clip whose proxy is missing produces a `PreviewFailure` naming that clip while
   the remaining clips still render.
10. Progress events include an initial `Started` with a real worker count, one event
    per clip, and a final `Finished` or `Cancelled`.
11. Paths with spaces and non-ASCII characters render correctly.
12. No original media file and no proxy is modified: verified by comparing file lists,
    sizes, and modification times before and after.
13. The test suite skips its media portion cleanly when FFmpeg is absent, matching the
    existing integration-suite convention, and says so rather than reporting success.

## Independent test charter

Falsify order and duration fidelity, which is what the user will actually check.

- Build your own source clips with distinguishable content — a different solid colour
  and a different audio tone per clip — so order and boundaries are verifiable by
  sampling rather than by trusting metadata.
- Probe the output with `ffprobe` and compare per-segment and total durations against a
  plan sum you compute yourself.
- Sample a frame at the midpoint of each expected segment and assert the colour matches
  the clip the plan put there. This catches an off-by-one in concat order that duration
  checks miss entirely.
- Assert audio presence and tone order by extracting the audio and checking the
  dominant frequency per segment.
- Attack the boundaries: a plan with one clip; a plan with a 0.04-second range; a plan
  with 200 clips; a clip whose range starts at 0; a clip whose range ends exactly at
  the proxy duration; a clip repeated three times with different ranges.
- Attack cancellation by cancelling at the first, middle, and last clip, and confirming
  no completed-preview record exists afterwards.
- Confirm no transition was introduced by checking that the frame immediately before a
  boundary is fully the previous clip's colour and the frame after is fully the next
  clip's.
- Confirm proxies and originals were untouched.

## Review charter

Risk domain: a preview that misrepresents the cut, and a render that damages media or
leaves corrupt artifacts.

Questions:

1. Can the output order differ from plan order for any input?
2. Is any transition, fade, or filter introduced, including by an FFmpeg default?
3. Are child-process arguments structured rather than shell-assembled?
4. Does a cancelled or failed render ever leave an artifact recorded as complete?
5. Does a single clip failure abort the whole batch?
6. Does anything write to originals or proxies?
7. Are progress events emitted with a real worker count, or a hardcoded one?
8. Does the suite report success when it actually skipped the media portion?

Severity rules: wrong clip order, an introduced transition, a shell-assembled command,
a corrupt artifact recorded as complete, or a write to media is blocking. Encoder
settings, resolution choice, and progress granularity are non-blocking.

Smallest acceptable remedy: correct the concat ordering or argument construction and add
the frame-sampling test that exposed it.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- OTIO export, which is D01-T022.
- Playing the preview in the UI, which is D01-T023.
- Transitions, effects, colour, audio mixing — all deferred by `PRD.md` §21.4.
- Rendering at full resolution or delivery quality.
- Frame-accurate conforming against originals.
- Changing the proxy representation.

## Stop conditions

Stop and escalate when:

- frame-accurate boundaries cannot be achieved from proxies because of their GOP
  structure, which is a representation decision rather than a render bug;
- resumption cannot be implemented without a new job-state table, which would be a
  migration and therefore an architecture change;
- FFmpeg introduces a transition or resamples audio in a way that cannot be disabled;
- the render needs originals rather than proxies to be correct;
- a needed FFmpeg capability is absent from the version `AGENTS.md` §6 requires.

## Commands

```sh
cargo test -p rcdvlog-core --test editplan_preview
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
ffprobe -v error -show_entries format=duration -of default=nk=1:nw=1 <preview output>
```

Expected result: tests pass, clippy is clean, and the probed duration matches the plan
sum within one frame.

## Handoff

Report the render pipeline and FFmpeg invocation shape, the duration and order
verification results including the frame-sampling evidence, the audio verification, the
cancellation and interruption behavior, the per-clip failure handling, the progress
event sequence, and confirmation that media was untouched. State whether resumption is
implemented or the render restarts cleanly. List any stop condition encountered. The
implementer does not approve this task.

## Readiness note

Ready. Validation and the initial render pipeline exist; the checkpoint isolates
runtime safety, shared ETA, cancellation, and real-media acceptance as the remainder.
