# Live voice review transcription

Play + record review combines the embedded video, microphone controls and a
read-only live transcript. The regular Record review page can also show live
words, with video matches only where its synchronization evidence supports them.
The **Show live transcription and refine it automatically after recording**
checkbox defaults on. Turning it off keeps the explicit post-recording workflow.

## Model choice and setup

The draft uses **multilingual Whisper Tiny** (`ggml-tiny.bin`, 39M parameters,
about 75 MiB on disk), with Silero VAD and greedy CPU decoding on four threads.
The multilingual model supports English and Mandarin; the English-only `.en`
variant is unsuitable for these reviews. This is a provisional speed/quality
tradeoff, not a claim of code-switching accuracy. On this CPU the 4/8/11-second
English smoke samples took 5.2–5.9 seconds per Tiny update, versus 8.0–8.2 seconds
for Base and 7.0–9.1 seconds for quantized Base. These are decoder measurements,
not end-to-end latency promises; longer speech, contention and uploads add delay.
The final pass keeps the
existing **large-v3-turbo q5_0** model and its quality, coverage re-split and
Traditional Chinese display rules.

Install the live model explicitly on the server:

```sh
RCDVLOG_SPEECH_MODEL_DIR=/absolute/model/directory \
  rcdvlog-server admin install-live-speech-models
```

This installs Tiny and Silero in the configured model directory (or the shared
application model directory). The final model must also already be installed;
existing `models install` commands manage it. Recording, polling and worker
startup never download models and never send microphone audio to a cloud.

Upstream model information: [whisper.cpp models](https://github.com/ggml-org/whisper.cpp/blob/master/models/README.md).

## Processing and persistence

Control catalog migration 19 adds opt-in state and provisional utterance tables.
The existing two-second PCM upload and durable audio/event outboxes stay the
recording evidence. The browser's enable request stores the intent to run both
passes. A single worker inside the server lifecycle visits opted-in sessions.
It keeps the Tiny engine loaded while sessions capture, and releases it when
none remain active. Polling is read-only; it never invokes inference.

Every four seconds of newly received audio, the worker revisits **at most the
latest contiguous 30 seconds**. It verifies chunk hashes, downmixes/resamples,
runs VAD, packs speech into the shared 30-second model windows, applies coverage
re-splits and quality flags, and atomically replaces overlapping draft rows.
Silence never reaches ASR. The 30-second model window convention is unchanged;
the four-second interval is a refresh cadence. Prefix lines outside the current
window remain visible. A sentence at the moving window boundary can be revised
or shortened; the full pass resolves those provisional boundaries.

No recording gap is filled or bridged. An upload backlog moves the live window
to the newest contiguous received range rather than queueing hours of stale
inference. The full pass still processes every received range, including late
uploads and audio omitted from the live preview. The UI displays the processed
recording position, a bounded tail of 200 lines, and a follow-live toggle; scrolling
back pauses following.

Loading and decoding take the existing cross-process local-heavy lease. A busy
lease delays the preview and never delays PCM upload. Decoding occurs away from
HTTP and capture threads. The worker checks cancellation between windows and
before committing. A racing finalize/delete prevents draft publication. Expired
recordings stop live work. Temporary live failures retain prior text and retry;
model absence is shown honestly and does not stop capture.

## Movie matching

Draft utterances retain native recording-frame intervals. Every snapshot maps
those intervals through `SessionSync` and the shared core synchronizer using the
same immutable review target, calibrated capture clocks, playback observations
and discontinuity boundaries as the final transcript. New timing evidence can
therefore update thumbnails even without new text. The matching shot is selected
at the mapped utterance start, never at the time the ASR response arrived.

No movie time or thumbnail is invented when timing is pending, conflicting or
outside supported evidence. In particular, the current shared policy does not
extrapolate from one player observation, bridge a seek/pause, or estimate non-1x
playback. Thumbnails represent catalog shots, not exact frames extracted from
the YouTube embed. An independent TV still needs synchronization evidence.

## Final pass and recovery

After audio and events finish uploading and the recorder finalizes, the worker
enqueues the existing durable **local full-pass** job. The stored actor and
existing audio/settings fingerprint make this safe to repeat after a crash.
No recording is finalized early to obtain live text. Incomplete recordings use
only their received ranges under the existing full-pass rules.

The session page shows **Refining** and retains the draft until the selected
full run completes (or finds no speech). Full results then replace it. A partial,
failed or cancelled full pass retains the draft alongside the available full
results and the existing retry controls. Provisional rows are never editable and
never enter discussions, corrections, feedback or edit plans. Full runs retain
independent immutable identities; no human corrections are silently transferred
between runs. Deleted audio suppresses the draft view.

`GET …/review-sessions/{sessionId}/live-transcript` reads the projection;
`POST` on that path idempotently enables both local passes. Both require normal
server authentication; POST also requires CSRF and records an audit event.
The response reports model availability, state, processed/received seconds,
revision, final run ID, stable error code, sync policy/diagnostics and timed draft
lines with shot matches.

## Verification

Focused Rust tests cover opt-in, overlap replacement, bounded backlog, gaps,
silence, contention, cancellation, late sync evidence, deletion, final dispatch
and restart/idempotency. Migration tests upgrade schema 18 and reopen twice.
HTTP tests exercise auth, CSRF and the snapshot contract. Web interaction tests
cover provisional rendering, changing matches, refinement and failure retention.
The Chrome recording test also exercises live opt-in against a real server.

Run real model acceptance explicitly with the installed models and a 16 kHz mono
PCM16 public speech fixture (the upstream whisper.cpp `samples/jfk.wav` works):

```sh
RCDVLOG_SPEECH_MODEL_DIR=/absolute/model/directory \
RCDVLOG_LIVE_TEST_WAV=/absolute/jfk.wav \
  cargo test -p rcdvlog-server-core real_live_model_latency_and_full_pass -- --ignored --nocapture
```

This measures model load, 4/8/11-second draft updates and the final model pass.
It is not a room-microphone, Mandarin accuracy, diarization or HTTPS deployment
acceptance test. Those require an actual review recording on the deployed host.
