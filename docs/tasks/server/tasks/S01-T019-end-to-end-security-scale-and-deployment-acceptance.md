# S01-T019: End-to-End Security, Scale, and Deployment Acceptance

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "S01-T019",
  "epic_id": "S01",
  "title": "End-to-End Security, Scale, and Deployment Acceptance",
  "slug": "end-to-end-security-scale-and-deployment-acceptance",
  "status": "complete",
  "task_class": "qa",
  "depends_on": [
    "S01-T012",
    "S01-T015",
    "S01-T016",
    "S01-T017",
    "S01-T018"
  ],
  "principal_invariant": "The full Ubuntu/Mac/NAS/LAN/Tailscale deployment matrix passes automated gates, security acceptance, scale acceptance, and manual acceptance — and the evidence is recorded in ACCEPTANCE_TASK_19.md, not merely asserted.",
  "central_falsification": "Execute the full 17-row acceptance matrix on real hardware. For each row, record the exact command, exit code, and artifact. For each security criterion, attempt the prohibited action and verify rejection. For each scale criterion, measure at the declared threshold. Any gap between claim and evidence is a failure.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "All epic seams exercised end to end"
  ],
  "documents_by_role": {
    "contract_editor": [],
    "implementer": [],
    "independent_tester": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/ACCEPTANCE_TASK_19.md"
    ],
    "reviewer": [
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "integrator": [
      "docs/tasks/server/ARCHITECTURE.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [],
    "implementer": [],
    "independent_tester": [
      "docs/tasks/server/tasks/S01-T019-end-to-end-security-scale-and-deployment-acceptance.md"
    ],
    "integrator": []
  },
  "forbidden_paths": [
    "core/**",
    "apps/server/**",
    "apps/desktop/**",
    "apps/web/**",
    "apps/mobile/**",
    "apps/cli/**",
    "deploy/**"
  ],
  "verification_commands": [
    "pnpm --dir apps/desktop build",
    "pnpm --dir apps/web build",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings",
    "cargo test --workspace",
    "pnpm test:integration",
    "pnpm test:web:e2e",
    "docker compose -f deploy/server/compose.yaml config"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/server/EPIC.md",
      "regex": "\\| 190 \\| \\[S01-T019\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Prove the companion server end to end across the full deployment matrix: Ubuntu
server, Mac desktop, NAS, LAN, and Tailscale — with security, scale, and manual
acceptance evidence recorded, not merely asserted.

## Principal invariant

This task is correct when the full Ubuntu/Mac/NAS/LAN/Tailscale deployment matrix
passes automated gates, security acceptance, scale acceptance, and manual
acceptance — and the evidence is recorded in ACCEPTANCE_TASK_19.md, not merely
asserted.

## Inputs and outputs

Inputs:
- All S01 task packets (S01-T001 through S01-T018)
- Real hardware: Ubuntu server, Mac, NAS, physical network
- ACCEPTANCE_TASK_19.md evidence document

Outputs:
- 17-row acceptance matrix results with recorded evidence
- Security acceptance results
- Scale acceptance results
- Manual acceptance results
- Known gaps and external dependencies documented

## Security acceptance

- No test or implementation writes, renames, or deletes source media.
- Original NAS roots are bind-mounted read-only.
- No live SQLite file resides on or is synchronized through the NAS.
- LAN and Tailscale routes require application authentication and HTTPS.
- Secrets and absolute paths are not exposed through UI, logs, or URLs.
- APIs and CLIs use stable error codes and machine-readable contracts.

## Scale acceptance

- A sampled-fingerprint collision cannot merge distinct content after full verification.
- A project asset remains useful when its original is remote-only.
- Sync is idempotent across timeout/retry and converges after offline edits.
- Server restart does not lose acknowledged operations or completed job results.
- Collections remain responsive with thousands of assets.

## Manual acceptance

- Chrome can seek within a proxy without downloading the entire file first.
- Desktop annotation writes succeed while the server is unavailable.
- Unsafe chronology concurrency is surfaced rather than silently resolved.
- Existing desktop keyboard, focus, offline playback, and project behavior do not regress.

## Frozen decisions

1. This task records evidence; it does not change production code.
2. Findings that are defects go back to the owning task.
3. Findings that are limitations go into documentation.
4. A browser mock, unverified migration, or local automation alone is not release approval.
5. Manual acceptance uses disposable catalogs and generated representations.
6. The real NAS fixture is enumerated read-only first and is never a test-output destination.

## Owned paths

- **Independent tester:** this task packet (evidence and measurements only; no production code changes)
- **Integrator:** this task packet (planning reconciliation)
- **Implementer:** none (production changes are out of scope)

## Required interface

- No new interfaces. Exercises all existing server interfaces end to end.

## Acceptance oracle

1. Every automated gate passes (format, lint, test, build, integration, e2e, compose config).
2. Every row in the acceptance matrix has recorded evidence (command, exit code, artifact).
3. No security criterion is violated.
4. No scale criterion is violated.
5. Manual acceptance criteria pass on real hardware.
6. Known gaps are documented with explicit scope.
7. Release-topology acceptance is recorded honestly — unrun external checks are listed, not claimed.
8. Existing desktop behavior does not regress.

## Independent test charter

This task IS the independent test. Execute the full acceptance matrix. Record
commands, exit codes, and artifacts for every row. Attempt every security
prohibition and verify rejection. Measure every scale threshold. Do not claim a
pass without reproducible command evidence.

## Review charter

Risk domain: epic completion integrity. Questions: (1) Is every matrix row
evidenced or merely asserted? (2) Are security prohibitions actually enforced? (3)
Are scale thresholds measured? (4) Are known gaps documented? (5) Does existing
desktop behavior regress? Severity: P0 for claiming unrun tests passed or
security criteria unmet; P1 for unmeasured scale claims or undocumented gaps.
Approval vocabulary: `APPROVE` (epic complete), `APPROVE_WITH_FOLLOW_UP` (epic
complete with documented gaps), `REJECT` (acceptance evidence insufficient).

## Non-goals

- Automating physical hardware setup
- Performance optimization work discovered during acceptance (file it)
- Filling gaps in external dependencies (document them)
- Release deployment (acceptance is pre-release gating)

## Stop conditions

Stop and escalate when:

- A defect is found that requires production code changes (file it against the owning task)
- A matrix row cannot be run due to unavailable hardware (document the gap)
- A security criterion cannot be verified (document the gap, escalate if P0)

## Commands

```sh
pnpm --dir apps/desktop build
pnpm --dir apps/web build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm test:integration
pnpm test:web:e2e
docker compose -f deploy/server/compose.yaml config
```

## Handoff

ACCEPTANCE_TASK_19.md is the sole evidence base for S01-T020 (documentation
update) and the epic completion gate. Every claim in documentation must be
traceable to this evidence.
