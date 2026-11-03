# V01 architecture contract: passive soundtrack sync on real room audio

Status: frozen design for epic [V01](EPIC.md), written 2026-11-03. Packets in
`tasks/` cite sections here (§n). Implementers may rely on these decisions but
may not redesign them; if one does not fit, escalate.

## §1 Problem and evidence

The RC6 real microphone session (180 s, laptop mic in a room, editor talking
over the TV) produced **0 of 31 matched windows**. See
`../data/editorial-review/rc6-passive-sync-2026-11-03/REPORT.md`. All paths in
this document are relative to the `vlogcutter/` repo root.

The numpy replica `../data/editorial-review/rc6-passive-sync-2026-11-03/diagnose_probe.py`
reproduces the Rust scores exactly. It shows that the matcher found the right
position but then rejected it:

| Mic window | Best movie pos | Score | Best elsewhere |
|---|---|---|---|
| 0:06 | 0:05.04 | 0.615 | 0.194 |
| 0:12 | 0:11.04 | 0.624 | 0.267 |
| 1:18 | 1:17.04 | 0.621 | 0.148 |
| 1:42 | 1:41.04 | 0.682 | 0.233 |
| 1:48 | 1:47.04 | 0.732 | 0.227 |
| 2:30 | 2:29.04 | 0.677 | 0.196 |

There are four failures, each addressed below:

1. **Absolute threshold.** All six peaks share one offset (movie = mic − 0.96 s),
   and each leads the next-best position by 0.36–0.50. The fixed
   `minimum_similarity = 0.8` rejected them all. Fixed in §4.
2. **No diagnostics.** Unmatched reports drop where the peak was, so the
   near-miss was invisible. Fixed in §3.
3. **Speech masking.** In windows where the editor speaks, the close-mic voice
   dominates the 16-band log-delta features, and the true offset scores only
   ≈0.0–0.3. Fixed in §6 and §7.
4. **Sparse anchors.** `SyncPolicy.max_anchor_gap_seconds = 15` would leave most
   utterances unknown even with six matches, because the matches are 12–60 s
   apart. Fixed in §8 and §9.

## §2 Compatibility rules (apply to every packet)

- **v1 must replay byte-identically.** Existing cached reports, plans, indexes
  and receipts use recipe
  `soundtrack.log-band-delta.fft512-hop128.smooth3-lag6.pcm8k.v1` and schema
  version 1. Their JSON is hashed for cache keys and digests. Therefore:
  - Every new field is **optional and omitted when unset**, using
    `#[serde(default, skip_serializing_if = "Option::is_none")]` or
    `"Vec::is_empty"`. Re-serialising a v1 object then gives identical bytes and
    an identical digest.
  - Every new behaviour is **off unless its new field is set**. No serde
    default may switch new behaviour on.
- Keep `#[serde(deny_unknown_fields)]`, every existing bound (`MAX_PRODUCTS`,
  4–20 s queries, query quotas), cancellation checks and `comparison_products`
  accounting in every new loop.
- Determinism: no randomness and no dependence on thread order. Break ties by
  offset ascending.
- The thresholds below are **calibrated recommendations backed by §10 evidence**.
  Callers set them explicitly; they are never serde defaults. Never lower
  `SyncPolicy.minimum_confidence` (0.8).
- Do not touch the editorial chronology gate (`../data/chronology-audit/**`).

## §3 Peak diagnostics

`SoundtrackMatchReport` gains `best_peaks: Vec<SoundtrackPeak>`. The field is
optional and omitted when empty, and holds up to 4 peaks, best first. It is
filled for **every** report that reaches the search, including unmatched and
ambiguous ones. Quiet and insufficient-variation reports leave it empty.

```rust
pub struct SoundtrackPeak {
    pub similarity: f64,
    pub reference_offset_frames: usize,
    /// reference.start_seconds + (offset*HOP + query_len_samples/2) / PCM_RATE
    pub movie_seconds: f64,
    pub subwindow_similarities: [f64; 3],
}
```

`SessionQueryEvidence` gains `best_peaks` (optional, omitted when empty). It
holds the best 4 peaks across all completed references for that query:

- Merge peaks whose `movie_seconds` differ by less than 16×HOP/PCM_RATE
  (0.256 s), keeping the higher score.
- Sort by similarity descending, then `movie_seconds` ascending.

## §4 Distinctive acceptance

`SoundtrackPolicy` gains `distinctive: Option<DistinctivePolicy>`, omitted
when None:

```rust
pub struct DistinctivePolicy {
    pub minimum_similarity: f64, // range 0.3..=1.0; recommended 0.5
    pub minimum_margin: f64,     // range 0.1..=0.8; recommended 0.25
    pub minimum_z_score: f64,    // range 3.0..=20.0; recommended 6.0
}
```

