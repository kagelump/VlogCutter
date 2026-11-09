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
