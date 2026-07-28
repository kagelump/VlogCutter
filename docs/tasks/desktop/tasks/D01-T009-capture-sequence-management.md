# D01-T009: Capture-sequence management

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T009",
  "epic_id": "D01",
  "title": "Capture-sequence management",
  "slug": "capture-sequence-management",
  "status": "draft",
  "task_class": "implementation",
  "depends_on": ["D01-T008"],
  "principal_invariant": "Filename identity is scoped to its capture sequence, so two cards that reuse the same filename counter are never treated as one monotonic chain.",
  "central_falsification": "Ingest two folders from one camera whose filenames both run C0001 to C0100, and confirm the proposal yields two sequences and that a clock epoch on one leaves the other's derived times untouched.",
  "architecture_decisions_frozen": false,
  "load_bearing_seams": ["ingest metadata -> capture sequence assignment"],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md", "AGENTS.md"],
    "independent_tester": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md"],
    "reviewer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md", "AGENTS.md"],
    "integrator": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T009-capture-sequence-management.md"],
    "implementer": [
      "core/rcdvlog-core/src/chronology/sequences.rs",
      "core/rcdvlog-core/tests/chronology_sequences.rs"
    ],
    "independent_tester": ["core/rcdvlog-core/tests/chronology_sequences_independent.rs"],
    "integrator": ["docs/tasks/desktop/EPIC.md", "docs/task-cycles/D01-T009/**"]
  },
  "forbidden_paths": ["apps/**", "packages/**", "core/rcdvlog-server/**", "core/rcdvlog-core/src/chronology/solver.rs", ".github/**"],
  "verification_commands": [
    "cargo test -p rcdvlog-core --test chronology_sequences",
    "cargo test --workspace",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T009\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Propose capture sequences from observable evidence and let the user split, merge,
and reassign them, so that a camera's filename chain is modeled per card rather
than per camera.

## Principal invariant

This task is correct when filename identity is scoped to its capture sequence, so
two cards that reuse the same filename counter are never treated as one monotonic
chain.

## Inputs and outputs

Inputs:

- Ingest-time asset metadata: camera identity, containing folder, volume, filename
  prefix, filename counter, codec, resolution.
- The frozen `CaptureSequence` type and the `captureSequence*` operations.

Outputs:

- A proposal function producing candidate sequences with the boundary evidence for
  each.
- `ProjectCatalog` methods emitting split, merge, and reassign operations.

## Frozen decisions

- Boundary evidence is exactly the `PRD.md` §8.2 list: different SD-card volume,
  filename counter reset, folder reset, filename prefix change, large
  discontinuity, explicit user split. No other heuristic is added.
- Proposals are never auto-applied. `PRD.md` §21.4 forbids automatic chronology
  changes without user confirmation, and sequence boundaries change derived order.
- A "large discontinuity" threshold is a named constant with a documented
  rationale, not a magic number scattered across branches.
- Splitting is non-destructive: it creates a new sequence and reassigns a counter
  range. Merging preserves both sequences' assets. Neither ever drops an asset.
- Every asset always belongs to exactly one sequence. There is no unassigned
  state.

## Owned paths

The implementer owns the sequences module and its tests. `solver.rs` is
forbidden — this packet supplies facts, the solver consumes them, and the boundary
keeps both reviewable.

## Required interface

```rust
pub enum BoundaryEvidence {
    VolumeChange { from: String, to: String },
    CounterReset { previous: u64, next: u64 },
    FolderReset { from: String, to: String },
    PrefixChange { from: String, to: String },
    LargeDiscontinuity { gap: u64 },
    UserSplit,
}

pub struct SequenceProposal {
    pub proposed_id: String,
    pub camera_key: String,
    pub label: String,
    pub volume_hint: String,
    pub filename_prefix: String,
    pub asset_ids: Vec<String>,
    pub boundary_evidence: Vec<BoundaryEvidence>,
}

pub fn propose_sequences(assets: &[AssetTimeHypothesis], hints: &[IngestHint])
    -> Vec<SequenceProposal>;

impl ProjectCatalog {
    pub fn apply_sequence_proposals(&self, project_id: &str, proposals: &[SequenceProposal]) -> Result<()>;
    pub fn split_sequence(&self, sequence_id: &str, at_counter: u64, label: &str) -> Result<CaptureSequence>;
    pub fn merge_sequences(&self, into_id: &str, from_id: &str) -> Result<CaptureSequence>;
    pub fn reassign_asset_sequence(&self, asset_id: &str, sequence_id: &str) -> Result<()>;
}
```

## Acceptance oracle

1. Two folders from one camera, both containing `C0001.MP4` through `C0100.MP4`,
   produce two proposals with `VolumeChange` or `FolderReset` evidence.
2. A single folder running `C0101` to `C0244` produces one proposal.
3. A counter reset within one folder — `C0240`, `C0241`, `C0001`, `C0002` —
   produces two proposals with `CounterReset` evidence.
4. A prefix change — `C0101.MP4` then `DJI_0001.MP4` — produces two proposals.
5. `split_sequence` at counter 150 leaves assets below 150 in the original
   sequence and moves 150 and above to the new one, and no asset is lost.
6. `merge_sequences` unions the asset sets, and the operation is idempotent on
   replay.
7. After any split, merge, or reassign, every project asset still has exactly one
   sequence.
8. A clock epoch on one sequence leaves the other sequence's derived times
   unchanged — the whole point of the model.
9. Proposals are deterministic: the same assets in a shuffled order produce the
   same proposals in the same order.
10. Non-ASCII and space-containing filenames are handled without panic and their
    counters parse correctly.

## Independent test charter

Falsify counter-collision isolation.

- Build your own corpus: one camera, three cards, each with `C0001`–`C0050`,
  interleaved on disk so folder order does not match card order.
- Assert three sequences, and assert that an epoch on card two shifts exactly 50
  assets.
- Attack the boundary rules with adversarial input: a folder whose counters run
  backwards; a folder with a single asset; a camera whose filenames have no digit
  run at all; two cards with identical folder names on different volumes; counters
  with leading zeros of differing widths (`C001` and `C0001`).
- Attack split and merge: split at the first counter, at the last counter, and at
  a counter no asset has. Merge a sequence into itself. Merge an empty sequence.
  None may lose an asset or panic.
- Confirm no proposal is applied without an explicit call to
  `apply_sequence_proposals`.
- Shuffle input order across 32 seeds and assert identical proposals.

## Review charter

Risk domain: a boundary heuristic that either splits a real chain or merges two
cards, both of which corrupt derived order.

Questions:

1. Can any asset end up in zero or two sequences?
2. Is the discontinuity threshold a named, documented constant?
3. Is any heuristic present that `PRD.md` §8.2 does not list?
4. Can a proposal be applied without explicit user action?
5. Is proposal output deterministic under input reordering?
6. Do split and merge preserve every asset?

Severity rules: asset loss, an asset in two sequences, an auto-applied proposal,
or nondeterministic proposals is blocking. Threshold value tuning and label
wording are non-blocking.

Smallest acceptable remedy: add the invariant check and the fixture case that
exposed the gap.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Camera identification improvements beyond the existing `camera` field.
- Any UI for reviewing proposals, which is D01-T015 and D01-T016.
- Ingest preview, which is separate `PRD.md` §21.1 step 3 work.
- Changing the solver.
- Automatic acceptance of proposals.

## Stop conditions

Stop and escalate when:

- two boundary rules disagree on the same asset pair and the model does not say
  which wins;
- proposal requires reading media files rather than existing metadata;
- an ingest hint the proposal needs is not captured at ingest time, which would
  require changing ingest — a different seam;
- split or merge cannot preserve operation replay semantics;
- the volume a card came from is not recoverable from stored metadata.

## Commands

```sh
cargo test -p rcdvlog-core --test chronology_sequences
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected result: all tests pass and clippy is clean.

## Handoff

Report the boundary rules as implemented with the discontinuity constant and its
rationale, the proposal determinism evidence, the split/merge/reassign results
including the asset-preservation checks, and the counter-collision isolation
result. List any stop condition encountered. The implementer does not approve this
task.

## Promotion note

`draft` until D01-T007 publishes `docs/tasks/desktop/CHRONOLOGY_MODEL.md`. The
contract editor then reconciles the interface above with the frozen
`CaptureSequence` type and `captureSequence*` operation payloads before promoting.
