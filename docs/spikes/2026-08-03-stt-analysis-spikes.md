# Spike report: multilingual transcription and AI clip analysis (round 1, STT)

Date: 2026-08-03
Scope: STT round only. VLM is round 2 and is sketched at the end.
Status: findings only. **No production code was written.** Everything below ran
in a throwaway harness outside the workspace.

Two decisions taken after the measurements, folded in below:

- **CPU-only is accepted for this version.** Metal is not required, so the
  macOS 14 Metal issue (Finding 5) is a recorded limitation, not a blocker.
- **The corpus is English / Japanese / Mandarin**, with little or no Cantonese.
  Mandarin detections are therefore *correct*; a Cantonese detection would be
  wrong. This re-scopes Finding 8 -- see there.

## What was asked and what was done

Approach A from the brainstorm: narrow single-variable spikes in a dependency
order, each with measured numbers, against real footage from
`~/Movies/VideoProjects/rcdvlog-expo.ykQEF6/`.

Two user constraints shaped every recommendation:

1. The app must be able to download and run the whole stack itself. Requiring
   the user to install and configure LM Studio or Ollama is unacceptable
   friction.
2. It must integrate cleanly with the existing Rust code.

## Environment (this matters -- the numbers are machine-specific)

| Item | Value |
| --- | --- |
| Machine | MacBook Air, Apple M3, 24 GB unified memory, **fanless** |
| macOS | 14.8.5 (Sonoma), SDK 15.2 |
| Free disk at start | 9.9 GB |
| FFmpeg | 7.1.3 |
| Model | `ggml-large-v3-turbo-q5_0.bin`, 574 MB |
| whisper-rs | 0.16.0 (vendors whisper.cpp + ggml) |
| Threads | 4 |

The fanless M3 Air and the 9.9 GB free disk are real product constraints, not
incidental. Model footprint and sustained-load behavior belong in the design.

## Corpus

Everything ran against the real Osaka/Expo trip project, not fixtures.

| Item | Value |
| --- | --- |
| Assets in catalog | 273 |
| Proxy files | 306 (273 unique assets after `.v3` dedupe) |
| Total proxy audio | 129.1 min |
| Clip length | mostly 7-22 s, matching the PRD's 5-30 s assumption |

Content is genuinely multilingual: Japanese station/venue announcements,
Mandarin conversation between the travelers, English announcements.


## Summary of findings

| # | Finding | Consequence |
| --- | --- | --- |
| 1 | Only **8%** of footage duration is speech; **176/273** clips have none | Gate at clip level; ~20x compute saving |
| 2 | Both engines **invent speech from silence** at high confidence | VAD gating is a correctness requirement |
| 3 | whisper bills fixed **30 s windows**; PRD's 8-25 s chunking was **30% slower** | Pack windows instead of small chunks |
| 4 | LID confidence >=0.9 predicts good output (CER 0.26, 100% agreement) | Useful predictor, weak rejection gate |
| 5 | **Rust-only stack works**; Metal blocked on macOS 14 | Constraint satisfied; CPU-only accepted for v1 |
| 6 | CPU RTF 0.57; peak RSS 1.15-1.38 GB; thermals inconclusive | Workable; needs a sustained-load test |
| 7 | Cloud: 33/33 valid JSON, **$0.28/hr**, but inconsistent language tags | Domain-validate values, not just schema |
| 8 | **Cantonese silently rewritten as Mandarin** at 0.99 confidence | Model limitation; **low impact** -- corpus is en/ja/cmn |
| 9 | A chunk spanning a language switch **dropped speech in 3/7 cases** | Coverage check + re-split |
| 10 | FTS5 default tokenizer finds **nothing** in Japanese/Chinese | Character-segment CJK before indexing |
| 11 | Built-in VAD: 885 KB, RTF **0.0019**, fully deterministic | No ONNX dependency needed |
| 12 | `no_speech_prob` **useless**; repetition + token prob caught **4/4, 0 false** | Corrects my earlier recommendation |
| 13 | Separate LID pass costs **+34%** for **identical** language decisions | Use internal auto-detect |

Findings 2, 3, 9, 10 and 12 each contradict something in the PRD as written or
something I assumed at the start of this spike.

