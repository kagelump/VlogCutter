# Spike report: VLM clip analysis (round 2)

Date: 2026-08-03
Scope: VLM round. Round 1 (STT) is in
[2026-08-03-stt-analysis-spikes.md](2026-08-03-stt-analysis-spikes.md).
Status: findings only. **No production code was written.**

Carried over from round 1 and treated as settled here: CPU-only ships for this
version, the corpus is English/Japanese/Mandarin, and the stack must be
runnable by the app itself with no Ollama or LM Studio install.

## Environment and corpus

Same machine and project as round 1: Apple M3 MacBook Air, 24 GB, fanless,
macOS 14.8.5; the real Osaka/Expo project (273 assets). Frame-packs were built
from the 720p `.v3` proxies unless stated otherwise. Originals (3840x2160) are
on a mounted NAS volume and were read read-only, never written.

| Component | Choice |
| --- | --- |
| Cloud VLM | `google/gemini-3-flash-preview` via OpenRouter |
| Local VLM | Qwen3-VL-4B-Instruct Q4_K_M (2.4 GB) + mmproj F16 (797 MB) |
| Local runtime | llama.cpp built from source, **CPU-only**, `llama-mtmd-cli` |
| Frame-pack recipe | `framepack.2fps.skip400ms.max40.win3-8.sharp-bright-motion.dhash8-d5.v1` |

## Summary of findings

| # | Finding | Consequence |
| --- | --- | --- |
| 1 | Frame-pack selection is **byte-identical across runs** (20/20 clips) | PRD determinism criterion met |
| 2 | Frame-packs **differ between proxy and original** (3/4 clips); original is **44x slower** | Pin the frame-pack to one representation and record it |
| 3 | The VLM **does not confabulate on blank input** -- 4/4 black, 1/1 noise correctly refused | Opposite of round 1's audio result; controls still required |
| 4 | Cloud: **24/24 valid JSON**, $0.0041/clip, thumbnail timestamps always valid | Cloud path is production-plausible today |
| 5 | **Label sprawl: 48 unique labels from 51 across 12 clips** | Free-form labels are unusable as facets; seed the prompt with existing vocabulary |
| 6 | A JSON grammar did **not** prevent invalid output -- a repetition loop truncated it | Bound every array with `maxItems` |
| 7 | Local CPU: **41 s/clip, 4.8 GB RSS, OCR recall 1/6 vs cloud** | Cloud is the default; local is an offline fallback, background-only |

---

## Finding 1: frame-pack selection is deterministic

The recipe samples candidates at 2 fps (skipping the first and last 400 ms,
capped at 40), scores each candidate for sharpness (variance of Laplacian),
brightness and motion, rejects motion-blur peaks, picks the best frame per
temporal window, and drops perceptual duplicates by dHash distance.

Run twice over 20 clips, the manifests were **byte-identical**, including
timestamps, metric values, frame counts and selection reasons. Selected counts
ranged over 3, 4, 7 and 8 frames, so the 3-8 band in the PRD is genuinely
exercised rather than pinned at one value.

This satisfies the PRD acceptance criterion "re-running the same frame-pack
recipe produces the same selected timestamps and manifest" -- **for the same
input**. Finding 2 is the important qualification.

## Finding 2 (important): proxy and original do not produce the same frame-pack

Same recipe, same clip, two representations:

| clip | from 720p proxy | from 4K original | match |
| --- | --- | --- | --- |
| `..._0185_D` | 2.9, 10.9, 13.4, 15.4 | 0.9, 4.9, 10.9, 13.4, 15.4 | no (**4 vs 5 frames**) |
| `..._0186_D` | 0.4, 6.4, 9.4, 10.9 | 0.4, 5.4, 9.4, 9.9 | no |
| `..._0187_D` | 0.4, 2.9, 7.9 | 0.4, 2.9, 7.9, 10.4 | no (3 vs 4 frames) |
| `..._0188_D` | 0.4, 1.9, 4.4, 7.4 | 0.4, 1.9, 4.4, 7.4 | **yes** |

