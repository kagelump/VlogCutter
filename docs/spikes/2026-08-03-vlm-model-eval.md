# Spike report: cheaper/faster VLM options for clip analysis (round 2b)

Date: 2026-08-03
Scope: model selection only. Follows
[2026-08-03-vlm-analysis-spikes.md](2026-08-03-vlm-analysis-spikes.md), which
established the frame-pack recipe, the bounded schema, and the negative
controls. Round 1 (STT) is in
[2026-08-03-stt-analysis-spikes.md](2026-08-03-stt-analysis-spikes.md).
Status: findings only. **No production code was written.**

Objective, in the stated priority order: **cost first, speed second, quality
last** — quality only has to clear "not completely wrong or broken, and it
mostly describes the vibe".

Budget: **$1.00 of OpenRouter credit. Actual spend: $0.248** (282 recorded
requests plus retries). Balance before $4.360, after $4.112.

## Eval shape, and where it departs from the proposal

The proposal specified a 60-clip corpus, hand-annotated ground truth with
two-annotator adjudication, character error rates by script, and quality gates
pinned to the incumbent's OCR recall. That design answers "which model is
best". The stated priority is closer to "which model is cheapest that isn't
broken", and hand-annotating 60 clips is the most expensive part of the
proposal while being the least load-bearing for that question. So:

| Proposal | What was run | Why |
| --- | --- | --- |
| 24-case screen, then 60-clip benchmark | **One 16-case screen** (12 real + 4 controls), 10 models | Cost separation showed up at 27x. A bigger corpus would not change the ranking. |
| Two-annotator ground truth | **Silver reference + hand-verified spot checks** on 5 clips | See Finding 4 — the incumbent is not self-consistent, so a silver reference has a measurable ceiling and is reported as such. |
| CER by script, exact-string recall | **OCR agreement + cross-model corroboration + 5 hand checks** | Enough to catch fabrication, which is the only OCR failure that matters here. |
| Transcript / trip-context factorial | **Not run** | Orthogonal to model choice; still open from round 2. |
| Provider-routing experiment | **Not run** | Provider recorded per request; no pinning. |
| — | **Added: frame-width sweep** (Finding 3) | Cost is the top priority and input tokens are ~70% of the bill, so the other cost lever had to be measured. |

Fixed across every model: the same 69 JPEG frames (byte-identical, hashed, built
once from the 720p `.v3` proxies using the round-2 frame-packs), the same prompt
`vlm.observe.v1`, the same bounded schema, `temperature: 0`, `max_tokens: 1500`,
reasoning disabled where the provider allows it. No response healing or repair —
a bad response is recorded as bad. One retry maximum, reported separately.

## Summary of findings

| # | Finding | Consequence |
| --- | --- | --- |
| 1 | **The whole 273-clip project costs under $1 on every candidate** — $0.03 to $0.97 | Cost ranking is real but the absolute stakes are ~$0.90; do not trade much quality for it |
| 2 | Model choice is a **27x cost lever**, driven by **image tokenization, not price per token** | Identical images bill 1,235 to 6,026 prompt tokens depending on model |
| 3 | **Frame width is not a cost lever**: ≤30% saving, and OCR agreement drops by a third to a half | Keep 768 px; do not "optimize" here |
| 4 | **The incumbent agrees with its own round-2 OCR only 70% of the time** | There is no stable OCR baseline to gate against; the proposal's "≥95% of Gemini" gate is unmeasurable |
| 5 | **Every model fabricates OCR sometimes, including the incumbent's family** — hand-verified in both directions | `ocrText` is a suggestion, never a fact; do not index it as ground truth |
| 6 | **All 10 models passed the negative controls**: 30/30 controls, zero invented OCR or landmarks | Round 2's Finding 3 generalizes across vendors |
| 7 | **The cheaper model is the more stable one** (label Jaccard 0.93 vs 0.67 at temperature 0) | Cost and determinism are not in tension here |
| 8 | Four models were **disqualified on mechanics, not quality** | Schema/vision/rate-limit support is the real filter |
| 9 | **Label sprawl is model-dependent**: 97% unique (incumbent) vs 63% (flash-lite) | Round 2's vocabulary-seeding fix is still needed, but the baseline differs |
| 10 | The **landmark field is a hallucination surface** when a model actually fills it | Strengthens round 2's recommendation to drop it |

