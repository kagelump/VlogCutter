# Analysis run policy, authorization and capability pilots

Issue #120 adds a fail-closed preflight around paid analysis work. The policy
materializes what a proposed run would do; authorization and pilot observations
are separate records bound to that exact policy.

## Lifecycle

The lifecycle below describes existing VLM CLI/desktop batches. The bounded
native speech pilot is described separately below; native bulk and CLI/desktop
selection are not yet available.

Existing explicit CLI/desktop batch invocations remain usable: the core treats
that invocation as approval intent, materializes the exact concrete policy,
records authorization, runs the bounded pilot, and only then starts bulk work.
That automatic materialization is still one exact policy; it never turns a
single-asset/sample result into project-wide or different-model authorization.

1. **Describe** re-reads canonical SQLite ownership and source identity and
   emits one `RunPolicy` containing:
   - project ID and the exact project-owned asset IDs;
   - original source fingerprints, durations, representation fingerprints,
     sampled representation fingerprints, transcript fingerprints and the
     time-mapping identity;
   - explicit provider, primary model, complete model allowlist and permitted
     fallbacks;
   - payload selection (`frames`, `frames-and-transcript`, and explicit
     audio/transcript choices that currently fail readiness);
   - explicit prompt context: whether the exact current project vocabulary is
     attached, with its digest/count, and an explicit absence record for trip
     context. Frames-only requests never read or transmit a transcript, and the
     batch/pilot runners load vocabulary only when this approval is present;
   - expected cost, requested ceiling, cost basis and bounded readiness
     blockers;
   - prompt, output-schema, recipe and tool versions;
   - a deterministic provider cost-bound contract fingerprint over the exact
     registered context, completion and provider price ceilings used for
     admission and wire routing.

   Description is read-only and never resolves a key into the output. It may
   report only `credentialsPresent: true|false`.

2. **Authorize** re-materializes the policy, refuses any unsupported capability,
   source-readiness or budget problem, and stores the complete policy JSON under
   its content-addressed `policyId`. This is approval of scope/model/payload/
   budget. It is not a provider capability observation and performs no network
   request.

3. **Pilot** requires the exact authorization row. Local frame extraction uses
   the shared cancellable local-heavy lease, and cancellation is checked again
   immediately before transmission, so a pre-transmission cancel makes zero
   provider calls. It runs at most one bounded provider request against the
   first exact scoped asset, using the real frame pack or editorial source-time
   contract for the selected policy. The response is first parsed, then checked
   against the configured JSON Schema, then checked by the existing
   domain/source-time semantics. Only a schema-valid and semantically valid
   response with complete usage is `passed`. A passed pilot is persisted as
   **observed capability**, never as a promise of future availability, price or
   semantic correctness.

4. **Run** accepts only the stored `policyId`, re-materializes the policy, and
   refuses to proceed unless the policy digest is unchanged and its pilot is
   `passed`. The runner uses the policy's exact asset scope, model allowlist,
   fallbacks and payload. Pilot cost is deducted from the same authorized
   ceiling before bulk work begins.

Any change to scope/source identity, model, allowlist/fallback, payload, budget,
prompt/schema/recipe/tool versions, or static capability contract changes the
policy digest. Transient credential/model presence and readiness do not change
that digest.
Old approvals and pilots therefore stop matching and cannot be reused for a
broader or altered batch.

## SQLite provenance

Migration 37 adds:

- `analysis_run_authorizations(policy_id, project_id, policy_json, authorized_at)`
- `analysis_run_pilots(policy_id, project_id, status, record_json, started_at, finished_at)`
- `analysis_runs.policy_id`