---

## Finding 1 (headline): only 8% of the footage contains speech

Silero VAD over all 273 clips:

| Measure | Value |
| --- | --- |
| Total duration | 129.1 min |
| Total detected speech | 9.9 min (**8%**) |
| Clips with any speech | 97 / 273 |
| Clips with > 3 s speech | 46 / 273 |
| Clips with **zero** speech | **176 / 273 (64%)** |

**Implication:** the dominant cost saving is skipping clips entirely, not
sub-clip chunking. Any design that transcribes every clip wastes roughly an
order of magnitude of compute on this corpus.

## Finding 2 (headline): silence makes both engines invent speech

This is a correctness finding, not a performance one.

**Local whisper** on a low-speech clip produced the same sentence six times:

```
ご飯を食べてみます。  (x6, identical segments)
```

**The cloud model is worse.** Given a synthesized 10-second file of *pure
digital silence*, `google/gemini-3-flash-preview` returned:

```
[ja  conf 0.98]  すみません、東京駅はどこですか？
[en  conf 0.95]  Go straight and turn left.
[cmn conf 0.92]  謝謝你，我知道了。
```

It invents a fluent multilingual exchange and reports 0.92-0.98 confidence.
This was **not** caused by a leading prompt: with a neutral prompt that
explicitly said "never guess or invent speech," pure silence still produced two
invented English sentences.

A positive control proves the model is not broken -- given known synthetic
speech it is excellent, including Cantonese:

| | text |
| --- | --- |
| truth | 呢度啲嘢食真係好正，我哋食多啲啦。 / The food here is honestly incredible. |
| got | 呢度啲嘢食真係好正，我哋食多啲啦。 / The food here is honestly incredible. |

Exact match, correct `yue` and `en` tags, correct boundaries.

**Implication:** VAD gating is a **correctness requirement, not an
optimization**, for local and cloud backends alike. Model-reported confidence
must never be trusted as a hallucination gate -- on empty audio it is
confidently wrong. This directly contradicts leaning on the PRD's
"language confidence" field for filtering.

## Finding 3 (headline): the PRD's 8-25 s chunking makes things slower

whisper processes **fixed 30-second windows**. A chunk costs a full window
regardless of its length.

Measured decode cost per chunk (all chunks < 30 s, so 1 window each):

| Measure | Value |
| --- | --- |
| Median decode per window | 5.16 s |
| Correlation of decode time with chunk length | r = **0.29** |
| Correlation of decode time with transcript length | r = -0.05 |

Cost is essentially flat per window. Head-to-head on the same 30 clips:

| Mode | Audio processed | Wall time | Aggregate RTF |
| --- | --- | --- | --- |
| VAD chunks (8-25 s) + LID pass + forced decode | 403 s (61%) | **494 s** | 1.23 |
| Whole clip, whisper internal auto-detect | 663 s (100%) | **379 s** | 0.57 |

**The PRD pipeline was 30% slower while processing 39% less audio.** Two causes:

1. Each 8-25 s chunk burns a whole 30 s window.
2. The separate LID pass re-runs the encoder.

Measured cost per 30 s window by path (medians; the split between encoder and
decoder is inferred from these totals, not measured separately):

| Path | Measured cost |
| --- | --- |
| Decode with language already forced | ~5.2 s |
| whisper internal auto-detect + decode | ~10.3 s |
| Separate `lang_detect()` pass + forced decode | ~14.8 s |

The separate LID pass is the most expensive option and buys only the
probability vector. Note the LID figure includes `create_state()` allocation,
which was inside the timer, so it slightly overstates the pass itself.

**Implication:** chunk to fill 30-second windows, not 8-25 s. Concatenate a
clip's speech regions (dropping silence) and pack them into as few 30 s windows
as possible. Use whisper's internal auto-detect rather than a separate LID
pass.

## Finding 4: language confidence predicts transcript quality (but see Finding 12)

Local output compared against the cloud reference on the 31 chunks where the
cloud reported speech, bucketed by local top-1 language probability:

| local LID top-1 | n | median CER vs cloud | language agreement |
| --- | --- | --- | --- |
| >= 0.90 | 15 | **0.26** | **15 / 15 (100%)** |
| 0.60 - 0.89 | 9 | 0.93 | 4 / 9 |
| < 0.60 | 7 | 1.00 | 5 / 7 |

