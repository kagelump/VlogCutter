# D01-T030: Edit-plan CLI and machine-readable diagnostics

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T030",
  "epic_id": "D01",
  "title": "Edit-plan CLI and machine-readable diagnostics",
  "slug": "edit-plan-cli-and-diagnostics",
  "status": "blocked",
  "task_class": "implementation",
  "depends_on": ["D01-T021", "D01-T022"],
  "principal_invariant": "Every edit-plan state and deliberate mutation needed by an external agent is available through stable JSON envelopes backed by the same core methods as the desktop.",
  "central_falsification": "Create, validate, activate, preview, export, and inspect one plan through separate CLI processes, then compare the catalog state and artifacts with direct core reads.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": ["edit-plan core surface -> CLI JSON envelope"],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/EDIT_PLAN_MODEL.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["AGENTS.md", "docs/agent-cli.md", "docs/tasks/desktop/EDIT_PLAN_MODEL.md"],
    "independent_tester": ["AGENTS.md", "docs/agent-cli.md", "docs/tasks/desktop/EDIT_PLAN_MODEL.md"],
    "reviewer": ["AGENTS.md", "docs/agent-cli.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "integrator": ["docs/agent-cli.md", "docs/tasks/desktop/EPIC.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T030-edit-plan-cli-and-diagnostics.md"],
    "implementer": ["apps/cli/src/edit_plan_commands.rs", "apps/cli/src/main.rs", "apps/cli/tests/edit_plan_cli.rs"],
    "independent_tester": ["apps/cli/tests/edit_plan_cli_independent.rs"],
    "integrator": ["docs/agent-cli.md", "docs/tasks/desktop/EPIC.md", "docs/task-cycles/D01-T030/**"]
  },
  "forbidden_paths": ["core/**", "apps/desktop/**", "apps/web/**", "apps/server/**", ".github/**"],
  "verification_commands": [
    "cargo test -p rcdvlog-cli --test edit_plan_cli",
    "cargo test -p rcdvlog-cli --test edit_plan_cli_independent",
    "pnpm test:integration",
    "cargo test --workspace",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T030\\].*\\| complete \\|"
    },
    {
      "path": "docs/agent-cli.md",
      "regex": "edit-plan validate"
    }
  ]
}
-->

## Goal

Expose the implemented edit-plan domain through the repository's public
non-interactive CLI contract. Today only the desktop adapter can create,
activate, validate, preview, export, or inspect revision/export history, leaving
agents without a stable way to diagnose or reproduce those workflows.

## Principal invariant

CLI commands call the same `Catalog` methods as Tauri and produce stable JSON
envelopes; no edit-plan or media rule is duplicated in the adapter.

## Inputs and outputs

Inputs are the frozen edit-plan model plus the completed preview and OTIO core
surfaces. Outputs are an `edit-plan` CLI family, process-level contract tests,
and complete command documentation.

## Frozen decisions

- Read commands open catalogs read-only and never migrate.
- Mutations are deliberately named and require explicit project/revision ids.
- Plan operations enter as a JSON file or stdin document, never a shell-encoded
  repeated argument shape.
- Success is one stdout envelope; failures are one stderr envelope with stable
  codes. Preview progress may use stderr NDJSON before final stdout.
- Export never silently replaces a path and never accepts credentials or media
  bytes on argv.

## Owned paths

The implementer owns one focused command module, registration in `main.rs`, and
process tests. The independent tester owns a separate multi-process workflow.
Core and desktop code are forbidden.

## Required interface

Provide commands for `status`, `revisions`, `show`, `create`, `activate`,
`validate`, `preview`, `export`, and `export-history`. Read results include
revision identity, active/stale state, ordered operations, validation findings,
preview identity/partial failures, and export digests without hiding paths that
the local desktop catalog already owns.

## Acceptance oracle

1. Every command returns the standard envelope and stable field types.
2. Status/revision/history commands remain usable during an uncommitted writer.
3. Create reads ordered operations from JSON and preserves their order.
4. Stale create/activate refuses with `edit_plan_stale_revision` naming actual.
5. Validate returns all findings and an explicit `renderable` boolean.
6. Preview streams bounded progress and returns partial failures honestly.
7. Export enforces the expected-digest overwrite contract and records history.
8. Separate-process reads match direct core state after every mutation.
9. Unknown ids, invalid JSON, newer schema, offline originals, media failure,
   and existing output map to documented stable codes.
10. `doctor` and every read command remain mutation-free.

## Independent test charter

Build a real temporary media fixture, create a plan through one process, inspect
and validate through another, render and export through later processes, then
compare SQLite-visible operations and probed artifacts using public core reads.
Attack stale revisions and output digest mismatch.

## Review charter

Reject human-formatted output, direct SQL, duplicated validation, writes in read
commands, arbitrary filesystem traversal beyond an explicit output path, or any
command that silently overwrites.

## Non-goals

- Changing the edit-plan schema or recipe.
- Resolve import acceptance.
- Remote server edit-plan APIs.
- Interactive prompts.

## Stop conditions

Stop if a needed core method is absent, if preview cannot expose truthful
progress after D01-T021, or if stable error mapping requires changing the shared
CLI registry outside this packet's boundary.

## Commands

```sh
cargo test -p rcdvlog-cli --test edit_plan_cli
cargo test -p rcdvlog-cli --test edit_plan_cli_independent
pnpm test:integration
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

List command names, sample envelope schemas, stable errors, process-test count,
preview progress semantics, and exact mutations proven to be idempotent or
digest guarded.
