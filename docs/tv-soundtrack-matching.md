# Passive TV soundtrack matching

`rcdvlog_core::tv_soundtrack` supplies independent candidate anchors to the
[shared timing engine](tv-review-sync.md). It compares a bounded microphone
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

## Shared API and clocks

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
repeated positions across windows conflicting in #223. Never stop at the first
window that matches.

Scores are not calibrated probabilities. A dominant competing playback from
another location in the reference can produce strong evidence for the wrong TV
position; audio alone cannot identify which physical source is the television.
Hybrid manual/marker consistency remains necessary in that case. A `matched` result contributes a
candidate anchor, never a confirmed edit/trim. Multiple `ambiguous` candidates
share a candidate group and must all be retained by #223; never pick just the
highest score. `unmatched` contributes no movie locator. The mapper's confidence
threshold is an explicit caller policy. Both defaults require similarity/confidence
of at least 0.8; the report records the exact matcher policy. Lower custom thresholds
can admit wrong candidates, especially when competing reference audio is louder. Strong manual/marker
conflicts remain visible and require review.

## Local CLI and bounds

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
bounded resumable units. [Local session orchestration](tv-review-local-session.md)
adds exhaustive reference coverage, bounded batches, private receipts and shared
utterance mapping over these primitives.

## Acceptance boundary

Procedural synthetic tests cover gain, room filtering, competing audio, repeated
regions, silence, codec/stereo/resampling, byte checks, reference offsets and
shared-mapper conflicts. These are controlled tests. Local real-final-mix crops
and injected distortions are useful engineering measurements, but they do not
reproduce actual YouTube encoding, TV speakers, laptop microphone clocks,
acoustic delay or human conversation. #227 must remain open until that topology
has measured error, recovery, false positives, unmatched intervals and ambiguity.
