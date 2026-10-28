# Analysis request accounting: admission and receipt reconciliation

This document describes the canonical persistence foundation and the runtime
integrations added for issue #122. The authorized run-policy pilot, bulk
observation, policy-bound moment discovery, chronology suggestions and label
consolidation, including transport and domain-validation fallback, are wired
through the accounted provider boundary. CLI inspection, typed reconciliation
and explicitly authorized targeted retry are now thin adapters over the same
canonical owner. Process-interruption recovery is verified with synthetic
transports; no live OpenRouter/native acceptance is claimed by this slice.

Native speech uses the same private admission -> prepare -> domain guard ->
dispatch marker -> one send -> receipt lifecycle as VLM requests. The core now
implements a bounded native capability pilot using selected proxy audio, local
FFprobe/FFmpeg and standalone whisper-rs Silero VAD. Native bulk, targeted retry
and CLI/desktop native selection remain separate follow-up work.

The pilot scans the explicitly authorized scope until the first speech asset and
sends only its first bounded window. It stores that partial typed result in the
existing SQLite pilot record before finalization or post-response cancellation;
it does not create a full-asset transcript. Exact prepared PCM/source-map and
canonical request/receipt proof allow later reuse. Known bad output is rejected,
missing comparable quality evidence remains review-required and unaccepted, and
unknown cost keeps the pilot uncertain. A default repeat cannot resend a cached
sample or bypass receipt-only interrupted work.

Migration 44 adds the explicit `no_provider_required` pilot status for a complete
local NoAudio/NoSpeech scope. It requires zero requests and zero unresolved ledger
usage, records no observed endpoint capability and does not authorize bulk.
NoAudio requires no model; detecting silence in audio-bearing clips still needs
VAD. Provider credentials are resolved by the native transport only when speech
reaches pre-dispatch preparation. Local preparation releases the heavy lease
before cloud I/O and keeps source/speech claims through result persistence.

## Native transcript provenance (C4B slice)

Migration 43 extends the existing canonical transcript owner with a nullable
native-policy association and native quality-recipe identity, plus
`native_transcript_windows`, which links one transcript to the exact accounting
request for each bounded provider window. Child rows store only window order,
measured latency, recognized reported language and a bounded typed quality
decision/reason/metric-coverage JSON. They store no raw audio, request/response
body, prompt, path, candidate-text copy, monetary counter, or second
provider/model identity.

`Catalog::native_transcript_provenance(asset_id)` is a read-only projection over
the canonical header, the frozen run authorization and the same accounting
request/event/receipt/attempt-history rows. All projection reads share one SQLite
read transaction. Requested provider/model/language/data policy/source come only
from the frozen policy; actual provider/model/generation/cost and unresolved
reserves come only from current accounting evidence. Requested broker and actual
serving upstream remain distinct canonical fields; missing actual identities stay
unknown and known differences remain visible. A late receipt reconciliation
therefore changes the next read projection without rewriting or resending the
transcript. Prior retry epochs remain visible from the same immutable attempt
history.

Historical reads validate the artifact against its own frozen recipe and source,
without requiring the current tariff or applying current quality thresholds to
an older quality recipe. Recorded decisions and finite metric facts remain
inspectable under their recorded recipe; inspection does not make them current
or authorize another request. New writes still require current quality rules.

The transcript owner provides one deterministic native recipe helper binding the
explicit provider-qualified request, registered endpoint/timestamp/window
semantics, shared Silero VAD artifact and packing identity, and native quality
recipe version. Native `RunPolicy.recipe_version` and the stored transcript
`recipe_key` must equal that helper's identity. Ordinary local saves clear any
prior native association atomically, and native saves validate the frozen
policy, exact asset/source scope, owned accounted run and settled or honestly
uncertain receipts inside the same `BEGIN IMMEDIATE` transaction that replaces
the header, segments and window links. The bounded core native pilot is wired;
full native asset transcription, native bulk and CLI/desktop selection remain
unavailable. Public policy readiness checks only the VAD artifact and credentials
for paid native pilots; transient prerequisite presence does not change approval
identity.

## Canonical owner

The project SQLite catalog remains the single accounting owner. Migrations 38
through 40 add child rows and the run accounting marker beneath the existing
`analysis_runs`/run-policy ownership:

- `analysis_budget_authorities` stores exactly one frozen ceiling, policy and
  provider identity, historical evidence, and dispatch-stop state per canonical
  project/policy or project/operation scope. The row is immutable.
