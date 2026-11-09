# TV review timing

Maps the reviewer's recording clock to movie time. The shared synchronizer is the only clock mapper; manual cues, acoustic markers, passive soundtrack matching and the experimental full-recording alignment all produce anchors or segments that it consumes. None of them is an editorial decision.

## Shared synchronizer

`rcdvlog_core::tv_review_sync` is the single pure Rust clock mapper for manual
cues, acoustic marker detections and passive soundtrack candidates. It owns no
recording storage, ASR, UI, editorial decision or canonical edit plan. All DTOs
use camelCase and deny unknown fields. `SyncRequest::from_json` enforces a 4 MiB
transport bound and version 1. The [request schema](contracts/tv-review-sync-v1.schema.json)
describes transport; Rust also enforces clock and cross-field relationships. Inputs are deterministic and retryable;
`map`/`map_to_cuts` take a cancellation callback and make no writes.

`tv-review sync-preview --input <file>` is a catalog-free JSON diagnostic. Input
is `{ "request": <SyncRequest>, "queries": [<SyncQuery>], "target": null }`.
Supply a strict `ReviewTargetHandoff` as `target` for conservative candidate
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


## Acoustic markers

A **marked review copy** is a separate derivative of the exact, verified
rendered movie. Its soundtrack carries short, audible markers, each of which
states the movie time it sits at. When YouTube plays that copy on the TV, the
laptop microphone recording picks up some markers among the
conversation. Detection turns those into anchors that say "recording time `r`
shows movie time `m`" for the shared synchronizer. This feature does not
align clocks itself.

| | |
| --- | --- |
| Owner | [`acoustic_markers.rs`](../core/rcdvlog-core/src/acoustic_markers.rs) |
| CLI | `rcdvlog review-markers plan\|export\|detect` (no catalog), in [`review_marker_commands.rs`](../apps/cli/src/review_marker_commands.rs) |

### Recipe `acoustic-marker.chirp-bfsk.v1`

Markers are **audible by design.** This version prioritizes reliability;
masking and inaudibility are not claimed.

**Waveform**

- **Preamble:** a 250 ms linear up-chirp from 1 to 4 kHz, then a 20 ms guard.
- **Payload:** 56 symbols of 30 ms each. A `1` is an up-chirp and a `0` a
  down-chirp, both between 1.5 and 3.5 kHz, with 5 ms raised-cosine edges.
- **Length:** about 1.95 s in total.
- **Band:** everything sits in the 1–4 kHz band that TV speakers, AAC and
  laptop microphones all keep. Nothing relies on ultrasonic content.

**Payload bits**

| Field | Bits | Notes |
| --- | --- | --- |
| version | 4 | |
| export key | 16 | A hash of the review target digest. It distinguishes exports; it carries no project identifier, path or secret. |
| movie time | 20 | In deciseconds |
| CRC-16/CCITT | 16 | |

**Timing**

- Markers start exactly on a 0.1 s grid: every `interval` (default 30 s,
  allowed range 5–600 s), starting at `first` (default 2 s). Each marker must
  fit entirely inside the movie.
- The encoded time is therefore the precise onset of the preamble.
- Default level is −14 dBFS, mixed equally into every channel. If the mix
  would clip, the whole copy is scaled uniformly and the applied gain is
  reported.

### Export (`review-markers export`)

The export runs on the owning host, which has the verified master.

- **Input checks.** The master's BLAKE3 and byte length must equal the
  `ReviewTargetHandoff`. The master is only ever read. The output is
  create-only `.mp4`.
- **What is written.**
  - The soundtrack is decoded at 48 kHz and the markers are mixed in.
  - The **video stream is copied**; the soundtrack is AAC 192 kb/s.
  - Container metadata is stripped.
  - Staging happens beside the output, and the final file is moved into place
    with a create-only rename.
- **Self-verification.** The copy is decoded again and only published if:
  - every planned marker is recovered with a consistent offset (spread under
    2 ms), and
  - the picture stream identity (codec, time base, start PTS, frame count)
    equals the master's.
- **Manifest.** `<output>.markers.json` (`marked-review-copy.v1`) records:
  - project, revision and viewed artifact
  - target digest
  - the master and marked BLAKE3 and length
  - both picture identities
  - the encode recipe, the marker plan and the mix report (input peak/RMS,
    silence flag, output peak, gain)
  - the verification result
  - limitations

  It contains no paths.