The report gains `distinctiveness: Option<f64>`. It is computed over all coarse
scores of the searched reference: z = (best − median) / (1.4826 × MAD). If the
MAD is 0, z is None, and None fails the z test.

Acceptance order in `match_pcm`:

1. The existing quiet, variation and per-third temporal-support checks are
   unchanged.
2. The old path is unchanged: best ≥ `minimum_similarity` with the existing
   margin rule.
3. Only if `distinctive` is Some and step 2 did not match: the query is Matched
   if best ≥ `d.minimum_similarity`, margin ≥ `d.minimum_margin` and z ≥
   `d.minimum_z_score`. Set reason `"distinctive_peak_low_similarity"` and
   anchor `confidence = min(1.0, 0.5 + margin)`.
4. Otherwise, the existing states and reasons apply.

## §5 Global margin across references (session)

A session query is searched against ~24 overlapping references, and a margin
computed inside one reference cannot see a repeat in another.

When the plan policy has `distinctive` set, `evidence_page` re-checks every
query that has a Matched reference. It compares the matched peak against the
merged §3 peaks from **all** references. If any peak at a different movie
position (at least 0.256 s away) breaks the applicable margin, the query state
becomes `ambiguous` and it emits no anchors.

With `distinctive` None, behaviour is unchanged from v1.

## §6 Speech mask and gap-placed windows

`SoundtrackSessionSpec` gains `speech_mask: Vec<SyncQuery>`, omitted when
empty. These are recording-clock intervals, which the caller builds from
transcript utterances padded ±0.25 s. The core never reads transcripts.

- **Mask.** `match_pcm` takes an optional per-frame weight slice (`&[f32]`, one
  weight per query feature frame, values in [0,1]). `similarity` multiplies
  both sums by the weight. If the unmasked weight totals less than 2 s of
  frames, or less than 0.5 s in any third, the query is unmatched with reason
  `"masked_insufficient"`.
- **Effective weights (revised 2026-11-03, owner-driven V01 cycle).** Masked
  samples are silenced before features are computed. Frame `n` reads samples
  `[(n-8)*HOP, n*HOP+FFT)`: its own window plus its smoothed lag-6 delta
  history. A frame that reads any silenced sample would score the jump from
  silence as a full-scale onset, so it is weighted zero as well. The
  sufficiency rule above counts these effective weights. Each masked span
  therefore costs about 0.19 s of extra frames. A 6 s window with 0.4 s gaps
  is honestly `masked_insufficient`; gap-placed windows are the intended way
  to score speech-heavy stretches. `mask_digest` still hashes the supplied
  mask weights.
- **Mask identity.** The report gains `mask_digest: Option<String>`, the BLAKE3
  of the weight bytes. The session cache key includes it.
- **Gap windows.** `SoundtrackSessionPlan::new` keeps the regular grid queries
  unchanged. It appends one query for each unmasked gap of at least 4 s, sized
  `min(gap, 20 s)` and centred in the gap. `SessionMatchQuery` gains
  `origin: Option<String>`: None for grid queries, `"speech_gap"` for gap
  queries. If the quota would be exceeded, keep the longest gaps, breaking ties
  by earliest start.
- **Gap windows, refinements (2026-11-03, owner-driven V01 cycle).**
  - A gap query whose range equals a grid query is dropped before the quota
    is applied, so it never takes a slot.
  - Gap queries are capped at `min(20 s, reference_seconds - 1, movie
    seconds)`.
  - References overlap by the longest planned query. Without a gap query
    longer than `query_seconds`, the reference windows are exactly v1's, so
    every query position still lies wholly inside some reference.
  - Cached receipts are checked against each query's own PCM length.
  - Gap query edges snap inward to pairs of PCM8k samples, relative to the
    segment start (12 native samples at 48 kHz). The decoded window then
    has exactly the expected length, and its even length puts the anchor
    centre exactly on the query centre. Found by the T010 seek run: an
    unaligned 48 kHz gap query failed the decoded-length check.

## §7 Recipe v2: weighted, robust features

`SOUNDTRACK_RECIPE_V2 = "soundtrack.log-band-delta.fft512-hop128.smooth3-lag6.pcm8k.wtd-robust.v2"`.
`SoundtrackSessionSpec` and `SoundtrackReferenceSpec` gain
`recipe: Option<String>`; None means v1.

- **Frame energy.** The index gains `frame_energy: Vec<f32>` (v2 only, omitted
  when empty): the broadband log energy of each frame.
