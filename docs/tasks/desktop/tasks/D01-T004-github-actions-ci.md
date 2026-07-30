# D01-T004: GitHub Actions CI with macOS native build

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T004",
  "epic_id": "D01",
  "title": "GitHub Actions CI with macOS native build",
  "slug": "github-actions-ci",
  "status": "complete",
  "task_class": "integration",
  "depends_on": ["D01-T003"],
  "principal_invariant": "Every push and pull request runs the full audited-green verification gate on Ubuntu and compiles the native macOS desktop bundle, and any seeded defect in either turns the run red.",
  "central_falsification": "On a scratch branch, seed one clippy warning, one failing Rust test, one failing desktop vitest case, and one Rust compile error reachable only from the Tauri desktop crate; each must independently produce a red run, and the Tauri error must be caught by the macOS job.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": ["repository verification gate -> CI workflow"],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/desktop/ARCHITECTURE.md", "docs/tasks/desktop/BASELINE_AUDIT.md", "AGENTS.md"],
    "independent_tester": ["docs/tasks/desktop/ARCHITECTURE.md", "docs/tasks/desktop/BASELINE_AUDIT.md", "AGENTS.md"],
    "reviewer": ["docs/tasks/desktop/ARCHITECTURE.md", "docs/tasks/desktop/BASELINE_AUDIT.md", "AGENTS.md"],
    "integrator": ["docs/tasks/desktop/ARCHITECTURE.md", "docs/tasks/desktop/BASELINE_AUDIT.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T004-github-actions-ci.md"],
    "implementer": [],
    "independent_tester": ["docs/task-cycles/D01-T004/falsification/**"],
    "integrator": [
      ".github/workflows/ci.yml",
      ".github/workflows/README.md",
      "package.json",
      "docs/tasks/desktop/BASELINE_AUDIT.md",
      "docs/task-cycles/D01-T004/evidence/**"
    ]
  },
  "forbidden_paths": ["core/**", "apps/**", "packages/**", "PRD.md", "Cargo.toml"],
  "verification_commands": [
    "node -e \"require('js-yaml')\" || true",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings",
    "cargo test --workspace",
    "pnpm check:frontend",
    "pnpm test:desktop",
    "pnpm test:web",
    "pnpm test:integration"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T004\\].*\\| complete \\|"
    },
    {
      "path": ".github/workflows/ci.yml",
      "regex": "runs-on:\\s*macos-latest"
    },
    {
      "path": ".github/workflows/ci.yml",
      "regex": "tauri build"
    },
    {
      "path": "AGENTS.md",
      "regex": "\\.github/workflows/ci\\.yml"
    }
  ]
}
-->

## Goal

Put this repository under continuous integration for the first time, enforcing
the verification gate the baseline audit proved green, and proving on every
change that the native macOS desktop bundle still compiles.

## Principal invariant

This task is correct when every push and pull request runs the full
audited-green verification gate on Ubuntu and compiles the native macOS desktop
bundle, and any seeded defect in either turns the run red.

## Inputs and outputs

Inputs:

- `docs/tasks/desktop/BASELINE_AUDIT.md` — the authoritative list of which gate
  commands are green and which host tools they need.
- `AGENTS.md` §6 (toolchain prerequisites) and §12 (the gate table).
- `package.json` scripts, including `test:desktop` added by D01-T003.

Outputs:

- `.github/workflows/ci.yml` with two jobs.
- `.github/workflows/README.md` stating what each job proves and, explicitly,
  what CI cannot prove.
- A `## CI enforcement` section appended to the baseline audit mapping every gate
  command to the job that runs it, or to the reason it is not enforced.
- An `AGENTS.md` §12 reference to the workflow, so the gate table and CI cannot
  drift apart unnoticed.

## Frozen decisions

Decided by the product owner on 2026-07-29 and by
`docs/tasks/desktop/ARCHITECTURE.md` §5.1. The implementer may not revisit these.