Before uploading, re-check the export key against the target with `plan`; a
copy can only be detected using the handoff it was made from.

The marked copy is **never an editorial deliverable**. The canonical movie,
captions, sidecars and edit-plan mappings are unchanged. Upload stays
explicit: use the existing upload-only YouTube commands, or upload manually.
Associating that video ID with the review target is a declaration, not
proof that YouTube serves these bytes.

### Detection (`review-markers detect`)

1. The recording is resampled to 16 kHz.
2. A matched filter is applied, normalised by local energy, with FFT
   overlap-save.
3. The onset is refined to sub-sample precision by parabolic interpolation.
4. Each symbol is classified by correlating it against the up-chirp and
   down-chirp templates.
5. Only candidates that pass CRC, version **and** export key become anchors.

Each anchor reports:

- `recordingSeconds`, `movieSeconds` and `offsetSeconds`
- `preambleScore`, mean and minimum bit confidence, and SNR
- `uncertaintySeconds`

Every other candidate is listed under `rejected` with a reason: `crc`,
`version`, `exportKeyMismatch` or `truncated`.

A missing marker is not evidence of a pause. A seek or rewatch shows up as
**inconsistent offsets**, which the synchronizer must treat as a discontinuity rather than
interpolate across. TV-to-microphone acoustic delay (about 3 ms per metre) is
included in the recording time.

