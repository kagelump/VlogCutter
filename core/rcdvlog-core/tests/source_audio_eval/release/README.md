# Source-audio MVP release comparison v1

Plan step 5 of `docs/tasks/tv-review/SOURCE_AUDIO_MVP_PLAN.md`: one bounded
release comparison of the opt-in `source-audio-pooled-mvp-v1` workflow
(`tv-review source-audio-session-report`). The machine-readable result is
`release-comparison-v1.sanitized.json` in this directory. Private media,
indexes, predictions and the generator/runner/scorer scripts stay in
`$SOURCE_AUDIO_EVAL_ROOT/mvp/release-v1/`; nothing here names private paths or
capture IDs (cases are numbered by `caseIndex`).

Frozen identities: recipe `source-audio-pooled-mvp-v1`; acceptance policy
`rc6-v2-density100-exact-bin-complete-evidence-anchor-identity-calibrated-v3`
(unchanged, no floor lowered); assembler `source-audio-segments-mvp-v2`;
128-second query blocks; FFT-512 / 100 hashes per second / fanout 3 index of
the exact RC6 final mix. Implementation commit `9a09aff`; binary and index
SHA-256 are in the JSON provenance.

## Result

Every check had **zero accepted wrong seconds**. A wrong second is a
supported second whose movie position is more than 100 ms from truth.

| Check | Cases | Correct supported / truth playback | Accepted wrong | Notes |
| --- | ---: | --- | ---: | --- |
| Frozen small-set recognition | 38 searches | 17/17 top-ranked at each density (max error 40 ms); 10 s v3 acceptance 5/6; 0/13 negatives | — | all 11 parity checks pass; 38/38 matcher outputs byte-identical to step 2 |
| Public continuous/seek fixtures | 2 | continuous: one segment at −2.96 s (truth −3.0); seek: two segments at −2.0 s and −15.04 s, transition unknown | 0 | `apps/cli/tests/tv_source_audio_cli.rs`, on every CI run |
| Fresh smoke set, positives | 6 | 136.8 s / 315.0 s (recall 20–73% per capture) | 0 | 0.24 s classified as playback-edge cells |
| Fresh smoke set, no playback | 10 | 0 supported seconds, 0 accepted windows | 0 | |
| Existing small-set negatives, scheduled | 13 | 0 supported seconds, 0 accepted windows | 0 | development data, reused |
| Six difficult historical 30 s windows | 6 | 24.0 s / 168.7 s; 2 windows partly recovered, 4 all unknown | 0 | recovery not required for the MVP |
| Full-recording replay (2710.8 s) | 1 | 1298 s / 2701 s (recall 48%) | 0 | 1352/1352 windows; 145 s wall; 355 MiB peak RSS |

Smoke-set positives (moderate room profile, 48 kHz, 2–4 s room pre-roll):

| Condition | Recall | Median / max offset error | Segments (truth) |
| --- | ---: | --- | --- |
| continuous, SNR 10 dB, PCM | 73% | 33.6 / 33.6 ms | 5 (1) |
| continuous, SNR 5 dB, AAC, +100 ppm | 29% | 28.7 / 29.4 ms | 1 (1) |
| continuous, SNR 10 dB, speech at 6 dB | 64% | 1.7 / 1.7 ms | 1 (1) |
| seek with 1.5 s pause, SNR 10 dB | 20% | 24.9 / 24.9 ms | 2 (2) |
| direct seek, SNR 8 dB, AAC | 42% | 9.4 / 9.4 ms | 2 (2) |
| rewind of 20 s, SNR 10 dB, Opus | 28% | 10.9 ms median; max 20 s inside the 0.12 s seek-edge cell | 2 (2) |

Seeks never bridged: no predicted segment overlaps two truth playback runs by
more than one cell.

Full-recording replay and resume: the uninterrupted run completed all 1352
scheduled windows. A run cancelled after 51 s had completed 448 windows. Its
resume reused those 448, computed the other 904 and produced a report
identical to the uninterrupted one. Without drift fitting, the replay's
+200 ppm clock drift and 0 dB speech fragment the playback into 119 segments,
against 6 truth runs that are effectively continuous.

## Defects found by this comparison and fixed before the final run

1. **Rebased final-window cells** (`ddd0e80`). Every 48 kHz capture whose
   epoch is not a whole number of 2 s steps was refused with
   "supporting cell outside its window", because the final window's chunk
   origin is quantized to the 11025 Hz extractor grid. Cells are now snapped
   onto the window's own grid.
2. **Whole-epoch queries** (`55ca92c`). The 45-minute replay completed 1 of
   1352 windows because each batch searched the whole epoch and hit the
   searcher's 2,000,000-cell vote cap. Long epochs are now searched in fixed
   128 s blocks plus 3 s of pairing context. Equivalence: 29 short captures are
   byte-identical, and two ~450 s sessions where whole-epoch search still
   fits show 0 differences in 449 windows (offset, acceptance, anchors,
   margins) and identical segments.
3. **Uncorroborated isolated cell** (`9a09aff`, assembler v2). In the rewind
   capture, a repetitive soundtrack passage matched an accepted window's
   offset in one cell 3 s before that window's main support run, producing a
   1 s segment 20 s wrong. A window now supports only its main contiguous
   run (the same 2 s gap rule as segment envelopes). This removes support
   only; recognition and acceptance are unchanged.

Intermediate runs are kept in the private release directory. Fix 3 was
prompted by the smoke set, so the final smoke result is development evidence,
not an untouched holdout.

## Scoring rules

Rules were fixed before the final predictions existed, and predictions were
hashed and locked before truth was opened. Measurements are taken once on each
recording timeline (10 ms grid); overlapping windows never add coverage.

- Correct: within 100 ms of the truth movie position at that recording time.
- Boundary: within one evidence cell (1.0 s) of a truth playback edge, and
  matching the truth segment across that edge. The design reports boundary
  uncertainty at one-cell granularity, separately from offset precision.
  Reverb tails count the same way.
- Accepted wrong: any other supported second. This blocks shipment.
- Re-scoring the pre-fix run with these rules still reports its 1.0 s wrong
  span, so the boundary rule does not hide wrong locations.

## Limits of this evidence

- All positives are synthetic room renderings of the exact final mix with
  known truth. No physical TV, room, laptop-clock or acoustic-delay truth is
  included, and none of these results is a population false-match rate.
- The smoke set (seed 20261105) was generated and frozen before matching.
  It reuses the RC6 v1 room/codec pipeline with new crop positions, and its
  10 no-playback captures were verified to share no PCM with older captures.
- The 13 reused negatives and the six difficult windows are previously viewed
  development data.

## Exact invocation

```sh
# Public fixtures (CI): full workflow on committed synthetic audio
cargo test -p rcdvlog-cli --test tv_source_audio_cli

# Private replay (requires the private evaluation root; run from
# $SOURCE_AUDIO_EVAL_ROOT/mvp/release-v1)
python3 generate_smoke_set.py            # once; refuses to regenerate a frozen set
python3 run_release_comparison.py --binary <rcdvlog at 9a09aff> \
  --replay-session <full-session capture id>
python3 score_release_comparison.py      # refuses unless predictions match their lock
```