- **Provider:** GitHub Actions. The remote is `github.com/kagelump/VlogCutter`.
- **Triggers:** `push` to `main` and `pull_request` targeting `main`.
- **Job `ubuntu`** on `ubuntu-latest` runs the headless gate:
  `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D
  warnings`, `cargo test --workspace`, `pnpm check:frontend`, `pnpm test:desktop`,
  `pnpm test:web`, `pnpm test:integration`, `pnpm check:openapi`,
  `pnpm check:docs`, `pnpm check:deployment`.
- **Job `macos`** on `macos-latest` runs `pnpm --dir apps/desktop tauri build`
  and `cargo test -p rcdvlog-desktop`. This job exists because the desktop is the
  product and a gate that never compiles it is not a gate.
- **FFmpeg is installed on both runners.** The CLI integration suite creates a
  real one-second FFmpeg fixture and skips its media portion when FFmpeg is
  absent; a silently skipping suite is not a gate.
- **Enforced set equals audited-green set.** Any command the audit classified
  `real_defect` and D01-T002 did not resolve, or that cannot run on a runner, is
  an explicit row in the audit's `## CI enforcement` section with a reason. It is
  never silently dropped from the workflow.
- **No `continue-on-error`, no `|| true`, and no `if: always()` used to mask a
  failing step.** A step that is allowed to fail is not a check.
- **Pinned toolchains.** Pin the Rust toolchain and the Node and pnpm versions to
  the versions the audit recorded. `pnpm@10.17.0` is already declared in the root
  `package.json` `packageManager` field; use it rather than a second source of
  truth.
- **Caching is permitted but never load-bearing.** A cold cache must produce the
  same result as a warm one. Do not key a cache in a way that lets a stale
  artifact satisfy a check.

## Owned paths

Integration class: the workflow files and both `package.json` manifests are
shared hot spots, and the baseline audit is a shared planning document. All of
them belong to the integrator. The independent tester owns a separate
falsification directory so its seeded-defect evidence is never confused with the
integrator's run evidence.

Every source path is forbidden. This task wires up verification; it does not fix
what verification finds. If CI reveals a defect the audit missed, that is a stop
condition.

## Required interface

```yaml
name: ci
on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

jobs:
  ubuntu:
    runs-on: ubuntu-latest
  macos:
    runs-on: macos-latest
```

Job ids are `ubuntu` and `macos` exactly, because later packets and branch
protection reference them by name. Each gate command is its own named step, so a
failure names the check that failed rather than a shell script line number.

## Acceptance oracle

1. `.github/workflows/ci.yml` parses as valid YAML and GitHub accepts it — a real
   run appears in the Actions tab for a pushed branch.
2. A run on an unmodified checkout of `main` is green in both jobs.
3. The `ubuntu` job's step list contains every command the audit classified
   green, with no command missing and no command wrapped in a construct that can
   swallow a nonzero exit.
4. The `macos` job runs `tauri build` and it succeeds, producing the bundle.
5. Seeded defect, clippy: adding an unused variable to a workspace crate turns
   the `ubuntu` job red at the clippy step.
6. Seeded defect, Rust test: making one `rcdvlog-core` assertion false turns the
   `ubuntu` job red at the `cargo test` step.
7. Seeded defect, desktop frontend: making one `apps/desktop/src/review/*.test.ts`
   assertion false turns the `ubuntu` job red at the `pnpm test:desktop` step.
   This case is the reason D01-T003 comes first.
8. Seeded defect, native only: a Rust compile error introduced in
   `apps/desktop/src-tauri/src/lib.rs` behind `#[cfg(target_os = "macos")]` turns
   the `macos` job red while the `ubuntu` job stays green. This is the case that
   proves the macOS job earns its cost.
9. `.github/workflows/README.md` lists the `AGENTS.md` §12 "do not pretend
   headless tests validate" items as things CI does not prove.
10. Grep of the workflow finds no `continue-on-error`, no `|| true`, and no
    `if: always()` on a verification step.

## Independent test charter

Falsify the claim that CI catches regressions.

- Work on a scratch branch. Seed each of the four defects in acceptance cases
  5–8 **separately**, push, and record the run URL, the job that failed, and the
  step that failed. A batch of four defects at once proves only that something is
  red.