- **Frame weight.** w = clamp((E − p20)/(p80 − p20), 0, 1), using the
  reference's own 20th and 80th percentiles. This weight multiplies with the §6
  mask.
- **Robust features (v2 only).** Subtract each frame's median across its 16
  bands, then clip to ±3. (v1 clips to ±8 and has no median step.)
- **No collisions.** The recipe string is part of every index digest and cache
  key, so v1 and v2 entries can never collide.
- **Refinements (2026-11-03, owner-driven V01 cycle).**
  - `frame_energy` is the log of the summed energy of the same 16 bands
    (≈94 Hz–3.8 kHz). Percentiles are nearest-rank. If p80 − p20 ≤ 1e-6,
    all weights are 1.
  - The scoring weight of frame `n` is the **minimum** frame weight over
    `n-8..=n`, the frames its smoothed lag-6 delta reads. A query frame
    therefore cannot score audio aligned with near-silent reference audio
    through its delta history; this is the same lesson as the §6 effective
    weights.
  - v1 is always spelled `recipe: None`. `Some(v1 name)` and unknown names
    are rejected.
  - The v1 digest input is unchanged. The v2 digest also binds
    `frame_energy`.

## §8 Offset consensus

New module `tv_soundtrack_consensus.rs`. Its input is a complete session's
evidence. An incomplete search is rejected with an error containing
`search_incomplete`.

1. For each query, compute offset = movie_seconds − mic_center_seconds for
   every `best_peaks` entry with similarity ≥ 0.25.
2. Sort the offsets and cluster them greedily within ±0.08 s.
3. A **segment** is a maximal run of queries, ordered by mic time, whose best
   in-cluster peak supports the same cluster. It needs at least 3 supporting
   queries, of which at least 2 are Matched.
4. A query Matched at a different cluster splits the run. The split is emitted
   as a `SyncBoundary`; there is no new boundary type.
5. Each segment reports:
   - `segment_id` and `offset_seconds` (the median);
   - `mic_start_sample`/`mic_end_sample` (the first and last supporting query
     centres) and the supporting query indices;
   - `residual_rms_seconds` and `drift_ppm` (least-squares slope × 1e6).
6. Reject a segment if |drift| > `SyncPolicy.max_drift_ppm`.

Output: `SoundtrackConsensus { schema_version: 1, plan_digest, segments,
boundaries, unsupported_queries }`. CLI:
`tv-review soundtrack-session-consensus --plan P --cache C`.

## §9 Consensus-gated interpolation

`SyncRequest` gains `consensus_segments: Vec<ConsensusSegment>`, omitted when
empty. The segments are copied from §8 output; the mapper never infers them.

In `map_slice`, interpolation is normally refused when the left and right
anchors are more than `max_anchor_gap_seconds` apart. It is allowed only if
both anchors and the whole query lie inside the same segment, with no boundary
between them. The result has:

- a new state, `SyncState::ConsensusInterpolated`;
- uncertainty = anchor uncertainty + residual_rms +
  `unobserved_uncertainty_ppm` × gap_seconds × 1e-6;
- a diagnostic `"consensus_segment:<segment_id>"`.

Extrapolation outside a segment's supporting span stays forbidden.
`soundtrack-session-sync` gains an optional `--consensus FILE` argument.

## §10 Real-room acceptance evidence

Folder: `../data/editorial-review/rc6-passive-sync-v2/`. Start from a copy of
the old `run.py`; never modify the old folder. Run all of the following.

1. **v1 replay.** Use the v1 recipe and policy. Expect 0 matches and the same
   best similarities as the 2026-11-03 report.
2. **v2 positive.** Use v2 with distinctive set to (0.5, 0.25, 6.0), the speech
   mask from the utterances in
   `rc6-transcription-2026-11-02/local-transcript.json`, and consensus. Expect:
   - at least 6 Matched windows, **every** one at offset −0.96 ± 0.08 s;
   - one segment covering roughly 0:05–2:55;
   - at least 20 of 27 utterances mapped (`Anchored`, `Interpolated` or
     `ConsensusInterpolated`).
3. **Negative controls.** Each must give 0 matches and 0 segments:
   a. the movie window 1000–1240 s only;
   b. the mic recording time-reversed;
   c. a different rendered movie that shares none of the first 4 minutes.
      Verify that before relying on it.
4. **Seek control.** Splice mic 0–90 s to mic 120–180 s. Expect two segments
   and a boundary near 90 s.
5. **Report.** Write `REPORT.md`, including per-query `best_peaks`. Claim no
   timing accuracy, because there is no independent playback ground truth.

If part 2 misses or any part 3 control matches, stop and report the numbers.
Do not tune the thresholds to make it pass.
