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

## Acceptance boundary

Synthetic acceptance is recorded in the project-specific evaluation artifacts.
It does not validate a TV, room, laptop-microphone clock, acoustic delay or
real-room false-alignment rate. The path remains experimental until an
independent playback-truth capture is measured; existing real-room recordings
without such truth are qualitative replay evidence only.
