# T01-T009: End-to-End Travel Acceptance

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "T01-T009",
  "epic_id": "T01",
  "title": "End-to-End Travel Acceptance",
  "slug": "travel-acceptance",
  "status": "ready",
  "task_class": "qa",
  "depends_on": [
    "T01-T008"
  ],
  "principal_invariant": "A multi-night, interrupted, bandwidth-limited trip with real camera media completes end to end: notes survive, no committed chunk is retransmitted, return-home reconciliation links card-ingested media to waiting provisional assets, and no file outside the archive root is written, renamed, or deleted.",
  "central_falsification": "Run the full 16-step trip simulation with real camera media over a throttled, unreliable network. After every interruption class (network drop, force-quit, server restart, card removal), verify no committed chunk was retransmitted. At return-home, verify every note is intact and attached to the correct clip, uploaded media is not re-transferred, and un-uploaded media links to its provisional assets.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "All travel epic seams exercised end to end"
  ],
  "documents_by_role": {
    "contract_editor": [],
    "implementer": [],
    "independent_tester": [
      "docs/tasks/travel/ARCHITECTURE.md",
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "reviewer": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ],
    "integrator": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [],
    "implementer": [],
    "independent_tester": [
      "docs/tasks/travel/tasks/T01-T009-travel-acceptance.md"
    ],
    "integrator": []
  },
  "forbidden_paths": [
    "core/**",
    "apps/server/**",
    "apps/desktop/**",
    "apps/web/**",
    "apps/mobile/**",
    "deploy/**"
  ],
  "verification_commands": [
    "pnpm --dir apps/desktop build",
    "pnpm --dir apps/web build",
    "pnpm --dir apps/mobile typecheck",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings",
    "cargo test --workspace",
    "pnpm test:integration"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/travel/EPIC.md",
      "regex": "\\| 90 \\| \\[T01-T009\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Prove the travel workflow end to end under conditions resembling an actual trip:
real camera media, a constrained and unreliable network, multiple interrupted
sessions, and a return-home reconciliation.

## Principal invariant

This task is correct when a multi-night, interrupted, bandwidth-limited trip with
real camera media completes end to end: notes survive, no committed chunk is
retransmitted, return-home reconciliation links card-ingested media to waiting
provisional assets, and no file outside the archive root is written, renamed, or
deleted.

## Inputs and outputs

Inputs:

- All travel task packets (T01-T001 through T01-T008)
- Real camera media fixtures
- Physical hardware: iPhone, card reader, SD cards, Ubuntu server, NAS
- Network throttling infrastructure

Outputs:

- Scripted multi-night trip simulation with recorded evidence
- Security acceptance results for the transfer surface
- Scale acceptance for a travel-sized card
- Documented throughput measurements and realistic expectations
- Recorded best-effort behaviors with observed reality

## Trip simulation (16 steps)

1. Create a trip project from the phone with no Mac involved.
2. Insert a card with real camera media from at least two camera models.
3. Inventory it and confirm assets appear on phone, web, and desktop before any upload.
4. Write notes against clips that have not uploaded.
5. Start uploading under a throttled connection with a priority policy.
6. Interrupt: kill the network mid-chunk.
7. Interrupt: force-quit the app.
8. Interrupt: restart the server.
9. Interrupt: remove the card mid-transfer, then reinsert it.
10. Resume and confirm no committed chunk is retransmitted.
11. Review arrived clips through mobile proxies, including with network disabled.
12. Shoot more footage onto the same card and re-inventory.
13. End the trip with uploads deliberately incomplete.
14. Return home, adopt the project on the desktop, and ingest the card locally.
15. Confirm uploaded media is not re-transferred and un-uploaded media links to its waiting provisional assets.
16. Confirm every note written during the trip is intact and attached to the right clip.

## Security acceptance

- Transfer endpoints reject requests without a valid device credential.
- Transfer endpoints are unreachable from outside the tailnet.
- The server listener remains loopback-only.
- Revoking the phone's credential immediately stops in-flight transfers.
- No credential appears in any URL, log line, or persisted request.
- Adversarial `cardRelativePath` values cannot escape the archive root.
- Non-transfer routes still enforce the 64 MB body limit.

## Scale acceptance

- A card with thousands of files inventories without exhausting phone memory.
- The server's resume query stays responsive for a card with thousands of objects.
- Ingesting a card's worth of files does not starve interactive server work.
- Project lists and asset lists remain responsive on the phone at that scale.

## Data-safety acceptance

- A full checksum manifest of the SD card is identical before and after the entire cycle.
- No file under any scan root is written, renamed, or deleted.
- No partial file ever appears beneath the archive root.
- No staging file is written to the NAS.
- Every archived file's hash matches the phone's declaration and the card's content.
- No surface presented a "safe to format" state at any point.

## Measurements to record

| Measurement | Why |
| --- | --- |
| Sustained upload throughput at several throttled rates | Sets honest expectations for hotel wifi |
| Wall-clock time to inventory a large card | The user waits on this before annotating |
| Time from upload completion to reviewable mobile proxy | The core "annotate while uploading" promise |
| Mobile proxy size ratio versus the 720p proxy | Validates the bandwidth rationale |
| Peak phone storage during a large upload | Validates the no-full-copy design |
| Resume overhead after each interruption class | Validates the chunk ledger |

## Frozen decisions

1. Findings that are defects go back to the owning task. Findings that are limitations go into documentation.
2. Do not fix performance problems inside acceptance — file them.
3. Compute-first enrichment becomes reasonable to plan only after this task passes.
4. Manual acceptance on real hardware: physical iPhone, physical card reader, real cards, actual Ubuntu server, actual NAS.
5. Network throttling via router-level or proxy-level shaper, not application-level latency simulation.
6. Use disposable projects and a scratch archive root; the real archive is never an acceptance destination.
7. The read-only real-media fixture is enumerated read-only only.

## Owned paths

- **Independent tester:** this task packet (evidence and measurements only; no production code changes)
- **Integrator:** this task packet (planning reconciliation)
- **Implementer:** none (production changes are out of scope)

## Required interface

- No new interfaces. Exercises all existing travel interfaces end to end.

## Acceptance oracle

1. The full trip simulation completes with every interruption class exercised.
2. No committed chunk is ever retransmitted after any interruption.
3. Notes written before upload are intact and correctly attached after return.
4. Return-home ingest re-transfers nothing that already uploaded.
5. Un-uploaded media links to its waiting provisional assets on the desktop.
6. Every security, scale, and data-safety criterion above passes.
7. All measurements are recorded in the documentation.
8. Every best-effort behavior is documented with observed results.
9. Existing desktop offline behavior, keyboard contract, and project behavior do not regress.
10. Existing web behavior does not regress.

## Independent test charter

This task IS the independent test. Execute the full 16-step trip simulation. For
each step, record commands, exit codes, and artifacts. For each interruption
class, verify the specific recovery behavior promised by the owning task. For
each measurement, record the actual observed value. For each best-effort
behavior, record what actually happened, not what was intended.

## Review charter

Risk domain: epic completion integrity. Questions: (1) Does the acceptance
evidence actually exercise every interruption class? (2) Are measurements
recorded or merely asserted? (3) Are best-effort behaviors documented with
observed reality? (4) Is any "safe to format" state presented? (5) Are existing
desktop/web behaviors unchanged? Severity: P0 for claiming completion without
evidence or presenting "safe to format"; P1 for unmeasured claims or undocumented
best-effort behaviors. Approval vocabulary: `APPROVE` (epic complete),
`APPROVE_WITH_FOLLOW_UP` (epic complete with documented limitations), `REJECT`
(acceptance evidence insufficient).

## Non-goals

- Automating physical card insertion and removal
- Performance optimization work discovered during acceptance (file it, do not fix it here)
- Compute-first enrichment features

## Stop conditions

Stop and escalate when:

- A defect is found that requires production code changes (file it against the owning task)
- A measurement cannot be taken due to infrastructure limitations (document the limitation)
- A best-effort behavior contradicts its owning task's promise (file against the owning task)

## Commands

```sh
pnpm --dir apps/desktop build
pnpm --dir apps/web build
pnpm --dir apps/mobile typecheck
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm test:integration
```

## Handoff

This task is the epic's exit gate. Every travel task's completion claim is
provisional until this task's evidence confirms it end to end. Record everything.
A single small file uploaded over a fast local link does not constitute
completion.