Both chunks the cloud judged silent had low local confidence (0.50 and 0.27)
and were hallucinations (`양념장` x5; garbled English). Korean was detected on
3/33 chunks despite no Korean being present -- a reliable hallucination marker
on this corpus.

CER is computed on normalized text (NFKC, Traditional->Simplified via OpenCC,
punctuation stripped, lowercased). **This normalization is required**: raw CER
scored semantically identical Chinese transcripts as near-total errors purely
because local emitted Simplified (变成卖) and cloud emitted Traditional
(變成賣). Any future accuracy gate must normalize before scoring or it will
report nonsense.

At high confidence the transcripts are genuinely good:

```
CLOUD: お客様、当日券購入希望のお客様は 正面のチケット引換所にお進みください。
LOCAL: お客様、当日券購入希望のお客様は 正面のチケット引き換え所にお進みください。
```

**Implication:** high language confidence is a good *predictor* of usable
output. It is not a good *rejection* rule -- Finding 12 measures that directly
and lands on repetition plus token probability instead. Suggestions that fail
the gate should never reach the user as accepted content.

## Finding 5: the whole stack can be Rust-only, with one caveat

This directly answers the "no LM Studio / no Ollama" constraint.

| Question | Answer |
| --- | --- |
| Can Rust own STT with no external app? | **Yes.** `whisper-rs` 0.16 vendors and builds whisper.cpp; clean build in 22-33 s from a cold `target`. |
| Python required? | **No.** |
| ONNX runtime required? | **No** -- whisper.cpp now ships Silero VAD built in (`ggml-silero-v5.1.2.bin`, 885 KB), exposed through `whisper-rs`'s `WhisperVadContext`. |
| Language probabilities available? | Yes, `state.lang_detect()` returns a full per-language probability vector. |
| Per-segment confidence available? | Yes, `no_speech_probability()` and `token_probability()`, free with the decode. But see Finding 12: `no_speech_probability()` measured as useless here; `token_probability()` is useful in combination. |
| FTS5 available? | **Yes, already.** `libsqlite3-sys` compiles `-DSQLITE_ENABLE_FTS5` unconditionally in the bundled build the repo already uses. No dependency change. |

So the shipping stack is: two model files (574 MB STT + 885 KB VAD),
downloaded on demand, one Rust crate, no external application, no Python, no
separate runtime. That satisfies the constraint cleanly.

### The caveat: Metal is blocked on macOS 14

Building `whisper-rs` with the `metal` feature produces a binary that will not
launch on this machine:

```
dyld: Symbol not found: _OBJC_CLASS_$_MTLResidencySetDescriptor
```

`MTLResidencySetDescriptor` is macOS 15+. ggml gates it on the **SDK** version
(`__MAC_OS_X_VERSION_MAX_ALLOWED >= 150000`), not the deployment target, so
building with the installed 15.2 SDK emits a hard reference that a macOS 14
host cannot resolve. Overriding `CFLAGS`/`OBJCFLAGS` did not reach ggml's ObjC
compile.

**Decision: CPU-only ships for this version**, so this is a recorded
limitation rather than a blocker. All numbers in this report are CPU-only,
which is also the correct fallback for the documented macOS 13+ floor in
`README.md` and `AGENTS.md`. Finding 6 shows CPU throughput is comfortably
sufficient once clips are gated.

When Metal is revisited for speed, the options are:

1. Pin/patch the vendored whisper.cpp to compile the residency-set path out
   (build-time patch in the sys crate, or a fork).
2. Ship Metal only for macOS 15+ and CPU below it, selected at runtime.
3. Raise the documented OS floor to macOS 15 -- a product decision, not a
   technical one.

Revisit only if measured CPU throughput becomes a user-visible problem. Note
that whichever option is chosen, the CPU path must keep working, because the
documented OS floor is below macOS 15.

## Finding 6: measured throughput and resource envelope (CPU-only)

| Measure | Value |
| --- | --- |
| Peak RSS | **1.38 GB** (q5_0 turbo, 4 threads) |
| Model load | 0.4-1.1 s |
| Decode per 30 s window (language forced) | ~5.2 s |
| Whole-clip aggregate RTF | 0.57 |
| Wall time per hour of *audio* | ~34 min |

