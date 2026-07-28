# D01-T002: Fix baseline gate defects

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T002",
  "epic_id": "D01",
  "title": "Fix baseline gate defects",
  "slug": "fix-baseline-gate-defects",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": ["D01-T001"],
  "principal_invariant": "Every command classified real_defect in the baseline audit exits zero, with no gate command that previously passed regressing.",
  "central_falsification": "Run the complete gate command list from a fresh clone of the fix commit and confirm every previously failing command now exits zero and every previously green command still does.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/ARCHITECTURE.md", "docs/tasks/desktop/BASELINE_AUDIT.md"],
    "implementer": ["docs/tasks/desktop/BASELINE_AUDIT.md", "AGENTS.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "independent_tester": ["docs/tasks/desktop/BASELINE_AUDIT.md", "AGENTS.md"],
    "reviewer": ["docs/tasks/desktop/BASELINE_AUDIT.md", "AGENTS.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "integrator": ["docs/tasks/desktop/EPIC.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T002-fix-baseline-gate-defects.md"],
    "implementer": [
      "docs/tasks/desktop/BASELINE_AUDIT.md",
      ".claude/skills/executing-bounded-task-cycle/SKILL.md",
      ".claude/skills/executing-bounded-task-cycle/references/epic-schema.md",
      ".claude/skills/writing-bounded-task-packets/references/epic-schema.md",
      ".claude/skills/writing-bounded-task-packets/assets/EPIC.template.md",
      "scripts/check-docs.mjs"
    ],
    "independent_tester": ["docs/task-cycles/D01-T002/verification/**"],
    "integrator": ["docs/task-cycles/D01-T002/evidence/**"]
  },
  "forbidden_paths": [".github/**", "core/rcdvlog-server/**", "apps/server/**", "apps/web/src/**"],
  "verification_commands": [
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings",
    "cargo test --workspace",
    "pnpm check:frontend",
    "pnpm test:web",
    "pnpm test:integration",
    "pnpm check:docs"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T002\\].*\\| complete \\|"
    },
    {
      "path": "docs/tasks/desktop/BASELINE_AUDIT.md",
      "regex": "(?s)## Resolution\\b.*real_defect rows resolved"
    }
  ]
}
-->

## Goal

Resolve every defect the baseline audit classified `real_defect`, so that
D01-T004 can encode a CI gate that is actually green rather than one that
enforces a subset and calls the rest quarantined.

## Principal invariant

This task is correct when every command classified `real_defect` in
`docs/tasks/desktop/BASELINE_AUDIT.md` exits zero, and no gate command that the
audit recorded green regresses.

## Inputs and outputs

Inputs:

- `docs/tasks/desktop/BASELINE_AUDIT.md`, specifically its `RD-n` sections. The
  ledger is the specification for this task. Nothing outside it is in scope.

Outputs:

- The minimum production change that resolves each `RD-n`.
- A `## Resolution` section appended to the audit recording, per `RD-n`: the
  commit, the files touched, the command re-run, and its new exit code, ending
  with the line `All N real_defect rows resolved`.

## Frozen decisions

- The ledger is exhaustive and closed. A defect not in the ledger is not in this
  task, even when noticed in passing — flag it for a separate packet instead.
- Fix the cause, not the check. Deleting a failing test, adding
  `#[allow(clippy::…)]`, loosening a lint, marking a test `#[ignore]`, or
  narrowing a command's arguments to make it pass are all prohibited unless the
  audit itself classified the check as wrong, and then only with the reviewer's
  explicit agreement recorded in the resolution.
- That exception is live for exactly one of RD-1's three root causes. The audit
  states the `{{TASK_ID}}` template placeholder links "should be excluded from
  link checking" — a claim that the check is wrong for unexpanded template
  placeholders, not that the document is broken. Changing `check-docs.mjs` to
  exclude them is therefore permitted, and requires the reviewer's explicit
  agreement recorded in the `## Resolution` section. The exception does not
  extend to the other two root causes: the missing `planning/ORCHESTRATION.md`
  target and the `tasks/E01-T00*.md` example references are broken documents,
  and suppressing them in the checker is prohibited.
- `AGENTS.md` §15 applies: no reformatting of `core/rcdvlog-core/src/lib.rs`,
  `core/rcdvlog-core/src/operations.rs`, or `apps/desktop/src/App.tsx` beyond the
  lines a specific `RD-n` requires.
- Environmental rows are explicitly out of scope. They are D01-T004's problem, in
  the form of what CI installs.

## Owned paths

This packet was `draft` for exactly one reason: its implementer path set is the
set of files named by the audit's `RD-n` rows, and that audit did not exist yet.
D01-T001 published `docs/tasks/desktop/BASELINE_AUDIT.md`, so the set is now
enumerable and this packet is promoted to `ready`.