Pilot records contain only bounded provenance: status, requested/actual model,
reported provider/generation IDs, attempted models, token counts, returned cost,
usage-completeness, stable failure code, structural/semantic validation
conclusions and a warning. VLM records contain no prompt/frame/transcript content.
Native records may additionally retain one explicitly partial, bounded typed
sample with accepted or review-required source-mapped candidates, plus local
NoAudio/NoSpeech facts. Rejected text is discarded. Neither kind retains raw audio
or response bodies, credentials, provider error text or absolute source paths.
An interrupted or unknown-usage pilot is recorded as `uncertain` and blocks
further provider calls until its receipt is reconciled. The operator inspects
the immutable request/receipt/event history, imports trusted late receipt
evidence, proves a marker-absent local failure, or explicitly retains an unknown
outcome. Reconciliation can refresh the existing pilot projection but cannot
invent a capability pass. An explicit retry flag alone cannot clear unknown
spending; a separate durable authorization for that particular request and
exact next epoch is required before one targeted paid dispatch. Retrying a fully
accounted failed pilot retains its known cost within the same immutable policy
ceiling.

## Provider cost-bound contract (#122 foundation)

This section describes the provider-side cost contract consumed by the canonical
#122 accounting owner. The full conservative bound is an input to durable
atomic admission, not a confirmed charge. Cross-process reservations, append-only
receipts and interrupted-request reconciliation are implemented in
[`analysis-accounting.md`](analysis-accounting.md).

For each registered OpenRouter model, the contract records:

- the public model context window and maximum completion length;
- routing ceilings in USD per million prompt/completion tokens, per image, and
  per request. `max_price.request` is one request fee, never a cap on the total
  request price;
- the exact `max_tokens` value sent on the wire (currently 1,500).

Public source evidence as of 2026-10-24 is the OpenRouter model metadata at
`https://openrouter.ai/api/v1/models` and the provider-routing documentation at
`https://github.com/OpenRouterTeam/docs/blob/main/guides/routing/provider-selection.mdx`.
The context/completion limits come from that public metadata. The prompt and
completion ceilings are explicit configured values at or above the highest
public context-tier metadata; the image and request ceilings are also explicit
configured routing limits. None of these are claims about current live prices.

`openrouter_request_cost_bound` computes a conservative admission bound from the
full model context window, that exact `max_tokens` value, the exact transmitted
frame count, and the separate price components. The same numeric ceilings are
sent to OpenRouter as `provider.max_price` with `require_parameters: true`.
Routing ceilings are configured limits, not claims about current live prices.
OpenRouter documents that `max_price` prevents a request from running when a
provider at or below the ceiling is unavailable.

The deterministic contract fingerprint is stored in `RunPolicy` and included in
its policy ID, so changing a context, completion or price ceiling invalidates
prior approvals rather than silently reusing them. Unregistered models and
missing, non-finite, negative or internally inconsistent bounds fail closed
before paid network I/O. The bound is only a reservation input: it is never
presented as a confirmed provider charge. Provider adherence to routing
metadata is assumed, arbitrary external invoice components are not controlled,
and a returned charge above the bound is retained as confirmed provider evidence
and durably stops further dispatch under that authority.

## JSON Schema dependency

Structural validation uses `jsonschema = 0.60.1` with
`default-features = false`. The validator is built with the crate's `.offline()`
retriever, so `$ref` resolution cannot fetch HTTP or filesystem resources.
This version requires Rust 1.85 or newer; the repository is verified with the
pinned Rust 1.97.1 toolchain. Existing small stable provider schema declarations
remain in the Rust domain modules. Schemars was not added because generating
schemas from Rust would not provide runtime validation and would replace stable
contracts for no functional gain.

Schema validity remains insufficient. The existing vision and moment domain
validators enforce explicit source-time names, millisecond units, inspected-span
containment, core/payoff ordering, frame-key grounding, dialogue-speech overlap
and edit-profile bounds. The test suite includes a schema-valid moment response
with inverted source times that the pilot refuses.

## CLI