Only 1 of 4 agreed. It is **not** a duration or dimension mismatch -- durations
were identical to the millisecond and both were probed the same way. The cause
is that sharpness and motion are **resolution- and codec-dependent**: a 4K
source and a 720p re-encode downscaled to the same 320 px working size still
yield different Laplacian variance and different frame-to-frame differences.
That shifts the per-window winner, and via the mean-motion rule it can shift the
target frame count too.

Cost, same four clips:

| Source | Mean time to build one frame-pack |
| --- | --- |
| 720p proxy (local disk) | **0.4 s** |
| 4K original (NAS) | **17.6 s** (44x slower) |

**Implications:**

1. **Pin frame-packs to the proxy.** It is 44x faster, it works when originals
   are offline (which the PRD requires), and it is already generated.
2. **Representation identity belongs in the manifest.** The recipe key alone is
   not sufficient provenance -- `framepack.<recipe>` must be qualified by which
   representation and which proxy version (the catalog already has v2 and v3
   proxies) plus that file's fingerprint. Without this, a proxy regeneration
   silently invalidates stored frame-packs.
3. Do not treat "same recipe" as "same result" across representations in the
   acceptance criteria. The PRD's wording should say so explicitly.

## Finding 3: the VLM does not confabulate on blank input

Round 1's headline was that both STT engines invented speech from silence at
high confidence. The same experiment on the vision path gives the **opposite**
result.

Each control replaces the real frames with degraded ones, keeping everything
else identical:

| control | n | correctly refused | invented OCR | invented landmarks |
| --- | --- | --- | --- | --- |
| black frames | 4 | **4 / 4** | 0 | 0 |
| RGB noise | 1 | **1 / 1** | 0 | 0 |
| heavy Gaussian blur | 4 | 2 / 4 flagged unusable | 0 | 0 |
| real frames | 12 | n/a | n/a | 0 |

On black frames it returned `hasUsableContent: false`, `unusable: true`, empty
arrays, and the description "The video consists entirely of black frames with
no visible content."

The two blurred clips that were *not* marked unusable still described the blur
honestly ("Extremely blurry and out-of-focus shots of what appears to be food"),
which is arguably correct rather than a failure -- a blurry clip is still
informative.

**Caveats, stated so this is not over-read:**

