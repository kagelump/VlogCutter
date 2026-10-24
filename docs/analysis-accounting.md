# Analysis request accounting: admission foundation

This document describes the admission/dispatch foundation added for issue #122.
It is persistence and offline-test infrastructure, not the runtime integration.
Provider wrappers, pilot/observation/moment/fallback call sites and CLI
inspection commands remain follow-up work. Until that integration is complete,
existing paid analysis execution is **not** made concurrent-runtime-safe by
this slice.

## Canonical owner

The project SQLite catalog remains the single accounting owner. Migration 38
adds child rows beneath the existing `analysis_runs`/run-policy ownership:

- `analysis_budget_authorities` stores exactly one frozen ceiling, policy and
  provider identity, historical evidence, and dispatch-stop state per canonical
  project/policy or project/operation scope. The row is immutable.
- `analysis_request_reservations` stores intended-request identity, policy,
  payload/source/cost-bound fingerprints, lifecycle, conservative admission
  bound, separately returned cost, confirmed cost, and unresolved-usage reserve.
- `analysis_request_events` is append-only lifecycle and evidence history.
- `analysis_provider_receipts` is present as an append-only table for the
  receipt/reconciliation slice. This slice exposes no receipt or settlement API.

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
5. release if and only if the marker is absent and local pre-upload failure is
   proven, or retain uncertainty after a potential upload.

The authority API takes no request payload fingerprint and does not invent one.
Request-specific model, source, run-ownership and payload checks are separate
and happen in reservation.

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
that contract remains an external fact that receipt reconciliation must retain
and expose, not a reservation estimate.

## Interruption boundary

The durable dispatch marker is committed before transport I/O. Missing marker
is the only local no-upload proof. Once the marker exists, a generic failure
code, process exit or local cleanup failure cannot authorize release; the
request stays dispatched/uncertain until supported receipt reconciliation is
implemented in the next slice. Request identity and dispatch-marker uniqueness
are enforced in SQLite, so they survive separate process reopen.

## Legacy evidence

Authorizations created before migration 38 have an empty
`accounting_contract` and require explicit reauthorization. Reauthorization
upgrades that contract, but it does not edit or erase a frozen authority and
does not unblock an already-stopped authority.

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
