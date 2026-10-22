---
description: Run the checked-in VlogCutter verification gate. Use --scope fast while iterating and the default pre-pr gate before review or handoff. Reports every check and exits nonzero when any check or prerequisite fails.
---

# Verification Gate

Run `pnpm gate` at the repository root. The executable implementation is
`scripts/pre-pr-gate.mjs`; it reads the Rust toolchain pin from
`.github/workflows/ci.yml`, prints each command's output and status, and runs
the remaining checks after a failure so the final summary shows every result.

## Fast iteration

Use `pnpm gate --scope fast` (or `node scripts/pre-pr-gate.mjs --scope fast`):
it runs `git diff --check origin/main...HEAD` for committed whitespace,
staged and unstaged `git diff --check` for local changes, pinned `cargo fmt`,
and `pnpm check:frontend`. Run `git fetch origin main` if the committed-candidate
check reports that `origin/main` is unavailable; the gate fails closed. After
Rust changes, run the relevant package test
directly too. For one named test, use
`pnpm test:focused <cargo-package> <exact-test-name>`; it pins the Rust
toolchain and fails if Cargo reports zero tests or omits the requested result.

## Mandatory pre-PR gate

Run `pnpm gate` with no arguments for the pre-PR checks. For a direct push or
PR creation, use `pnpm publish:after-gate push` or
`pnpm publish:after-gate pr`; the wrapper prints the candidate HEAD for the
handoff receipt, runs the real gate once, then checks that HEAD and the
index/worktree state still matches the pre-gate snapshot immediately before
each external action. The `push` action runs the
fixed upstream push; `pr` runs that same push followed by `gh pr create --fill`,
and stops if the push fails. Both refuse to start with a dirty tree.

The pre-PR gate checks diff whitespace, exact Rust version, fmt, clippy, the workspace tests,
frontend type checks, desktop unit/CUJ/coverage/browser tests, web tests, CLI
integration, OpenAPI, docs, script tests, and deployment config. Install
dependencies first (`pnpm install --frozen-lockfile`) and install Playwright's
desktop Chromium with `pnpm --dir apps/desktop exec playwright install
chromium` if it is not already present. These commands match the Ubuntu job in
`.github/workflows/ci.yml`.

## Failure handling

The gate preserves each command's exit code, prints all output (including
`test result` lines on a failed test command), and ends with one PASS/FAIL line
per check. Missing commands and a Rust toolchain other than the CI pin fail the
gate. A successful `cargo test` must report at least one executed test.

After changing code, re-run checks affected by that change; an earlier green
result is stale. After the pre-PR run, report its full output or the completed
CI run URL and its failed-step logs. Do not summarize an incomplete or failed
run as a pass.

See `AGENTS.md` §6 and §12 for portable prerequisites and the verification
contract.