Projected onto the real corpus using the recommended design (skip silent clips,
pack speech into 30 s windows, internal auto-detect at ~10.3 s/window): 9.9 min
of speech packs into ~20 windows, i.e. **roughly 3.5 minutes of CPU for the
entire 129-minute project**, plus VAD over the full pool. Compare with ~74 min
(129.1 min x measured RTF 0.571) if every clip were transcribed whole. The
gating decision is worth roughly **20x** on this corpus.

These are projections from measured per-window costs, not an end-to-end timing
of the recommended pipeline -- that pipeline was not built in this round.

Thermal behavior was **inconclusive**: normalized cost drifted +13% across the
chunk run but -14% across the clip run. `pmset -g therm` reports nothing on
Apple Silicon and `powermetrics` needs root, so sustained-throttle behavior on
the fanless Air remains **unmeasured**. It needs a dedicated long-run test
before any batch-ETA promise.

## Finding 7: OpenRouter works well and is cheap, but needs output normalization

33 real chunks through `google/gemini-3-flash-preview`:

| Measure | Value |
| --- | --- |
| Strict-JSON schema compliance | **33 / 33** |
| Total cost | **$0.0317** for 403 s of audio |
| Cost per hour of speech audio | **~$0.28** |
| Median latency | 3.9 s (max 14.8 s) |
| Correctly reported no speech | 2 / 33 |

Quality on real footage was clearly better than local q5_0 turbo.

**But strict JSON did not constrain string *values*.** Despite the schema and
an explicit instruction to emit `cmn`/`yue`, the model returned a mix of
`ja`, `en`, `zh-TW`, `zh-CN`, and bare `zh`. A JSON-schema-valid response is
therefore **not** a valid domain response. The PRD's "invalid model JSON is
rejected rather than partially applied" needs to mean domain validation against
a closed tag vocabulary, not just schema validation.

Cost projection: gating on VAD, a 100-hour corpus at this corpus's 8% speech
ratio is ~8 hours of speech, about **$2.30** of cloud STT. Cost is not the
reason to prefer local; privacy and offline capability are.

## Finding 8: Cantonese is silently converted to Mandarin (low impact here)

A synthetic ground-truth set was built with macOS `say` (Kyoko/ja,
Tingting/cmn, Sinji/yue, Samantha/en, Paulina/es): 12 items, 7 code-switched,
exact known text, language, and boundary times. This is clean studio audio, so
it **upper-bounds** accuracy on real footage.

Per-segment LID on monolingual segments: **18/20 = 90%**, median top-1
probability **1.00**.

Both failures were the same failure, and it is not a confidence problem:

| truth | detected | confidence | transcript produced |
| --- | --- | --- | --- |
| `yue` | `cmn` | **0.995** | 我們現在在大阪,今天好熱呀! |
| `yue` | `cmn` | **0.989** | 這裏的食物真是好正,我們多吃一點吧。 |

Ground truth was 我哋而家喺大阪，今日好熱呀 -- Cantonese. whisper did not merely
mislabel it, it **rewrote Cantonese into Mandarin phrasing and Traditional
characters**, at 0.99 confidence.

**Re-scoped after review: this is low impact for this product.** The footage is
English, Japanese, and Mandarin, with little or no Cantonese, so a `cmn`
detection on real clips is *correct* and the failure above is largely
hypothetical here. Two things still follow:

1. **The PRD should drop Cantonese from its target language list**, or state it
   as a known limitation. Listing it implies a capability this model does not
   have. (The cloud model did handle `yue` correctly in the positive control,
   so per-language routing is available if it ever matters.)
2. **The general lesson stands and is not about Cantonese:** high confidence
   does not mean correct language. Confidence separates hallucination on noisy
   audio (Finding 4) but not language confusion on clean audio. Any
   closely-related language pair can fail this way, silently, at ~1.0
   confidence.

Also note what this measurement *confirms*: on the 18 non-Cantonese segments,
LID was correct with median confidence 1.00 -- English, Japanese and Mandarin,
which is exactly this corpus's actual language mix.

