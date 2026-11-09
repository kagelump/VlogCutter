# TV voice review recording and transcription

Chrome records a reviewer's conversation while they watch a rough cut on a TV and stores it as **private session evidence** on the companion server, bound to one imported, immutable [review target](tv-review.md). Finalized recordings are then transcribed into timed utterances. Neither stage synchronizes to the movie or writes feedback; see [timing](tv-review-timing.md) and [discussions](tv-review-discussions.md).

## Recording

Chrome records a reviewer's conversation while they watch a rough cut on a TV,
then stores it as **private session evidence** on the companion server. The
recording is bound to one imported, immutable [review target](tv-review.md). Transcription, timing and feedback read this evidence later; see [Transcription](#transcription), [timing](tv-review-timing.md) and [discussions](tv-review-discussions.md).

In **Record review** the TV plays on its own. In **Play + record review**, the page embeds the target’s declared YouTube video and records playback observations alongside the microphone.
The only statement the page makes about the video is the reviewer's optional
**manual cue** ("the first frame is showing now"), which is recorded as a
declaration and not as measured sync.

### Play + record review

For a target with a YouTube ID, choose **Play + record review**, allow the microphone, acknowledge consent, and press Record. Press play in the embedded player after recording starts. Player position, state and playback rate are sampled every five seconds and on player state/rate changes; each observation retains its browser monotonic time, a contemporaneous microphone-clock pairing and clock epoch. Observations are durable session events and can be lost if local event storage fails (the recorder reports unsaved events). Pauses, buffering and seeks can make the mapping discontinuous. The observations are approximate and do not measure TV output latency or verify that the YouTube video matches the target. The existing synchronization pipeline does not yet consume these observations for automatic comment placement. An independent TV player is not observed.

### User flow

*Projects → TV reviews → cut → Record review*:

1. Open the target's YouTube video on the TV.
2. Press **Allow microphone**. The browser asks for permission and nothing is
   captured yet. Then pick the input device.
3. Confirm that everyone present knows they are being recorded (required). If a
   second speaker will be recorded, tick the separate opt-in disclosure.
4. Press **Record**. The recorder shows `Recording <elapsed>`. The level meter
   and upload counters update while you record.
5. Optionally press **Mark start** (or `M`) when the first frame appears.
6. Press **Stop**. The page flushes and uploads the tail and finalizes the
   session, then opens the session page.

Recording keeps running while the tab is in the background, as long as Chrome
keeps the AudioContext alive. The target page lists every session for that
exact target.

### Capture and clock

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
sample zero from request-start or packet arrival. TV alignment is the [synchronizer's](tv-review-timing.md)
job and has its own uncertainty.

Stop waits for a same-port worklet acknowledgment containing its final frame,
with all tail audio delivered first. It then drains storage and upload before
finalizing. A timeout records `captureTailStatus: unknown`, ends as interrupted,
and keeps the session incomplete; an arbitrary timer cannot certify the tail.
Concurrent stop calls share the same completion.

### Durability

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

### Server owner and API

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


## Transcription

A finalized [recording](#recording) is transcribed into **timed utterances on the
recording clock**. For provisional text during capture and automatic refinement,
see [Live transcription](#live-transcription). The results are stored in
their own tables in the companion server's control catalog. They are never
written into source-asset transcripts, project annotations, edit plans or
canonical feedback. Movie time comes later, from the shared synchronizer;
editing and export happen in [editorial discussions](tv-review-discussions.md).

| | |
| --- | --- |
| Owner | [`review_transcripts.rs`](../core/rcdvlog-server/src/review_transcripts.rs) (control migration 15) |
| HTTP adapter | [`review_transcript_http.rs`](../apps/server/src/review_transcript_http.rs) |
| Shared pieces | The narrow [`rcdvlog_core::review_speech`](../core/rcdvlog-core/src/lib.rs) facade: the `SttEngine` seam, Silero VAD, 30 s window packing and coverage re-split, quality gates, `WhisperEngine`, and the strict native OpenRouter transport |
| Job | `review_session.transcribe.v1` on the existing durable job engine |
| DTO | `review-transcript.v1` |

### Backends and authorization

| Backend | What runs | Requirements |
| --- | --- | --- |
| `local` | whisper.cpp large-v3-turbo q5_0 with Silero VAD, on this server | Both installed model files in `RCDVLOG_SPEECH_MODEL_DIR`, or the shared application model directory otherwise. **Models are never downloaded by a run.** The job takes the shared local-heavy filesystem lease, so it never overlaps proxy or hash work on the host. |
| `openrouter` | Local Silero VAD, then one native `openai/whisper-1` request per speech window | All of: `cloudUploadAcknowledged: true`, an `authorizedMaxCostUsd` ceiling, an OpenRouter credential on the server, and a model registered in `NATIVE_STT_MODELS`. |

Cloud runs follow these rules:

- Provider data collection is always `deny`. Zero data retention is the
  default in the UI.
- There is **no provider or model fallback**. An unregistered model is
  refused with 400.

Preflight (`POST …/review-sessions/{sessionId}/transcriptions/preflight`) never
sends audio, loads a model or reads audio. It reports:

- **blockers**:
  - `review_transcription_session_not_finalized`
  - `review_transcription_no_audio`
  - `review_transcription_local_model_unavailable`
  - `review_transcription_cloud_not_authorized`
  - `review_transcription_budget_required`
  - `review_transcription_provider_unavailable`
- **warnings**:
  - `diarization_unavailable`
  - `tv_dialogue_not_separated`
  - `recording_has_gaps`
  - `budget_below_worst_case`
- **an estimate**: received audio, number of segments, a worst-case window
  count, and the worst-case cost. Each 30 s window is reserved at its full
  registered price, which is $0.003 for `openai/whisper-1`. The provider
  endpoint has no wire-level price cap, which is why the server reserves.

### Processing

1. **Audio identity.** The audio fingerprint is SHA-256 over the format plus
   each chunk's sequence, frames and hash. A run is idempotent by
   `(session, audio fingerprint, backend/provider/model/hint/ZDR, recipe)`:
   - An identical request reuses its run.
   - A completed run is returned as is.
   - A failed, partial or cancelled run is re-queued with `attemptCount + 1`.
2. **Segments.** Audio is read only from verified chunk files, inside
   contiguous received ranges, in segments of at most 10 minutes. **Recording
   gaps are never bridged.** Each segment is downmixed to mono and resampled
   to 16 kHz with a Hann-windowed sinc low-pass. Unit tests check that
   in-band tone survives and 15 kHz does not alias.
3. **Speech detection.** The VAD gate runs first, and only speech is packed
   into 30 s windows. If a mixed-language window loses a language's coverage,
   its regions are decoded one by one. This is the same re-split rule the clip
   pipeline uses.
4. **Window cache.** Every decoded window is stored, keyed by `(run, segment,
   window, window fingerprint)`. Retries and crash recovery reuse cached
   windows, so **billable work is never repeated**.
5. **Cloud ledger.** For each cloud window, one immediate transaction:
   - checks committed spend plus the window's full reservation against the
     ceiling
   - inserts a `dispatched` marker

   Only then is the request sent, and the response settles the window with
   the provider-reported cost and provenance (generation id, actual provider
   and actual model). If a response is lost or a process crashes after the
   marker:
   - the window becomes `unknown`
   - it keeps counting at its reservation
   - it is **never resent**
   - its segment is failed with `review_transcription_dispatch_outcome_unknown`

   Unsent reservations are released on recovery.
6. **Budget exhaustion.** When the budget runs out, dispatch stops and the run
   ends `partial` with `review_transcription_budget_exhausted`. A retry with a
   new explicit ceiling resumes from the cached windows.
7. **Cancellation.** It is checked between windows and segments, through
   both the run flag and the job cancellation token. Completed work is kept.
8. **Run states.** A run ends in one of:
   - `completed`
   - `noSpeech`: VAD found nothing and no model was called
   - `partial`
   - `failed`
   - `cancelled`

### Transcript DTO (`review-transcript.v1`)

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
gaps, spend (authorized, reserved, known cost, unknown-cost windows),
`diarization: { status: "unavailable" }` and fixed limitations.

### Speaker diarization

**Not implemented, and deliberately not faked.** Whisper output is not speaker
evidence. Every utterance is `unknown` until either:

- a diarization backend is chosen and evaluated (for example pyannote via
  ONNX on the server, or a cloud diarization model), or
- a human corrects it. That is part of the correction workflow.

Choosing a diarizer is an open decision because it brings model, runtime,
cost and privacy trade-offs. Overlap, TV dialogue and background speakers
must stay `unknown` or flagged under any backend.

### Web

The session page has a **Transcript** panel. It offers:

- the backend choice
- for cloud: a ceiling, an explicit upload checkbox, and a ZDR checkbox
- **Check**, showing blockers, warnings and the worst-case quote
- **Transcribe**, **Resume / reuse** and **Cancel**, with polling while
  queued or running
- the utterance list, with Traditional display, raw text and a raw toggle,
  `Speaker ?`, flags, quality-gated rows and per-segment failure lines


## Live transcription

Play + record review combines the embedded video, microphone controls and a
read-only live transcript. The regular Record review page can also show live
words, with video matches only where its synchronization evidence supports them.
The **Show live transcription and refine it automatically after recording**
checkbox defaults on. Turning it off keeps the explicit post-recording workflow.

### Model choice and setup

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

### Processing and persistence

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

### Movie matching

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

### Final pass and recovery

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

