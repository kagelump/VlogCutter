# TV voice review transcription (#224)

This page covers transcribing a finalized [voice review recording](tv-voice-review-recording.md)
into **timed utterances on the recording clock**. For provisional text during capture
and automatic refinement, see [Live voice review transcription](tv-review-live-transcription.md). The results are stored in
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

## Backends and authorization

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

## Processing

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
gaps, spend (authorized, reserved, known cost, unknown-cost windows),
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

The session page has a **Transcript** panel. It offers:

- the backend choice
- for cloud: a ceiling, an explicit upload checkbox, and a ZDR checkbox
- **Check**, showing blockers, warnings and the worst-case quote
- **Transcribe**, **Resume / reuse** and **Cancel**, with polling while
  queued or running
- the utterance list, with Traditional display, raw text and a raw toggle,
  `Speaker ?`, flags, quality-gated rows and per-segment failure lines

## Tests

- **Rust** (`review_transcripts_tests.rs`). These use a model-free energy VAD
  and scripted engine, and a fake native transport.
  - local happy path: recording-clock starts within 0.15 s, the Simplified to
    Traditional projection, mixed script, unknown speakers, idempotent reuse
  - gaps never transcribed
  - each preflight blocker; unregistered model and cloud fields on a local
    request are refused
  - ceiling enforcement: one window admitted, and a ceiling below one window
    never dispatches
  - a lost response is unknown spend and is never resent on retry
  - crash recovery reuses cached windows (no second call)
  - cancellation
  - resampler band-limiting
  - projection never rewriting raw text
- **HTTP**: CSRF, preflight blockers and quote, 409 on an unauthorized start,
  400 on an unregistered model.
- **Vitest**: the consent, ceiling and quote flow; transcript rendering; the
  unfinalized state.
- **Playwright** on a real server with no models and no key: the honest
  "models not installed" and "no OpenRouter credential" states are shown and
  Transcribe is never offered.

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
