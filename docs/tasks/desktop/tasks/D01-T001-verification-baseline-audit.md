# D01-T001: Verification baseline audit

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T001",
  "epic_id": "D01",
  "title": "Verification baseline audit",
  "slug": "verification-baseline-audit",
  "status": "complete",
  "task_class": "qa",
  "depends_on": [],
  "principal_invariant": "Every command in the AGENTS.md section 12 verification gate has a recorded, reproducible pass or fail result against the current main commit.",
  "central_falsification": "Independently re-run three commands the ledger marks green and one it marks failing on a clean checkout of the recorded SHA; a ledger row that does not reproduce falsifies the audit.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["AGENTS.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "independent_tester": ["AGENTS.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "reviewer": ["AGENTS.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "integrator": ["docs/tasks/desktop/EPIC.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T001-verification-baseline-audit.md"],
    "implementer": [],
    "independent_tester": ["docs/tasks/desktop/BASELINE_AUDIT.md"],
    "integrator": ["docs/task-cycles/D01-T001/**"]
  },
  "forbidden_paths": ["core/**", "apps/**", "packages/**", "scripts/**", "Cargo.toml", "Cargo.lock", "package.json", ".github/**"],
  "verification_commands": [
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings",
    "cargo test --workspace",
    "pnpm check:frontend",
    "pnpm test:web",
    "pnpm test:integration",
    "pnpm check:openapi",
    "pnpm check:docs",
    "pnpm check:deployment"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T001\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Measure the true current state of the repository's verification gate before any
CI is written, and publish the result as the defect ledger that D01-T002 fixes
and the enforceable command set that D01-T004 encodes.

This is a measurement task. It changes no production code. It exists because CI
cannot honestly enforce a gate whose present pass/fail state nobody has
recorded, and because a fix packet cannot own its paths before the defects are
known.

## Principal invariant

This task is correct when every command in the `AGENTS.md` §12 verification gate
has a recorded, reproducible pass or fail result against the current `main`
commit.

## Inputs and outputs

Inputs:

- The current `main` commit, recorded by full SHA.
- The command table in `AGENTS.md` §12.
- The toolchain prerequisites in `AGENTS.md` §6.

Outputs:

- `docs/tasks/desktop/BASELINE_AUDIT.md` containing:
  - the audited commit SHA, the date, and the host toolchain versions
    (`rustc --version`, `cargo --version`, `node --version`, `pnpm --version`,
    `ffmpeg -version` first line, `ffprobe -version` first line);
  - one row per gate command with: verbatim exit code, wall time, and result
    classified as `green`, `real_defect`, or `environmental`;
  - for every non-green row, the verbatim first 40 lines of failure output, the
    file and line it points at when one exists, and a one-sentence statement of
    what is broken;
  - an explicit `real_defect` count, which is the size of the D01-T002 ledger;
  - an explicit `environmental` list naming what is missing on the host and
    whether CI can supply it.

## Frozen decisions

- Classification is three-valued and the boundary is fixed: `environmental`
  means the command fails only because a host tool or fixture is absent (missing
  FFmpeg, missing Playwright browsers, missing mounted volume) and would pass
  with it present. Everything else that is not green is `real_defect`. A slow
  command is not a defect. A flaky command is a `real_defect`.
- A command that fails partway through is recorded as failing. Do not re-run it
  with narrower arguments and record the narrower result.
- Each command is run from a clean tree at the recorded SHA. `cargo clean` is not
  required, but the tree must have no uncommitted changes; record `git status
  --porcelain` output as part of the evidence.
- Flakiness is measured, not assumed: any command that fails is re-run twice more
  and all three results are recorded.

## Owned paths

The independent tester owns the ledger, and it is the only file this task
creates. Every production path in the repository is forbidden, including
`.github/**` — writing CI is D01-T004's job, and an auditor who fixes what they
are auditing has destroyed the oracle. The integrator owns the cycle evidence
directory.

## Required interface

The ledger is consumed by two later packets, so its shape is fixed:

```md
## Gate results

| # | Command | Exit | Wall time | Classification |
| ---: | --- | ---: | ---: | --- |
| 1 | `cargo fmt --all -- --check` | 0 | 3s | green |

## Real defects

### RD-1: <one-line summary>

- Command: `<exact command>`
- Points at: `path/to/file.rs:123`
- Runs: fail, fail, fail
- Output:

```text
<verbatim first 40 lines>
```

## Environmental gaps

| Missing tool or fixture | Commands affected | Can CI supply it |
| --- | --- | --- |
```

Defect ids are `RD-1`, `RD-2`, … and are stable. D01-T002 and D01-T004 reference
them by id, so they must never be renumbered after publication.

## Acceptance oracle

- Every command in the `verification_commands` list appears exactly once in the
  gate results table.
- Every row has a numeric exit code. No row is blank, aspirational, or marked
  "not run" without a stated reason in the environmental table.
- Every non-green row has a corresponding `RD-n` entry or environmental entry.
- The recorded SHA matches `git rev-parse HEAD` at audit time.
- The `real_defect` count in the summary equals the number of `RD-n` sections.
- No file outside `docs/tasks/desktop/BASELINE_AUDIT.md` is modified.

## Independent test charter

Falsify the claim that the ledger is reproducible.

- Check out the recorded SHA into a fresh temporary clone — not the working tree
  the audit ran in.
- Re-run three commands the ledger marks `green` and confirm they are green.
- Re-run every command the ledger marks `real_defect` and confirm each fails,
  and that the failure output matches the recorded output in substance.
- Re-run one command the ledger marks `environmental` after supplying the
  missing tool, and confirm it then passes. If it still fails, the row was
  misclassified and is really a `real_defect`.
- Confirm `git status --porcelain` at the recorded SHA is empty, so the results
  are not contaminated by uncommitted work.
- Report any row that does not reproduce. A single non-reproducing row fails
  this task.

## Review charter

Risk domain: false confidence. A ledger that under-reports defects will produce
a CI workflow that enforces a weaker gate than the repository claims.

Questions:

1. Is any command classified `environmental` that is actually a code defect?
2. Is any command silently absent from the table?
3. Does any failure output get paraphrased where verbatim output was required?
4. Are defect ids stable and unique?
5. Does the ledger overstate green results by narrowing a command's scope?

Severity rules: a misclassified defect or an omitted command is blocking. A
missing wall time is non-blocking. Prose quality is not reviewable here.

Smallest acceptable remedy: amend the specific ledger row, re-run that command,
and record the corrected result. Do not rewrite the ledger.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Fixing any defect found. That is D01-T002.
- Writing any CI configuration. That is D01-T004.
- Auditing native or manual behavior. That is D01-T005.
- Judging whether the gate is the *right* gate.
- Adding new tests or new commands to the gate.

## Stop conditions

Stop and escalate when:

- a gate command cannot run at all because a prerequisite in `AGENTS.md` §6 is
  missing and cannot be installed without the user's authorization;
- a command's failure appears to corrupt state (writes into a source media tree,
  modifies a catalog outside a temporary directory, or leaves the tree dirty);
- the `real_defect` count exceeds eight, which means D01-T002 is too large for
  one packet and the contract editor must split it;
- any gate command requires network access to a service this repository does not
  control;
- fixing something looks faster than recording it. Record it anyway.

## Commands

```sh
git rev-parse HEAD
git status --porcelain
rustc --version && cargo --version && node --version && pnpm --version
ffmpeg -version | head -1 && ffprobe -version | head -1
```

Then, recording exit code and wall time for each:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm check:frontend
pnpm test:web
pnpm test:integration
pnpm check:openapi
pnpm check:docs
pnpm check:deployment
```

Expected result: each command produces a recorded exit code. A nonzero exit is a
valid and useful outcome for this task, not a failure of it.

## Handoff

Report the audited SHA, the full gate results table, the `RD-n` defect list with
its count, the environmental gap list, and any stop condition encountered. State
explicitly that no production file was modified, with `git status --porcelain`
output as evidence. The auditor does not approve this task and does not fix what
it finds.
