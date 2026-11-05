# Source-audio recognition MVP

Status: implementation design, 2026-11-04. This is a new, bounded delivery
scope following the RC6 eval 2 experiments. It does not certify the earlier
V01 epic or claim that the implementation described here already ships.

Implementation sequence: [MVP implementation plan](SOURCE_AUDIO_MVP_PLAN.md).
Existing contracts: [soundtrack matching](../../tv-soundtrack-matching.md),
[local sessions](../../tv-review-local-session.md), and
[shared synchronization](../../tv-review-sync.md).

## Decision and user outcome

Ship an opt-in local CLI workflow that matches a microphone recording against
the exact final movie soundtrack and returns recording segments with movie
offsets. Prefer one segment and one offset when the evidence supports continuous
playback. Return explicit unknown ranges when it does not.

The MVP locks in complete-evidence pooling and the demonstrated 10-second
recognizer. It adds a small conservative segment assembler and a usable JSON
report. Severe-room recovery, a new extractor, broad confidence calibration,
automatic default activation, and a browser interface are follow-up quality work.
The user can start using the supported matches without waiting for those gains.

No cue or watermark is required by this path. Existing synchronization methods
remain available and their stored evidence remains valid. Selecting this matcher
does not modify the movie or authorize editorial changes.

## Evidence being preserved

The local eval 2 receipt is `preservation/m1-build-and-evidence-receipt-v1.json`.
Its experiment ID is `rc6-v2-m1-exact-bin-pooling-anchor-identity-v1` and source
base is `b8ce2eda591cc4d62208fb192d94346c1b42b135`. The base commit alone does
not contain the experimental implementation: the preserved uncommitted source
and subsequent M1 patch are also required.

| Measurement | Observed result | Interpretation |
| --- | --- | --- |
| Complete small-set searches | 38/38, across two densities | Pooling precedes diagnostic truncation |
| Positive 10/20/30-second windows | 17/17 correct top-ranked offsets at each density; errors within 40 ms | Six captures, correlated windows; development evidence |
| Density100, 10-second v3 acceptance | 5/6 positives; 0/7 calibration and 0/6 audit negatives | Provisional policy, not a population false-accept estimate |
| Composite moderate example | Correct top candidate, rejected | Known recall limitation retained in MVP |
| Six difficult 30-second windows | 2/6 correct at density50; 3/6 at density100 | New severe-recovery work is needed |
| Hardest recording | Correct mode absent from top 64 at all three sampled positions | Lowering an acceptance threshold cannot fix candidate loss |

The 20/30-second acceptance floors are uncalibrated. They do not become
production fallbacks merely because their top candidate is sometimes correct.
No physical-room timing truth or independent final holdout has been established.

## Recognition contract

### Inputs and identity

Accept an exact rendered movie/index, microphone recording, capture-clock
identity/calibration and explicit matcher recipe. Bind receipts to full movie
digest, movie PTS origin/timebase, decoded audio identity, recording digest,
index identity, extractor, acceptance policy and assembler version. Reference
audio is the final mixed soundtrack, including narration, music and transitions.
Never substitute a camera-audio concatenation or another cut with a similar name.

Use the existing immutable index and local-session conventions. Keep audio,
indexes and detailed receipts in the user's derived-data directory. JSON is a
derived report, not a competing catalog or a human editorial decision store.

### Frozen recognition recipe

Start with FFT512, density100, fanout3 and exact 80 ms offset bins, matching the
eval 2 experiment. Pool all full-reference correspondences for each requested
window before shortening diagnostic output. Deduplicate query-anchor identities
within each offset hypothesis. Determine the strongest non-adjacent competing
bin from the full ranking, not the returned top-N list.

For automatic acceptance, use 10-second windows and the versioned v3 policy:

| Gate | Minimum |
| --- | ---: |
| Independent anchors | 33 |
| Occupied one-second time bins | 5 |
| Best/strongest non-adjacent competitor anchor ratio | 2.0 |
| Unique-anchor margin after shared-anchor removal | 2.0 |

The policy also requires complete reference search and complete accumulation.
Port the existing scorer's missing-competitor, zero-denominator and rounding
semantics exactly; do not replace undefined margins with invented confidence.
Ties and missing required evidence abstain. Scores are evidence, not probabilities.

Schedule 10-second windows every 2 seconds within each continuous capture-clock
epoch. Add a final full 10-second window ending at the epoch end if necessary;
deduplicate an identical scheduled window. Epochs shorter than 10 seconds remain
unknown. Scheduling is new MVP behavior and needs its own integration tests.
Overlapping windows are independently recognized; repeated votes never count as
independent confirmation or inflate segment support totals.

20/30-second windows remain diagnostic-only. No threshold tuning or extractor
sweep is required to deliver this MVP. Keep the single-window baseline available
so any assembly regression can be localized.