## Finding 9 (severe): one chunk spanning a language switch silently drops speech

The 7 code-switched items, each fed as a single chunk:

| item | truth | detected | outcome |
| --- | --- | --- | --- |
| `ja_en` | ja+en | ja | both kept |
| `cmn_en` | cmn+en | cmn | both kept |
| `ja_cmn` | ja+cmn | ja | both kept |
| `yue_en` | yue+en | cmn | both kept, Cantonese Mandarinized |
| `en_ja` | en+ja | ja | **English half missing** |
| `ja_en_ja` | ja+en+ja | ja | **English middle missing** |
| `en_es` | en+es | es | **English half missing** |

In **3 of 7** cases an entire language's speech vanished from the transcript
with no error and no low-confidence signal.

**Implication:** this validates the PRD's central premise -- per-chunk language
detection is genuinely necessary -- and it creates real tension with Finding 3's
"pack into 30 s windows" recommendation, because a packed window is *more*
likely to span a switch. Resolution below.

## Finding 10: FTS5 default tokenizer finds nothing in Japanese or Chinese

FTS5 is already available (Finding 5). The tokenizer is the problem.

Isolated test, indexing 次は新大阪です / 我们现在在大阪 / たこ焼きを二つください:

| query | `unicode61` (default) | `trigram` | `unicode61` + char segmentation |
| --- | --- | --- | --- |
| 新大阪 (3 char) | **0** | 1 | 1 |
| 大阪 (2 char) | **0** | **0** | **2** |
| 焼き (2 char) | **0** | **0** | 1 |
| たこ焼き (4 char) | **0** | 1 | 1 |
| 次 (1 char) | **0** | **0** | 1 |
| station | 1 | 1 | 1 |

`unicode61` treats an entire run of CJK as a **single token**, so transcript
search in Japanese or Chinese returns nothing. `trigram` fixes queries of 3+
characters but structurally cannot match 1-2 character queries -- and 大阪
(Osaka) is exactly the kind of two-character query this app must serve.

The fix that works for every length: store a shadow copy with each CJK
codepoint space-separated, index with `unicode61`, and issue CJK queries as
phrase matches. 大阪 then correctly matches both the Japanese and the Chinese
document.

At simulated target scale (5,000 assets, 29,096 indexed rows) both tokenizers
were fast and small: insert 0.10-0.17 s, queries 3-9 ms, trigram index +4.4 MB.
**Scale is not the risk here; tokenization is.**

Note: an earlier version of this spike reported that `unicode61` matched
Japanese queries. That was an artifact -- those queries were matching short OCR
values that happened to equal a whole CJK run, not transcript text. The
isolated test above is the correct result.

## Finding 11: whisper.cpp's built-in VAD is native, instant, and deterministic

Tested through `whisper-rs`'s `WhisperVadContext` on the same 33 chunks:

| Measure | Value |
| --- | --- |
| Model | `ggml-silero-v5.1.2.bin`, **885 KB** |
| Throughput | 403 s of audio in **784 ms** (RTF **0.0019**) |
| Determinism | **33 / 33** byte-identical segment lists on repeat runs |
| External dependencies | none -- no Python, no ONNX runtime |

VAD is effectively free (0.2% of the audio duration) and satisfies the PRD's
reproducibility criterion directly. There is no reason to gate transcription on
anything else, and no reason to take an ONNX-runtime dependency.

## Finding 12 (corrects an earlier assumption): the free confidence signals mostly do not work

I initially recommended `no_speech_probability()` as the cheap hallucination
gate. **Measured, it does not discriminate at all.** Across 33 chunks, with
"bad" defined as cloud-reported-silence or >=50% duplicated segments:

| signal | bad chunks (n=4) | good chunks (n=29) | useful? |
| --- | --- | --- | --- |
| `no_speech_prob` mean | median 0.000 | median 0.000 | **no -- zero separation** |
| avg `token_probability` | median 0.737 | median 0.728 | not alone |
| LID top-1 probability | median 0.690 | median 0.899 | weak as a gate |
| **repetition ratio** | **median 0.929** | **median 0.000** | **yes** |

