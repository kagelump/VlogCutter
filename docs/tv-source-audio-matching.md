# Full-recording source-audio alignment

The separate `tv-review source-audio-landmark-index` and
`tv-review source-audio-landmark-match` commands provide an experimental path
for matching a complete local microphone recording against the exact rendered
movie soundtrack. The path preserves the v1 short-window recipe and cache
format. It does not edit the movie, remove cues or watermarks, change editorial
selections, upload recordings, or claim physical-room timing accuracy.

## Build an immutable full-movie index

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

## Match one recording

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

## Pooled MVP: full-recording segment report

`source-audio-pooled-mvp-v1` is an opt-in matcher recipe that turns a whole
microphone recording into conservative recording segments with movie offsets.
It does not replace `source-audio-landmark-match`; selecting the older command
(or not running this one) is the rollback, and no catalog state changes either
way. Design and plan: `docs/tasks/tv-review/SOURCE_AUDIO_MVP_DESIGN.md` and
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

### Workflow

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

### Synthetic example (public fixtures)

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

### Known limitations

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

## Acceptance boundary

Synthetic acceptance is recorded in the project-specific evaluation artifacts.
It does not validate a TV, room, laptop-microphone clock, acoustic delay or
real-room false-alignment rate. The path remains experimental until an
independent playback-truth capture is measured; existing real-room recordings
without such truth are qualitative replay evidence only.