- The prompt gave an explicit escape hatch ("if the frames are black, blank or
  unreadable, set hasUsableContent false"). Round 1's neutral audio prompt also
  said "never invent" and the audio model confabulated anyway, so the difference
  is real -- but this is not a like-for-like prompt comparison.
- Zero landmark claims across **all 12 real clips** cuts both ways. The model
  never hallucinated a landmark, but it also never identified one, despite the
  footage being from a named venue. The PRD's "landmark candidates" field may
  simply not be productive with this model.

**Implication:** keep the negative controls as a permanent regression test, but
do not carry round 1's "the model will invent everything" assumption into the
VLM design. The measured risk here is different and smaller.

## Finding 4: the cloud path is production-plausible today

12 real clips plus 12 controls:

| Measure | Value |
| --- | --- |
| Strict-JSON schema compliance | **24 / 24** |
| Mean cost per real clip | **$0.0041** (3-8 frames at 768 px) |
| Median latency | 4.8 s |
| `thumbnailTimestamp` was one of the supplied frames | **12 / 12** |
| Best-moment range within clip bounds | **12 / 12** |

Extrapolated to the whole 273-clip project: **about $1.12**.

The constrained fields behaved: the model did not invent a thumbnail timestamp
outside the frame-pack, which is the field most likely to produce an invalid
edit decision.

OCR was genuinely useful and multilingual, reading real signage:
`未来につながる森`, `Welcome to EXPO 2025`, `住友電工`, `xin chào / VIETNAM`,
and a cover-charge notice in English.

## Finding 5: suggested labels do not converge into a usable vocabulary

Across 12 clips the model proposed **51 labels, of which 48 were unique**. Only
three (`Public Transport`, `Commuting`, `Museum Exhibit`) ever recurred. Case
style was inconsistent: 33 Title Case, 18 lowercase; 32 of 51 were multi-word.

Extrapolated to 273 clips this produces on the order of a thousand
near-unique labels. The repo's labels are free-form strings, so nothing stops
this, and the result would be a label list that cannot be used for filtering or
faceting -- exactly the opposite of the PRD's goal.

This is the same class of problem as round 1's inconsistent language tags: the
output is **schema-valid but vocabulary-inconsistent**. Schema validation cannot
catch it.

**Implication:** pass the project's existing label vocabulary into the prompt
and instruct the model to prefer existing labels, proposing a new one only when
nothing fits. Normalize case on ingest, and treat a proposed *new* label as a
weaker suggestion than a reused one in the review UI.

## Finding 6 (important): a JSON schema does not guarantee valid JSON locally

`llama-mtmd-cli` accepts `--json-schema`, which compiles the schema into a GBNF
grammar and constrains sampling. That sounds like a guarantee. It is not.

The first local run produced **invalid JSON** despite the grammar. The grammar
kept the *syntax* legal, but the model entered a repetition loop inside an
unbounded array and ran into the token limit mid-array:

```
"person holding spoon", "person holding fork", "person holding knife",
"person holding chopsticks", "person holding plate", "person holding bowl",
"person holding spoon", "person holding fork", ...   <- truncated at -n
```

This is the same failure family as round 1's whisper repetition hallucination,
appearing again in a completely different model and modality.

**The fix is in the schema, not the prompt:** give every array a `maxItems`.
The grammar then *cannot* express an unbounded list, so the loop is
structurally impossible rather than discouraged. A repeat penalty helps but is
not a guarantee.

**Implication for the PRD's "invalid or incomplete model JSON is rejected":**
that requirement is right, and this finding shows why it must cover
**truncation** specifically. Bound every array in the schema, set a generous
token limit, and treat a truncated response as a rejected result with a
recorded reason -- never as a partial success.

## Finding 7: local CPU VLM works, but is slow, heavy, and much weaker at OCR

Qwen3-VL-4B-Instruct Q4_K_M + mmproj F16, llama.cpp CPU-only, 512 px frames,
4 threads, `--json-schema` with bounded arrays:

| Measure | Local (Qwen3-VL-4B, CPU) | Cloud (gemini-3-flash) |
| --- | --- | --- |
| Valid JSON | 3 / 3 (after `maxItems`) | 24 / 24 |
| Mean latency per clip | **41 s** | **4.8 s** |
| Peak RSS | **4.8 GB** | n/a |
| Model footprint on disk | 3.2 GB | 0 |
| Cost | $0 | $0.0041 / clip |
| Scene description quality | good | good |
| **OCR recall vs cloud** | **1 of 6 strings** | baseline |

The OCR gap is the decisive difference, and it matters for this product
specifically -- travel footage is full of station names, pavilion signage and
menus, and round 2 Finding 4 shows the cloud model reads them in Japanese,
Chinese and English. Local found `VIETNAM` and missed everything else,
including `未来につながる森` and the English cover-charge notice.

Local scene descriptions were, by contrast, perfectly reasonable ("a dish of
sliced cucumbers garnished with leaves on a rectangular plate").

Projected over the 273-clip project:

| Path | Wall time | Cost |
| --- | --- | --- |
| Local CPU, sequential | **~3.1 hours** | $0 |
| Cloud | ~22 min at 1 request at a time, less with concurrency | **~$1.12** |

4.8 GB peak RSS is also four times whisper's footprint and would contend with
the existing four-worker VideoToolbox proxy pipeline on a 24 GB machine.

**Implication:** make the cloud VLM the default and treat local as the
offline/privacy option, with an honest in-product note that local OCR is much
weaker. Local analysis must be a background job that never runs concurrently
with proxy generation, never an inline "analyze this clip now" action. A larger
local model would likely close the OCR gap but does not fit the 8.5 GB disk
headroom or the CPU budget on this machine.

Note this reverses the round-1 conclusion for STT, where local whisper was
cheap enough (5 s/window, 1.15 GB) to be the sensible default. **STT local,
VLM cloud** is the combination the measurements support.

---

# Recommended changes to the PRD

### 1. Build frame-packs from the proxy, and put representation identity in the manifest (Finding 2)

The PRD says the frame-pack comes from "the current proxy or online original".
Measured, those two produce different frame-packs, so "or" is not safe. Pick the
proxy: 44x faster, available offline, already generated.

The manifest's identity must therefore include the representation kind and
version (`proxy.h264-720p.orientation-v3`) and that file's fingerprint, not just
the frame-pack recipe key. A proxy regeneration must invalidate stored
frame-packs, exactly as the existing v2 -> v3 proxy transition would have.

### 2. Bound every array in the output schema (Finding 6)

`maxItems` on every array. Locally this is load-bearing: without it, a
repetition loop produced grammar-legal but truncated, unparseable JSON. With it,
the same clip returned valid JSON and ran ~40% faster. Treat truncation as an
explicit rejection reason in the provenance record.

### 3. Seed the label prompt with the project's existing vocabulary (Finding 5)

Otherwise labels do not converge -- 48 unique from 51 across 12 clips. Pass the
existing project labels, instruct the model to prefer them, normalize case on
ingest, and rank a reused label above a newly-invented one in the review UI.

### 4. Keep the negative controls as a permanent regression test (Finding 3)

Black, noise, and blurred variants of a real frame-pack. Round 1 showed the
audio path fails this test badly; round 2 shows the vision path currently
passes it. That is a property to protect across model and prompt changes, not a
one-time result.

### 5. Reconsider the "landmark candidates" field (Finding 3)

Zero landmarks were proposed across 12 real clips at a named venue. The field
is currently costing schema surface and prompt tokens for no output. Either
drop it, or supply trip context (the PRD already contemplates this) so the model
has something to anchor to.

# How this maps onto the existing architecture

Consistent with the round-1 conclusions and the AGENTS.md invariants:

- **Frame extraction is another managed FFmpeg child process**, like proxies and
  thumbnails. The frame-pack is a derived artifact in the project cache with a
  versioned recipe key.
- **Batch ETA**: weight by **frames**, not source bytes -- cost scales with
  frame count and image tokens, not clip size.
- **Suggestions never mutate annotations.** Accepted labels go through the
  existing typed label operation; an accepted thumbnail timestamp maps naturally
  onto the reserved thumbnail quality `10` ("VLM-chosen frame") already in the
  PRD; accepted trims create a new edit-plan revision.
- **Provenance**: model, prompt version, frame-pack recipe, representation
  identity, cost and runtime, per the PRD. Round 1 and round 2 both show why the
  rejection *reason* needs to be stored too.

# What round 2 did NOT establish

- **Small local sample.** The local VLM ran on **3 clips**; cloud on 12 real
  plus 12 controls. The OCR gap (1 of 6 strings) is directionally clear but
  not a precise recall figure.
- **One local model, one quantization, one cloud model.** A larger local model
  or a different quantization may close the OCR gap; untested, and constrained
  by 8.5 GB free disk.
- **No hand-labeled ground truth.** Cloud output is again a *silver* reference.
  Descriptions were checked for plausibility against the clips' human notes,
  not scored.
- **No transcript fusion.** The PRD wants the transcript passed to the VLM. The
  plumbing exists in the harness but was not exercised, so the claim that
  transcripts improve descriptions is untested.
- **No trip-context test**, which is the most likely fix for the empty
  landmark field.
- **No thermal or concurrency test.** The 4.8 GB local figure was measured
  alone, not alongside proxy generation, which is the situation that actually
  matters.
- **No accept/reject persistence work.** V5 from the round-1 proposal (results
  as derived artifacts, suggestions never mutating annotations) is still
  unexercised in both rounds -- it is design-complete but unbuilt.
- **Frame-pack recipe is a first draft.** It is deterministic and produces
  sensible frames, but no one has judged whether its picks are the *best* 3-8
  frames; that needs human review.

# Suggested next step

The measurements now support writing the implementation spec: local whisper.cpp
for STT, cloud VLM by default with a local fallback, frame-packs pinned to the
proxy, bounded schemas with domain validation, and a review surface where
nothing is applied without explicit acceptance.

The one open question worth another short spike before that spec is
**transcript fusion plus trip context** -- both are cheap to test on the
existing harness and both could change the shape of the prompt and the stored
result.
