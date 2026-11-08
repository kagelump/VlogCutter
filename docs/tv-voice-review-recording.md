# TV voice review recording (#222)

Chrome records a reviewer's conversation while they watch a rough cut on a TV,
then stores it as **private session evidence** on the companion server. The
recording is bound to one imported, immutable [review target](tv-review-handoff.md)
(#221). This feature does not transcribe (#224), synchronize (#223), or write
feedback (#225). Those owners read this evidence later; see
[TV review transcription](tv-review-transcription.md).

The page never plays, controls or observes YouTube. The TV plays on its own.
The only statement the page makes about the video is the reviewer's optional
**manual cue** ("the first frame is showing now"), which is recorded as a
declaration and not as measured sync.

## User flow

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
  - consent and permission gating, refusal and retry, cue keyboard shortcut,
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
