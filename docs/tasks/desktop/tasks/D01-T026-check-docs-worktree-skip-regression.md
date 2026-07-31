# D01-T026: Fix check-docs worktree-skip regression

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T026",
  "epic_id": "D01",
  "title": "Fix check-docs worktree-skip regression",
  "slug": "check-docs-worktree-skip-regression",
  "status": "ready",
  "task_class": "implementation",
  "depends_on": ["D01-T002"],
  "principal_invariant": "check-docs.mjs finds and checks all Markdown files when invoked from inside a git worktree, producing the same result as when invoked from the main checkout.",
  "central_falsification": "Run check-docs.mjs from within a `git worktree add` checkout — or a simulated directory whose .git is a file — and observe 0 Markdown files checked.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": ["scripts/check-docs.mjs"],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T026-check-docs-worktree-skip-regression.md"],
    "implementer": [],
    "independent_tester": ["scripts/check-docs.test.mjs"],
    "reviewer": ["scripts/check-docs.mjs", "scripts/check-docs.test.mjs"],
    "integrator": ["docs/tasks/desktop/EPIC.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T026-check-docs-worktree-skip-regression.md"],
    "implementer": ["scripts/check-docs.mjs"],
    "independent_tester": ["scripts/check-docs.test.mjs"],
    "integrator": ["docs/tasks/desktop/EPIC.md"]
  },
  "forbidden_paths": ["core/**", "apps/**", "packages/**", "Cargo.toml", "Cargo.lock", ".github/**"],
  "verification_commands": [
    "node --test scripts/check-docs.test.mjs",
    "pnpm check:docs",
    "node scripts/check-docs.mjs"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T026\\].*\\| check-docs worktree-skip regression \\|"
    }
  ]
}
-->

## Goal

Fix a regression in `scripts/check-docs.mjs` introduced by D01-T002: the
`walk()` function's git-worktree-skip guard fires on the walk's own starting
root when `check-docs.mjs` is invoked from inside a `git worktree` checkout,
causing the entire walk to exit before recursing into any subdirectory. The
script silently reports "Documentation check passed (0 Markdown files)" and
exits 0, having checked nothing.

The fix must ensure the worktree-skip guard still applies to nested worktrees
discovered during recursion but never to the walk's starting root. A regression
test must catch this class of failure automatically.

## Principal invariant

This task is correct when `check-docs.mjs` finds and checks all Markdown files
when invoked from inside a git worktree checkout, producing the same result as
when invoked from the main checkout.

## Inputs and outputs

Inputs:

- `scripts/check-docs.mjs` at the D01-T002 baseline (contains the buggy guard).
- The git worktree layout: a worktree root has `.git` as a file whose content
  is `gitdir: <path>`, while the main checkout root has `.git` as a directory.

Outputs:

- `scripts/check-docs.mjs` with the `walk()` guard scoped to non-root
  directories only.
- `scripts/check-docs.test.mjs` — a Node built-in test-runner regression suite
  that simulates a worktree root and asserts the checker finds files.

## Frozen decisions

- The worktree-skip guard is correct in concept — nested git worktrees
  (e.g. `.claude/worktrees/**`) should still be skipped during recursion. Only
  the unconditional application to the walk's own root is the defect.
- The fix must not add dependencies or change the script's invocation contract.
  `pnpm check:docs` and bare `node scripts/check-docs.mjs` must continue to
  work unchanged.
- The regression test uses `node:test` (Node.js built-in test runner), which
  requires no additional dependencies, runner config, or vitest setup. It is
  invocable with `node --test scripts/check-docs.test.mjs`.
- The test fixture simulates a worktree by creating a temporary directory whose
  `.git` is a regular file (not a directory). An actual `git worktree add` is
  not required for the test — the guard only cares about `statSync().isFile()`.

## Owned paths

The implementer owns `scripts/check-docs.mjs` for the fix. The independent
tester owns `scripts/check-docs.test.mjs` for the regression suite. The
integrator owns the queue entry in `EPIC.md`.

## Required interface

No interface change. `check-docs.mjs` is invoked the same way and exits with
the same contract: stdout success message with file count on pass, stderr
failure list and exit 1 on failure.

## Acceptance oracle

| # | Case | Expected |
| ---: | --- | --- |
| 1 | `node scripts/check-docs.mjs` from main checkout | ≥100 Markdown files checked, exit 0 |
| 2 | `node scripts/check-docs.mjs` from inside a `git worktree add` checkout | ≥100 Markdown files checked, exit 0 |
| 3 | `pnpm check:docs` from either checkout | exit 0 |
| 4 | `node --test scripts/check-docs.test.mjs` | 3/3 tests pass, including simulated worktree |

## Independent test charter

Falsify the claim that `check-docs.mjs` works correctly from inside a worktree:

1. Create a `git worktree add` checkout of the repository.
2. Run `node scripts/check-docs.mjs` inside that worktree.
3. Confirm the output reports >0 Markdown files checked (the regression reports
   0).
4. Run `pnpm check:docs` inside the worktree and confirm exit 0.
5. Run `node --test scripts/check-docs.test.mjs` inside the worktree and
   confirm all tests pass.

Additionally, verify the regression test itself catches the old code:
temporarily revert the fix, run the test, and confirm at least one test fails.

## Review charter

Risk domain: silent false-pass. A documentation checker that checks nothing and
exits 0 is worse than no checker at all — it creates false confidence that the
docs are valid when they may contain broken links, unbalanced fences, or
missing required artifacts.

Questions:

1. Does the fix correctly distinguish "root directory" from "subdirectory with
   a .git file"?
2. Does the regression test actually fail against the buggy code (i.e., is it
   a valid falsification)?
3. Are nested worktrees still skipped during recursion?
4. Does the test fixture include a nested `.git`-file subdirectory and verify
   its contents are excluded?

Severity rules: a silent 0-file pass from within a worktree is blocking. A test
that doesn't actually fail against the old code is blocking. Non-blocking:
prose quality in comments, fixture file count matching exact repo state.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Adding vitest configuration at the repository root.
- Extending the checker with new validation rules.
- Fixing any other D01-T002 regressions.
- Changing how `pnpm check:docs` is wired in `package.json`.

## Stop conditions

Stop and escalate when:

- The fix requires changing the script's public interface (invocation, exit
  code contract, stdout/stderr format).
- The regression test cannot be written with `node:test` alone and would
  require adding a devDependency.
- The worktree-skip guard cannot be scoped to non-root directories without
  breaking nested-worktree skipping.

## Commands

```sh
# Run the checker from the main checkout — must find files.
node scripts/check-docs.mjs
# Expected: "Documentation check passed (N Markdown files)." with N ≥ 100

# Run the regression test suite.
node --test scripts/check-docs.test.mjs
# Expected: 3/3 tests pass

# Run the full docs check via pnpm.
pnpm check:docs
# Expected: exit 0
```

## Handoff

Report the changed paths (`scripts/check-docs.mjs`,
`scripts/check-docs.test.mjs`), the exact output of each verification command,
and any stop condition encountered. Confirm that the checker reports the same
file count from both the main checkout and a git worktree checkout. The
implementer does not approve this task.