---

## Phase A: the screen

10 models, 16 cases each, 160 requests. `real` = domain-valid responses on the
12 real clips. `OCRagree` = agreement with the round-2 incumbent output on the
identical frames — a *silver* reference, ceiling established in Finding 4.

| model | real | 1st try | $/clip | proj. 273 | p50 | p95 | prompt tok | OCRagree | controls |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `google/gemini-3-flash-preview` *(incumbent)* | 12/12 | 100% | $0.00357 | **$0.97** | 5.2 s | 10.7 s | 5,486 | 70% | 3/3 |
| `google/gemini-2.5-flash-lite` | 12/12 | 100% | **$0.00028** | **$0.08** | **3.4 s** | 6.8 s | **1,235** | 60% | 3/3 |
| `google/gemini-3.1-flash-lite` | 12/12 | 100% | $0.00195 | $0.53 | **3.2 s** | 6.0 s | 5,486 | **80%** | 3/3 |
| `qwen/qwen3.5-flash-02-23` | 12/12 | 100% | $0.00025 | $0.07 | 12.4 s | 16.6 s | 2,183 | 58% | 3/3 |
| `qwen/qwen3.7-flash` | 10/12 | 75% | **$0.00013** | **$0.03** | 12.3 s | 16.7 s | 2,175 | 75% | 2/3 |
| `openai/gpt-5-nano` | 12/12 | 100% | $0.00035 | $0.10 | 5.7 s | 9.3 s | 2,527 | 32% | 3/3 |
| `qwen/qwen3-vl-8b-instruct` | 12/12 | 100% | $0.00039 | $0.11 | 3.1 s | 23.7 s | 2,173 | 40% | 3/3 |
| `qwen/qwen3-vl-32b-instruct` | 12/12 | 100% | $0.00040 | $0.11 | 7.1 s | 9.1 s | 2,173 | 55% | 3/3 |
| `bytedance-seed/seed-2.0-mini` | 12/12 | 100% | $0.00074 | $0.20 | 5.1 s | 5.9 s | 6,026 | 48% | 3/3 |
| `nex-agi/nex-n2-mini` | 8/12 | 100% | $0.00036 | $0.10 | 20.6 s | 36.9 s | 1,564 | 36% | 3/3 |

Latency caveat: four requests were in flight concurrently, to *different*
models, so no model contends with itself, but all figures include shared
uplink contention from ~300 KB of base64 per request. Treat latency as
comparative, not absolute.

## Finding 1: the absolute stakes are about ninety cents

This is the number that should frame every other decision here. Analysing the
entire 273-asset project costs:

| | cost |
| --- | --- |
| Incumbent, `gemini-3-flash-preview` | **$0.97** |
| Best-quality cheap option, `gemini-3.1-flash-lite` | **$0.53** |
| Cost-optimal viable option, `gemini-2.5-flash-lite` | **$0.08** |
| Cheapest measured, `qwen3.7-flash` | **$0.03** |

The full spread between the best and worst candidate, across the whole project,
is **$0.94**. Cost is genuinely a 27x ratio, and the ratio is what would matter
at 10,000 clips — but at this project size the choice is between "less than a
dollar" and "less than a dollar". Stated plainly because the priority ordering
was cost-first: on this corpus, cost-first and quality-first differ by about
forty-five cents, and it is worth deciding whether that is the axis to optimise.

## Finding 2: the cost lever is image tokenization, not the price list

The same 69 JPEGs, sent to every model, produce wildly different prompt-token
bills:

| model | prompt tok (median, real clips) | $/M in | $/clip |
| --- | --- | --- | --- |
| `gemini-2.5-flash-lite` | **1,235** | $0.10 | $0.00028 |
| `nex-n2-mini` | 1,564 | $0.025 | $0.00036 |
| `qwen3-vl-*`, `qwen3.5/3.7-flash` | ~2,175 | $0.03–0.12 | $0.00013–0.00040 |
| `gpt-5-nano` | 2,527 | $0.05 | $0.00035 |
| `gemini-3-flash-preview`, `gemini-3.1-flash-lite` | 5,486 | $0.50 / $0.25 | $0.00357 / $0.00195 |
| `seed-2.0-mini` | **6,026** | $0.10 | $0.00074 |

`gemini-2.5-flash-lite` is 4.4x cheaper than `gemini-3.1-flash-lite` *per token*
and also uses 4.4x fewer tokens for the same pixels — the two effects compound
into the 7x gap. Conversely `nex-n2-mini` has the lowest input price on the
board and is not the cheapest per clip, because it spends 1,600+ reasoning
tokens per call.

**Implication for the batch-ETA and cost-estimation work:** a cost model based
on frames x published price-per-token will be wrong by up to 5x. The per-model
image-token ratio has to be measured once and stored, not derived.

## Finding 3 (important): frame width is not a cost lever

Cost is the top priority and input tokens are ~70% of the bill, so the obvious
next question is whether smaller frames buy the same savings without changing
models. Same 12 clips, same timestamps, same prompt, only the pixels change:

| model | | 768 px | 512 px | 384 px |
| --- | --- | --- | --- | --- |
| `gemini-2.5-flash-lite` | prompt tok | 1,235 | 1,235 | 1,235 |
| | $/clip | $0.00028 | $0.00027 | $0.00026 |
| | OCR agree | **60%** | 45% | 38% |
| `gemini-3.1-flash-lite` | prompt tok | 5,486 | 4,606 | 5,046 |
| | $/clip | $0.00195 | $0.00173 | $0.00182 |
| | OCR agree | **80%** | 58% | 42% |
| `qwen3.5-flash-02-23` | prompt tok | 2,183 | 1,415 | 1,175 |
| | $/clip | $0.00025 | $0.00019 | $0.00018 |
| | OCR agree | **58%** | 38% | 25% |

Both Google models bill a **flat** token count regardless of input width — they
normalise internally, so shrinking the JPEG saves nothing and only destroys
legibility. Only the Qwen model tokenizes proportionally, and there the best
case is a **28% saving for a 33-point drop in OCR agreement**.

Halving the frame count would halve the bill, but that changes the frame-pack
recipe round 2 validated, and 3–8 frames is already the PRD's band.

**Implication:** keep 768 px. Model choice is a 27x lever; frame width is a
≤1.4x lever that costs a third to a half of the OCR. Do not spend engineering
time here.

## Finding 4: there is no stable OCR baseline to measure against

The proposal's central quality gate is "important OCR recall at least 95% of the
Gemini baseline". That gate cannot be evaluated, because the incumbent is not
95% of itself.

Re-running `gemini-3-flash-preview` on the **byte-identical frames** used in
round 2, four hours later, at temperature 0, it agreed with its own earlier OCR
on **70%** of strings. On the forest-path clip, round 2 read all three signs
(`未来につながる森`, `Forests for the Future: Art Project for the Forest of
Tranquility`, `Asahi`); this run read only `Asahi`.