LID probability is a good quality *predictor* (Finding 4) but a poor *rejection*
gate: rejecting below 0.9 catches 3/4 bad while wrongly rejecting 15/29 good.
High confidence implies good output; low confidence does not imply bad output.

The combination that worked, using only signals that are free from the single
decode pass:

```
reject if  repetition_ratio >= 0.3  OR  mean_token_probability < 0.20
```

| Result on 33 chunks | |
| --- | --- |
| Bad chunks caught | **4 / 4** |
| Good chunks wrongly rejected | **0 / 29** |

Repetition alone catches 3/4 with zero false positives; the mean-token-probability
clause catches the fourth (0.11, against a good-chunk minimum of 0.207).

**Caveat: n=33 with only 4 positives.** These thresholds are a starting point
to validate on a larger set, not tuned constants. The qualitative conclusion --
repetition is the strong signal, `no_speech_prob` is useless here -- is safe;
the exact numbers are not.

## Finding 13: the separate LID pass buys nothing

Direct A/B on the same 33 chunks:

| Mode | Total time | Peak RSS | Language decisions |
| --- | --- | --- | --- |
| Internal auto-detect | **368 s** | **1.15 GB** | baseline |
| Separate `lang_detect()` + forced decode | 494 s (**+34%**) | 1.38 GB | **identical, 33/33** |

The expensive path produced exactly the same language for every chunk. Its only
product is the probability vector -- which Finding 12 shows is a weak gate
anyway. Use internal auto-detect.

---

# Recommended changes to the PRD

These follow from the measurements above. Each names the finding that forces it.

### 1. Replace "8-25 second chunks" with 30-second window packing (Finding 3)

The chunking recipe should be: detect speech regions -> drop silence ->
concatenate a clip's speech into as few 30 s windows as possible -> record the
mapping from window offsets back to true clip timestamps. Chunk boundaries at
8-25 s cost a full window each and made the pipeline 30% slower than doing
nothing clever.

The mapping back to source time is the part that needs care and tests: after
concatenating non-adjacent speech regions, every returned segment timestamp
must be translated through the region map before it is stored.

**But packing conflicts with Finding 9**: a fuller window is more likely to span
a language switch, and a spanned switch silently drops speech. Resolve it with a
coverage check rather than by choosing one extreme:

1. Pack speech regions into 30 s windows (fast path).
2. After decoding, compare **transcribed segment duration against the speech
   duration that went into the window**. Finding 9's failures dropped a whole
   utterance, so the shortfall is large and easy to detect.
3. On a large shortfall, re-run that window split at its internal silence
   boundaries -- which are exactly the likely switch points -- and keep the
   split result.

This pays the extra window only for the minority of clips that actually
code-switch. It needs a test per Finding 9 row, using the synthetic set, which
has exact ground truth for precisely this case.

### 2. Make VAD gating a correctness gate, not a cost optimization (Findings 1, 2)

Clips with no detected speech must be marked "no speech" and **never sent to any
model**, local or cloud. Skipping them avoids 64% of clips on this corpus and,
more importantly, avoids fabricated transcripts.

### 3. Do not use model-reported confidence as the hallucination gate (Finding 2)

The cloud model reported 0.92-0.98 confidence on invented speech, and whisper
reported 0.99 while rewriting Cantonese as Mandarin. The measured gate
(Finding 12) is:

1. **VAD speech presence** -- primary, model-independent, essentially free
   (Finding 11), and the only signal not produced by the model being judged.
2. **`repetition_ratio >= 0.3 OR mean_token_probability < 0.20`** -- caught
   4/4 bad chunks with 0/29 false rejections.
3. **Coverage shortfall** (Recommendation 1) for dropped-language detection.

Do **not** use `no_speech_probability()`: measured across 33 chunks it was
0.000 for good and bad alike (Finding 12). This corrects an assumption I held
earlier in this spike.

Be precise about what each signal catches. LID confidence **predicts quality**
(Finding 4: >=0.9 gave 100% language agreement, 0.26 median CER) but is a poor
**rejection** gate (rejecting below 0.9 discards 15/29 good chunks). And no
confidence signal catches language confusion on clean audio (Finding 8: 0.99
confidence, wrong language, rewritten content).

### 3b. Index CJK with character segmentation, not the default tokenizer (Finding 10)