- `analysis_request_reservations` stores intended-request identity, policy,
  payload/source/cost-bound fingerprints, lifecycle, conservative admission
  bound, separately returned cost, confirmed cost, and unresolved-usage reserve.
- `analysis_request_events` is append-only lifecycle and evidence history.
- `analysis_provider_receipts` is append-only provider evidence. Each receipt
  has its own stored fingerprint, so exact repeated reconciliation is a no-op.
  Migration 39 indexes generation IDs and enforces that a non-empty generation
  ID belongs to one request per requested router/service, independent of the
  optional upstream provider metadata.

There is no JSON ledger, second database, scheduler or competing paid balance.
Existing run and pilot summaries remain historical evidence or projections, not
independent live ceilings.

## Published token estimates (schema 57)

The ledger can retain an explicitly registered published token tariff separately
from a provider-reported charge. This is the accounting prerequisite for the
direct-video transport in issue 176; it does not itself enable a native upload or
a new authorization path. Existing VLM and native-speech adapters keep their
original receipt rules and reject a published token quote at their boundary.

`AnalysisRequestCostBound.publishedTokenTariff`, when present, binds the exact
version, provider, model, service tier, published input/cache/output rates and
price source into the cost-bound fingerprint. Registered versions are immutable;
add a new version when rates change, preserving historical definitions. The first
registration is direct Gemini 3.5 Flash-Lite Standard/Flex using Google's
[published pricing](https://ai.google.dev/gemini-api/docs/pricing) verified on
2026-10-28. Those rates are external assumptions, not a wire-enforced invoice cap.

A receipt's optional `usagePricing` carries reported prompt, cached-prompt,
candidate, thinking, tool-prompt and total token counts, reported tier, latency and
tariff fingerprint. Every required count must be present and consistent, with
cached input no greater than prompt input, checked integer sums, and input/output
within the admitted caps. The actual provider/model/tier must match the frozen
contract. Thinking is billed with output; cached tokens receive only the returned
cache credit. A repeated request never proves a cache hit.

A usable priced receipt completes the request with `estimatedCostUsd`, while
`reportedCostUsd` stays absent and `confirmedCostUsd` stays zero. Requested/actual
identity and all usage evidence remain in immutable receipt events. Unknown or
contradictory usage keeps the full unresolved reservation. The amount is a
published-rate estimate, never a provider invoice or a listening-quality result.

Admission, snapshots, operation/pilot projections and aggregate totals include
estimated spend exactly once alongside confirmed charges, active reservations
and uncertainty. A later complete provider-reported charge replaces the current
estimate; immutable earlier receipt events retain the old estimate. The resulting
charge can exceed its admission bound, stop dispatch and exhaust the budget.
Once an actual charge is confirmed, conflicting replacement charges are refused.
Metadata-only enrichment cannot erase an estimate or a prior reported cost floor.

Historical quotes/receipts omit the optional new fields, preserving their existing
fingerprints. Zero estimate totals are omitted from old projection JSON. Read-only
schema-56 diagnostics use an absent estimate projection without migrating, while
writable upgrades preserve existing accounting rows.

## Required C call ordering

Before runtime creates the analysis run or pilot rows for a new attempt, C must
call `Catalog::initialize_analysis_accounting_authority` with the project/policy
(or project/operation) authority context. The call freezes pre-ledger evidence
once and must precede rows that belong to the new runtime attempt; otherwise a
first reservation could misclassify those fresh rows as unknown legacy migration
spend.

The intended order is:

1. initialize and freeze the canonical authority;
2. create the runtime-owned `analysis_runs`/pilot rows;
3. reserve the complete provider-owned request cost bound;
4. commit the durable dispatch marker immediately before transport I/O;
5. reconcile immutable provider evidence, release only when the marker is
   absent and local pre-upload failure is proven, or retain uncertainty.

The authority API takes no request payload fingerprint and does not invent one.
Request-specific model, source, run-ownership and payload checks are separate
and happen in reservation.

## C1 accounted pilot boundary

`analysis::accounting::AccountedVlmProvider` wraps exactly one provider
transmission. Passing that wrapper into the existing `ProviderChain` means each
primary, transport-fallback and domain-invalid-fallback attempt admits, reserves
and receipts independently. The pilot runtime phase order is:

1. validate the exact authorized policy and prior-pilot state;
2. initialize/freeze the canonical authority before creating any new run row;
3. prepare local frames/transcript/prompt without ledger admission or I/O;
4. open the owned `analysis_runs` row with the accounting-contract marker;
5. compute the full provider-owned cost bound and reserve it in one `IMMEDIATE`
   transaction;
6. run local provider preparation and cancellation checks;
7. commit the dispatch marker immediately before the one transport call;
8. record immutable receipt evidence before domain/schema validation;
9. project attempted models and tokens from durable dispatch/receipt rows, and
   project cost from the canonical accounting aggregate.

A missing real run id is a typed `LocalAdmission` denial, not an unclassified
provider error. Local admission, configuration, pre-upload, post-dispatch and
cancellation categories stop the chain without consuming a fallback or poisoning
provider health. Diagnostics derive only from the typed category and numeric
HTTP status; caller/provider exception text is never marked safe or persisted.
A post-dispatch receipt-persistence failure records uncertainty and asks for
manual reconciliation; it never claims the receipt was retained. SQLite
transactions never span provider I/O.

A reservation is released only when the dispatch marker is absent and a local
pre-upload failure or cancellation is proven. Once the marker exists, a generic
error code, process exit or local cleanup cannot prove no upload, so the request
remains active/uncertain until supported reconciliation. An explicit retry for
the same intended request requires the caller's consideration flag and a durable
authorization for that exact prior request and next epoch.

## C2 bulk observation runtime boundary

`Catalog::analyze_authorized_with_provider_and_workers` verifies the existing
whole-policy authority before creating an accounted analysis run, then opens
that run with the real run id and policy marker. The run no longer takes its
ceiling from a live remaining-balance projection: caller-visible options are
checked against the policy's immutable `budget_usd`, while atomic admission
recomputes confirmed, active, and uncertain exposure from the one canonical
authority.

Each pending asset is prepared locally first. Immediately before provider I/O,
the worker constructs a fresh `AccountedVlmProvider` with the real run id,
policy identity, authorized source identity, operation, and a restart-stable
asset-level intended-request key. The existing `ProviderChain` receives that
wrapper, so primary, transport fallback, and domain-invalid fallback each get an
independent reservation and immutable receipt. Local frame extraction,
credential/configuration, cancellation, and admission failures happen before a
dispatch marker and never consume a paid fallback. Cancellation is checked
again immediately before each primary/fallback transmission.

Budget exhaustion or another local admission denial stops claiming queued work
without cancelling requests that already reserved their full cost. An explicit
caller cancellation separately stops queued work and reaches the pre-dispatch
cancellation checks. This preserves the admitted request when a losing worker's
denial arrives before it dispatches; the same transactional authority still
rejects unaffordable requests and settles every completed receipt.

Report `cost_usd` comes from the canonical current-run projection and therefore
contains confirmed receipts only; provider-returned amounts stay in outcomes,
and reservations/uncertainty stay separate on the canonical authority. Worker
count is capped at the authority's in-flight bound. Unresolved or interrupted
usage blocks a later bulk run, and the same asset-level intended request cannot
be blindly replayed merely by starting a new run.

## C2 moment-discovery runtime boundary

Policy-bound moment discovery verifies the same frozen authority before opening
an accounted run. Its provider-injectable boundary rejects caller mismatch in
scope, payload, model/fallback, whole budget, parameters, or source identity
rather than silently replacing the fields. The moment pipeline then creates a
fresh accounted wrapper for every actual step transmission, using the real run
id, asset source identity, and a step key derived from phase, span, samples, and
the seed candidate. Multiple assets and multiple coarse/refinement windows
therefore receive distinct durable request identities, while cached completed
steps remain reusable without a new request.

Step retries remain governed by the existing `retry_uncertain` consideration
flag plus durable per-request authorization. A cache or force flag alone cannot
authorize a new epoch after an interrupted/uncertain paid attempt. Report and
run cost projections use the same current-run canonical confirmed spend as bulk
observation; the moment store's per-step receipts remain cached content
provenance, not a competing balance.

The unaccounted moment pipeline is compiled only under `#[cfg(test)]` for
focused domain tests. Production entry points always supply an authorized policy
and therefore cannot silently bypass the accounted boundary.

## Strict admission and exposure bound

Reservation runs in a SQLite `BEGIN IMMEDIATE` transaction. It revalidates the
canonical project, policy authorization, requested provider, model allowlist,
source scope and run ownership, then recomputes one balance:

```
committed = confirmed charges + active reservations + unresolved uncertainty
```

The complete conservative provider-owned bound must fit the remaining ceiling.
It is never clipped to available budget. At most four requests may be active per
authority. This document does not claim a finite software dollar guarantee
against arbitrary external provider invoice deviations: reservation is strict
under the provider cost-bound contract assumptions. An external charge outside
that contract remains an external fact that receipt reconciliation retains and
exposes, never a reservation estimate.

## Interruption and receipt settlement

The durable dispatch marker is committed before transport I/O. Missing marker is
the only local no-upload proof. Once the marker exists, a generic failure code,
process exit or local cleanup failure cannot authorize release. A dispatched or
crashed request stays active/uncertain until a supported reconciliation call.

Receipt settlement runs in one `BEGIN IMMEDIATE` transaction. It validates the
request lifecycle and known provider/model/generation identities, appends the
immutable receipt and event, and updates the single confirmed/uncertain
projection. Reserved and locally released requests can never become charged
completions.

A prior incomplete returned cost is not a final charge. A later complete receipt
with the same known identity may correct the reported amount; both receipts stay
in history and the final charge is confirmed once. Incomplete updates retain
`max(previous uncertainty, original admission bound, new reported floor)` and
therefore cannot shrink uncertainty. Missing cost, model or provider metadata
remains unknown rather than being inferred from the requested route or model.
Receipt identities must have no surrounding whitespace, so differently padded
generation IDs cannot bypass service-scoped ownership. Complete metadata may be filled once by later matching evidence; exact repeated
evidence is a no-op, while conflicting known identities or already-confirmed
charges are rejected.

An observed returned cost above the request bound is persisted honestly, stops
further dispatch on the authority, and is never represented as a reservation or
estimate.

## Explicit retries and unknown outcomes

An exact request identity cannot be dispatched twice by default. A retry requires
an explicit durable authorization event for the immediately prior request and
its exact next epoch, is bounded to four attempts, and is fenced by the SQLite
`IMMEDIATE` transaction plus the unique `(attempt_base_key, attempt_epoch)`
constraint. A caller-side flag alone never grants a paid retry. Prior receipts,
confirmed charges and uncertainty remain in history.

`AnalysisRequestReconciliation::UncertainOutcome` is the supported conservative
reconciliation for a crashed dispatched request. It retains uncertainty and does
not mark the request as a local no-upload release. Reconciliation never
automatically resends a potentially completed paid request.

## Canonical aggregates

`Catalog::analysis_accounting_aggregate` is the whole-policy projection: it
includes all operations and the frozen legacy evidence exactly once. The
optional-operation API (for example
`analysis_accounting_aggregate_for_operation(policy_id, "pilot", run_id)`) lets a
pilot projection include only pilot attempts, including primary, fallback and
prior-run attempts and provable frozen legacy pilot evidence, while excluding
bulk charges. Frozen generic bulk summaries remain in the whole-policy budget;
they cannot be attributed to a more specific modern operation.

Confirmed returned cost, active reservation and uncertain reserve remain separate
fields. Current-run provenance is selected by append-only
`analysis_request_events.event_id` receipt insertion order after filtering to
completed requests whose `run_id` equals the caller's current run. Wall-clock
timestamp ties, clock skew and receipt-key ordering therefore cannot select an
earlier primary attempt over the actual final fallback, and a late receipt from
an older run adds its charge without overwriting current-run provenance.
Missing actual provider identity stays `None`; it is never inferred from the
requested route. Token projections likewise use the latest token-bearing receipt
event, so metadata-only enrichment cannot erase known usage or multiply tokens.
The projection is not a second balance: run-policy state is updated from this
canonical calculation.

## Legacy evidence

Authorizations created before migration 38 have an empty
`accounting_contract` and require explicit reauthorization. Reauthorization
upgrades that contract, but it does not edit or erase a frozen authority and does
not unblock an already-stopped authority.

`analysis_runs` has no usage-completeness column. Therefore a terminal analysis
run status, including completed/failed/cancelled, never turns its aggregate cost
into a confirmed provider charge. Its known non-negative cost is retained as a
conservative unresolved-usage reserve and the authority stops further admission.
A valid historical pilot record with explicit `usageComplete=true` may contribute
its known full charge once; its pilot-stage run row is excluded to avoid double
counting. Incomplete pilots, orphaned pilot-stage runs, missing/negative/
non-finite amounts and unreadable pilot JSON all remain unknown usage rather than
zero-charge proof. Original run and pilot history is preserved and frozen with
the authority.


## Compatibility with spending after an authority snapshot

Migration 40 extends the existing analysis-run owner with an accounting-contract
marker. Every accounted pilot, bulk, moment and non-policy operation run sets
that marker only after initializing its budget authority. A marked zero-request
cancelled run does not invent unknown paid usage. The marker alone is not a
confirmed charge; it only distinguishes ledger-owned lifecycle rows from
untracked legacy summaries.

An older runtime may have written run or pilot spending after an authority was
initialized by migrations 38/39. Admission and dispatch recheck that history in
an IMMEDIATE transaction. Newly observed unknown floors and the dispatch stop
commit before returning a denial. Append-only evidence belongs to the same
Catalog/accounting owner and is included separately in canonical projections;
it never rewrites the frozen legacy authority or claims a confirmed charge.

Pilot summaries own their represented run's floor. Frozen amounts receive credit
once, repeated observations append only increases, and independent runs keep
separate evidence. Operation compatibility scans unlinked chronology cache rows
and unmarked operation runs independently, so presence of one owner cannot hide
the other. Frozen operation entries carry the same per-owner base key and floor
credit used by later delta appends; a later cost increase therefore adds only the
increase and a repeated observation adds nothing. Unknown zero-cost legacy work
remains unknown; unreadable frozen pilot records retain their prior reserve.
Direct evidence deletion is rejected while project-owned cleanup still cascades.

## Non-policy operation authorities

Chronology suggestions and label consolidation are not RunPolicy-owned, so they
use the same canonical authority machinery with a project/operation scope such as
`operation:<project>:chronology_suggestions`. The authority fingerprint binds the
provider cost contract, requested route, model allowlist/fallback settings,
operation recipe and finite budget. It intentionally excludes the run id and
per-request payload, so a new process cannot manufacture a fresh ceiling merely
by creating a new run or request identity.

Each actual transmission, including a transport fallback or a domain-invalid
fallback, crosses `AccountedVlmProvider` independently. The durable dispatch
marker is committed before provider I/O, and the receipt is committed before the
adapter parses or validates response JSON. Consequently a paid invalid response
still leaves a confirmed receipt and cannot be blindly resent. Local cancellation,
credential/preparation failure and admission denial release only while the
dispatch marker is absent and do not consume a fallback attempt.

Chronology keeps its original result-cache key solely as result identity. For a
new request the canonical ledger is the only paid in-flight permit: no pending
cache row is created before admission. After provider evidence and domain review,
the canonical provenance (confirmed current-run cost, separate uncertainty,
actual provider/model/generation, run id and request keys) is populated first.
The completed cache row and one append-only `cache_result` link for every ledger
attempt are then written in one `IMMEDIATE` transaction, so neither can exist
without the other. Persisting either side cannot expose a cache row as unlinked
legacy spending. A completed cache row is reused without a paid call. Historical
running/interrupted cache rows remain visible legacy unknown evidence and block a
blind resend, but they are not a second permit for new work. Existing legacy
cache costs are collected once by the same compatibility owner. A completed old
row counts as confirmed only when its provenance explicitly proves complete usage
and it has a known non-negative cost. Missing completeness, unreadable entries,
running rows and malformed amounts remain unknown-usage reserves and stop further
dispatch.

Operation reports expose confirmed current-run cost separately from active
reservations and uncertain-usage reserves. Actual provider/model/generation
identity is read exclusively from receipt evidence; missing identity stays
`None`/empty rather than being inferred from the requested route.

## Operator recovery commands

The CLI exposes the canonical owner without a second ledger or scheduler:

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze accounting inspect \
  --project <project-id> --policy <policy-id> --operation pilot \
  --run <run-id> --request <request-key> --limit 100 --offset 0

rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze accounting reconcile \
  --project <project-id> --request <request-key> \
  --receipt-file /absolute/receipt-evidence.json

rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze accounting reconcile \
  --project <project-id> --request <request-key> --local-no-upload \
  --failure-code local_extraction_failed

rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze accounting reconcile \
  --project <project-id> --request <request-key> --unknown-outcome

rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze accounting retry-authorize \
  --project <project-id> --request <request-key>

rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze accounting targeted-retry-dispatch \
  --project <project-id> --request <request-key> \
  --expected-next-attempt-epoch 1 \
  --payload-file /absolute/original-vlm-request.json
```

`inspect` opens SQLite read-only and filters in exact SQL scope. Request/run
filters return only their matching budget scope or scopes. It returns canonical
budget snapshots with confirmed, active-reserved and uncertain-usage
amounts separate, plus each selected request's append-only events, provider
receipts and full prior-attempt history. Actual provider/model/generation values
come only from immutable receipt evidence. Prompts, frame bytes and provider
bodies are not accounting columns.

`reconcile` performs no provider I/O and accepts exactly one evidence mode.
`--receipt-file` imports trusted typed user-provided receipt metadata bounded to
256 KiB.
`--local-no-upload` is accepted only when the durable dispatch marker is absent;
the failure category must itself be typed local evidence. `--unknown-outcome`
retains the conservative reserve when evidence is insufficient. Repeating the
same receipt or failure evidence is idempotent and does not grow history. A
receipt can complete an interrupted request but can never invent pilot
capability/domain validation. When the request belongs to the currently recorded
pilot run, the existing pilot projection is refreshed conservatively: known
usage is kept, a crashed `running` pilot becomes failed/review-required if usage
is settled or uncertain if it is not, and a late receipt from an older run does
not replace current-run provenance.

`retry-authorize` durably authorizes exactly the particular prior request's next
epoch. It is deliberately separate from paid dispatch and cannot be replaced by
a generic force flag. `targeted-retry-dispatch` then requires the expected epoch,
the exact original `VlmRequest` JSON payload file and a real new owned analysis
run. The payload file format is the camel-case `VlmRequest` shape:

```json
{"prompt":"...","frames":["data:image/jpeg;base64,..."],"schema":{"type":"object"}}
```

The operator must supply the original payload; it cannot be reconstructed from
the stored fingerprint. Before resolving credentials or constructing a
transport, the core re-proves project/request ownership, the latest attempt and
durable authorization, the expected next epoch (four attempts including the
first), the unchanged policy/source/vocabulary when policy-owned, and the exact
payload/model fingerprints. Both fresh and targeted admission call the same
canonical owner validator for active authorization/accounting contract,
project, policy/provider, model allowlist, approved source scope, whole budget
and real running owned run. The production wrapper rejects a requested route
other than its actual OpenRouter route before constructing a client. After
`provider.prepare` and immediately before the dispatch marker, the current
policy/source/vocabulary proof is re-run; a provider that mutates local evidence
is denied with zero transmission and a marker-absent release. It then compares
the current provider-owned full cost bound fingerprint to the immutable prior
contract. Any mismatch fails closed before a new run or paid call. Admission
reuses the original
`attempt_base_key`, consumes only the authorized next epoch atomically, retains
all earlier confirmed and uncertain usage, and applies the same in-flight cap
and whole-budget ceiling. A stale/concurrent consumer is denied.

The targeted retry performs exactly one original-model transmission through
`AccountedVlmProvider`; it never repeats the original domain's whole fallback
chain. The returned provider body is present only in that command's JSON result.
It is never stored in SQLite, events, logs, human actions or diagnostics, and it
is not automatically applied to clip fields, labels, chronology, moments or
analysis caches. Domain review remains an explicit separate action. A thin
cancellation-aware core entry point checks before local preparation, before the
quote/reservation/prepare phases and again through the existing accounted
boundary before the wire. Cancellation before the dispatch marker releases only
the absent-marker reservation; after the wire, known receipts are retained and
unknown usage remains uncertain.

## Production transport call-site audit

Every production paid path reaches a raw `VlmProvider::analyze` only through
`AccountedVlmProvider`: run-policy pilots and bulk observation, bulk/authorized
moment discovery, chronology suggestions, label consolidation and the new
targeted-retry entry point. The moments unaccounted pipeline is compiled only
under `#[cfg(test)]`; synthetic fake providers in domain unit tests are not
production call sites. This slice adds no alternate network implementation or
credential/base-URL test flag.

## Remaining acceptance boundary

The canonical implementation and offline process-interruption tests are
complete for this slice. Live OpenRouter streaming/native interruption and
provider-receipt retrieval are not claimed by these tests: receipt evidence is
explicitly user-provided, and inspection/reconciliation never query or resend a
potentially completed paid request. Any future adapter-specific batch helper
that reaches a paid transmission without a real owned run id, source/payload
identity and whole-authority context must be wired through this owner before it
can be a supported paid path.

## Native STT audio reservation contract (B1, not wired)

B1 adds one optional `AnalysisAudioCostBound` component to the existing
`AnalysisRequestCostBound`. It carries the actual bounded speech-window duration,
the conservative full-window billable-seconds maximum, and the approved
duration rate in USD/second. Native audio bounds use zero prompt, completion and
image components; the existing canonical reservation table remains the only
balance and continues to admit or deny the quote's whole `maximum_cost_usd`
without clipping. A VLM quote omits the optional audio field during serialization,
so prior VLM JSON and cost-bound fingerprints are byte-identical.

`native_stt_request_cost_bound` is owner-local to the verified OpenRouter STT
registry. It accepts only validated `NativeSttRequest`/`NativeSttAudioWindow`
values and reserves the full registered 30-second window at the approved
external-tariff ceiling, including when the actual speech window is shorter.
The deterministic contract identity binds the registered provider, model,
endpoint, window limit, integer micro-USD/second ceiling and billing semantics;
route, window or rate drift therefore invalidates an old approval.

The native STT endpoint does not enforce `provider.max_price`, `order`, `only`
or `ignore`. This quote is an admission reservation, not a hard invoice cap and
not a provider charge. A tariff drift or external invoice above the reservation
remains an external fact: existing receipt settlement records it, retains unknown
usage and stops the authority. No new billing account, price cap, ledger or
provider retry path is introduced here.

`analysis_native_stt_payload_fingerprint` hashes the exact output of the
existing validated native request-body builder, the registered endpoint/model,
and the complete packed-window source mapping. Raw base64 WAV remains transient
hash input only; persisted request rows carry only the bounded content-free
digest. Changed audio, language, model/provider policy, temperature, endpoint or
window mapping changes the fingerprint. Production cloud selection, dispatch,
CLI/desktop wiring and native retry recovery remain intentionally disconnected
in this slice.

## Native STT policy authorization (B2, superseded runtime blocker)

B2 extends the existing `RunPolicy` owner with the explicit
`native_speech_transcription` kind. A native proposal binds the exact scoped
asset/source identities, provider-qualified native endpoint/model contract,
optional language, supported `zdr`/`data_collection` provider policy, local
Silero/packing recipe, full-window tariff identity and budget. Its expected cost
is windows x the registered full 30-second window x the registered external
duration rate; before VAD runs, the window count is the conservative source
duration upper bound `floor(2 * duration_ms / 30_000) + 1`. This estimate is not
the VLM reference clip cost and is not a provider invoice cap.

B2 originally kept the proposal blocked with
`native_accounted_runtime_unavailable` and a failed
`native_stt.accounted_runtime` capability. B3 supersedes that fact only because
the shared accounted native request leaf now exists: real
`Catalog::authorize_run_policy` can persist the exact native policy, and a
caller can initialize the existing authority and create a real running
accounted run before invoking the leaf. High-level native pilot/bulk
orchestration is still absent, so `ready_for_pilot` remains false and an
explicit native-pilot-unavailable warning is reported separately from
provider/model authorization.

## Native STT accounted pilot leaf (B3)

`AccountedNativeSttProvider` owns exactly one explicitly supplied bounded
`NativeSttAudioWindow` for an authorized native policy. It is restricted to the
existing `pilot` operation and a stable `native_speech_transcription:<asset>`
intended-attempt key. The caller initializes the whole-policy authority before
creating the owned `analysis_runs` row; the adapter does not create budgets,
runs, scopes, credentials, retries or fallback chains.

The native quote and payload identity are always computed by the existing
`native_stt_request_cost_bound` and
`analysis_native_stt_payload_fingerprint` owners. Admission calls the canonical
SQLite `BEGIN IMMEDIATE` reservation path with a typed native request-family
proof. Inside that same transaction the owner rechecks the authorized policy,
current native cost contract, audio-only/fixed-temperature/language/data-policy
binding, one-model/no-fallback policy, exact project-owned source scope and real
running accounted run. After resolving the unique canonical scope, that same
immediate transaction proves every native packed-region source end fits inside
the authorized asset duration. The strict native WAV constructor also requires
source regions to be monotonic and non-overlapping; the lenient local decode
helper is unchanged. A VLM wrapper cannot spend against a native policy or a
native audio quote, and it rejects a native duration quote even in a lower-level
non-policy operation context before preparation or ledger admission. A native
wrapper cannot spend against a VLM policy or token quote.

After the local transport `prepare`, a mandatory pre-dispatch guard reloads the
canonical authorization, re-materializes the current policy identity, validates
the exact source scope entry and referenced asset, and rejects changed source or
policy before the durable marker. The marker is committed immediately before
the single `NativeSttTransport::transcribe` call. The real
`OpenRouterSttClient` uses its existing no-cancel send leaf after the marker and
performs no second cancellation check that could masquerade as local no-upload
proof.

Returned provider evidence is settled before domain outcome validation.
Provider/model/generation identity is persisted only when returned; known
finite non-negative cost produces complete usage, while missing or malformed
cost remains uncertain even for a structurally valid transcript. Token counts
are preserved as `None` when absent and are never fabricated or clamped on
`u64 -> u32` overflow. Optional billed audio seconds are retained separately
from both the local input window and the admission reservation in immutable
receipt event evidence; the optional receipt JSON field is omitted when absent
so historical VLM receipt fingerprints remain byte-identical. A malformed body
or non-2xx response still returns a typed unusable outcome after its known
charge is retained. Transport failure after the marker remains uncertain and is
never automatically retried.

Native targeted retry dispatch remains intentionally unwired in B3. Existing
generic inspection/reconciliation can inspect and reconcile a native request,
but the VLM targeted-retry wrapper rejects native policies and no high-level
native retry parser/permit path exists yet. A default repeat of a completed or
uncertain native request is fenced by the canonical stable intended-attempt key
and cannot be forced merely by creating a new run.

## Incomplete native speech checkpoints

Native window recovery extends the existing transcript owner. Migration 45 adds
an optional versioned window plan and its semantic fingerprint to
`asset_transcripts`; candidates remain solely in `transcript_segments`, window
links remain in `native_transcript_windows`, and financial facts remain in the
shared request/receipt ledger. Historical complete native transcripts without a
plan remain readable and do not acquire inferred coverage.

The internal checkpoint writer begins a positive prepared-speech plan and
atomically appends one contiguous quality-evaluated result at a time. It proves
current preparation/audio/map, frozen authorization, source ownership and settled
request/receipt identity in the same immediate transaction as canonical row
replacement. An exact canonical pilot sample can seed the first window without
another upload. The incomplete header stays `failed` with
`native_coverage_incomplete`, including after every planned window is retained:
a later clip assembly step must apply complete-clip quality before acceptance.
Candidates are explicitly unaccepted, excluded from AI context and seekable for
human inspection. Provider segment counts survive rejection of candidate text;
missing token probabilities remain absent.

Historical reads use the recorded policy and structural facts. Current reuse
separately re-proves every retained window against newly prepared PCM and current
quality. Missing typed results after an active reservation, dispatch marker or
settled request require recovery; they are never ordinary cache misses authorizing
a new upload. This persistence boundary does not enable bulk execution, targeted
retry or native CLI/desktop selection.

## Complete native clip assembly (C10 slice)

A fully retained native checkpoint can now be assembled into the existing
terminal transcript artifact without additional provider dispatch. The internal
finalizer borrows the live prepared speech and exact native request, re-proves
the current source, frozen authorization, exact plan/payload for every paid
window, canonical request/run and settled or honestly uncertain receipt inside
the same SQLite `IMMEDIATE` transaction, then reuses the canonical transcript
writer. SQL failure retains the entire completed checkpoint unchanged.

Pure assembly validates every planned index exactly once, mapped candidate
ordering/language/quality and whole-clip repetition. Per-window quality
observations are preserved verbatim. Provider segment counts remain the sum of
recorded metric totals even when candidate text is discarded; local VAD speech
duration, not provider billed duration, is recorded. Native token probability
stays absent. All-empty provider output becomes `no_accepted_speech`; any mixed
empty/rejected window is a body-free quality rejection, never local silence or
accepted speech. Review-required candidates stay explicitly unaccepted. New
terminal artifacts retain the same plan/fingerprint for historical coverage
inspection; legacy complete artifacts remain plan-less and readable, and the
incomplete-checkpoint accessor returns `None` for terminal output. This does not
enable native bulk, targeted retry, current terminal reuse or CLI/desktop
selection.

## Native transcript currentness

Generic missing preparation reuses native terminal evidence only after one
canonical snapshot proves its frozen recipe, current selected proxy, complete
window plan, quality and request ownership. Current accepted native Speech is
selected through the same owner for policy transcript fingerprints and AI prompt
text. Historical, review-required, rejected, incomplete and malformed native
candidates remain available for human inspection and outside accepted prompt
context. Existing evidence reuse uses its recorded financial contract; new
writes and uploads retain their separate current authorization and budget gates.

Missing-proxy and other unproved native work are visible review/recovery
exceptions. A generic local missing pass preserves those headers and paid window
links, including if native work appears after planning and before the speech
claim. Explicit selected local transcription still uses its local recipe and can
replace native output through the existing claim and atomic writer. Aggregate
coverage includes proved native terminal work; its compatibility `recipeKey`
field describes the local recipe, while each artifact retains its actual recipe
and provider provenance.
