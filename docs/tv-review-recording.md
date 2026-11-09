# TV voice review recording (#222)

Chrome records a reviewer's conversation while they watch a rough cut on a TV,
then stores it as **private session evidence** on the companion server. The
recording is bound to one imported, immutable [review target](tv-review.md)
(#221). This feature does not transcribe (#224), synchronize (#223), or write
feedback (#225). Those owners read this evidence later; see
[TV review transcription](tv-review-recording.md#transcription).

In **Record review** the TV plays on its own. In **Play + record review**, the page embeds the target’s declared YouTube video and records playback observations alongside the microphone.
The only statement the page makes about the video is the reviewer's optional
**manual cue** ("the first frame is showing now"), which is recorded as a
declaration and not as measured sync.

## Play + record review

For a target with a YouTube ID, choose **Play + record review**, allow the browser’s microphone request, and press Record above the video. Press play in the embedded player after recording starts. Player position, state and playback rate are sampled as soon as both the player and microphone capture are ready, every five seconds thereafter, on player state/rate changes, and immediately before the user stops recording. Each observation retains its browser monotonic time, a contemporaneous microphone-clock pairing and clock epoch. Observations are durable session events and can be lost if local event storage fails (the recorder reports unsaved events). Pauses, buffering and seeks can make the mapping discontinuous. The observations are approximate and do not measure TV output latency or verify that the YouTube video matches the target. The shared synchronization pipeline uses valid playback observations to estimate comment positions and shot thumbnails; uncertainty and interruptions remain explicit. An independent TV player is not observed.

## User flow

*Projects → TV reviews → cut → Record review*:

1. Open the target's YouTube video on the TV.
2. The page requests microphone permission on load. Allow it in the browser,
   then pick the input device. Recording begins only when you press Record;
   blocked permission offers a retry button.
3. Press **Record** above the player. There are no consent checkboxes. The recorder shows `Recording <elapsed>`. The level meter
   and upload counters update while you record.
4. Optionally press **Mark start** (or `M`) when the first frame appears.
5. Press **Stop**. The page flushes and uploads the tail and finalizes the
   session, then opens the session page.

Recording keeps running while the tab is in the background, as long as Chrome
keeps the AudioContext alive. The target page lists every session for that
exact target.

## Capture and clock

- `getUserMedia` requests the device with `echoCancellation`,
  `noiseSuppression` and `autoGainControl` all off, and `channelCount` ideally 1.
  The settings the browser actually applied are stored with the session.
  They are recorded, not trusted.
- [`pcm-capture-worklet.js`](../apps/web/public/pcm-capture-worklet.js) copies
  each render quantum verbatim and tags it with the AudioContext `currentFrame`.
  That frame counter is the **recording clock**. Recording frame 0 is the first
  captured sample.
- If `currentFrame` jumps, the worklet reports a discontinuity. The current
  chunk is closed, the next chunk starts at the true later frame, and a
  `captureDiscontinuity` event is stored. The server then sees a gap, not
  stitched audio.
- Samples are stored as interleaved `pcm_s16le` at the context rate (48 kHz
  requested). The float → int16 quantization is the only transformation.
- Timing that is not on the recording clock is correlation data only:
  - each chunk's main-thread `performance.now()` on delivery (not its sample-zero origin)
  - each event's `performance.now()`
  - the server's receive time for every chunk and event

  None of these is a client wall-clock claim. `clientStartedPerformanceMs`
  on session create is read before the create round trip and worklet start,
  so it is not a capture-start time; use clock samples instead.
- **Clock samples** pair the recording clock with `performance.now()`. A
  `clockSample` event is queued at the first delivered audio, every 5 s, on
  resume and at stop. `audioFrame` is the sample index (the shared sync
  engine's `CapturePosition.sampleIndex`), `performanceMs` the paired reading,
  and `detail` holds `clockEpoch`, `method` and a declared `uncertaintyMs`:
  - `getOutputTimestamp` reads both clocks together: ± one render quantum
    (128 frames, ≈2.7 ms at 48 kHz);
  - the `currentTime` fallback reads them separately: ± two quanta.
- **Clock epochs.** A suspended or interrupted AudioContext stops its sample
  clock while wall time continues. The transition is recorded as a
  `clockSample` with `detail.transition` and no pairing, and each resume starts
  a new `clockEpoch`. Pairs from different epochs must never be fitted
  together. The server requires `clockEpoch` on every clock sample, and a
  pairing sample must carry `performanceMs` and a finite non-negative
  `uncertaintyMs`.
- A manual cue is stored on the recording clock (`AudioContext.currentTime`
  rebased to frame 0, with `outputLatency` and `clockEpoch` in its detail),
  together with the declared movie second (0 = first frame). Converting that
  declared elapsed second to the target's raw movie PTS uses the selected
  target's exact origin and timebase in the shared sync engine; the server's
  receive time is never a capture time.
- **Stop.** The worklet flushes its partial batch and then acknowledges the
  frame after the last one it delivered; port ordering means the audio is
  already on the main thread. The recorder drains to exactly that frame
  before closing the context and finalizing. If no acknowledgment arrives
  within 1 s, the tail is unknown: the final frame count is extended to the
  context clock's position at stop, a `captureDiscontinuity` with
  `reason: "stopUnacknowledged"` records the missing frames, and the server
  reports that span as a gap (the session is `incomplete`), never as audio.

A versioned `clockSample.detail` (`schema: review-capture-calibration.v1`)
retains the sample/performance pairing. The main thread sends a probe with its
monotonic send time; the worklet answers on a processing quantum with the native
frame and first captured frame; receipt time closes the causal bracket. Detail
keeps the original performance epoch/send/receive values, session-relative
`probeSentNs`/`probeReceivedNs`, `sampleIndex`, `sampleRate`, `firstSample`,
`clockId`, estimated `monotonicOriginNs` and `monotonicToleranceNs`. The bound
includes half the roundtrip plus one rendering quantum. It is render-thread
calibration, not a measured ADC/acoustic delay. Slow (>500 ms), negative or
invalid observations stay uncalibrated. Probes repeat every 30 s; suspend/resume
records a discontinuity and starts a new clock identity. A consumer must retain
these observations and reject incompatible epochs rather than reconstructing
sample zero from request-start or packet arrival. TV alignment remains #223's
job and has its own uncertainty.

Stop waits for a same-port worklet acknowledgment containing its final frame,
with all tail audio delivered first. It then drains storage and upload before
finalizing. A timeout records `captureTailStatus: unknown`, ends as interrupted,
and keeps the session incomplete; an arbitrary timer cannot certify the tail.
Concurrent stop calls share the same completion.

## Durability

Each 2-second chunk is SHA-256 hashed and written to an IndexedDB **outbox**
before any upload. The outbox has a 256 MiB cap. The uploader drains the outbox
in sequence order:

- **Transient failures** (network, 503 `review_session_busy`, 507
  `low_disk_space`, 5xx) back off from 0.5 s up to 10 s.
- **Permanent refusals** stop the drain and keep the local copy. These are:
  - conflict
  - corrupt
  - limit
  - not recording
  - not found
  - auth
- **Identical retries** are safe: the server returns the original receipt with
  `duplicate: true`.

If the outbox or the browser quota is full, that chunk's frames are reported as
a `localBufferOverflow` event and become a gap. They are never dropped silently.

After a reload, tab close or lost network, the TV reviews, cut and record pages show
**Finish upload**. Recovery uploads buffered chunks and durable events, then finalizes as
`interrupted`. It records an unknown-tail event because the restarted page
cannot know the worklet's last frame. The last stored prefix does not become
an acknowledged complete recording. Unflushed samples may be absent; their
length is unknown, and no silence or frame count is invented. Uploaded chunks
remain retrievable for later processing.

Manual cues, calibration and health events are written to an IndexedDB v2
event outbox before upload, with stable event IDs and retryable payloads (up to
10,000 events, 4 KiB per local event; server detail remains 2 KiB). Cues count
as saved only after local durable acceptance. Capture events (cues,
discontinuities, clock samples and device loss) use the same outbox, are posted
ahead of audio, retried with the same backoff, and deleted only after the server
acknowledges them. An event that could not even be stored locally is counted on
screen as unsaved, retained in recovery metadata and blocks false completion.
The recorder shows how many events still await the server. On the server, a
replayed `eventId` must carry the identical payload (kind, frame,
`performanceMs`, declared movie time and detail); any difference is
`idempotency_conflict`, which stops the drain rather than being accepted
silently. A session is never finalized while audio or an event for it is still
buffered in the browser.

## Server owner and API

The server owner is [`review_sessions.rs`](../core/rcdvlog-server/src/review_sessions.rs)
(control catalog migration 14). The adapter is
[`review_session_http.rs`](../apps/server/src/review_session_http.rs). It uses
the same session/device auth, CSRF on mutations, and Host/Origin checks as the
rest of the server.

| Route | Notes |
| --- | --- |
| `POST …/review-sessions` | Create. Idempotent by `clientSessionKey`. Requires consent, `pcm_s16le`, 1–2 channels, a supported rate, and a target whose readiness is `ready`; otherwise its blocker code is returned with 409. At most 4 sessions record at once (503 `review_session_busy`). |
| `PUT …/{sessionId}/chunks/{sequence}?startFrame&frameCount&sha256&captureEndMs` | Raw body of at most 2 MiB. The server checks the hash, checks that length equals frames × channels × 2, and refuses overlap or a different body for an existing sequence (409). It writes the file with fsync and a temp-file rename, then commits the row. A 4-permit concurrency gate returns 503 with `Retry-After`. Session total is capped at 2 GiB / 7 h. |
| `POST …/events` | `manualSyncCue`, `captureDiscontinuity`, `localBufferOverflow`, `deviceLost`, `pageInterrupted` or `clockSample`. Idempotent by `eventId`; a replay with a different payload is `idempotency_conflict`. The `detail` object is at most 2 KiB. |
| `POST …/finalize` | Takes `finalFrameCount` and an `endReason`. The server re-hashes every stored chunk and computes received ranges, missing sequences and gaps over `[0, finalFrameCount)`. Identical repeats are idempotent. |
| `GET …/{sessionId}`, `GET …/review-sessions` | Detail and bounded list. A recording past its 12 h lifetime is marked `abandoned` and can still be finalized. |
| `GET …/audio.wav` | Only for `completed` sessions. Each chunk is re-verified and the result is streamed. Incomplete audio is never padded (409 `review_session_audio_unavailable`). |
| `GET …/chunks/{sequence}` | Exact stored bytes, re-verified, for any non-deleted session. |
| `DELETE …/{sessionId}` | Removes the audio directory. The metadata, events and chunk ledger remain as a tombstone. |

States are:

- `recording`
- `completed`: contiguous coverage of the whole declared length
- `incomplete`: listed gaps or an explicitly unknown capture tail
- `corrupt`: a stored file is missing or its hash changed
- `deleted`

"Pending upload" exists only in the browser, as outbox chunks.

**Privacy and retention.** Audio lives under `<data dir>/review-evidence/<sessionId>/`.
That is never the derived/media root, and the path is never returned or logged.
Error logs carry only the stable code. Evidence is kept until deleted.
**Raw audio is not part of server backups** (the control-catalog metadata is),
which the session page states next to the **Download WAV** action. The
microphone is enabled by `Permissions-Policy: microphone=(self)`; camera and
geolocation stay disabled. Session evidence is separate from source-asset
transcripts and from canonical edit-plan revisions. The rendered movie is
never touched.

## Tests

- **Rust core** (`review_sessions_tests.rs`):
  - complete recording with out-of-order and duplicate chunks, cue and WAV bytes
  - gaps with a reported local overflow
  - conflicting, overlapping, corrupt and oversized chunks
  - a stored chunk modified after upload (marked corrupt)
  - consent and readiness and the concurrency limit
  - full event-payload idempotency, frame limits and unknown-tail evidence
  - deletion tombstone
  - abandoned-but-finalizable sessions
- **HTTP**: auth, CSRF, the Permissions-Policy header, duplicate and corrupt
  chunks, WAV streaming, 409 after deletion, and audit events.
- **Vitest**:
  - chunker rebasing, discontinuity gaps and stereo interleave
  - SHA-256 vector
  - outbox bound
  - uploader backoff and permanent stop
  - recovery finalization and the refusal to finalize while audio is buffered
  - microphone permission on load, refusal and retry, cue keyboard shortcut,
    queued stop
  - incomplete session view and deletion
- **Playwright with headless Chromium** against a real disposable server, using
  Chrome's `--use-fake-device-for-media-stream` microphone:
  - record, cue, stop, the completed session, a WAV of the expected length,
    reopen, delete, and a 409 afterwards
  - reload mid-recording, recover, and an `interrupted` finalization

## Manual real-device acceptance (not run)

Run against the HTTPS deployment in desktop Chrome, with a real microphone and
a real TV:

1. Refuse the microphone permission, then allow it from site settings and retry.
2. Record 10+ minutes while the TV plays. Use the manual cue on the first frame,
   and let the tab go to the background for part of the time.
3. Unplug a USB microphone mid-recording. Expect the session to end as
   `deviceLost`, with the audio captured so far kept.
4. Disable the network for 30 s mid-recording. Expect buffered chunks to upload
   after reconnection.
5. Close the tab mid-recording, reopen it, and use **Finish upload**.
6. Fill the server disk to `RCDVLOG_MIN_FREE_BYTES`. Expect 507 retries with
   the audio kept in the browser.
7. Download the WAV and confirm it plays. Delete the session, and confirm that
   the audio directory is gone and the tombstone remains.

Record the browser version, device, sample rate the context actually used,
applied processing flags, and any gaps. None of this was exercised in the
cloud environment.

# TV voice review transcription (#224)

This page covers transcribing a finalized [voice review recording](tv-review-recording.md)
into **timed utterances on the recording clock**. For provisional text during capture
and automatic refinement, see [Live voice review transcription](tv-review-recording.md#live-voice-review-transcription). The results are stored in
their own tables in the companion server's control catalog. They are never
written into source-asset transcripts, project annotations, edit plans or
canonical feedback. Movie time comes later, from the shared synchronizer (#223);
editing and export happen in [editorial discussions](tv-review-discussions.md).

| | |
| --- | --- |
| Owner | [`review_transcripts.rs`](../core/rcdvlog-server/src/review_transcripts.rs) (control migration 15) |
| HTTP adapter | [`review_transcript_http.rs`](../apps/server/src/review_transcript_http.rs) |
| Shared pieces | The narrow [`rcdvlog_core::review_speech`](../core/rcdvlog-core/src/lib.rs) facade: the `SttEngine` seam, Silero VAD, 30 s window packing and coverage re-split, quality gates, `WhisperEngine`, and the strict native OpenRouter transport |
| Job | `review_session.transcribe.v1` on the existing durable job engine |
| DTO | `review-transcript.v1` |

## Backends and model selection

| Backend | What runs | Requirements |
| --- | --- | --- |
| `local` | whisper.cpp large-v3-turbo q5_0 with local Silero VAD | Installed models in `RCDVLOG_SPEECH_MODEL_DIR`. Runs never download models and use the shared local-heavy lease. |
| `openrouter` | Local Silero VAD, then the selected native transcription model | Installed VAD model, an OpenRouter credential and a model in the live native-transcription catalog. |

`GET …/projects/{projectId}/review-transcription-models` reads OpenRouter's
public models API, filtered to audio input, transcription output and the native
`/audio/transcriptions` endpoint. The server caches metadata for five minutes.
Models documented as incompatible with `verbose_json` are excluded. Provider
format differences may still produce an error, which is displayed to the user.
There is no provider or model fallback.

Choosing OpenRouter authorizes sending the recording. Standard provider
retention is used and data collection remains `deny`. There are no upload/ZDR
checkboxes, spending ceilings, admission reservations or budget checks in the
review workflow. Legacy request fields remain accepted for saved-run
compatibility but do not control cloud execution. This does not change the
separate analysis workflow's cost-bound registry or accounting policy.

Preflight checks finalized audio, installed models, provider credentials and
model eligibility. It does not send audio or load a speech model. Its duration
and segment counts describe the recording; costs are reported after execution.

## Processing

1. **Audio identity and attempts.** SHA-256 covers the recording format and
   verified chunk identities. Local retries resume cached work. Every explicit
   cloud retry creates a fresh immutable run, including after an unknown
   response or a completed run. Concurrent submissions reuse matching active
   work. Earlier attempts and their reported/unknown costs remain unchanged.
2. **Segments.** Read only verified chunk files in contiguous received ranges,
   in segments of at most ten minutes. Recording gaps are never bridged.
   Downmix to mono and resample to 16 kHz with a Hann-windowed sinc low-pass.
3. **Speech detection.** Local VAD packs only speech into at-most-30-second
   windows. Coverage re-splitting handles missing language regions.
4. **Window persistence.** Successful decoded windows are cached by run,
   segment, window and fingerprint. Worker crash recovery reuses cached work.
   Durable dispatch markers prevent duplicate background sends within a run;
   they do not prevent an explicit retry from creating a new run.
5. **Provider results and cost.** Preserve reported usage/cost, generation ID,
   returned provider/model and latency independently of transcript usability.
   Missing cost stays unknown and never blocks execution. The UI reports
   known cost and the number of requests with unreported cost after execution.
6. **Errors.** Keep HTTP status, bounded credential-redacted provider error
   messages/codes and safe upstream details. Response validation errors include
   input duration and provider duration/segment-end measurements. No raw
   response body or audio is placed in errors. Strict timestamp bounds remain.
7. **Cancellation and states.** Cancellation is checked between windows and
   segments; completed evidence is kept. Terminal states are `completed`,
   `noSpeech`, `partial`, `failed` and `cancelled`.

## Transcript DTO (`review-transcript.v1`)

Each utterance carries:

- **Identity:** `utteranceId`, `segmentIndex`, `windowIndex`.
- **Time:** `startFrame`/`endFrame` and `startSeconds`/`endSeconds` on the
  recording clock.
- **Text and language:**
  - `text`: raw as decoded, never translated or converted.
  - `language`: the model-reported tag.
  - `script`: `latin`, `hanTraditional`, `hanSimplified`, `hanAmbiguous`,
    `mixed` or `other`.
  - `displayText` and `displayProjection`: a Traditional Chinese (Taiwan)
    character projection by [ferrous-opencc](https://crates.io/crates/ferrous-opencc)
    `s2tw` (Apache-2.0, pure Rust). There is no phrase or vocabulary
    substitution, and `text` is unchanged. `zhconv` was rejected because it
    is GPL-licensed.
- **Speaker:** `speaker: "unknown"` with
  `speakerEvidence: "diarizationUnavailable"`.
- **Confidence:** `meanTokenProbability` comes from the local decoder only.
  The cloud provides none and is flagged `probabilityEvidenceUnavailable`.
- **Flags:**
  - `qualityGate:repetition|low_token_probability|invalid_probability_evidence`
  - `lowConfidence`
  - `unexpectedLanguage` (anything other than en or zh)
  - `languageUnknown`
  - `nearRecordingGap`
- **Suppression:** `suppressed` is true when a window's quality gate fired.
  The text is kept as evidence and the UI shows it dimmed. The whole
  conversation is not discarded for one bad window.

Each run also carries its segments (with per-segment failures), the recording
gaps, reported cost and unknown-cost request counts (legacy budget/reservation fields remain in the DTO for historical runs),
`diarization: { status: "unavailable" }` and fixed limitations.

## Speaker diarization

**Not implemented, and deliberately not faked.** Whisper output is not speaker
evidence. Every utterance is `unknown` until either:

- a diarization backend is chosen and evaluated (for example pyannote via
  ONNX on the server, or a cloud diarization model), or
- a human corrects it. That is part of #225's correction workflow.

Choosing a diarizer is an open decision because it brings model, runtime,
cost and privacy trade-offs. Overlap, TV dialogue and background speakers
must stay `unknown` or flagged under any backend.

## Web

The Transcript panel provides backend selection, a live OpenRouter model
dropdown, readiness checking, Transcribe and Cancel. Failed attempts display
provider HTTP status, code, message and request ID when returned. Costs appear
after execution as informational reported totals; there is no spending-limit
input. History retains previous results and explicit retries show a new attempt.

## Tests

Rust tests cover model discovery filtering, shared wire bounds, credential
redaction, absence of budget/checkbox gates, alternate model execution,
in-flight deduplication, fresh explicit retries, retained provider errors and
costs, crash recovery, cancellation, gap handling and local transcript quality
projections. HTTP tests retain authentication and CSRF checks. Vitest covers
model selection, request shape, error display and informational costs. Chrome
journeys use disposable catalogs and fake microphones; no provider upload is
needed for those tests.

## Not verified

- Real Whisper or OpenRouter decoding of English/Mandarin code-switching
  conversation.
- Per-model WER or CER.
- Any diarization accuracy.
- Film dialogue behind reviewers, quiet passages, or real room audio.
- Concurrent memory use next to proxy work on the server.

The public two-speaker code-switching fixture the issue asks for is not in the
repository. It needs a licensed recording, and should be chosen together with
the diarization decision. Real-audio evaluation belongs to the local
acceptance stage.


## Historical failures

Older failures that discarded their provider response/receipt cannot be
reconstructed. They remain visible in history. A new explicit cloud attempt
can now execute without being blocked by an earlier unknown outcome or cost.


## Provider response compatibility

Recognized returned language codes and full names are matched without case
sensitivity. The shared native transcript preserves the exact provider value;
review utterances use canonical codes (`English` → `en`). Unknown,
missing or malformed language values are still rejected.

For review requests using the three Whisper models, the sent derivative WAV
is rounded up to Whisper's 20 ms timestamp grid by appending at most 19 ms of
zero-valued samples. This handles a 14.150 s input whose last timestamp token
ends at 14.160 s. Response validation still uses the exact duration of the
actual sent WAV; no provider timestamps or source recording samples are
rewritten. Recording-clock projection uses the original speech-window map,
and padding-only spans are omitted. Successful window caches retain
`input_padding_ms` alongside the original decoded endpoints. Registered
source-analysis requests and other review models keep their original WAVs.

The explicit `review_stt_repro` Rust example accepts a PCM16 WAV, model ID and
new output directory. It uses the same resampler, VAD, packing and audio
preparation as review capture, sends one native request and saves private
response/audio files plus a body-free receipt in an owner-only directory:

```sh
RCDVLOG_SPEECH_MODEL_DIR=/absolute/model/directory \
  cargo run -p rcdvlog-server-core --example review_stt_repro -- \
  INPUT.wav openai/whisper-large-v3-turbo NEW_OUTPUT_DIRECTORY
```

It does not mutate a project catalog or retry the provider request. The checked-in
regression fixtures preserve the reproduced provider's shape and numeric times,
with private text and identifiers removed.

# Live voice review transcription

Play + record review combines the embedded video, microphone controls and a
read-only live transcript. The regular Record review page can also show live
words, with video matches only where its synchronization evidence supports them.
The **Live transcription**
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
The transcript displays shot thumbnails without timing captions. For a final
line with an unknown start, the first safe `movieTimeRanges` span can supply a
thumbnail linked to that observed video position; its hover hint identifies it
as later observed playback rather than the line's beginning. The start locator
and full timing diagnostics remain unchanged in the workspace data.

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
