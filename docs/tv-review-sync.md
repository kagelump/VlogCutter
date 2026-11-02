# Shared TV review synchronization v1

`rcdvlog_core::tv_review_sync` is the single pure Rust clock mapper for manual
cues, acoustic marker detections and passive soundtrack candidates. It owns no
recording storage, ASR, UI, editorial decision or canonical edit plan. All DTOs
use camelCase and deny unknown fields. `SyncRequest::from_json` enforces a 4 MiB
transport bound and version 1. The [request schema](contracts/tv-review-sync-v1.schema.json)
describes transport; Rust also enforces clock and cross-field relationships. Inputs are deterministic and retryable;
`map`/`map_to_cuts` take a cancellation callback and make no writes.

`tv-review sync-preview --input <file>` is a catalog-free JSON diagnostic. Input
is `{ "request": <SyncRequest>, "queries": [<SyncQuery>], "target": null }`.
Supply a strict #221 `ReviewTargetHandoff` as `target` for conservative candidate
operation IDs. It checks target/movie digest, rational timebase, origin and
duration; it cannot retarget a later revision or manufacture source-frame trims.
The [synthetic fixture](../core/rcdvlog-core/tests/fixtures/tv-review-sync/manual-v1.json)
contains only invented clocks and identities.

Each `CaptureClock` has an opaque `clockId`, sample rate, first/last absolute
sample indices and `monotonicOriginNs`. This origin corresponds to sample zero,
even when `firstSample` is nonzero. Each `CapturePosition` retains clock ID,
absolute sample index and original monotonic nanoseconds; those must agree with
the clock calibration within declared `monotonicToleranceNs` (at most 1 s).
This tolerance acknowledges sample/monotonic calibration error and is not a
movie-time confidence claim. Movie alignment uses samples and measures drift. Adapters must start a new clock identity on
capture discontinuity or supply corrected clock calibration explicitly. Do not
substitute wall time, blob sequence numbers or rounded browser milliseconds.
All v1 integers stay within the JavaScript safe range; nanoseconds must use a
session-relative monotonic epoch (under roughly 104 days), not system uptime.

`SyncMovieClock` binds the entire final rendered movie digest, rational tick
size, origin PTS and duration in ticks. `moviePts` is raw output PTS, not elapsed
seconds. Convert using `(PTS - originPts) * numerator / denominator`. Encoders
and ASR providers must not invent a new movie clock.

Every `SyncAnchor` retains `evidenceId`, capture position, raw movie PTS, source
(`manualCue`, `acousticTimecode`, `passiveSoundtrack`), confidence, movie-tick
uncertainty, capture-sample uncertainty and optional `candidateGroupId`. Evidence
IDs reference privately retained original detections; the mapper does not copy
raw microphone bytes into a transport. Scores are component-specific evidence,
not calibrated probabilities. Below-threshold anchors remain in the report and
in `rejectedEvidenceIds`. Multiple positions in one candidate group stay
conflicting until explicitly resolved, even if one candidate has a higher score.

`boundaries` carry evidence ID, clock ID, sample index and kind (`pause`,
`buffering`, `seek`, `rewind`, `resume`, `resync`, `captureInterrupted`). No interval
crossing a boundary is interpolated. Silence never creates a boundary. A new
manual/automatic anchor after a discontinuity begins another alignment run;
rewinds can map different recording times to the same movie position. Unknown
intervals preserve their recording references for later re-sync.

Queries split at anchor/boundary positions. Consistent neighboring anchors within
`maxAnchorGapSeconds` support a local affine estimate. Measured drift from 1x is
reported and checked against configured drift, observation uncertainty and
residual tolerance. Non-forward or implausible progress is conflicting and needs
re-sync. Sparse gaps are unknown. A single cue allows approximate uninterrupted
1x extrapolation only when `maxExtrapolationSeconds` explicitly enables it
(default 0, maximum 30 seconds). This cannot observe an unreported TV pause or
seek. Two anchors cannot prove what occurred between them; configure short gaps
and uncertainty appropriate to playback stability.

Results label `anchored`, `estimated`, `conflicting` or `unknown`. Estimated
bounds include both capture/movie uncertainty, configured residual tolerance
and growth for unobserved playback variation; they are rounded outward to movie
ticks. Estimates remain floating-point PTS on the exact declared timebase,
never frame precision claims. Conflicting/unknown results have no operation IDs;
wide accepted bounds may include several pictured cuts. Human correction and
feedback reconciliation remain separate. Reports retain the original anchor
array, rejected IDs, alternative estimates, evidence IDs and diagnostics.

Limits: 64 capture clocks, 4096 anchors/boundaries each, 256 queries, 8192 output
slices, 8192 estimates/operation IDs across a response, and 64 pair combinations
per slice. Oversized result requests fail with a scope-narrowing diagnostic.
Movie candidate queries validate the immutable target once and use its ordered
intervals; they do not repeatedly hash the entire target for each slice. Large ambiguous groups remain
unknown. Component processing and storage cancellation belong to their owners;
mapper cancellation aborts without side effects. This implementation is a
conservative local piecewise affine model, not a statistical global optimizer.
It intentionally exposes contradictions instead of silently deleting outliers.
Synthetic tests measure known injected drift and clock conversions; no actual
YouTube -> TV -> microphone timing acceptance is claimed here.
