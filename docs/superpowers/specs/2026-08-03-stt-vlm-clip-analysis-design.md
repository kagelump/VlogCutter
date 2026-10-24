# Design: STT and VLM clip analysis

Date: 2026-08-03
Status: approved, pending implementation plan

Inputs: the three spike reports under `docs/spikes/`
([STT round 1](../../spikes/2026-08-03-stt-analysis-spikes.md),
[VLM round 2](../../spikes/2026-08-03-vlm-analysis-spikes.md),
[VLM model eval round 2b](../../spikes/2026-08-03-vlm-model-eval.md)).
Every measurement cited below comes from those reports; nothing here is new
measurement.

## 1. Purpose and priorities

Analysis exists to serve two jobs, in this order:

1. **Rough-cut assembly input.** Per-clip signals plus a proposed best-moment
   range, so the edit-planning stage starts from a sorted, filterable,
   pre-trimmed candidate list instead of 273 opaque filenames.
2. **Better thumbnails.** A VLM-chosen representative frame, committed at the
   thumbnail quality `10` slot the PRD already reserves.

Explicitly **not** goals for this version: search/findability, faster manual
review as an end in itself, semantic retrieval.

Model selection priority is **cost first, speed second, quality last**, where
the quality bar is "not broken, and it mostly describes the vibe."

## 2. Shape

Two stages, each independently runnable, both producing derived artifacts with
recipe identity in the established proxy/thumbnail style.

```
transcribe (local, offline, no API key)   →   analyze (cloud, needs API key)
  VAD gate → window pack → decode → gate      framepack(proxy) + transcript → VLM → validate
```

STT runs first as its own project-wide batch. VLM reads whatever transcript is
already stored; a clip with no stored transcript still analyzes, it just gets no
speech context.

Domain logic lives in `rcdvlog-core` under a new `analysis/` module. FFmpeg runs
as a managed child process exactly like the proxy and thumbnail paths.
`whisper-rs` is an ordinary Rust crate. No Python, no ONNX runtime, no external
application, no competing datastore — consistent with the AGENTS.md invariants.

## 3. STT pipeline

Local, CPU-only, `whisper-rs` 0.16 vendoring whisper.cpp. Models downloaded on
demand with checksum verification: `ggml-large-v3-turbo-q5_0.bin` (574 MB) and
`ggml-silero-v5.1.2.bin` (885 KB).

1. **Extract audio** — 16 kHz mono WAV from the *proxy* audio via a managed
   FFmpeg child process.
2. **VAD gate** — whisper.cpp's built-in Silero VAD (round 1 Finding 11: RTF
   0.0019, deterministic across repeat runs). A clip with zero detected speech
   is stored as `no_speech` and is **never sent to any model**. This is a
   correctness gate, not a cost optimization: round 1 Finding 2 showed both
   local and cloud engines invent fluent speech from digital silence at
   0.92–0.98 reported confidence. It also skips 64% of clips on the reference
   corpus.
3. **Window packing** — concatenate the clip's speech regions, dropping silence,
   and pack into as few 30-second windows as possible. Record the mapping from
   window offsets back to true clip timestamps. Not the PRD's 8–25 s chunks:
   round 1 Finding 3 measured that recipe at 30% *slower* while processing 39%
   less audio, because whisper bills a full 30 s window per chunk regardless of
   chunk length.
4. **Decode** with whisper's internal auto-detect. No separate `lang_detect()`
   pass — round 1 Finding 13 measured it at +34% time and +20% peak RSS for
   *identical* language decisions on 33/33 chunks.
5. **Timestamp translation** — every returned segment timestamp is translated
   back through the region map before storage. This is the highest-risk piece of
   arithmetic in the stage and gets dedicated tests.
