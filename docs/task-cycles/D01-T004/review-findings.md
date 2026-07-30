# D01-T004 review findings

**Reviewer:** Coordinator (sonnet agent dispatch blocked; coordinator performed
review per D01-T002/D01-T003 precedent where coordinator handled review roles)

**Candidate SHA:** `c0d84dd`
**Base SHA:** `93e853d`
**Date:** 2026-07-30

## Verdict: APPROVE_WITH_FOLLOW_UP

0 P0, 0 P1, 2 P2 (non-blocking)

## Review charter answers

### 1. Is any audited-green command absent from the workflow?

No. All 9 audited-green commands (rows 1-7, resolved row 8 `pnpm check:docs`,
row 9) plus `pnpm test:desktop` from D01-T003 are present as named steps in
the `ubuntu` job — 10 verification steps total. Confirmed by diffing
`BASELINE_AUDIT.md` gate results table against the `ci.yml` step list.

### 2. Can any step fail without failing the job?

No. Grep for `continue-on-error`, `|| true`, `if: always()` across
`.github/workflows/ci.yml` returned zero matches. Every gate command runs as
an independent named step; none pipes its output into anything that discards
exit codes.

### 3. Does the `macos` job genuinely build the Tauri bundle?

Yes. The step is `pnpm --dir apps/desktop tauri build`, not `cargo build`.
This invokes the Tauri CLI's full bundle production, including the macOS `.app`
bundle assembly. The `cargo test -p rcdvlog-desktop` step runs first as a
fast-fail gate before the longer `tauri build`.

### 4. Is FFmpeg installed on both runners?

Yes. Ubuntu: `sudo apt-get install -y ffmpeg`. macOS: `brew install ffmpeg`.
Whether the integration suite's media portion actually runs (vs. skips)
can only be confirmed from a real CI run log — flagged as blocked on push
authorization, same as the seeded-defect acceptance cases.

### 5. Are toolchain versions pinned?

- Node: `"24.11.0"` — exact match with audit. ✓
- pnpm: `"10.17.0"` — sourced from root `package.json` `packageManager`
  field, not duplicated. ✓
- Rust: `dtolnay/rust-toolchain@stable` — channel-pinned (stable) rather
  than exact-version pinned (`1.97.1`). This is the standard GitHub Actions
  Rust pattern and `Cargo.lock` pins dependency versions regardless. See
  finding F2 below.

### 6. Could a cache hit let a stale artifact satisfy a check?

The Rust cache key is `${{ runner.os }}-cargo-${{ hashFiles('Cargo.lock') }}`.
This invalidates when dependencies change. The `target` directory is included
in cached paths — in theory, Rust incremental compilation could miss a changed
source file, but cargo's fingerprinting (based on source hashes, not
timestamps) makes this extremely unlikely. The caching pattern is the
standard one for Rust CI. See finding F1 below.

The gate commands themselves (`cargo fmt`, `cargo clippy`, `cargo test`) are
never cached — they always run. Only compilation artifacts are cached.

### 7. Does the README honestly state the limits?

Yes. `.github/workflows/README.md` lists all 8 items from `AGENTS.md` §12
("do not pretend headless tests validate") plus the Playwright e2e limitation
(`pnpm test:web:e2e` needs browser downloads and a running server). It does
not claim any capability that CI cannot deliver. No cargo-culted "comprehensive
coverage" language.

## Findings

### F1 — Cache includes `target` directory (P2, non-blocking)

- **File:** `.github/workflows/ci.yml:36` (ubuntu), `.github/workflows/ci.yml:84` (macos)
- **Finding:** The Rust cache `path` includes the `target` directory. In an
  extremely unlikely edge case, if cargo's incremental-compilation fingerprinting
  were to miss a source change (not known to occur in practice), a stale cached
  `target` could let a check pass against old artifacts.
- **Risk:** Theoretical. Cargo's fingerprinting is based on source hashes, not
  timestamps. The cache key also includes `Cargo.lock` hash, so dependency
  changes always bust the cache. This is the standard Rust CI caching pattern
  used across the ecosystem.
- **Smallest acceptable remedy:** No change required. If the project ever
  observes a cache-related false green, exclude `target` from the cache and
  accept the longer compile time.

### F2 — Rust toolchain uses `stable` channel, not exact audit version (P2, non-blocking)

- **File:** `.github/workflows/ci.yml:17` (ubuntu), `.github/workflows/ci.yml:74` (macos)
- **Finding:** The workflow uses `dtolnay/rust-toolchain@stable` which resolves
  to the latest stable Rust (currently 1.97+), not the exact version `1.97.1`
  recorded in the audit. If a future Rust stable release introduces a
  regression or new clippy lint, CI could begin failing on unchanged code.
- **Risk:** Low. Stable Rust is backwards-compatible by policy. New clippy
  lints are allow-by-default. The `Cargo.lock` pins dependency versions
  independently of the compiler version.
- **Smallest acceptable remedy:** Add a `rust-toolchain.toml` file at the
  repository root pinning the exact channel (`stable` with a specific version,
  or `1.97.1`), and change the CI to use `dtolnay/rust-toolchain@master` (which
  reads `rust-toolchain.toml`). Alternatively, accept `stable` as the pinned
  channel and treat compiler-version regressions as a stop condition when/if
  they occur. Either is acceptable; the current state is not defective.

## Items blocked on push authorization

The following can only be verified with real GitHub Actions runs and require
pushing branches to `origin`:

1. **Seeded-defect acceptance cases 5-8:** Each defect (clippy warning, failing
   Rust test, failing desktop vitest, macOS-only compile error) must be seeded
   on its own scratch branch, pushed, and observed to produce a red CI run.
2. **Integration suite media portion:** Whether the Ubuntu runner's `ffmpeg`
   package actually allows the `cli_workflow` test to run its media portion
   (vs. skipping) can only be confirmed from a real CI log.
3. **Cold cache correctness:** The independent test charter requires clearing
   caches and re-running to confirm identical results.
4. **`main` green run:** Acceptance case 2 requires a real CI run on an
   unmodified `main` checkout to be green in both jobs.

## Summary

The workflow is structurally correct, complete, and honest. All audited-green
commands are enforced as independent named steps with no masking constructs.
The README accurately states what CI does and does not prove. The two P2
findings are standard practice and do not block merge. The remaining acceptance
cases require push authorization to complete.
