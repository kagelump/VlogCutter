# CI workflows

## `ci.yml`

Runs for every pull request targeting `main`, main push, merge queue,
manual dispatch, and Monday 03:17 UTC full run. There are no workflow path
filters. The stable merge gate is **`ci-required`**.

### Concurrent verification

| Job | Checks |
| --- | --- |
| `linux-rust (lint)` | Pinned Rust formatting and workspace/all-target Clippy with warnings denied |
| `linux-rust (tests)` | Full workspace tests (including CLI/media integration), OpenAPI freshness |
| `frontend` | All frontend typechecks, full desktop Vitest with coverage floors (including CUJ), web Vitest, docs, script regressions, deployment invariants |
| `browser` | Existing desktop Chromium Playwright tier in Microsoft's official image |
| `macos` | Desktop Rust tests and full native Tauri bundle, when platform-relevant |
| `changes` | Conservative macOS decision, recorded in its log and step summary |
| `ci-required` | Always evaluates every required result, including failed/cancelled/missing jobs and justified skips |

Steps retain `if: ${{ !cancelled() }}` so a failed gate does not hide later
failures. Rust matrix fail-fast is disabled. Setup failures still fail the
job; later gates may report missing prerequisites rather than silently pass.
No coverage threshold or test exclusion was changed. Full coverage includes
`src/test/cuj`; `cargo test --workspace` includes `cli_workflow`. The full
local gate no longer repeats those suites, while focused commands remain.
`pnpm test:web:e2e` remains a separate local server/process acceptance tier;
it was not in the previous full CI and is not part of the isolated desktop
browser container.

### Bounded Linux setup and browser provisioning

Linux uses `ubuntu-24.04` instead of a moving release alias. The runner image
still receives updates. `scripts/ci-linux-deps.sh` records installed versions,
installs only missing FFmpeg/GTK/WebKit/AppIndicator/SVG packages, and uses
`--no-install-recommends`. A temporary apt source list selects the signed
Ubuntu HTTPS archive/security endpoints rather than the hosted Azure HTTP
mirror that repeatedly trickled downloads in measured runs. The runner's own
sources are untouched; third-party package repositories are excluded from this
step, and the temporary source list is removed on exit. Apt has one retry, 20-second socket timeouts and a
30-second lock timeout. Total update/install deadlines are 90/180 seconds,
with a five-minute step deadline. A trickling download cannot keep the job
successful after an hour. Failures retain the apt output; they require a new
run rather than an unbounded retry loop. `pkg-config`, FFmpeg and ffprobe
checks make a missing tool fail explicitly instead of skipping media tests.

The browser job uses
`mcr.microsoft.com/playwright:v1.61.1-noble@sha256:cf0daee9b994042e011bc29f20cdff1a9f682a039b43fcd738f7d8a9d3bcd9d6`.
The registry digest was verified on 2026-10-26. This official image supplies
browsers and their OS libraries; it does not supply this project's Playwright
npm dependency. Node/pnpm and frozen-lockfile installation still run. There is
no runtime `playwright install`, apt or browser-library install in this job.
`scripts/ci-browser-image.mjs` rejects a lockfile/image version mismatch; update
the tag and verified digest together when Playwright changes. See
[Microsoft's Docker guidance](https://playwright.dev/docs/docker).

A custom Linux Rust container is deferred: it would need separately maintained
GTK, FFmpeg, Clang and Rust dependencies, image publishing, and pull/cache
measurement. First measure bounded apt on hosted runners. The official browser
image can be measured directly through the Actions `Initialize containers`
step; do not count pull overhead as zero.

### Conservative macOS policy

Only a nonempty diff consisting entirely of **modified existing** files from
this exact allowlist may skip macOS:

- `deploy/server/Caddyfile`
- `deploy/server/compose.yaml`
- `deploy/server/example.env`

These configure the Linux companion deployment and are not inputs to desktop
compilation or packaging. All new/deleted/renamed files run macOS, as do the
Dockerfile, server Rust (shared dependencies/features), shared core, frontend,
Cargo/npm manifests, tooling, workflows, docs, mixed diffs, and unknown paths.
There is intentionally no broad `apps/server/**`, `core/**` or docs-only skip.

PRs use the base/head merge-base diff, including forks. Main pushes compare
before/after. Missing SHAs, missing objects, malformed diffs, initial pushes,
empty diffs and detector failure run macOS. Manual dispatch, merge groups and
weekly scheduled runs always run macOS. The decision job logs the skip reason
and writes it to the summary. Regression coverage is in
`scripts/ci-policy.test.mjs` and includes aggregate skip/failure combinations.

### Required checks and rollout

On 2026-10-26 the branch-protection endpoint reported `Branch not protected`
and the repository rulesets endpoint returned an empty list. There is no
existing required-check configuration to migrate. If branch protection is
enabled, require **only `ci-required`** (GitHub Actions) rather than the old
`ubuntu`/`macos` names or optional jobs. Requiring `macos` directly defeats
conditional skipping. The aggregate explicitly requires detector success,
all Linux checks, and macOS success unless the detector explicitly permitted
a skip; an absent output cannot permit one. It runs with `always()` so failed
or skipped dependencies cannot leave the merge gate Pending. See
[GitHub workflow syntax](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax).

### Local verification and publication

`pnpm gate --checks /absolute/path/checks.json` runs candidate whitespace,
the pinned formatter/toolchain, workspace Clippy, frontend typechecking, and
structured focused commands. Use `pnpm gate --scope fast` only for iteration.
`pnpm gate --scope pre-pr` runs the full Linux-equivalent coverage sequentially
on a provisioned local machine; install browser/media prerequisites first.
Neither scope proves native macOS behavior. Before merge, require all relevant
exact-head CI results and inspect failures. Run `pnpm publish:after-gate push`
or `pnpm publish:after-gate pr` with `--checks` to publish a clean, verified
candidate; the wrapper rechecks identity before external actions.

### Pins, cache and disk

Rust 1.97.1, Node 24.11.0, pnpm 10.17.0 and all action commit pins remain
explicit. Rust components include rustfmt/Clippy. Keep `AGENTS.md` section 6
in step with deliberate toolchain updates. `Swatinem/rust-cache` prunes
workspace artifacts before saving, with `cache-on-failure: true`; plain caches
of the roughly 14 GB target tree grow without pruning. The Rust matrix uses distinct lint/tests cache keys so the first job to finish
cannot save a lint-only cache for the test job. Separate Rust jobs increase
cold build/cache and runner-minute costs; measure them alongside
wall-clock improvement. Rust workers reclaim unused SDKs before building,
leaving `/opt/hostedtoolcache` intact. No nextest or test-thread policy change
is made: SQLite/file-lock/concurrency behavior retains Cargo's existing runner.

### Untrusted pull requests

Keep `pull_request`, never `pull_request_target`, with workflow-level
`contents: read`, SHA-pinned actions, and no secrets in steps. Fork PRs cannot
restore their caches into trusted main runs (GitHub cache scope rules); same
repository contributors still require review. No job publishes images/packages
or writes repository state. The container runs only the checkout's local test
app; it does not browse external untrusted sites. Manual dispatches are full
runs and must not be used to expose credentials to candidate code.

### What CI does not prove

CI does not prove Resolve import, native dialogs/permissions/playback,
sleep/wake or volume reconnect, NAS scale, real Ubuntu/Compose/NFS/SMB
restart behavior, LAN Caddy trust or Tailscale topology, signing/notarization,
or laptop CPU/RSS/thermals. Browser tests prove Chromium behavior against the
in-memory IPC harness, not native media/persistence. See `AGENTS.md` section 12.