```bash
# Describe a policy. No credentials, network or writes.
rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze preflight \
  --project <project-id> --asset <asset-a> --asset <asset-b> \
  --provider openrouter --primary-model qwen/qwen3.7-flash \
  --budget-usd 0.50 --payload frames-and-transcript

# Persist explicit approval for that exact policy.
rcdvlog ... analyze authorize ...same policy flags...  # add --project-vocabulary to approve vocabulary context

# Run one bounded pilot.
rcdvlog ... analyze pilot ...same policy flags...

# Execute only the stored exact policy and matching passed pilot.
rcdvlog ... analyze run --policy-id <policy-id>

# Inspect and recover interrupted paid requests without network I/O.
rcdvlog ... analyze accounting inspect --project <project-id>
rcdvlog ... analyze accounting reconcile --project <project-id> \
  --request <request-key> --receipt-file <path>
rcdvlog ... analyze accounting retry-authorize --project <project-id> \
  --request <request-key>
rcdvlog ... analyze accounting targeted-retry-dispatch --project <project-id> \
  --request <request-key> --expected-next-attempt-epoch <next> \
  --payload-file <original-vlm-request.json>

# Moment discovery uses the same lifetime:
rcdvlog ... moments plan ...      # describe, includes runPolicy
rcdvlog ... moments authorize ... # persist exact approval
rcdvlog ... moments pilot ...     # bounded editorial/pilot request
rcdvlog ... moments run --policy-id <policy-id>
```

The legacy `analyze missing`, `moments discover` and preparation paths remain
available, but policy-bound execution is the canonical paid boundary and is the
only path that carries a matching approval and pilot into bulk work.

## Native speech proposal (B2, not enabled)

`AnalysisRunKind::NativeSpeechTranscription` / serialized
`native_speech_transcription` is now representable in the existing `RunPolicy`
owner as a proposal-only native cloud-speech choice. The common
`RunPolicyRequest` provider/model/allowlist/budget/exact-asset fields remain
authoritative; the native parameters carry only an optional lowercase ISO 639-1
language, the supported `zdr`/`data_collection` data-policy fields, and `force`.
The provider/model pair must resolve the exact registered native
`openrouter` + `openai/whisper-1` audio-transcription contract. There is no
default, chat-slug inference, fallback or extra allowlist, and native requests
always canonicalize to the `audio` payload. Project vocabulary, transcripts,
frames, prompts and trip context are never attached or bound by a native
proposal. Unsupported provider routing such as `order`, `only`, `ignore` or
`max_price`, and unknown parameter fields, fail during strict deserialization
rather than being silently dropped.

Describe reuses the existing ownership, excluded/duplicate, exact
`RunAssetScope`, derived-proxy fingerprint, freshness and source-identity
helpers. An offline original with a usable current derived proxy remains valid;
a missing/stale proxy is blocked. The proposal binds the native endpoint/window/
rate contract via `native_stt_cost_bound_contract_id()`, the shared Silero
VAD/packing recipe, fixed protocol version labels, language, data policy, exact
scope and budget. Its `recipeVersion` comes from the transcript owner's single
`native_stt_transcript_recipe()` helper for a validated registered request: a
`native-stt.transcript-recipe.v1:<blake3>` identity over the explicit
provider/model, language, provider data policy, fixed temperature, registered
endpoint/response/timestamp/input/window semantics, shared Silero VAD artifact,
packing algorithm and native quality-recipe version. Budget, tariff and asset
scope stay in the run-policy identity but not in the transcript cache recipe, so
budget/scope approval changes do not create a false transcript-cache change.
Quality or native protocol/config semantic changes do change the recipe and thus
future approval identity. Invalid or unregistered proposals keep a stable
`:unregistered` placeholder and their structured blockers; they never acquire a
valid recipe, authorization or runtime capability. Historical native approval,
receipt and accounting rows are not reset. Native expected cost uses the
registered full-window duration
tariff and the conservative pre-VAD source-duration window upper bound
`floor(2 * duration_ms / 30_000) + 1`; it never uses the VLM reference clip
estimate or constructs a fake audio request. The cost basis and warnings state
that the external duration tariff is not a wire-enforced hard invoice cap and
that above-bound or unknown usage remains owned by receipt settlement.

