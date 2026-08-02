# D01-T017: Chronology CLI surface

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T017",
  "epic_id": "D01",
  "title": "Chronology CLI surface",
  "slug": "chronology-cli-surface",
  "status": "draft",
  "task_class": "implementation",
  "depends_on": ["D01-T014", "D01-T027"],
  "principal_invariant": "The CLI reports the same total order and the same derived times as the desktop for the same catalog.",
  "central_falsification": "Produce the ordered asset list and derived times through both the desktop command layer and the CLI for the same catalog, including after a chronology change, and diff them.",
  "architecture_decisions_frozen": false,
  "load_bearing_seams": ["chronology solution -> CLI JSON envelope"],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md", "AGENTS.md", "docs/agent-cli.md"],
    "independent_tester": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md", "docs/agent-cli.md"],
    "reviewer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md", "AGENTS.md", "docs/agent-cli.md"],
    "integrator": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "docs/agent-cli.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T017-chronology-cli-surface.md"],
    "implementer": [
      "apps/cli/src/main.rs",
      "apps/cli/tests/chronology_cli.rs"
    ],
    "independent_tester": ["apps/cli/tests/chronology_cli_independent.rs"],
    "integrator": ["docs/tasks/desktop/EPIC.md", "docs/agent-cli.md", "docs/task-cycles/D01-T017/**"]
  },
  "forbidden_paths": ["core/rcdvlog-core/src/**", "core/rcdvlog-server/**", "apps/desktop/**", "apps/web/**", "apps/server/**", ".github/**"],
  "verification_commands": [
    "cargo test -p rcdvlog-cli",
    "pnpm test:integration",
    "cargo test --workspace",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T017\\].*\\| complete \\|"
    },
    {
      "path": "docs/agent-cli.md",
      "regex": "chronology explain"
    }
  ]
}
-->

## Goal

Expose chronology through the non-interactive JSON CLI, so an external agent can
inspect sequences, epochs, phases, exceptions, constraints, contradictions, and the
explanation for any clip — and so the desktop's ordering can be checked against a
second implementation path.

`PRD.md` §21.2 lists a non-interactive JSON CLI for chronology among the
launch-critical capabilities.

## Principal invariant

This task is correct when the CLI reports the same total order and the same derived
times as the desktop for the same catalog.

## Inputs and outputs

Inputs:

- The core chronology surface from D01-T007 through D01-T014.
- The existing CLI conventions in `apps/cli/src/main.rs`: `Commands` enum,
  per-command subcommand enums, `command_name`, stable JSON envelopes, and
  machine-readable error codes.

Outputs:

- A `Chronology` subcommand group with `sequences`, `epochs`, `phases`,
  `exceptions`, `constraints`, `contradictions`, `explain`, and `status`.
- A `Diagnostics` subcommand group with `list` and `acknowledge`, reading the
  D01-T027 surface. This is the CLI half of the error-surfacing system: every
  diagnostic the application records must be retrievable non-interactively,
  including `sync.operation_quarantined`, which shipped in D01-T007 with no reader
  at all.
- JSON contract tests for every command, per the `AGENTS.md` §12 expectation that
  every documented CLI command has one.
- `docs/agent-cli.md` updated.

## Frozen decisions

- Read-only with respect to project facts. This packet adds no mutating
  chronology command. Mutation through the CLI would be a second apply path that
  could bypass the confirmation `PRD.md` §21.4 requires, and the agent workflows
  that need it are `PRD.md` §23 Milestone 5 work. `diagnostics acknowledge` is the
  sole write, and it mutates only an observation's acknowledged state — no
  operation, no aggregate revision, no derived order.
- Envelopes match the existing CLI exactly. Do not invent a second envelope shape
  for chronology.
- Every failure exits nonzero with a stable error code string, per `AGENTS.md` §11.
- Output is deterministic: the same catalog produces byte-identical JSON across
  runs, so an agent can diff it.
- The existing `Clock { Apply }` command is retained and continues to work through
  the compatibility wrapper from D01-T010. Removing it would break documented agent
  workflows.
- The CLI consumes the same core functions the desktop does. It does not
  reimplement ordering, and `core/rcdvlog-core/src/**` is forbidden precisely so
  that constraint is structural rather than aspirational.

## Owned paths

The implementer owns the CLI source and its contract tests. The core is forbidden:
if the CLI needs something the core does not expose, that is a stop condition,
because a CLI-only code path is exactly how the CLI and desktop come to disagree.

## Required interface

```text
rcdvlog chronology sequences   --project <id> [--json]
rcdvlog chronology epochs      --project <id> [--sequence <id>]
rcdvlog chronology phases      --project <id>
rcdvlog chronology exceptions  --project <id>
rcdvlog chronology constraints --project <id>
rcdvlog chronology contradictions --project <id>
rcdvlog chronology explain     --asset <id>
rcdvlog chronology status      --project <id>

rcdvlog diagnostics list        --project <id> [--severity error|warning|info]
                                [--kind-prefix <prefix>] [--include-acknowledged]
rcdvlog diagnostics acknowledge --project <id> --diagnostic <id>
```