The PRD's searchable-transcript requirement fails silently with FTS5's default
settings -- Japanese and Chinese transcripts return zero hits. Store a shadow
text column with CJK codepoints space-separated, index with `unicode61`, and
issue CJK queries as phrase matches. Do not reach for `trigram`: it cannot
serve two-character queries such as 大阪.

Because search must jump to a transcript time range, index at **segment
granularity** (asset id, start ms, end ms, kind, text) rather than one document
per asset. Measured cost at 5,000-asset scale is negligible (Finding 10).

### 4. Domain-validate model output, not just its JSON schema (Finding 7)

Strict JSON schema compliance was 33/33 while language tags were still
inconsistent (`zh-TW`, `zh-CN`, `zh`). Validation must check values against a
closed vocabulary and normalize (`zh-TW`/`cmn`/`zh` -> a single canonical tag)
before persistence. Reject-and-record rather than partially apply.

### 5. Use whisper's internal auto-detect (Findings 3, 13)

Measured A/B: internal auto-detect took 368 s and 1.15 GB peak RSS; the
separate `lang_detect()` pass took 494 s (+34%) and 1.38 GB, and produced
**identical language decisions on all 33 chunks**. It buys only the probability
vector, which Finding 12 shows is a weak gate. Reserve it for an explicit
"why did it pick this language?" affordance, not the default path.

### 5b. Use whisper.cpp's built-in VAD, not an ONNX dependency (Finding 11)

885 KB model, RTF 0.0019, deterministic across repeat runs, no Python and no
ONNX runtime. It is the cheapest and most reliable component in the pipeline
and it carries the correctness gate from Recommendation 2.

### 6. Ship CPU-only; record the Metal limitation (Finding 5)

Decided: CPU-only for this version. Finding 6 shows CPU throughput is
sufficient once clips are gated. Record the macOS 14 Metal issue so it is not
rediscovered, and keep the CPU path working regardless, since the documented OS
floor is below macOS 15.

### 7. Correct the PRD's target language list (Finding 8)

Drop Cantonese, or mark it explicitly unsupported. English, Japanese and
Mandarin -- the actual corpus languages -- were identified correctly with
median confidence 1.00.

# How this maps onto the existing architecture

Nothing here requires violating the AGENTS.md invariants. Concretely:

- **No new runtime dependency.** `whisper-rs` is a normal crate; FTS5 is already
  compiled in. No permanent Python service, consistent with the invariant.
- **Domain logic in `rcdvlog-core`.** Audio extraction is another managed
  FFmpeg child process, exactly like the existing proxy/thumbnail paths.
- **Recipe identity, like proxies.** Transcripts are derived artifacts with a
  recipe key in the established style, e.g.
  `stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1`, so re-running is
  idempotent and stale results are detectable. The existing versioned-recipe
  and "regenerate missing" patterns carry over directly.
- **Batch ETA.** Use `core/batch-estimator` with a new task key and the real
  worker count, per the existing rule. Weight should be **30 s windows**, not
  source bytes, because Finding 3 shows cost tracks windows.
- **Suggestions never mutate annotations.** Accepted labels must go through the
  existing typed label operation; accepted trims through a new edit-plan
  revision. AI output lands in its own tables and is copied on explicit accept.
- **CLI first.** Per the repo convention, expose transcription and diagnosis as
  stable JSON CLI commands alongside any desktop surface.

# What round 1 did NOT establish

Stated plainly so none of this is mistaken for settled:

- **No Metal numbers.** Everything is CPU-only (Finding 5) -- now an accepted
  decision rather than a gap, but it does mean no measured GPU headroom figure
  exists if throughput later matters.
- **Sustained thermal behavior is unmeasured** and drifted inconsistently.
- **No hand-labeled ground truth on real footage.** Local-vs-cloud CER uses the
  cloud as a *silver* standard that is provably unreliable on low-speech audio.
  Real accuracy claims still need human labels.
- **Only one local model was tested** (`large-v3-turbo` q5_0). Whether a larger
  or less quantized model closes the gap with cloud is untested; disk headroom
  (8.5 GB free) constrained this.
- **Only one cloud model was tested.**
- **Word-level timestamps, speaker turns, and overlap reconciliation** across
  window boundaries were not exercised.