B2 is deliberately blocked: a failed `native_stt.accounted_runtime` capability
and `native_accounted_runtime_unavailable` blocker keep both
`ready_for_authorization` and `ready_for_pilot` false. `authorize_run_policy`
therefore refuses native proposals before any authorization, run or budget row
is written, and existing VLM pilot/bulk/recovery/provider-boundary entry points
reject the kind before local preparation, credentials or a provider call. A
future accounted native runtime must replace that blocker with its own shared
reservation/dispatch/receipt boundary; the registered protocol check is not an
observed pilot.

## Boundaries and non-goals

- No cloud audio/STT production backend is wired here. A protocol-only native
  transcription leaf exists under `analysis/openrouter_stt.rs`, but it is not
  selectable from CLI/desktop/preparation and has no accounting authorization
  path yet. Native speech policies can be described with an explicit
  provider/model contract but remain blocked by
  `native_accounted_runtime_unavailable`; issue #119 remains open until the
  accounted runtime and adapters exist.
- Concurrent atomic full-bound reservations, append-only receipt reconciliation
  and explicit targeted retry are owned by #122's canonical catalog accounting.
  No second general-purpose spend ledger, cloud scheduler or provider invoice
  query is introduced.
- The pilot is a synthetic/public-fixture capability check. It does not replace
  human editorial review and it does not claim that a prompt bypasses external
  approval review.
- Original media and human-authored fields remain immutable. Pilot frame
  extraction writes only to the managed project cache.

Native speech cost proposals reserve a conservative pre-VAD upper bound for bulk speech windows plus one separate capability-pilot window. Each window reserves the full registered 30-second duration; pilot and bulk spending use the same eventual policy ceiling. No pilot or bulk call is enabled by a proposal in this slice.

C4A binds native approval/cache identity to the deterministic recipe above. C4B adds migration 43 and the read-only canonical transcript-to-accounting provenance projection. Native high-level pilot/bulk orchestration, VAD extraction/model installation, typed native targeted retry, backend selectors and CLI/desktop adapters remain deferred; no native execution is enabled by this change and `ready_for_pilot` stays false.


## Bounded native speech pilot

Core callers explicitly select `RunPolicyRequest::native_speech_transcription`
and authorize that exact proposal before using
`Catalog::run_native_speech_pilot` or its injected-transport/cancellation variant.
The generic core pilot entrypoint routes an explicitly native kind to this path;
it never converts native audio into a VLM request. Local/offline transcription
remains the default, and these core APIs do not imply native CLI/desktop or bulk
support.

The pilot scans selected proxies with FFprobe and local Silero VAD, sends at most
one 30-second packed speech window, and keeps a typed partial result under the
existing pilot record. A passed native pilot observes protocol/source mapping
with complete accounting usage; it is not a full-asset transcript or an accuracy
claim. Candidates with insufficient confidence evidence remain explicitly
review-required and unaccepted. Missing actual serving identities remain unknown.
A different, unverified returned model identifier requires review rather than an
assumed alias match. Requested broker and serving upstream are distinct fields.

All selected sources with verified local NoAudio/NoSpeech facts produce
`no_provider_required`, with no paid request and `capabilityObserved: false`.
This does not unlock bulk. NoAudio requires no VAD model; successful VAD silence
on an audio stream does. Native execution defers secure credential resolution
until an actual speech transmission needs local transport preparation. Missing
credentials/model files affect paid-pilot readiness and carry explicit
remediation without changing the approved policy identity.

Receipts settle before cached domain results and post-response cancellation.
Unknown usage remains uncertain; repeated invocation, force or a generic retry
flag cannot blindly resend prior native paid work. Receipt-only interruptions
require explicit reconciliation and later targeted recovery support.