`command_name` values, following the existing dotted convention:
`chronology.sequences`, `chronology.epochs`, `chronology.phases`,
`chronology.exceptions`, `chronology.constraints`,
`chronology.contradictions`, `chronology.explain`, `chronology.status`,
`diagnostics.list`, `diagnostics.acknowledge`.

`diagnostics acknowledge` is the one mutating command in this packet, and it is
admissible under the read-only decision above because acknowledging an
observation changes no project fact, emits no operation, and cannot alter derived
order. It is the exception, not a precedent for chronology mutation.

`chronology status` returns the counts of each fact kind, the number of assets with
a null trip time, the number of contradictions by strength, and the ordered asset
id list — the last being what makes desktop/CLI order comparison a one-line diff.

## Acceptance oracle

1. Every command emits valid JSON in the repository's existing envelope shape and
   exits zero on success.
2. Every command exits nonzero with a stable error code for: an unknown project id,
   an unknown asset id, a missing required argument, and a catalog at a newer schema
   version.
3. `chronology status` on a project produces the same ordered asset id list as the
   desktop's `project_chronology` command for the same catalog.
4. The same comparison holds after applying a chronology change through the desktop:
   both surfaces report the new order identically.
5. `chronology explain --asset <id>` reports the same corrections, confidence, and
   times as the desktop's `explain_asset_chronology`.
6. Output is byte-identical across 10 repeated runs on an unchanged catalog.
7. `chronology contradictions` on a project with one hard and one soft contradiction
   reports both with their strengths.
8. Every command works on a catalog with zero chronology facts, returning empty
   collections rather than erroring.
9. `clock apply` still works and produces the same result it did before this change.
10. Every command has a JSON contract test, and the test asserts field names and
    types rather than only exit code.

## Independent test charter

Falsify the claim that the CLI and desktop agree.

- Build your own catalog through the CLI, then run the same comparison after each of:
  a sequence split, a bounded epoch, an exception, a phase addition, and a hard
  constraint. Diff the ordered asset lists each time.
- Compare not only order but every derived time and confidence value, since two
  surfaces can agree on order while disagreeing on displayed time.
- Attack the envelopes: assert the exact top-level keys against the existing CLI
  commands' envelopes, so chronology has not quietly introduced a variant shape.
- Attack error paths: a nonexistent catalog path; a catalog file that is not SQLite;
  a project id containing a shell metacharacter; an asset id from a different
  project.
- Assert determinism by running each command 10 times and comparing bytes.
- Confirm the CLI links against the core ordering functions rather than
  reimplementing them, by grepping the CLI source for sorting or time arithmetic on
  assets. Any found is a finding.
- Run the commands with a non-ASCII project name and a path containing spaces.

## Review charter

Risk domain: a second implementation of ordering drifting from the first, which
would make the CLI actively misleading to an agent.

Questions:

1. Does the CLI sort, offset, or otherwise compute chronology itself anywhere?
2. Do the envelopes match the existing CLI convention exactly?
3. Does every command have a stable error code on every failure path?
4. Is output deterministic?
5. Was any mutating chronology command added, contrary to the frozen decision?
6. Does `clock apply` still behave as documented?
7. Does every command have a real contract test asserting shape, not just exit code?

Severity rules: a divergence from the desktop's order or times, chronology
computation in the CLI, an envelope deviation, or a missing error code is blocking.
Help-text wording and argument aliases are non-blocking.

Smallest acceptable remedy: delegate the computation to the core function and add the
comparison test that exposed the divergence.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Mutating chronology through the CLI.
- Agent workspace projection, script revisions, and edit-plan CLI commands, which are
  `PRD.md` §23 Milestone 5 and D01-T019 onward.
- Changing existing CLI commands beyond adding the new group.
- Human-readable table output; JSON is the contract.
- Any core change.

## Stop conditions

Stop and escalate when:

- a command needs core data the core does not expose;
- the existing envelope shape cannot represent an explanation without a new variant,
  which is a contract decision;
- desktop and CLI order disagree and the cause is in the core rather than the CLI —
  report it rather than compensating in the CLI, since compensating would hide a real
  determinism defect;
- a stable error code does not exist for a failure this surface can produce.

## Commands

```sh
cargo test -p rcdvlog-cli
pnpm test:integration
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
grep -nE "sort_by|sort_unstable|Duration::seconds" apps/cli/src/main.rs
```

Expected result: tests pass, clippy is clean, and the grep finds no chronology
computation in the CLI.

## Handoff

Report every command added with its `command_name` and envelope, the desktop/CLI
order and time comparison results including after each change kind, the determinism
evidence, the error codes per failure path, and confirmation that `clock apply` is
unchanged. List any stop condition encountered. The implementer does not approve this
task.

## Promotion note

`draft` until D01-T014 completes. The contract editor confirms the core exposes
everything these commands need, so the forbidden-core constraint does not force a
stop during implementation.
