# Source-audio MVP implementation plan

Status: ready to implement, 2026-11-04.
Contract: [MVP design](SOURCE_AUDIO_MVP_DESIGN.md).

Deliver a usable opt-in CLI recognizer and conservative segment report before
resuming algorithm research. Each step ends in a reviewable implementation
commit. The existing eval 2 results are the starting baseline, not a reason to
restart an open-ended benchmark campaign.

## 1. Import and freeze the demonstrated implementation

Start from current main in an isolated worktree. Preserve other agents' staged,
unstaged and untracked work. Set `SOURCE_AUDIO_EVAL_ROOT` locally to the eval 2
directory; neither a private media path nor any recording belongs in Git.

Recover the source from the preserved baseline patch and untracked-source
snapshot, then apply the M1 pooling/anchor-identity delta. Reconcile only required
domain and CLI changes with main. Do not copy the entire old CLI file over main
or import unrelated agent instructions. Do not import the old segmentation
solver as the new algorithm merely because it is present in the snapshot.

Artifact paths below are relative to `SOURCE_AUDIO_EVAL_ROOT`:

| Artifact | Purpose / frozen identity |
| --- | --- |
| `preservation/receipt.json`, `preservation/vlogcutter-worktree.patch`, `preservation/source/` | Base working-source provenance; base Git HEAD alone is insufficient |
| `preservation/m1-build-and-evidence-receipt-v1.json` | Source, binary, index, scripts and evidence identities |
| `preservation/m1-window-pooling-and-anchor-identity.patch` | SHA-256 `afc1774af82c0e2197a6e138fe6e127a100ca1ca424eb1236ebe5b91d35593d0` |
| `policies/pooled-recognition-policy-v3-anchor-aware.json` | SHA-256 `75b006faea7287bd31ce000b9fd2439290f41fdcf3a6ee8e2b22f6eea19efd4e` |
| `manifests/m1-small-set-v1.json` | SHA-256 `80ae9adc10138f33f72b168c57f79ba5d1f9a1dc803a581700ad36daeabb6b44` |
| `reproduction/m1-small-set-anchor-aware-results-v2.json` | SHA-256 `41dcc1ee05923f1897b8a1f902606ca5cdaad9edddd0349c59c031f5d23eabb7` |
| `manifests/m1-targeted-long-window-probe-v2.json` | Six difficult windows for bounded regression, not a new sweep |

Vendor the small policy, sanitized expected metrics and reusable evaluation
runner into an appropriate repository test/eval directory. Keep private media,
indexes, binaries and large correspondence dumps local. Give the runner an
explicit media-root argument and document the private replay prerequisite.
Generate deterministic public synthetic audio for CI rather than copying RC6.

Exit: provenance verified, minimal import builds, frozen recognition outcomes
reproduced before changing architecture. Load the configured Rust environment
before treating a missing PATH entry as an unavailable compiler.

## 2. Make recognition a core-owned versioned API

Move pooling, identity deduplication, candidate ranking and v3 acceptance into
`core/rcdvlog-core/src/` with typed request/result structs. The experimental
pooler currently lives in `apps/cli/src/tv_review_commands.rs`; avoid preserving
that architecture. Reuse the landmark index implementation and strict index
validation. Keep old correlation recipes and output formats unchanged.

Add focused tests for complete evidence despite top-N diagnostic truncation,
duplicate anchors, shared competitor anchors, exact versus adjacent bins,
ties/missing margins, incomplete searches and invalid limits. Test scorer parity
including zero-denominator and rounding behavior. Record a new recipe/policy
identity if any semantics must change; do not silently call it the frozen v3.

Exit: the core API reproduces all 17/17 positive rankings at both existing
densities, density100 10-second acceptance 5/6, and 0/13 negative accepts on the
small set. The moderate composite remains a documented rejection. Scores and
decisions do not depend on the diagnostic mode limit.

## 3. Run bounded sessions and assemble conservative segments

Implement the design's 10-second/2-second schedule with native sample origins,
bounded query batches, complete-reference lookup, cancellation and progress.
Key resumable derived results by all input, recipe and scheduling identities;
never reuse results after a recording/index/policy change. Count overlapping
window work separately from unique recording coverage.

Implement the small core segment assembler from the design: compatible offsets,
one segment when supported, explicit unknown holes, known-boundary splits,
conflict abstention, no drift fitting and no global-offset extrapolation.
Retain actual supporting positions for the existing synchronization mapper.

Required deterministic cases:

- continuous playback with multiple accepted windows coalesces to one segment;
- two different seeks yield separate offsets and an unknown transition;
- weak/negative windows between agreeing runs remain explicit unknown ranges;
- overlapping windows do not multiply support, and incompatible windows conflict;
- a two-second gap is preserved inside an envelope, a longer gap splits it;
- pause, rewind and capture-epoch boundaries prevent interpolation;
- chunk edges, a final scheduled window and nonzero native sample origins retain
  correct offset sign and raw movie PTS conversion;
- a recording shorter than ten seconds produces a valid all-unknown result;
- cancellation/budget exhaustion never turns an incomplete window into a match.

Use both constructed evidence tests for assembler edge cases and real generated
audio through the extractor for continuous playback and seek integration. The
second group proves the connection to recognition, not just a fabricated input.

Exit: supported segments and unknown ranges form a non-overlapping, complete
accounting of the requested recording domain, even when segment envelopes
contain holes. No invented movie position is assigned to a hole or conflict.

## 4. Expose the usable CLI workflow

Keep the experimental index/match command names where compatible; add an
explicit `source-audio-pooled-mvp-v1` selection and a full-recording report path.
All domain behavior stays in core. Document the exact request schema and commands
to index a final movie, match a recording, inspect segments/rejections and map a
recording-time query through `tv_review_sync`. Include a complete synthetic
example with expected offset sign, not placeholder-only JSON.

Add process-level tests using the built CLI, actual WAV decoding and an immutable
index. Cover digest mismatch, stale cache, missing/corrupt media, JSON error
envelopes, path handling, cancellation and an unknown utterance. Confirm existing
manual/cue and soundtrack CLI compatibility with their focused suites. Reports
are read-only suggestions; recording editorial notes remains a separate action.

Exit: a user can run the documented workflow and consume segments/offsets without
Python post-processing, private scorer imports or a custom debug binary.

## 5. Run one release comparison and ship the bounded MVP

Freeze the candidate recipe before this run. Preserve predictions before scoring
truth. Run the following bounded checks; do not expand to every historical
parameter sweep:

| Check | MVP delivery criterion |
| --- | --- |
| Frozen small-set recognition | Preserve step 2's rankings and acceptance decisions |
| Public continuous/seek audio fixtures | Correct supported offsets within 100 ms; continuous fixture coalesces; seeks do not bridge |
| Six difficult historical windows | Report recovery/unknown/wrong separately; zero accepted wrong spans; positive recovery is not required |
| Existing 13 small-set negatives and scheduling on those captures | Zero accepted windows/spans; reused development evidence only |
| New fixed-seed smoke set, frozen before running | At least six moderate continuous/seek captures and ten independent no-playback captures; zero accepted wrong spans; report recall without tuning to this set |
| Full-recording replay | One available session completes within declared limits; record runtime, peak RSS, total supported/unknown duration and interruption/resume behavior |
| Compatibility | Existing selected-matcher behavior, cache identities and synchronization tests pass |

The new smoke set checks integration, not a statistically established false-match
rate. Measure correct/incorrect accepted duration and unknown duration once on
the recording timeline, plus offset errors and extra boundaries. Do not count
overlapping recognized windows as extra coverage or independent successes.
Keep the rejected moderate example and severe failures visible in the report.
Real recordings without independent truth provide qualitative review only.

A wrong accepted location, identity/clock error or incomplete-search acceptance
blocks shipment of the affected path. Fix that correctness issue or conservatively
abstain; do not lower floors to achieve coverage. Low severe/moderate recall alone
does not reopen the research phase or block this opt-in MVP.

Publish the implementation, recipe/policy IDs, reproducible public fixture runner,
sanitized comparison, exact invocation and known limitations. Run focused core
and CLI tests, formatter/lints and repository publication checks; require CI on
the exact implementation head before merge. Do not claim the MVP issue complete
while it contains only these planning documents.

## Quality issue: separate scope

After locking the MVP, investigate all offset bins on the same six difficult
windows, then test one evidence-driven extractor or ranking change. Run a pinned
`audfprint` baseline on identical source/query pairs. Correct fixture severity
labels, broaden moderate examples, separate calibration from untouched final
data and calibrate longer-window acceptance before enabling it. Add independent
TV/laptop playback truth and drift/boundary measurements before promising
real-room reliability or making this the default.

The quality work may proceed independently but does not gate the MVP's preservation,
packaging and conservative integration. A later quality recipe must be compared
against the shipped MVP and retain an explicit rollback path.