- **The recommended pipeline was never built or timed end to end.** Findings 1,
  3, 11 and 12 are measurements of its *components*; the projected ~3.5 min for
  the corpus is arithmetic on those, not an observed run.
- **The reject gate rests on 4 positive examples** (Finding 12). Thresholds need
  validation on a larger, ideally hand-labeled set before they are constants.
- **Cantonese is unsupported by this model** (Finding 8). Low impact given the
  actual en/ja/cmn corpus, but the PRD's language list should be corrected.
- **No VLM work at all** -- that is round 2.

---

# Round 2 proposal: VLM spikes

Round 1 changes the round-2 design before it starts, so these are re-scoped
rather than copied from the PRD.

**The LM Studio plan is already ruled out** by your constraint. Round 1 proved
the equivalent pattern works: `whisper-rs` vendors and builds whisper.cpp with
no external app. The VLM analogue is `llama.cpp` via Rust bindings
(`llama-cpp-2`) with a Qwen3-VL GGUF plus its `mmproj` vision projector, or a
bundled `llama-server` sidecar binary the app launches itself. Both keep the
"app downloads and runs the stack" property; neither asks the user to install
anything.

### V1. Local VLM runtime seam (do this first -- it gates everything)

Question: can a Qwen3-VL-class GGUF run in-process from Rust on an M3 Air with
24 GB, with acceptable memory alongside the existing 4-worker VideoToolbox
proxy pipeline?
Measures: build complexity, peak RSS, seconds per frame-pack, model footprint
against the **8.5 GB free disk**.
Kill criterion: if peak RSS plus proxy generation exceeds comfortable headroom,
local VLM is a "run when idle" feature, not an inline one.

### V2. Frame-pack determinism

Question: does the same clip and recipe always select the same frames?
This is a PRD acceptance criterion and is cheap to test exhaustively.
Measure: byte-identical manifests across repeated runs and across
proxy-versus-original inputs. Note the existing catalog already has v2/v3 proxy
generations, so "same clip" must mean same source fingerprint plus recipe.

### V3. Structured-output reliability (highest risk, per Finding 7)

Round 1 showed 33/33 JSON-schema-valid responses that still contained an
invalid tag vocabulary, and a model that confabulates confidently on empty
input. For VLM the equivalent risk is confident description of frames that do
not show what is claimed.
Measures: schema validity, **domain** validity against closed vocabularies, and
a deliberate negative control -- feed black, blurred, and unrelated frames and
count how often the model invents content. Round 1 shows this control is not
optional.

### V4. Local-versus-cloud quality on the same frame-packs

Same frame-pack to local GGUF and to an OpenRouter vision model. Round 1's
cost data suggests cloud VLM will also be inexpensive at this corpus size, so
the decision will again turn on privacy and offline behavior rather than cost.

### V5. Persistence and accept/reject flow

Shared with STT: results as derived artifacts with recipe identity, suggestions
that never mutate annotations until accepted, accepted labels through the
existing typed label operation.

**Suggested order:** V1 gates V3/V4; V2 and V5 are independent and can proceed
in parallel.

# Reproducing this

Harness lives in the session scratchpad (throwaway, outside the workspace):

```
scratchpad/spike/
  rust/            whisper-rs harness (stt-spike, vad_test)
  py/              vad.py corpus.py synth.py openrouter_ref.py compare.py analyze.py fts.py
  models/          ggml-large-v3-turbo-q5_0.bin, ggml-silero-v5.1.2.bin
  out/             all raw JSONL results
```

Two traps worth recording, because both cost real time and both fail silently:

1. **Silero ONNX needs 64 samples of carried context** prepended to each 512
   sample window (576 total at 16 kHz). The input shape is dynamic, so feeding
   a bare 512 window returns uniform ~0.001 probabilities instead of an error.
   The first full-corpus run reported "zero speech in all 273 clips" because of
   this. Any Rust `ort` port must replicate it exactly -- or avoid the issue
   entirely by using whisper.cpp's built-in VAD.
2. **CER on CJK must be normalized** before scoring (Finding 4), or Simplified
   versus Traditional output alone reports near-total error on identical
   meaning.