### Core ownership and bounded execution

Move experimental pooling out of `apps/cli/src/tv_review_commands.rs` into
`rcdvlog-core`; the CLI only validates requests, invokes core and formats results.
Use typed camelCase DTOs and the existing JSON/error/cancellation conventions.
Preserve old recipe outputs and cache identities; introduce a distinct opt-in
recipe such as `source-audio-pooled-mvp-v1`.

Search bounded query chunks against the entire reference. Preserve native
recording sample coordinates through resampling and chunk boundaries. Anchor
identity includes capture epoch and absolute feature-frame position; a local
chunk frame number alone is not unique. Extract with sufficient overlap/context
and prove chunked versus single-query equivalence on the frozen probes.

Retain existing validated one-hour recording/reference limits. Publish explicit
per-batch window, scan and memory limits, progress, completed-window counts and
cancellation state. Do not ask the existing prototype to pool every session
window in one call: it caps requests at 256 windows and 50 million cell scans.
Return incomplete work explicitly and never accept a window whose search was
truncated. Compact returned diagnostics may be truncated after scoring.

## From recognized windows to segments

The assembler is deliberately small; do not revive the old requirement for
several individually strong one-second matches before weak evidence can pool.

1. Retain each accepted window's offset and the one-second cells/anchor positions
   that actually support it. A recognized window is not proof of uninterrupted
   playback throughout all ten seconds.
2. Reconcile overlapping accepted windows in recording coordinates. Compatible
   offsets differ by at most one 80 ms bin. A cell with incompatible accepted
   hypotheses is conflicting/unknown; do not choose one using continuity alone.
3. Group adjacent supported cells with compatible offsets in the same epoch.
   Use a median offset, requiring every member to remain within 80 ms of that
   representative; avoid transitive chains that gradually drift away. Deduplicate
   anchor IDs when reporting segment support.
4. Prefer a single group for uninterrupted support. An internal unsupported gap
   of at most two seconds may remain inside one segment's envelope only when
   both sides agree and there is no contradictory evidence or declared boundary.
   Record that gap explicitly as unknown; it contributes no support and is not
   automatically mapped. Longer gaps split segments.
5. Split on known pause/seek/rewind/interruption boundaries, epoch changes and
   incompatible evidence. Transition cells remain unknown. Do not extrapolate
   the dominant offset over the rest of the recording.

MVP mapping has fixed playback rate 1.0. Drift fitting and variable playback
speed are deferred. If offsets cease to agree, split or abstain. Report offset
precision separately from boundary uncertainty: evidence uses one-second cells
and ten-second recognition windows, so a 40 ms synthetic offset error does not
imply a 40 ms seek-boundary estimate.

## Report and downstream use

Return a versioned report with `status`, input/recipe identities, completeness,
recognized windows, `segments`, `unknownRanges`, alternatives and diagnostics.
Suggested segment fields:

- `captureClockId`, native half-open `startSample`/`endSample`;
- `offsetSeconds`, `playbackRate: 1.0`, supported spans and evidence IDs;
- `unknownSpans`, offset uncertainty and boundary uncertainty;
- deduplicated support counts and policy/rejection reasons.

Define offset unambiguously as
`movieElapsedSeconds = recordingSecondsWithinCaptureEpoch + offsetSeconds`.
For example, recording [20, 40) seconds with offset +100 maps supported portions
to movie [120, 140). Convert elapsed movie time to raw PTS using the declared
origin and rational timebase. A new epoch has its own recording-time origin.

Provide a read-only adapter to the existing `tv_review_sync` query path using
actual retained support positions. Never synthesize two independent anchors
from one pooled mode to bypass mapper gates. The mapper may return less coverage
than the segment envelope; preserve that result. Unknown, conflicting and
gap-crossing utterances retain recording timestamps and no automatic movie
location. Manual/cue evidence conflicts remain visible.

The usable MVP surface is the CLI plus a documented invocation and JSON report.
It does not need server persistence, a migration, ASR changes or a UI. Rollback
is selecting the existing matcher recipe; no catalog repair is needed.

## Delivery boundary and follow-up

MVP acceptance requires preserved single-window results, real CLI/index/report
integration, conservative segment tests and no accepted wrong ranges on the
bounded regression set specified in the implementation plan. Severe examples
may remain unknown. Recovery of those examples is not an MVP prerequisite.

The quality issue owns the six-window all-offset diagnostic, an established
`audfprint` comparison, better extraction/ranking, representative moderate
fixtures, per-duration calibration, fresh holdouts, drift and independent
real-room timing evidence. Those findings must not silently change this recipe.

Known limitations to ship in the user-facing documentation: an experimental
opt-in, ten seconds of usable audio needed, incomplete coverage in difficult
audio, possible missed playback changes, no physical-TV false-match guarantee,
and no claim to identify the physical speaker emitting a matched soundtrack.
