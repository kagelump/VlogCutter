# S01-T020: Repository-Wide Documentation Update

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "S01-T020",
  "epic_id": "S01",
  "title": "Repository-Wide Documentation Update",
  "slug": "update-all-documentation",
  "status": "complete",
  "task_class": "review",
  "depends_on": [
    "S01-T019"
  ],
  "principal_invariant": "Every repository document accurately describes the implemented companion server, separates completed behavior from deferred architecture, and every implementation claim is traceable to S01-T019 acceptance evidence.",
  "central_falsification": "Audit every repository document: search for stale statements that the server, sync, durable jobs, web review, auth, or global media identity do not exist. Verify every Status marker in task documents matches reality. Verify every command example runs against the current implementation. Verify no document claims an unrun test passed.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Documentation → all implemented subsystems"
  ],
  "documents_by_role": {
    "contract_editor": [],
    "implementer": [],
    "independent_tester": [],
    "reviewer": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/ACCEPTANCE_TASK_19.md",
      "AGENTS.md",
      "PRD.md",
      "README.md"
    ],
    "integrator": [
      "docs/tasks/server/ARCHITECTURE.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [],
    "implementer": [],
    "independent_tester": [],
    "reviewer": [
      "docs/agent-cli.md",
      "docs/batch-estimation.md",
      "docs/integration-testing.md",
      "docs/server/**",
      "docs/superpowers/**",
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/ACCEPTANCE_TASK_19.md",
      "docs/tasks/travel/ARCHITECTURE.md",
      "README.md",
      "AGENTS.md",
      "PRD.md"
    ],
    "integrator": [
      "docs/tasks/server/EPIC.md"
    ]
  },
  "forbidden_paths": [
    "core/**",
    "apps/**",
    "deploy/**",
    "Cargo.toml",
    "package.json"
  ],
  "verification_commands": [
    "pnpm --dir apps/desktop build",
    "pnpm --dir apps/web build",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings",
    "cargo test --workspace",
    "pnpm test:integration"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/server/EPIC.md",
      "regex": "\\| 200 \\| \\[S01-T020\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Update every repository document and user/developer contract to describe the
implemented companion server accurately, using S01-T019 acceptance evidence and
clearly separating completed behavior from deferred architecture.

## Principal invariant

This task is correct when every repository document accurately describes the
implemented companion server, separates completed behavior from deferred
architecture, and every implementation claim is traceable to S01-T019 acceptance
evidence.

## Inputs and outputs

Inputs:
- S01-T019 acceptance evidence (ACCEPTANCE_TASK_19.md)
- All S01 task packets and their acceptance results
- Current repository documentation

Outputs:
- Updated PRD.md, AGENTS.md, README.md
- Updated docs/agent-cli.md, docs/batch-estimation.md, docs/integration-testing.md
- New focused documents: deployment, sync, storage, jobs, API, security, backup/recovery, troubleshooting
- Updated CLI --help, example config, Compose comments, API schema descriptions
- Marked historical task documents as point-in-time engineering artifacts
- Travel epic amendments reflected (project creation, writable archive root)
- Validated markdown links, headings, code blocks, and commands

## Frozen decisions

1. ACCEPTANCE_TASK_19.md is the sole evidence base — no claim without trace.
2. Current implementation facts and future plans are labeled distinctly.
3. Original immutability, local SQLite placement, operation sync, and node-local paths are consistent in every document.
4. Commands, paths, env vars, ports, versions, and error codes match checked-in behavior exactly.
5. Examples use placeholders or disposable paths.
6. No documentation tells users to put catalogs on NAS, expose Funnel, disable security, or write into original roots.
7. AGENTS.md remains an operational handoff, not a narrative.
8. README.md stays concise and links to focused documents.
9. Architecture file records material deviations with rationale.
10. Task files reflect final completion status only when criteria genuinely passed.
11. Markdown links, headings, code blocks, and commands are validated.

## Owned paths

- **Reviewer:** all documentation files (`docs/**`, `*.md`, `deploy/server/*.md`)
- **Integrator:** `docs/tasks/server/EPIC.md` (epic completion markers), this task packet

## Required interface

- No new interfaces. Audits all existing documentation against implementation reality.

## Acceptance oracle

1. Every required repository document is audited and updated or explicitly confirmed unaffected.
2. PRD.md and AGENTS.md accurately separate implemented server behavior from remaining gaps.
3. A fresh Ubuntu setup can follow the deployment documentation without undocumented steps.
4. CLI/API/config examples pass against the final implementation or a checked validation script.
5. Sync/conflict/storage/representation docs match code-level identities and semantics.
6. Backup and restore documentation matches the verified S01-T019 drill.
7. No document claims an unrun test or deferred feature is complete.
8. No secret or private real-project path is introduced.
9. Markdown link and formatting checks pass.
10. `rg` audit finds no known stale server-deferred or unsafe guidance outside historical context.
11. ARCHITECTURE.md, EPIC.md, and all task statuses match the actual delivered scope.
12. Historical task handoff and review documents are labeled as such.

## Independent test charter

This task IS the review. Audit every repository document against S01-T019
evidence. Search for stale statements. Verify every command example executes.
Verify every Status marker. Verify markdown links resolve. Verify no document
claims a deferred feature is implemented. Verify the two travel amendments
(server-origin project creation, writable archive root) are reflected correctly.

## Review charter

Risk domain: documentation accuracy and completeness. Questions: (1) Does any
document claim an unrun test passed? (2) Does any document describe deferred
behavior as implemented? (3) Do command examples execute without error? (4) Are
travel amendments reflected? (5) Are historical documents labeled? Severity: P0
for claiming unrun tests or deferred features as complete; P1 for broken command
examples or stale statements. Approval vocabulary: `APPROVE` (epic complete),
`APPROVE_WITH_FOLLOW_UP` (epic complete with documented follow-ups), `REJECT`
(documentation insufficient).

## Non-goals

- Adding new product behavior under the guise of documentation
- Claiming unrun manual acceptance passed
- Rewriting historical notes as if they were current contracts
- Copying secrets, real absolute private paths, or volatile local output into examples

## Stop conditions

Stop and escalate when:

- S01-T019 evidence is insufficient to verify a documentation claim
- A document describes behavior that doesn't exist in the implementation
- A command example fails against the current implementation
- A travel amendment contradicts the current server architecture document

## Commands

```sh
pnpm --dir apps/desktop build
pnpm --dir apps/web build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm test:integration
```

## Handoff

This task is the epic's final gate. Only after it passes may the companion-server
epic be marked complete.
