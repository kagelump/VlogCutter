# Analysis run policy, authorization and capability pilots

Issue #120 adds a fail-closed preflight around paid analysis work. The policy
materializes what a proposed run would do; authorization and pilot observations
are separate records bound to that exact policy.

## Lifecycle

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
   - prompt, output-schema, recipe and tool versions.

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
prompt/schema/recipe/tool versions, or readiness changes the policy digest.
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
conclusions and a warning. They never contain prompts, frame/transcript content,
raw response bodies, credentials, provider error text or absolute source paths.
An interrupted or unknown-usage pilot is recorded as `uncertain` and blocks
further provider calls until its receipt is reconciled. An explicit retry flag
cannot clear unknown spending. Retrying a fully accounted failed pilot retains
its known cost within the same policy ceiling. #122 owns concurrent atomic reservations; this
feature does not claim a general ledger.

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

# Moment discovery uses the same lifetime:
rcdvlog ... moments plan ...      # describe, includes runPolicy
rcdvlog ... moments authorize ... # persist exact approval
rcdvlog ... moments pilot ...     # bounded editorial/pilot request
rcdvlog ... moments run --policy-id <policy-id>
```

The legacy `analyze missing`, `moments discover` and preparation paths remain
available, but policy-bound execution is the canonical paid boundary and is the
only path that carries a matching approval and pilot into bulk work.

## Boundaries and non-goals

- No cloud audio/STT backend is implemented here. Audio payloads are represented
  so preflight fails readiness explicitly; issue #119 remains separate.
- Concurrent atomic reservations and a general spend ledger are issue #122 work.
  This change only binds one run ceiling and refuses unsafe unknown-usage retries.
- The pilot is a synthetic/public-fixture capability check. It does not replace
  human editorial review and it does not claim that a prompt bypasses external
  approval review.
- Original media and human-authored fields remain immutable. Pilot frame
  extraction writes only to the managed project cache.