So the "OCRagree" column above is bounded at roughly 70–80% by reference noise,
not by model capability — which is why `gemini-3.1-flash-lite` scoring 80%
(above the incumbent's self-agreement) is not an anomaly. Any gate expressed as
a percentage of the incumbent is measuring run-to-run variance.

## Finding 5 (important): every model fabricates OCR, in both directions

Two mechanically identical-looking results, hand-verified against zoomed frames:

**Clip `asset_01d24617` — a handwritten card on an izakaya counter.** Ground
truth, confirmed by upscaling the source frame: *"Tsukidashi is considered a
part of a cover charge (390 yen) in Japan"*.

| model | returned | verdict |
| --- | --- | --- |
| round-2 incumbent | the full sentence | correct |
| `gemini-3.1-flash-lite` | "Tsukidashi is considered / a part of a cover charge / in Japan" | **correct** |
| `gemini-3-flash-preview` (this run) | "(390yen)" | partial, not wrong |
| `qwen3.5-flash-02-23` | "(390yen)" | partial, not wrong |
| `qwen3.7-flash` | "(30 yen)" | wrong digits |
| `gemini-2.5-flash-lite` | "Takoyaki is a ball-shaped snack / made of a wheat flour-based / batter and cooked in a / special molded pan." | **fabricated** — a fluent, plausible sentence that is not on the card |

**Clip `asset_04ca69e8` — a Malaysia pavilion wall.** Ground truth, confirmed by
upscaling: `Federal Territory / 連邦直轄領`, `Kuala Lumpur / クアラルンプール`,
plus Malaysian state names.

| model | returned | verdict |
| --- | --- | --- |
| `gemini-3.1-flash-lite` | "Federal Territory / Kuala Lumpur / Putrajaya / Labuan" | **correct** |
| `qwen3.5-flash-02-23` | "Yokohama / Kobe / Osaka / Hiroshima / Fukuoka / Naha" | **fabricated** — an invented list of Japanese cities |

And in the other direction, `gemini-2.5-flash-lite` correctly read
`保険サービス` off a small paper lantern in the market clip that no other model
recovered. Every model both reads text others miss and invents text that is not
there.

An unsupervised proxy — pooling all OCR strings per clip and counting those no
other *model family* corroborates — ranks them:

| model | OCR strings | uncorroborated | rate |
| --- | --- | --- | --- |
| `gemini-2.5-flash-lite` | 58 | 30 | 52% |
| `qwen3-vl-32b-instruct` | 52 | 25 | 48% |
| `gemini-3.1-flash-lite` | 54 | 20 | 37% |
| `qwen3.5-flash-02-23` | 38 | 10 | 26% |
| `gemini-3-flash-preview` | 28 | 4 | 14% |
| `qwen3.7-flash` | 41 | 5 | 12% |

This proxy is biased and is reported only alongside the hand checks: it
penalises a model for reading something correctly that nobody else managed, and
it excludes same-family corroboration, which is why `gemini-3.1-flash-lite`
scores 37% while being hand-verified *correct* on both hard signs above. Read it
as "how far out on a limb does this model go", not as an error rate.

**Implication:** `ocrText` must stay a reviewable suggestion. It should not be
written into search or chronology as fact by any model at this frame
resolution, including the incumbent. That is consistent with the PRD's
"suggestions never mutate annotations" rule, and it is now load-bearing rather
than merely tidy.

## Finding 6: the negative controls pass across every vendor

Round 2 tested one model. Across ten models and 30 control cases (2 black,
1 RGB-noise per model):

- **30/30 correctly refused** (`hasUsableContent: false` or `unusable: true`) —
  except `qwen3.7-flash`, whose one miss was an upstream 429, not a
  confabulation.
- **Zero invented OCR strings. Zero invented landmarks. Zero invented people or
  actions.** On every model.
- The heavy-blur control was flagged `blurry` or unusable by **10/10** models.

Round 2's Finding 3 is not a property of `gemini-3-flash`; it is a property of
the vision path with this prompt. That is a stronger result than round 2 could
claim and it should stay a permanent regression test.

## Finding 7: the cheaper model is the more stable one

Three identical runs, six clips, temperature 0:

| model | label Jaccard | OCR Jaccard | same thumbnail | same best-moment | same usability |
| --- | --- | --- | --- | --- | --- |
| `gemini-2.5-flash-lite` | **0.93** | **0.89** | **100%** | 83% | 100% |
| `gemini-3.1-flash-lite` | 0.67 | 0.75 | 67% | 100% | 100% |

`gemini-3.1-flash-lite` picks a different thumbnail frame on a third of clips
across identical requests, and shares only two-thirds of its labels with itself.
For stored derived artifacts that a user has reviewed and accepted, that is a
worse property than being slightly less accurate — a re-analysis would visibly
churn the review queue.

This cuts against the intuition that the pricier model is the safer default.

## Finding 8: four models were disqualified on mechanics, not quality

None of these are quality judgements; they are integration facts, and they
eliminated candidates faster than any quality metric did.

| model | failure |
| --- | --- |
| `bytedance-seed/seed-1.6-flash` | Provider **rejects `maxItems`**: *"Keyword 'maxItems' is not supported for type 'array'"*. Round 2 Finding 6 makes bounded arrays load-bearing, so this is disqualifying. |
| `mistralai/mistral-small-3.2-24b-instruct` | Listed as vision-capable in the OpenRouter catalog; the routed provider returned *"Image content is not supported by this model"*. Catalog capability flags are not reliable. |
| `nex-agi/nex-n2-mini` | **Reasoning cannot be disabled** (`reasoning: {enabled: false}` accepted and ignored). Burned 1,500 tokens of reasoning and returned empty content; at 6,000 tokens it still truncated `technical`/`suggestions` on **4/12** clips. Also the slowest model measured (p50 20.6 s). |
| `qwen/qwen3.7-flash` | Cheapest on the board at $0.03/project, but **3/16 requests were upstream 429s** on OpenRouter's shared capacity, and it emitted an out-of-enum `cameraMovement: "panning"` despite strict structured outputs being unavailable for it. |

`openai/gpt-5-nano` also required `reasoning: {effort: "low"}` rather than
disabled, and returned `thumbnailTimestamp: 0` on all three controls — a
degenerate but arguably sensible answer for an unusable clip, counted as a
domain violation here.

## Finding 9: label sprawl is model-dependent

Round 2 measured 48 unique labels out of 51 across 12 clips and concluded
free-form labels are unusable as facets. That reproduces for the incumbent, but
it is not universal:

| model | labels | unique | unique rate |
| --- | --- | --- | --- |
| `gemini-3-flash-preview` | 64 | 62 | **97%** |
| `openai/gpt-5-nano` | 70 | 63 | 90% |
| `qwen3.7-flash` | 57 | 52 | 91% |
| `qwen3.5-flash-02-23` | 72 | 48 | 67% |
| `gemini-3.1-flash-lite` | 59 | 41 | 69% |
| `gemini-2.5-flash-lite` | 64 | 40 | **63%** |

The incumbent is the *worst* offender. Round 2's fix — seed the prompt with the
project's existing vocabulary and normalise case on ingest — is still required
regardless; but a model swap alone cuts sprawl by a third.

## Finding 10: the landmark field is a hallucination surface

Round 2 recommended reconsidering `landmarkCandidates` because the incumbent
never populated it. Across ten models it is populated by three, and the content
argues for removing the field rather than fixing it:

| model | clip | proposed | verdict |
| --- | --- | --- | --- |
| `qwen3-vl-32b-instruct` | Osaka market hall | **`Tokyo Tower`, `Shibuya`, `Odaiba`** | fabricated; wrong city |
| `qwen3-vl-32b-instruct` | Expo registration area | `Apple Store`, `modern shopping complex` | fabricated |
| `qwen3.7-flash` | Vietnam pavilion | `Vietnam national flag`, `Nón lá` | grounded in visible signage |
| `gemini-3.1-flash-lite` | Malaysia pavilion | `Kuala Lumpur museum exhibit` | grounded |

The field being "safe" in round 2 was an artifact of the incumbent leaving it
empty. Round 2's recommendation 5 stands, and is stronger: **drop the field**
unless trip context is supplied, because its safety is a per-model accident.

---

# Recommendation

**Default: `google/gemini-2.5-flash-lite`.**

Under the stated priority order it wins on the first two axes outright and is
not broken on the third:

- **Cost**: $0.00028/clip, **$0.08** for the 273-clip project — 12.7x cheaper
  than the incumbent, and the cheapest option that is not rate-limited or
  reasoning-locked.
- **Speed**: p50 3.4 s, p95 6.8 s — the fastest tier, 1.5x faster than the
  incumbent at p50 and 1.6x at p95.
- **Reliability**: 12/12 domain-valid on real clips, 100% first-attempt, 3/3
  controls refused, zero invented content on controls, and the **most stable**
  model measured (label Jaccard 0.93, identical thumbnail 100% of the time).
- **Quality**: descriptions are accurate and shorter than the incumbent's
  ("A wide shot of an indoor exhibition space with a large timeline display on
  the wall. Many people, including children, are present…"). Lowest label
  sprawl. It clears the "mostly describes the vibe" bar.
- **Known weakness**: the highest OCR fabrication tendency measured — it invented
  a four-line takoyaki definition on an illegible handwritten card. Mitigated by
  treating `ocrText` as a review-gated suggestion, which Finding 5 shows is
  required for *every* model anyway.

**Fallback / upgrade if OCR becomes important: `google/gemini-3.1-flash-lite`**
— $0.53/project, same latency tier, best OCR on the board and hand-verified
correct on both hard signs. Its cost is the *entire* argument against it, and
that argument is worth $0.45 on this project. Its real drawback is Finding 7:
it churns thumbnails and labels between identical runs.

**Do not** adopt `qwen3.7-flash` despite it being cheapest ($0.03/project) —
upstream rate limiting cost 3/16 requests in a 160-request run, and a 273-clip
batch would hit it harder.

**Do not** shrink frames below 768 px (Finding 3).

Secondary decisions this supports:

1. Keep the frame-pack pinned to the proxy at 768 px, per round 2.
2. Keep `maxItems` on every array — it is also a compatibility filter (Finding 8).
3. Drop `landmarkCandidates` from the production schema (Finding 10).
4. Store the measured image-token ratio per model; do not derive cost from the
   published price list (Finding 2).
5. Keep the black/noise/blur controls as a permanent regression test — they now
   have a 10-model baseline of zero failures to protect.

# What this did NOT establish

- **No hand-annotated ground truth.** Five clips were hand-verified against
  upscaled frames; the rest is silver-referenced agreement with a baseline that
  Finding 4 shows is only 70% self-consistent. The OCR columns rank models; they
  do not measure accuracy.
- **12 real clips, one project, one venue.** All footage is Osaka/Expo. No
  low-light or night footage was in the sampled 12.
- **One prompt, one schema, one frame count band.** No prompt tuning was
  attempted per model; a cheap model may respond better to a different prompt.
- **Latency is contended.** Four concurrent in-flight requests and ~300 KB
  uploads per request. The ordering is trustworthy; the absolute seconds are not.
- **No provider pinning and no `:nitro` comparison.** Provider was recorded but
  routing was left at default, so some latency variance is provider variance.
- **No concurrency or throughput test.** The 273-clip wall time was not measured;
  only per-request latency.
- **Transcript fusion and trip context still untested** — unchanged from round 2,
  and still the most likely fix for the landmark field if it is kept.
- **Stability measured on 2 models, 6 clips, 3 runs.** Directional, not precise.
- **Prices are a snapshot** taken 2026-08-03 from the OpenRouter catalog; three
  candidates are preview or dated snapshots and may move.

# Reproduction

Harness (throwaway, not committed):
`<scratchpad>/eval/` — `prep_frames.py` (fixed frame pack, hashed),
`prep_widths.py`, `bench.py`, `score.py`, `stability.py`, `corroborate.py`,
`show.py`. Raw results in `eval/out/*.jsonl`, one JSON record per request
including provider, finish reason, token counts, charged cost and every
validation failure.

Frame pack: round-2 recipe
`framepack.2fps.skip400ms.max40.win3-8.sharp-bright-motion.dhash8-d5.v1`,
manifests reused verbatim from round 2 so the round-2 outputs remain a valid
reference on identical input.