`to_sync_anchors` converts accepted detections into the shared synchronizer's
`SyncAnchor` (`acousticTimecode`, [shared timing contract](tv-review-timing.md#shared-synchronizer)):
exact marker movie PTS, ±50 ms movie uncertainty for encoder priming and
platform re-encode, capture uncertainty from detection timing, and a
per-detection evidence ID. `review-markers detect` returns them as
`syncAnchors`. A test maps a rewatch through `tv_review_sync`: the interval
between consistent markers is `estimated`, and the seek is never interpolated.
Server-side detection on stored session audio is not wired yet.


## Passive soundtrack matching

`rcdvlog_core::tv_soundtrack` supplies independent candidate anchors to the
[shared timing engine](tv-review-timing.md#shared-synchronizer). It compares a bounded microphone
window with the exact rendered movie's final mixed audio. No source-clip
concatenation, ASR, provider calls, YouTube fetching or audio suppression occurs.
Original microphone evidence stays with the recording owner.

The version-1 recipe is
`soundtrack.log-band-delta.fft512-hop128.smooth3-lag6.pcm8k.v1`: mono 8 kHz PCM, Hann-windowed
512-sample FFTs every 128 samples (16 ms), 16 logarithmic frequency bands,
three-frame smoothing, and
96 ms temporal log-energy differences. Those differences remove constant gain
and much stationary room coloration. Bounded coarse-to-fine normalized
correlation searches candidate offsets, with peak separation and temporal
support checks. Silence/low variation, weak peaks and similar repeated positions
remain unmatched/ambiguous. A high score in one part of a seeked/buffered query
cannot establish continuous alignment of the whole window.

The dependency-free FFT/correlation approach is a deliberately small local
baseline. It trades scalable full-movie landmark indexing and time-warp matching
for deterministic, inspectable processing of short windows. It does not claim
robustness when two close talkers dominate the TV. Speed changes may require
manual/marker anchors or shorter re-sync windows; there is no automatic ASR-text
alignment or sound separation.

### Shared API and clocks

`SoundtrackReferenceSpec` version 1 binds the movie BLAKE3, exact rational movie
clock/origin/duration, reference start and duration. Derive it from the selected
verified target (or separately authorized historical review policy). It is a
caller assertion; the matcher cannot authenticate the issuer or validate an
edit plan. `SoundtrackIndex::from_movie` freshly hashes that entire local movie
and checks the declared digest before decoding only its final mixed first audio
stream with FFmpeg restricted to local file/pipe protocols; playlists cannot
fetch network media. Inputs must be regular local files. It retains the reference window offset on the movie clock.
`from_pcm` is a low-level controlled-fixture/decoder adapter and does not
independently verify movie bytes. Do not label arbitrary PCM as a verified movie.

Indexes include recipe, reference identity, PCM digest and content digest.
`from_json` bounds input and rejects modified/version-mismatched indexes. Reuse
an index to avoid repeatedly hashing/decoding a full movie; reprocessing a query
is deterministic. A cached index records its prior reference observation and
never promises current artifact availability. Digests establish consistency,
not authenticity. Raw samples are not serialized in the index, but features are
private derived soundtrack data and must remain local/private.

`SoundtrackCapture` retains the original recording evidence ID, clock ID, sample
rate, first native sample and monotonic origin at native sample zero. Supply one
mono8k query window derived from that exact native range; resampling must not
silently drop audio or change its timeline. The matched anchor is at the query
center and maps back to the original native sample clock. It carries reference
movie ticks, similarity as component confidence, movie uncertainty, capture
uncertainty and candidate group. The report retains query/reference PCM digests,
index identity, scores, peak margin, grid, reasons and deterministic work counts.

A `matched` result is unique only within that reference window. Full-movie
callers must search every applicable index and retain all plausible alternatives.
Reuse the same query evidence ID across indexes: anchor IDs bind the entire
reference index/query/capture identity, while their shared candidate group makes
repeated positions across windows conflicting in the synchronizer. Never stop at the first
window that matches.

Scores are not calibrated probabilities. A dominant competing playback from
another location in the reference can produce strong evidence for the wrong TV
position; audio alone cannot identify which physical source is the television.
Hybrid manual/marker consistency remains necessary in that case. A `matched` result contributes a
candidate anchor, never a confirmed edit/trim. Multiple `ambiguous` candidates
share a candidate group and must all be retained by the synchronizer; never pick just the
highest score. `unmatched` contributes no movie locator. The mapper's confidence
threshold is an explicit caller policy. Both defaults require similarity/confidence
of at least 0.8; the report records the exact matcher policy. Lower custom thresholds
can admit wrong candidates, especially when competing reference audio is louder. Strong manual/marker
conflicts remain visible and require review.

### Local CLI and bounds

- `tv-review soundtrack-reference --spec <JSON> --movie <local-file>` hashes and
  indexes a 4..600 second reference. Save the normal envelope's `data` object as
  an index. It does not upload or copy the movie.
- `tv-review soundtrack-match --index <JSON> --query <mono8k.wav> --capture <JSON>`
  searches one 4..20 second window with default policy. It emits the normal JSON
  report envelope. Decode/resample the capture through its recording owner first,
  preserving original-clock calibration; this command never guesses a native
  microphone clock from a WAV header.

Both commands take the existing shared host scheduling lease. Server/batch
callers must also hold that lease around decoding/matching; low-level pure PCM
methods do not nested-acquire it. Rust entrypoints check cancellation per frame
and search offset. A cancelled or unmatched query leaves prior indexes and raw
recordings intact; retry against the same evidence/index is safe.

Limits are 600 seconds of reference PCM, 20 seconds per query, 16 MiB index JSON,
50 million correlation products, at most 16 refined peaks, and 2..8 reported
candidates. Requests exceeding the compute budget need a shorter reference
window; they cannot silently broaden work. Reference decoding has a 180-second
deadline. No persistent service, extra model, provider spend or schema migration
is introduced. Indexes may be regenerated, and independent query windows provide
bounded resumable units. [Local session orchestration](tv-review-timing.md#local-session-matching)
adds exhaustive reference coverage, bounded batches, private receipts and shared
utterance mapping over these primitives.

The experimental full-recording landmark index and piecewise-segment path is a
separate recipe and CLI contract; see [full-recording source-audio alignment](tv-review-timing.md#full-recording-source-audio-alignment-experimental).


## Local session matching

`tv_soundtrack_session` orchestrates the existing [final-mix matcher](tv-review-timing.md#passive-soundtrack-matching)
and [shared clock mapper](tv-review-timing.md#shared-synchronizer). The media host keeps the exact
rendered movie and its private reference features. Microphone WAV segments can
be transferred from the authenticated recording owner by the operator. Only
path-free anchor evidence needs to return to that owner. No provider, YouTube
fetch, new server, ASR invocation or canonical project write is involved.

### Input and ownership

The version-1 `SoundtrackSessionSpec` contains the existing `SyncRequest`,
native WAV segment declarations, reference/query durations and matcher policy.
The [schema](contracts/tv-soundtrack-session-v1.schema.json) references the shared
sync schema. Rust additionally enforces sample/clock relations, nonoverlapping
segments, complete reference coverage and quotas. A plan deterministically binds
all inputs and policy to a BLAKE3 digest. Editing an emitted plan is rejected.

Each `SessionAudioSegment` declares an opaque segment/clock ID, first original
sample, frame count and full WAV BLAKE3. WAV frame zero must correspond to that
first sample. The file must have the declared native sample rate, exactly the
declared frames and one or two channels. It is hashed before new matching work;
metadata guards detect changes during a batch. Short local FFmpeg decodes
resample bounded windows to mono8k without changing the retained original clock.
Neither a WAV header, request-start timestamp nor packet arrival establishes
monotonic calibration. The recording producer must supply the actual calibrated
`CaptureClock` and clock epochs. Missing calibration remains an integration
blocker; this worker does not manufacture it.

Source/project catalogs remain on their owning host. Do not copy live SQLite
files or put private WAVs, plans, receipts, feature indexes or path bindings in
GitHub. A local `audio-files.json` is an array of `{ "segmentId": "…", "path":
"/absolute/local/native.wav" }`. Paths do not appear in the plan or anchors.
This is an explicit file handoff, not an automatic cross-host transport or a
claim that metadata-only target import transfers the final movie.

### Bounded, resumable commands

Save each normal success envelope's `data` as the input for the next command:

```sh
rcdvlog tv-review soundtrack-session-plan --input session-spec.json
rcdvlog tv-review soundtrack-session-run --plan plan.json --movie final.mp4 \
  --audio-files audio-files.json --cache /private/local/matching --max-tasks 8
rcdvlog tv-review soundtrack-session-run --plan plan.json --movie final.mp4 \
  --audio-files audio-files.json --cache /private/local/matching \
  --start-task 8 --max-tasks 8
rcdvlog tv-review soundtrack-session-evidence --plan plan.json \
  --cache /private/local/matching --first-query 0 --query-count 32
rcdvlog tv-review soundtrack-session-sync --plan plan.json \
  --cache /private/local/matching --queries utterance-frames.json --target target.json
```

Resume at the returned `nextTask`, until it equals `taskCount`. Repeating a
batch reuses checked receipts instead of recomputing it. Tasks are query-major,
then reference-major. Reference indexes are cached and content-checked; a new
movie observation is hashed once per batch needing uncached indexes, rather
than once per reference. Cached evidence describes its prior byte observation,
not current movie availability. Receipts and indexes publish create-only with
fsync. A corrupt cache fails visibly; it is never silently replaced. Primitive
cancellation/failure leaves already committed receipts intact. `--cancel-file`
requests cancellation; retrying the same cursor safely skips completed tasks.
The existing host heavy lease surrounds all media work.

`utterance-frames.json` is an array of native `SyncQuery` intervals. The sync
command gathers nearby matching windows, preserves manual/marker evidence, and
uses the same Rust `map_to_cuts`. It returns the evidence, `searchComplete` and
full anchored/estimated/conflicting/unknown report. At most 128 passive windows
may support one request; narrow the utterance page if it exceeds that bound.

For explicitly authorized historical-movie discussion, use
`--historical-target historical-review.json` instead of `--target`. The distinct
historical DTO is validated and the response says
`mappingPurpose: historicalRenderedArtifactReview`. This is picture lookup on
that movie, with current source/constraints/renderability still unestablished;
it never converts the DTO into a strict handoff, changes server import policy,
approves a render or authorizes an edit. The two target arguments conflict.

### Completeness, gaps and ambiguity

Reference windows overlap by a full query duration and cover the whole declared
movie, including both edges. Every reference is searched for a query before
its passive anchors become eligible for the shared mapper. An interrupted or
partial search remains `pending` and contributes no passive cut locator. A
match in the first reference is insufficient. Manual/marker evidence may still
support a separate estimate while passive search is pending.

Duplicate confirmations within one 20 ms search-grid neighborhood are coalesced
with uncertainty widened to include every contributing observation; original
reports remain in the private cache. Distinct movie positions remain alternatives
in the same candidate group, including repeats found in separate indexes. More
than 64 alternatives remain unusable (`tooManyAlternatives`) rather than picking
one. Native missing-audio intervals add explicit capture boundaries. Known
pause/seek/discontinuity boundaries also split query planning; no query spans
one. Short ranges retain their recording references but cannot yield a match.

Limits: 512 segments, 512 references, 4096 query windows, one million planned
tasks, 1–128 tasks per execution (default 8), and 1–64 queries per evidence page.
Queries are 4–20 seconds; references are at most 120 seconds. These are bounded
units, not a claim that exhaustive correlation is cheap for hours of footage.
Inspect `taskCount` and measure a batch before committing to a long run. The
worker does not automatically start the entire plan or spend external money.


## Full-recording source-audio alignment (experimental)

The separate `tv-review source-audio-landmark-index` and
`tv-review source-audio-landmark-match` commands provide an experimental path
for matching a complete local microphone recording against the exact rendered
movie soundtrack. The path preserves the v1 short-window recipe and cache
format. It does not edit the movie, remove cues or watermarks, change editorial
selections, upload recordings, or claim physical-room timing accuracy.

### Build an immutable full-movie index

Pass the verified movie clock and a bounded landmark recipe in JSON:

```json
{
  "movie": {
    "movieDigest": "<full movie BLAKE3>",
    "timeBaseNumerator": 1,
    "timeBaseDenominator": 240000,
    "originPts": 0,
    "durationPts": 648479824
  },
  "fftSize": 512,
  "hashesPerSecond": 100,
  "fanout": 3
}
```

```bash
rcdvlog tv-review source-audio-landmark-index \
  --input /local/index-request.json \
  --movie /local/verified-final-movie.mp4 \
  --output /local/private-derived/landmarks.json
```

The command re-hashes the complete movie, decodes its first audio stream using
local FFmpeg protocols, and binds the result to the exact movie clock, decoded
PCM, recipe and complete posting-list digest. It writes the index atomically
without replacing an existing artifact. FFT-512 and FFT-1024 use distinct
recipe IDs. Density is 20–100 hashes/second and fanout is 1–3. Indexes are
limited to one hour of movie audio and 64 MiB of serialized postings.

### Match one recording

The input wraps the existing `SyncRequest` and identifies the calibrated
capture clock. Optional `searchPolicy` and alignment `policy` objects are
receipt-bearing research controls; omit them to use the defaults. The `sync`
property must be a complete valid v1 request.

```json
{
  "sync": "<complete SyncRequest v1>",
  "captureClockId": "<clock-id>",
  "searchPolicy": {
    "rarityWeighting": true,
    "saturationDownweighting": true,
    "independenceCaps": true
  },
  "policy": {
    "minimumTileAnchors": 4,
    "minimumTileSeconds": 1,
    "minimumFrequencyDiversity": 4,
    "minimumSegmentTiles": 6,
    "minimumSegmentAnchors": 10,
    "minimumSegmentSeconds": 6,
    "scoreBaselinePerTile": 2.1,
    "segmentChangePenalty": 3.0,
    "ambiguityMargin": 0.75,
    "maxDriftPpm": 2000,
    "maximumGapSeconds": 2.0,
    "gapPenaltyPerSecond": 0.75,
    "anchorStepSeconds": 5
  }
}
```

```bash
rcdvlog tv-review source-audio-landmark-match \
  --index /local/private-derived/landmarks.json \
  --input /local/match-request.json \
  --recording /local/recording.wav
```

The result contains complete-search counts and the top 12 raw offset modes per
one-second query tile, before alignment gates. It also returns supported
`alignment.segments`, explicit unknown and inferred spans, competing offsets,
and the shared mapper's recording-time slices. The bounded raw modes help
separate missing landmark votes from per-tile or segment-selection rejection.
The sequence solver retains up to 128 modes per tile so repeated weak offsets
can accumulate; receipts report the top 12 alternatives plus a selected mode
that ranked below them.
Each accepted segment reports its native half-open
sample range, movie-time affine mapping, matched status, evidence spans,
alternatives, uncertainty, score, support counts, and movie/clock/recipe/index/
policy identities. The current inference uses one-second evidence bins, a
full-sequence competing-offset check, and an explicit extra-segment penalty.
It can bridge up to two seconds of internal unsupported time only when the
offsets on both sides agree and no contradictory or ambiguous mode occupies
the gap. The gap is recorded in `inferredSpans`, uncertainty grows by its
duration, and the mapper receives anchors only from supported evidence spans.
Longer gaps, seeks, and ambiguous transitions remain unknown. Drift is fitted
only when spread-out observations show a bounded improvement over 1x. The
mapper retains existing manual, cue and watermark evidence.

For local diagnosis only, a request may set
`diagnosticOffsetEvidencePerTile` from 1 to 512. Values above the default 12
are accepted only for recordings of at most 60 seconds. This changes the
bounded diagnostic list, not the search or alignment decision.

`rarityWeighting`, `saturationDownweighting`, and `independenceCaps` default to
`true`. Turning them off exists for controlled local ablations and is not a
production recommendation. Scores are evidence weights, not probabilities.
The alignment command does not claim which physical sound source produced a
match. The full query decode is bounded to one hour; cancellation is available
through `--cancel-file` on both commands. Derived indexes and recordings remain
private and local.

### Pooled MVP: full-recording segment report

`source-audio-pooled-mvp-v1` is an opt-in matcher recipe that turns a whole
microphone recording into conservative recording segments with movie offsets.
It does not replace `source-audio-landmark-match`; selecting the older command
(or not running this one) is the rollback, and no catalog state changes either
way. Design and plan: `4c5eb2f:docs/tasks/tv-review/SOURCE_AUDIO_MVP_DESIGN.md` and
`SOURCE_AUDIO_MVP_PLAN.md`. Domain behavior lives in `rcdvlog-core`
(`source_audio_pooled`, `source_audio_sessions`, `source_audio_segments`); the
CLI only reads files, calls core and formats JSON.

What it does:

1. Schedules 10-second windows every 2 seconds inside each declared capture
   epoch, plus a final window ending at the epoch end. Epochs shorter than
   10 seconds stay unknown.
2. Recognizes each window against the complete movie index with
   complete-evidence pooling and the frozen v3 acceptance policy
   (`rc6-v2-density100-exact-bin-complete-evidence-anchor-identity-calibrated-v3`:
   at least 33 independent anchors, 5 occupied one-second bins, a 2.0 ratio to
   the strongest non-adjacent competitor, and a 2.0 unique-anchor margin). A
   truncated, cancelled or budget-infeasible search never becomes a match.
3. Assembles accepted windows into segments (assembler
   `source-audio-segments-mvp-v2`). Each accepted window supports only its
   main contiguous run of one-second cells. Compatible offsets differ by at
   most one 80 ms bin, every member stays within 80 ms of the segment's median
   offset, conflicting cells stay unknown, and gaps of up to 2 seconds may sit
   inside one segment as explicit unknown spans. Longer gaps, seeks, rewinds,
   declared boundaries and epoch changes split segments. Playback rate is fixed
   at 1.0; no drift is fitted and no offset is extrapolated.

The offset convention is
`movieElapsedSeconds = recordingSecondsWithinEpoch + offsetSeconds`. A negative
offset means the movie position is earlier than the recording position.
Recording [20, 40) s at offset +100 maps supported portions to movie
[120, 140) s; raw PTS use the index's declared movie origin and timebase.

#### Workflow

Build the immutable index exactly as above with `fftSize` 512,
`hashesPerSecond` 100 and `fanout` 3 (the only calibrated density). Then write
a report request:

```json
{
  "recipeIdentity": "source-audio-pooled-mvp-v1",
  "movieDigest": "<optional: must equal the index's movie BLAKE3>",
  "recordingDigest": "<optional: must equal the recording file's BLAKE3>",
  "captureClock": {
    "clockId": "<clock-id>",
    "sampleRate": 48000,
    "firstSample": 0,
    "lastSample": 2880000,
    "monotonicOriginNs": 0,
    "monotonicToleranceNs": 1000000
  },
  "epochs": [{ "epochId": "e1", "startSample": 0, "endSample": 2880000 }],
  "domain": { "startSample": 0, "endSample": 2880000 },
  "boundaries": [],
  "pooledModeLimit": 8
}
```

- `captureClock.sampleRate` must equal the recording's native audio rate (the
  command refuses a mismatch), and sample 0 of the file is
  `captureClock.firstSample`. Samples are native, half-open and never move
  under resampling.
- `epochs` are ordered, non-overlapping continuous capture stretches inside
  `domain`. Each epoch has its own recording-time origin (`startSample`).
- `domain` is the recording range the report must fully account for.
- `boundaries` are optional known pause/seek/rewind/interruption
  `SyncBoundary` entries; transitions stay unknown.
- `pooledModeLimit` (1–64) only bounds diagnostic rows; decisions never depend
  on it. Unknown keys are rejected.

```bash
rcdvlog tv-review source-audio-session-report \
  --input /local/report-request.json \
  --index /local/private-derived/landmarks.json \
  --recording /local/recording.wav \
  --cache /local/private-derived/source-audio-sessions \
  --output /local/private-derived/report.json \
  [--cancel-file /local/cancel]
```

The recording is any local file with an audio stream that FFmpeg can decode;
it is re-hashed after decoding and refused if it changed. Progress lines go to
stderr as `{"event":"source-audio.session.progress","completed":N,"total":M}`.
Per-window results persist under `--cache`, keyed by the recording, index,
recipe, policy, schedule and capture-clock identities; a rerun after
cancellation reuses completed windows and any identity change recomputes.
Creating the cancel file stops after the current batch: the command still
exits 0 with `session.status: "cancelled"`, the remaining windows reported as
incomplete, and `report.status: "incomplete"`. Recordings and each epoch are
limited to one hour. Long epochs are searched in bounded query chunks: fixed
128-second blocks from the epoch origin, each extended 3 seconds past its last
window for landmark-pairing context. An epoch that fits one block keeps a
single whole-epoch chunk. Pooled calls are capped at 256 windows and 50 million
offset-cell scans.

The stdout envelope is `tv-review.source-audio-session-report` with
`data = {schemaVersion, session, report}`; `--output` receives the bare
`report`. Read these report fields:

| Field | Meaning |
| --- | --- |
| `status` | `complete` or `incomplete` (some windows were not searched) |
| `segments[]` | `startSample`/`endSample` envelope, `offsetSeconds`, `playbackRate: 1.0`, `supportedSpans[]` (with `movieStartPts`/`movieEndPts` and evidence IDs), `unknownSpans[]` (holes inside the envelope), `retainedSupport[]` |
| `unknownRanges[]` | recording ranges with no movie position and a `reason`: `no_accepted_support`, `weak_or_negative_evidence`, `conflicting_offsets`, `incompatible_offset_transition`, `declared_boundary_transition`, `unsupported_gap`, `epoch_shorter_than_recognition_window` or `outside_capture_epoch` |
| `completeness` | `requestedSeconds = supportedSeconds + unknownSeconds`, `searchComplete`, accepted and incomplete window counts |
| `recognizedWindows[]` | every scheduled window: offset, `accepted`, `failureReasons`, `retained` |
| `alternatives[]` | cells where accepted windows disagreed |

Unknown ranges, unknown spans and conflicts never receive a movie position.

To place an utterance or any recording-time range, map it through the shared
synchronization mapper using only the report's retained support:

```json
{
  "sessionId": "<review-session-id>",
  "query": { "clockId": "<clock-id>", "startSample": 288000, "endSample": 384000 },
  "policy": {
    "minimumConfidence": 0.8, "maxAnchorGapSeconds": 15.0,
    "maxExtrapolationSeconds": 0.0, "maxDriftPpm": 2000.0,
    "residualToleranceSeconds": 0.1, "unobservedUncertaintyPpm": 5000.0
  }
}
```

```bash
rcdvlog tv-review source-audio-map-query \
  --report /local/private-derived/report.json \
  --input /local/query.json
```

The result `state` is `mapped`, `partiallyMapped`, `unknown`, `conflicting` or
`gapCrossing`. Only `mapped`/`partiallyMapped` slices carry movie PTS
candidates; the others keep the query's recording timestamps and return no
movie location. A report with an unknown `schemaVersion` is refused. Reports
are read-only suggestions: recording an editorial note from a mapped position
is a separate, explicit action.

#### Synthetic example (public fixtures)

The committed fixtures in `core/rcdvlog-core/tests/source_audio_eval/synthetic/`
reproduce the whole workflow; `apps/cli/tests/tv_source_audio_cli.rs` runs it
on every CI build. `recording-continuous.wav` (11025 Hz, 15 s) contains 3 s of
room noise and then the 12 s `movie.wav` from its start, so the expected offset
is `0 = 3 + offsetSeconds`, i.e. **−3.0 s**.

```bash
F=core/rcdvlog-core/tests/source_audio_eval/synthetic
MOVIE=$(b3sum --no-names "$F/movie.wav")               # full-file BLAKE3
REC=$(b3sum --no-names "$F/recording-continuous.wav")

cat > index-request.json <<EOF
{"movie": {"movieDigest": "$MOVIE", "timeBaseNumerator": 1,
           "timeBaseDenominator": 11025, "originPts": 0, "durationPts": 132300},
 "fftSize": 512, "hashesPerSecond": 100, "fanout": 3}
EOF
cat > report-request.json <<EOF
{"recipeIdentity": "source-audio-pooled-mvp-v1",
 "movieDigest": "$MOVIE", "recordingDigest": "$REC",
 "captureClock": {"clockId": "synthetic-continuous", "sampleRate": 11025,
                  "firstSample": 0, "lastSample": 165375,
                  "monotonicOriginNs": 0, "monotonicToleranceNs": 1000000},
 "epochs": [{"epochId": "e1", "startSample": 0, "endSample": 165375}],
 "domain": {"startSample": 0, "endSample": 165375}}
EOF
cat > query.json <<EOF
{"sessionId": "synthetic-review",
 "query": {"clockId": "synthetic-continuous", "startSample": 66150, "endSample": 88200},
 "policy": {"minimumConfidence": 0.8, "maxAnchorGapSeconds": 15.0,
            "maxExtrapolationSeconds": 0.0, "maxDriftPpm": 2000.0,
            "residualToleranceSeconds": 0.1, "unobservedUncertaintyPpm": 5000.0}}
EOF

rcdvlog tv-review source-audio-landmark-index --input index-request.json \
  --movie "$F/movie.wav" --output landmarks.json
rcdvlog tv-review source-audio-session-report --input report-request.json \
  --index landmarks.json --recording "$F/recording-continuous.wav" \
  --cache sessions --output report.json
rcdvlog tv-review source-audio-map-query --report report.json --input query.json
```

The mapping `policy` is a complete `SyncPolicy`; every field is required (the
values above are the defaults).

Observed result (release build): progress reports 4 of 4 windows; the report is
`complete` with one segment and one unknown range:

```json
{
  "segments": [{
    "startSample": 33075, "endSample": 165375,
    "offsetSeconds": -2.96, "playbackRate": 1.0,
    "supportedSpans": [{ "startSample": 33075, "endSample": 165375,
                         "movieStartPts": 441, "movieEndPts": 132741 }]
  }],
  "unknownRanges": [{ "startSample": 0, "endSample": 33075,
                      "reason": "no_accepted_support" }],
  "completeness": { "requestedSeconds": 15.0, "supportedSeconds": 12.0,
                    "unknownSeconds": 3.0, "searchComplete": true,
                    "acceptedWindowCount": 4, "incompleteWindowCount": 0 }
}
```

The recording's [3, 15) s maps to movie [0.04, 12.04) s: within 40 ms of the
true −3.0 s delay, as the sign convention predicts. The session separately
reports 40 s of overlapping window work against 15 s of unique recording
coverage. The query for recording [6, 8) s (samples 66150–88200) returns
`state: "mapped"` with candidates covering movie PTS 33516–55566 (about
[3.04, 5.04) s). The same query over the noise pre-roll (samples 11025–22050)
returns `state: "unknown"` with diagnostic `query_in_unknown_range` and no
candidates.

#### Known limitations

- Experimental and opt-in; not the default matcher.
- At least ten seconds of usable audio is needed before anything can match.
- Coverage in difficult audio (heavy noise, reverb, foreground speech or codec
  damage) is incomplete: those spans stay unknown rather than being guessed.
  In the bounded release comparison
  (`core/rcdvlog-core/tests/source_audio_eval/release/`), moderate synthetic
  room captures recovered 20–73% of their playback and a 45-minute replay
  48%, with zero accepted wrong seconds. Four of six difficult historical
  windows stayed entirely unknown.
- Short pauses or seeks of under about two seconds can be missed inside an
  envelope; boundaries are only known to roughly one second, separately from
  the offset precision.
- Clock drift is not fitted; a drifting capture clock splits into successive
  segments rather than one sloped mapping.
- There is no physical-TV false-match guarantee, and no claim about which
  physical speaker produced the matched soundtrack.