6. **Coverage check** — compare transcribed segment duration against the speech
   duration fed into the window. Round 1 Finding 9 showed a window spanning a
   language switch silently drops an entire language's speech in 3 of 7 cases,
   with no error and no low-confidence signal. On a large shortfall, re-split
   that window at its internal silence boundaries (which are the likely switch
   points) and keep the split result. This pays an extra window only for the
   minority of clips that actually code-switch, resolving the tension between
   Finding 3 (pack) and Finding 9 (don't pack).
7. **Reject gate** — `repetition_ratio >= 0.3 OR mean_token_probability < 0.20`
   (round 1 Finding 12: caught 4/4 bad chunks with 0/29 false rejections).
   `no_speech_probability()` is **not** used: measured median 0.000 for good and
   bad alike, i.e. zero separation. Language confidence is **not** used as a
   rejection gate: it predicts quality well (Finding 4) but rejecting below 0.9
   discards 15/29 good chunks. A rejected transcript is stored with its reason
   and is **not** fed to the VLM.

Thresholds in step 7 rest on 4 positive examples (Finding 12's stated caveat).
They are configuration constants with their provenance recorded, not tuned
values.

**Recipe key:** `stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1`

### Native cloud STT protocol boundary (slice A, not enabled)

The optional cloud-speech adapter is split from the local offline workflow and
from the production workflow. `analysis/openrouter_stt/` owns a typed native
transcription leaf; it is not connected to the catalog, CLI, preparation or
desktop selection yet. `SpeechBackendSelection` requires an explicit
`LocalWhisper` or provider-qualified `NativeOpenRouter { provider, model }`
choice. The only verified cloud entry is `openrouter` +
`openai/whisper-1`, checked against the current public transcription-model and
endpoint snapshots. An arbitrary chat slug, an unqualified model id, a silent
default and a fallback model are all rejected.

Silero detection now lives in `analysis/silero_vad.rs`. It loads the VAD model
without constructing a whisper ASR context, so a future cloud path can run the
same local gate without loading the 574 MB local model. The request planner
concatenates only the VAD-selected, packed speech regions into genuine 16 kHz
mono signed 16-bit PCM WAV windows, capped at 30 seconds and a bounded byte
size. An empty VAD result produces no audio windows and therefore no provider
request; an audio-less clip remains the existing `no_audio` outcome before this
boundary is reached.

The cloud wire contract is the native
`POST /api/v1/audio/transcriptions` endpoint, not chat completions. It sends raw
base64 WAV in `input_audio.data` with `format: "wav"`,
`response_format: "verbose_json"` and `timestamp_granularities: ["segment"]`.
The `provider` object accepts only the supported data-policy fields `zdr` and
`data_collection`; routing restrictions such as `order`, `only`, `ignore` and
`max_price` fail locally because they are not applied by the STT endpoint and
must not be presented as enforceable. Temperature is explicit and bounded to
`0..=1`; language is an optional explicit ISO 639-1 code.

Response validation is strict and window-relative: text, language, duration and
nonempty segments are required; segment timestamps must be finite, nonnegative,
ordered and inside the window. Optional confidence, average log-probability and
word confidence remain optional measured evidence (missing never becomes 0 or
1). Provider/header provenance is retained only when actually returned, and the
`X-Generation-Id` header is accepted only when it is a bounded valid id. A
malformed or unusable body, including a non-2xx response, returns a typed unusable outcome while keeping
the receipt, known cost and latency for future canonical settlement; unknown
cost/tokens stay separately unknown. Response bodies, audio bytes, transcripts
and credentials are never embedded in error text.

**B2 proposal-only policy (issue #119).** The existing shared `RunPolicy` owner
now has an explicit `native_speech_transcription` kind and native parameter
variant, while this protocol leaf remains the only source of the registered
endpoint/data-policy/window/rate contract. A proposal uses the common
provider/model/allowlist/budget/exact-asset request fields; the native parameters
carry only optional ISO 639-1 language, `zdr`/`data_collection` and `force`. It
must canonicalize to the audio-only payload, bind the derived-proxy source
identity and native cost-bound contract, and estimate cost from the registered
full-window duration tariff with a conservative pre-VAD packed-window upper
bound. No transcript, vocabulary, frames or chat prompt is bound. Because the
shared accounted native runtime is not implemented, the proposal always reports
`native_accounted_runtime_unavailable`, remains unready, and is refused by
authorization. Existing VLM pilot/provider/recovery entry points explicitly
reject the new kind before credential resolution, local preparation or a
provider call. CLI/desktop cloud execution and native accounting remain future
wiring; local offline transcription is unchanged.

This slice deliberately performs no catalog writes, schema changes, budget
reservation, payload/scope authorization, transcript persistence, automatic
retry, fallback or provider invoice query. Those remain the next accounting and
orchestration slices; the existing repetition/low-token-probability rejection
gate stays unchanged and is not duplicated here. Credentials reuse the existing
`OPENROUTER_API_KEY` / `~/.openrouter.key` convention only when the explicit
cloud client is constructed. Redirects are disabled: one invocation cannot resend audio to a redirected endpoint. The test endpoint override is unavailable in production builds.

**Batch estimation, two-phase.** VAD sweeps the whole project first — at RTF
0.0019 this is effectively free — which yields exact 30-second window counts.
Transcription then runs with real weights. Weight is **30 s windows**, not
source bytes, because round 1 Finding 3 shows cost tracks windows and is
essentially uncorrelated with chunk length (r = 0.29) or transcript length
(r = -0.05). One worker by default: peak RSS is 1.15–1.38 GB and concurrent STT
was never measured alongside the four-worker VideoToolbox proxy pipeline.

**Language scope:** English, Japanese, Mandarin. Cantonese is unsupported — round
1 Finding 8 showed whisper silently rewrites Cantonese into Mandarin phrasing
and Traditional characters at 0.99 confidence. The PRD's language list should be
corrected to match.

**Accepted limitation:** CPU-only. Metal is blocked on macOS 14 because ggml
gates `MTLResidencySetDescriptor` on the SDK version rather than the deployment
target (round 1 Finding 5). The documented OS floor is below macOS 15, so the
CPU path must keep working regardless.

## 4. VLM pipeline

### Frame-pack

Recipe
`framepack.2fps.skip400ms.stride-max40.win3-8.sharp-bright-motion.dhash8-d5.v2`,
frames encoded as **768 px JPEG**.

The 40-candidate budget is **spread evenly across the whole clip**, not taken
from its head. FFmpeg decodes the clip in full either way, so a head-truncated
budget bought nothing and made every clip longer than 20 s describe only its
opening — title, labels, best moment and representative thumbnail all drawn from
the first twenty seconds of a multi-minute clip. `v1` truncated; `v2` strides.

Frame-packs are built from the **proxy only**. Round 2 Finding 2 measured the
proxy at 0.4 s versus 17.6 s for the 4K original on NAS (44x), and — more
importantly — the same recipe produced *different* frame-packs from the two
representations on 3 of 4 clips, because sharpness and motion metrics are
resolution- and codec-dependent. "Proxy or original" is therefore not a safe
"or". No proxy means explicit failure, matching the existing proxy-input rule.

Manifest identity is **recipe key + representation kind/version + that proxy
file's fingerprint**, not the recipe key alone. A proxy regeneration must
invalidate stored frame-packs, exactly as the catalog's v2 → v3 proxy transition
would have.

Frame width stays at 768 px. Round 2b Finding 3 measured shrinking as a ≤1.4x
cost lever that costs a third to a half of OCR agreement — and both Google models
bill a flat token count regardless of input width, so it saves nothing there at
all.

### Prompt and schema

Prompt `vlm.observe.v2`. Inputs: the frame-pack, the stored transcript text if
any, the project's current consolidated label vocabulary, and an optional
project-level trip-context string.

Every array in the schema carries `maxItems`. This is load-bearing, not tidy:
round 2 Finding 6 showed a JSON grammar kept the *syntax* legal while the model
entered a repetition loop inside an unbounded array and truncated at the token
limit. Bounding makes the loop structurally inexpressible. It is also a provider
compatibility filter (round 2b Finding 8 disqualified a model for rejecting
`maxItems`).

| field | constraint |
| --- | --- |
| `title` | string, ≤60 chars, English |
| `description` | string, ≤400 chars, English |
| `labels` | array, `maxItems` 5, prefer the supplied vocabulary |
| `usable` | boolean |
| `unusableReason` | enum, nullable |
| `bestMomentStartMs`, `bestMomentEndMs` | must fall within clip bounds |
| `thumbnailTimestampMs` | must equal one of the supplied frame timestamps |
| `cameraMovement` | enum **including an explicit `other`** |
| `ocrText` | array, `maxItems` 8 |

`landmarkCandidates` is **dropped**. Round 2 saw the incumbent leave it empty on
all 12 clips; round 2b found that safety was a per-model accident and that
models which do populate it fabricate — `qwen3-vl-32b` proposed *Tokyo Tower,
Shibuya, Odaiba* for an Osaka market hall (Finding 10).

`cameraMovement` needs the `other` escape specifically because `qwen3.7-flash`
emitted an out-of-enum `panning` and has no strict structured-output support on
OpenRouter (round 2b Finding 8).

Titles and descriptions are returned in **English** regardless of the language of
speech or signage in the clip.

### Validation

Two layers, in order:

1. **Schema validation.**
2. **Domain validation** against closed vocabularies and bounds: enum membership,
   `thumbnailTimestampMs` ∈ the supplied frame timestamps, best-moment range ⊂
   clip bounds, label normalization.

Round 1 Finding 7 and round 2 Finding 5 are the same lesson in two modalities:
a JSON-schema-valid response is not a domain-valid response. 33/33 schema-valid
STT responses still returned a mix of `ja`, `en`, `zh-TW`, `zh-CN` and bare `zh`.

**Truncation is an explicit rejection reason**, never a partial success. A
rejected response writes nothing to the asset; the reason is recorded in the
provenance row.

### Provider chain

Default `qwen/qwen3.7-flash`; on any failure, retry once on
`google/gemini-2.5-flash-lite`. An in-memory, per-process counter flips the
default to `gemini-2.5-flash-lite` for the remainder of the process once
`qwen3.7-flash` has failed 10 times. Upstream 429s and domain violations both
count toward that total. The counter resets when the process restarts.

Provenance records the model that **actually produced** the stored result, so a
single batch will legitimately contain mixed provenance.

This is a deliberate departure from the round 2b recommendation, which says "do
not adopt `qwen3.7-flash`". The decision was taken with the measured tradeoff in
view:

| | $/273-clip project | p50 | p95 | 1st-try | controls |
| --- | --- | --- | --- | --- | --- |
| `qwen/qwen3.7-flash` | $0.03 | 12.3 s | 16.7 s | 75% | 2/3 |
| `google/gemini-2.5-flash-lite` | $0.08 | 3.4 s | 6.8 s | 100% | 3/3 |

The fallback plus circuit breaker is the mitigation for the 3-in-16 upstream
429 rate. `qwen3.7-flash`'s one control miss was an upstream 429, not a
confabulation (round 2b Finding 6).

### Cost model

Per-model **measured image-token ratio** is stored in the model registry, not
derived from published per-token prices. Round 2b Finding 2: identical images
bill 1,235 to 6,026 prompt tokens depending on model, so a cost model based on
frames × price-per-token is wrong by up to 5x.

Batch key `vlm.observe-v2.<model>-w<n>-v1` — per model, because latency differs
about 4x across candidates and old throughput is not comparable. Weight is
**frames**, and the real request concurrency is passed to the estimator per the
existing rule.

## 5. Ownership and re-analysis

| field | owner | mechanism |
| --- | --- | --- |
| `notes` | human only | untouched by analysis |
| `title` | AI-written, human-editable | new `assets` column, via `AssetPatch` |
| `description` | AI-written, human-editable | new `assets` column, via `AssetPatch` |
| `labels` | AI-written, human-editable | existing typed label operation |
| best-moment range | suggestion | stored on the analysis row, never auto-applied to an edit plan |
| thumbnail | derived artifact | committed at quality `10` |

`title` replaces the filename as the clip's display identity in the review queue,
the story ribbon, and exported OTIO clip names. The filename remains stored and
visible as the link back to source media.

`AssetPatch` is a full annotation replacement on both the Rust and TypeScript
sides; adding `title` and `description` requires changing both together, per the
AGENTS.md warning.

The typed label operation payload gains an **optional** `origin: human | ai`
field. Optional preserves sync compatibility with existing operations. Origin is
what lets consolidation and forced re-analysis remap AI-authored labels without
touching human-authored ones.

**An analyzed clip is never re-analyzed.** `analyze missing` skips it.
Re-analysis happens only when the analysis is deleted or `--force` is passed, and
force is latest-wins — including over human edits to `title`, `description`, and
AI-origin labels. Force reports the count of human-edited clips it will overwrite
before it runs.

The best-moment range is a suggestion that pre-fills the source range when a clip
is added to an edit plan. It never selects clips for inclusion and never creates
an edit-plan revision on its own — rough-cut selection remains the edit-planning
stage's job, per the AGENTS.md invariant.

## 6. Label vocabulary and consolidation

Round 2 Finding 5 measured 48 unique labels from 51 across 12 clips; round 2b
Finding 9 showed the rate is model-dependent (63%–97%) but never good enough to
face on. Free-form labels do not converge into a usable vocabulary on their own.

Two passes:

**Pass 1** writes labels immediately from the model's proposals, so a batch
delivers value before consolidation runs.

**Pass 2 is a remap, not a re-analysis** — one cheap text-only model call, no
vision, no frame-pack, no new frames. It therefore does not violate the
never-re-analyze rule. It takes the distinct proposed labels with their counts,
returns a canonical vocabulary of roughly 20–40 labels plus a mapping, stores
both, and emits a batch of typed remove/add operations scoped to `origin: ai`.
Human-authored labels are never remapped.

**Trigger:** automatic the first time a project crosses 25 analyzed clips; after
that, explicit via `labels consolidate`. Bootstrapping is a one-time problem —
once a vocabulary exists it is seeded into every subsequent prompt, so later
clips mostly land on it directly.

AI labels are normalized to a canonical lowercase form for matching; the
vocabulary carries the display form.

## 7. Surfaces

CLI first, per repo convention, with the standard single JSON success envelope on
stdout, machine-readable JSON errors on stderr, and JSON progress events for long
batches:

```
rcdvlog transcribe --project <id> [--asset <id>] [--force]
rcdvlog transcribe missing --project <id>
rcdvlog analyze --project <id> [--asset <id>] [--force]
rcdvlog analyze missing --project <id>
rcdvlog analyze status --project <id>
rcdvlog labels consolidate --project <id>
rcdvlog labels vocabulary --project <id>
```

Desktop: an Analyze batch action alongside the existing proxy and thumbnail
batches; title and description shown in the review queue and editable inline; the
VLM-chosen frame committed as thumbnail quality `10`. The `asset_thumbnails`
CHECK constraint already permits quality 10.

**Credentials:** the OpenRouter API key lives in the macOS Keychain through the
existing `apps/desktop/src-tauri/src/keychain.rs`, under its own service.
`OPENROUTER_API_KEY` is the CLI/agent override. No key present means `analyze`
fails with an actionable error; `transcribe` continues to work fully offline,
preserving the "degrades gracefully to the complete manual workflow" criterion.

## 8. Extensibility

`trait SttEngine` and `trait VlmProvider` with a model registry carrying
per-model quirks: reasoning toggle support, structured-output support, `maxItems`
support, and the measured image-token ratio. A `ProviderChain` owns
default → fallback → circuit-breaker and is the only place that logic lives.

- Adding a model is a registry entry plus its quirks.
- Adding an output field is a prompt-version and schema-version bump; the recipe
  key changes, which makes existing results detectably stale rather than silently
  mixed.
- A local VLM runtime and a search/index layer both slot in behind these seams
  without a migration rewrite.

## 9. Testing

- **Negative controls as a permanent regression test.** Black, RGB-noise and
  heavy-blur variants of a real frame-pack. Round 2b established a 10-model,
  30/30-clean baseline with zero invented OCR, landmarks, people or actions;
  that is a property to protect across model and prompt changes, not a one-time
  result. Mocked in unit tests, live in a marked integration test.
- **Frame-pack determinism** — byte-identical manifests across repeated runs
  (round 2 Finding 1 achieved 20/20).
- **Window-map arithmetic** — segment timestamps translate correctly back to
  clip time after packing non-adjacent speech regions.
- **Coverage check and re-split** — one case per round 1 Finding 9 row, using the
  synthetic code-switch set, which has exact ground truth for precisely this
  failure.
- **Domain validation** — out-of-enum value, truncated array, thumbnail timestamp
  outside the frame-pack, best-moment range outside clip bounds. Each must reject
  cleanly with a recorded reason and write nothing.
- **Provider chain** — fallback on failure, breaker trips at 10, provenance
  records the serving model.
- **Consolidation** — AI-origin labels remap, human-origin labels do not.

## 10. Non-goals for this version

Local VLM inference; search, FTS5 indexing and CJK tokenization; embeddings and
semantic search; landmark candidates; vision-based clock anchors; word-level
timestamps; speaker diarization; Metal acceleration.

## 11. Known risks

1. **Transcript fusion is untested.** Both spikes flag it explicitly as never
   exercised — round 2's "no transcript fusion", round 2b's "still open". It is
   the core of the STT-feeds-VLM requirement and it is the only part of this
   design with no measurement behind it. Measure it on ~10 clips with and without
   transcript before freezing `vlm.observe.v2`.
2. **Mixed provenance is the normal case.** At the measured 3-in-16 upstream 429
   rate, a 273-clip batch plausibly serves 40–50 clips from the fallback model.
   Results will not be model-homogeneous, and `qwen3.7-flash` and
   `gemini-2.5-flash-lite` differ measurably in label sprawl (91% vs 63% unique)
   and OCR behavior.
3. **Sustained thermal behavior is unmeasured.** Round 1's normalized cost drifted
   +13% and −14% in two runs on the fanless M3 Air, and `powermetrics` needs root.
   The two-phase estimator surfaces drift honestly rather than promising an ETA
   the machine cannot hold.
4. **`ocrText` is a suggestion, never a fact.** Round 2b Finding 5 hand-verified
   fabrication in both directions across every model family, including a fluent
   four-line takoyaki definition invented for an illegible handwritten card. It is
   stored and displayed with attribution; it is not indexed and not treated as
   ground truth. This is why deferring search is coherent rather than merely
   convenient.
5. **Reject-gate thresholds rest on 4 positive examples.** The qualitative
   conclusion (repetition is the strong signal, `no_speech_prob` is useless) is
   safe; the exact numbers are not, and should be revisited against a larger
   hand-labeled set.