- Confirm case 8 rigorously: the `ubuntu` job must stay green while `macos` goes
  red. If both fail, the defect was not macOS-specific and the case is unproven.
- Independently reconstruct the enforced command set by parsing
  `.github/workflows/ci.yml`, and diff it against the audit's green list. Report
  every command in the audit's green list that no step runs.
- Grep the workflow for `continue-on-error`, `|| true`, `if: always()`,
  `--no-fail-fast` used to hide failures, and any step that pipes a command into
  something that discards its exit code.
- Verify a cold cache: clear the caches, re-run, and confirm the result is
  identical.
- Confirm the CLI integration suite's media portion actually ran rather than
  skipping, by finding its FFmpeg-dependent output in the log. A skipped media
  suite that reports success is the exact failure mode this packet exists to
  prevent.
- Delete every seeded defect and confirm the branch is green before finishing.

## Review charter

Risk domain: a green badge over an unenforced gate. That is worse than no CI,
because it converts absence of evidence into apparent evidence.

Questions:

1. Is any audited-green command absent from the workflow?
2. Can any step fail without failing the job?
3. Does the `macos` job genuinely build the Tauri bundle, or only run
   `cargo build`?
4. Is FFmpeg installed on both runners, and does the integration suite's media
   portion run rather than skip?
5. Are toolchain versions pinned, and is `pnpm` sourced from the existing
   `packageManager` field rather than duplicated?
6. Could a cache hit let a stale artifact satisfy a check?
7. Does `.github/workflows/README.md` honestly state the limits from
   `AGENTS.md` §12, or does it overclaim?

Severity rules: a swallowed failure, an omitted audited-green command, a skipping
media suite reported as passing, or a `macos` job that does not build the bundle
is blocking. Cache tuning, step naming, and job ordering are non-blocking.

Smallest acceptable remedy: add the missing step or remove the masking construct,
then re-run and attach the run URL.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Fixing any defect CI reveals.
- Release automation, artifact publishing, code signing, or notarization.
- Coverage reporting, badges, or dependency scanning.
- Running Playwright web end-to-end tests in CI — `pnpm test:web:e2e` needs
  browser downloads and a server, and it stays a local and manual check in this
  packet.
- Configuring branch protection, which requires repository administrator rights
  the implementer does not have. Recommend it in the handoff instead.
- Ubuntu, Compose, NAS, LAN, or Tailscale topology acceptance, which belongs to
  the server epic.

## Stop conditions

Stop and escalate when:

- a gate command the audit recorded green fails on a runner, which means the
  audit and CI disagree and the discrepancy needs a decision rather than a
  workaround;
- `tauri build` requires a signing identity or a secret to complete;
- enforcing a command would require changing source code, which is forbidden
  here;
- the `macos` runner cannot install a required prerequisite;
- making CI green appears to require `continue-on-error` or any other masking
  construct;
- pushing branches or opening pull requests against the remote has not been
  authorized by the user — CI cannot be validated without a real run, and that
  authorization is the user's to give.

## Commands

Locally, before pushing:

```sh
node -e "const fs=require('fs');const t=fs.readFileSync('.github/workflows/ci.yml','utf8');if(!/runs-on:\s*macos-latest/.test(t))throw new Error('macos job missing');console.log('workflow shape ok')"
grep -nE "continue-on-error|\|\| true|if: always\(\)" .github/workflows/ci.yml && echo "MASKING FOUND" || echo "no masking constructs"
```

Then the gate itself, which CI must reproduce:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm check:frontend
pnpm test:desktop
pnpm test:web
pnpm test:integration
```

Expected result: the grep reports no masking constructs, the shape check prints
`workflow shape ok`, and every gate command exits zero locally and in the
`ubuntu` job.

## Handoff

Report the workflow file, the enforced command list mapped to jobs, the
audited-green commands not enforced with reasons, the run URL for a green `main`
run, and the four separate run URLs for the seeded-defect cases including the
one proving `macos` catches what `ubuntu` misses. Recommend the branch-protection
settings the user should apply, and state that applying them requires their
administrator access. List any stop condition encountered. The integrator does
not approve this task.
