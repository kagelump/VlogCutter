# Analysis request accounting: admission and receipt reconciliation

This document describes the canonical persistence foundation and the C1
authorized-pilot runtime integration added for issue #122. The authorized
run-policy pilot, including transport and domain-validation fallback, is wired
through the accounted provider boundary. Bulk observation, ad-hoc analysis,
CLI inspection/reconciliation commands and the remaining runtime adapters are
still follow-up work, so those paths are **not** yet made concurrent-runtime-safe
by this slice.

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
marker. The C1 pilot creates marked runs only after initializing its budget
authority. A marked zero-request cancelled run does not invent unknown paid
usage. Other production paid call sites remain unwired in this slice; the marker
creation API alone is not evidence of runtime integration.

An older runtime may have written run or pilot spending after an authority was
initialized by migrations 38/39. Admission and dispatch recheck that history in
an IMMEDIATE transaction. Newly observed unknown floors and the dispatch stop
commit before returning a denial. Append-only evidence belongs to the same
Catalog/accounting owner and is included separately in canonical projections;
it never rewrites the frozen legacy authority or claims a confirmed charge.

Pilot summaries own their represented run's floor. Frozen amounts receive credit
once, repeated observations append only increases, and independent runs keep
separate evidence. Unknown zero-cost legacy work remains unknown; unreadable
frozen pilot records retain their prior reserve. Direct evidence deletion is
rejected while project-owned cleanup still cascades.

## Remaining C2 integration (not implemented here)

The following paid runtime call sites still bypass `AccountedVlmProvider` and
must be wired before issue #122 can be considered complete:

- bulk observation: `Catalog::analyze_missing_with_options_and_provider`,
  `Catalog::analyze_missing_with_provider`,
  `Catalog::analyze_authorized_with_provider`, and
  `Catalog::analyze_authorized_with_provider_and_workers` in
  `core/rcdvlog-core/src/lib.rs`, including `analysis::vlm::analyze_clip`;
- moment discovery: `Catalog::discover_moments_with_provider` and
  `Catalog::discover_authorized_moments_with_provider`;
- chronology suggestions: `Catalog::suggest_chronology_with_provider` and
  `analysis::chronology_suggestions::suggest_with_provider`;
- CLI/desktop/HTTP adapters and any inspection/reconcile/retry commands. No CLI
  command is added by C1.

Each remaining call site needs a real owned run id, full-policy authority
context, and the same per-attempt boundary before paid dispatch is considered
concurrent-safe.
