# Acoustic movie-time markers for TV review copies (#226)

A **marked review copy** is a separate derivative of the exact, verified
rendered movie. Its soundtrack carries short, audible markers, each of which
states the movie time it sits at. When YouTube plays that copy on the TV, the
laptop microphone recording (#222) picks up some markers among the
conversation. Detection turns those into anchors that say "recording time `r`
shows movie time `m`" for the shared synchronizer (#223). This feature does not
align clocks itself.

| | |
| --- | --- |
| Owner | [`acoustic_markers.rs`](../core/rcdvlog-core/src/acoustic_markers.rs) |
| CLI | `rcdvlog review-markers plan\|export\|detect` (no catalog), in [`review_marker_commands.rs`](../apps/cli/src/review_marker_commands.rs) |

## Recipe `acoustic-marker.chirp-bfsk.v1`

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

## Export (`review-markers export`)

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
Associating that video ID with the review target (#221) is a declaration, not
proof that YouTube serves these bytes.

## Detection (`review-markers detect`)

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
**inconsistent offsets**, which #223 must treat as a discontinuity rather than
interpolate across. TV-to-microphone acoustic delay (about 3 ms per metre) is
included in the recording time.

## Tests

- **Core** (`acoustic_markers_tests.rs`, synthetic only):
  - payload round trip; CRC rejects every single-bit flip
  - plan placement and bounds
  - exact timestamps on a clean round trip (under 0.5 ms)
  - recovery with two-talker-like interference, room noise and a 60% TV volume drop mid-recording (offset error under 2 ms)
  - a different export's markers are rejected, not anchored
  - zero anchors from music plus conversation or pure noise
  - a rewatch yields `[2, 32, 32, 62, 92]` with offsets `[1, 1, 16, 16, 16]`
  - a truncated recording reports the cut marker
  - mixing never clips
  - **survival through a real FFmpeg AAC 96 kb/s transcode**
  - a real marked-copy export: master untouched, picture identical, markers
    self-verified, path-free manifest, create-only, wrong master refused
- **CLI process contract** (`review_markers_cli.rs`): plan, export (with a
  non-ASCII path), create-only refusal envelope, and detection from a WAV of
  the marked copy.

## Not verified

The following is local acceptance, not cloud work:

- A YouTube transcode
- TV speakers, room acoustics, or a real laptop microphone
- Two real talkers and real music or dialogue loudness
- Subjective audibility (A/B)
- Detection and false-positive rates on real recordings
- Measured alignment error after #223

The synthetic interference is a stand-in for speech, not a speech corpus.
Server-side detection on stored session audio, and feeding anchors into the
#225 workspace, are waiting for the shared #223 interface.
