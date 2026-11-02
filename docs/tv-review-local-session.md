# Local TV review session matching

`tv_soundtrack_session` orchestrates the existing [final-mix matcher](tv-soundtrack-matching.md)
and [shared clock mapper](tv-review-sync.md). The media host keeps the exact
rendered movie and its private reference features. Microphone WAV segments can
be transferred from the authenticated recording owner by the operator. Only
path-free anchor evidence needs to return to that owner. No provider, YouTube
fetch, new server, ASR invocation or canonical project write is involved.

## Input and ownership

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

## Bounded, resumable commands

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

## Completeness, gaps and ambiguity

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

## Acceptance

Generated tests exercise an actual local render, native-rate capture derived
from its final mixed audio, CLI planning/resume, complete-reference search,
ambiguous repeats, overlap confirmations, gaps, cancellation, modified evidence
and strict/historical picture candidates at a cut boundary. Synthetic clocks
are explicitly declared inputs. They do not measure actual YouTube/TV acoustic
delay, reviewer overlap, microphone calibration, ASR quality or HTTPS topology.
The real recording and those hardware measurements remain required.