The audit recorded one `real_defect` (RD-1, `pnpm check:docs`), which is below
the split threshold of eight and confined to a single seam — documentation link
integrity. No split into `D01-T002a`/`D01-T002b` is required.

`path_ownership.implementer` is the four skill documentation files RD-1 names,
plus `scripts/check-docs.mjs`, plus the ledger's `## Resolution` section. The
checker is owned because RD-1 has three distinct root causes and one of them —
the `{{TASK_ID}}` placeholder link in `EPIC.template.md` — cannot be fixed in
the template alone: `check-docs.mjs` has no placeholder-exclusion mechanism, so
the fix is either an exclusion rule in the checker or a change to how the
template expresses the link. Both must be reachable.

Nothing is added to `path_ownership.independent_tester` beyond its cycle
directory. RD-1 is a repository-wide link check with no test fixtures of its
own; the tester's reproduction is running `pnpm check:docs`, not editing a test
file.

`pnpm check:docs` was added to `verification_commands` at promotion. It was
absent from the drafted list, which would have left the acceptance oracle
("every `RD-n` command exits zero") unobservable by this packet's own gate,
since RD-1's command is `pnpm check:docs`.

### Note on the files this packet edits

Four of the five source files belong to the `executing-bounded-task-cycle` and
`writing-bounded-task-packets` skills — the machinery that executes this very
cycle. Editing them is in scope and permitted; `.claude/**` is not a forbidden
path. The implementer must confine changes to the specific broken links RD-1
names and must not alter skill behavior, procedure, or schema semantics in
passing. A change to what a skill *instructs* rather than what it *links to* is
a stop condition.

## Required interface

No new public interface. This task changes no signatures, adds no operation
types, and adds no dependencies. If resolving an `RD-n` appears to require any
of those, that is a stop condition.

## Acceptance oracle

- Every `RD-n` command exits zero.
- Every command the audit recorded green still exits zero.
- The audit's `## Resolution` section accounts for every `RD-n`, with no row
  marked resolved without a re-run exit code.
- `git diff --stat` touches only files traceable to a specific `RD-n`.
- No test was deleted or newly ignored, and no lint was newly suppressed.
  `git diff` contains no added `#[ignore]`, no added `#[allow(` and no added
  `eslint-disable`, unless the resolution justifies it against a named `RD-n`.

## Independent test charter

Falsify the claim that the gate is now green.

- Use a fresh clone at the fix commit, not the implementer's working tree, so
  build caches and stray untracked files cannot mask a failure.
- Run the full `verification_commands` list, in order, and record every exit
  code.
- Run each `RD-n` command three times to catch a fix that only sometimes works;
  the audit already established which commands were flaky.
- Diff the implementer's change against the ledger and confirm every touched file
  maps to a defect id. An unexplained file is a finding.
- Grep the diff for suppression patterns: `#[ignore]`, `#[allow(`,
  `eslint-disable`, `.skip(`, `xit(`, `--no-verify`.
- Do not reuse the implementer's fixtures where a defect was in test data; build
  your own minimal reproduction of the original failure and confirm it now
  passes.

## Review charter

Risk domain: symptom suppression. The failure mode is a green gate purchased by
weakening the checks rather than fixing the code.

Questions:

1. Does each change address the cause the audit identified, or the symptom?
2. Was any assertion weakened, any test removed, any lint suppressed?
3. Is any change outside the ledger's scope?
4. Does any fix alter product behavior rather than correctness of the check, and
   if so, is that behavior change the right one?
5. Do the large files stay unreformatted per `AGENTS.md` §15?

Severity rules: a suppressed check, a deleted test, or a behavior change not
required by the ledger is blocking. An unnecessarily broad but harmless edit is
non-blocking with a note.

Smallest acceptable remedy: revert the offending hunk and fix the cause, or
escalate the row as needing its own packet.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Improving the gate, adding checks, or raising coverage.
- Fixing environmental gaps.
- Refactoring code that merely looks poor near a defect.
- Anything under `.github/**`, which belongs to D01-T004.

## Stop conditions

Stop and escalate when:

- resolving an `RD-n` requires an architecture change, a new dependency, a new
  migration, or a change to a public behavior invariant;
- resolving an `RD-n` requires touching `core/rcdvlog-server/**` or
  `apps/server/**`, which are forbidden here;
- two `RD-n` rows turn out to have the same root cause in a shared seam, making
  this task cross more than one seam;
- the only way to make a command pass is to weaken it;
- a defect turns out to be a genuine product bug requiring a design decision
  rather than a repair.

## Commands

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm check:frontend
pnpm test:web
pnpm test:integration
pnpm check:docs
```

Expected result: every command exits zero.

## Handoff

Report each `RD-n` with its root cause, the files changed, the command re-run and
its exit code, and the full gate result list. State explicitly whether any
suppression pattern was added and why. List any stop condition encountered. The
implementer does not approve this task.
