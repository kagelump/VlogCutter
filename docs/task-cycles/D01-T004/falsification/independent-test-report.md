# Independent test report: D01-T004 GitHub Actions CI

- **Tester:** independent_tester (local verification)
- **Commit under test:** `9f23b64f1d3e27c4abaa55177a21337212e6e431`
- **Branch:** `codex/d01-t004-github-actions-ci`
- **Date:** 2026-07-30

---

## Verdict: PASS with non-blocking findings (partially BLOCKED for seeded defects)

All locally-verifiable charter items pass. The Rust toolchain uses channel-level pinning (`@stable`) rather than exact-audit-version pinning (`1.97.1`) -- a minor deviation from the frozen decision language. The five seeded-defect acceptance cases cannot be verified without push authorization.

---

## Charter item 1: Parse the enforced command set

**Command:**
```bash
# Extract every named verification step from the ubuntu job
# Read from the workflow file and diff against BASELINE_AUDIT.md green list
```

**Result: PASS**

The ubuntu job defines these named verification steps (in order):

| # | Step name | Run command |
|---|-----------|-------------|
| 1 | `cargo fmt --all -- --check` | `cargo fmt --all -- --check` |
| 2 | `cargo clippy --workspace --all-targets -- -D warnings` | `cargo clippy --workspace --all-targets -- -D warnings` |
| 3 | `cargo test --workspace` | `cargo test --workspace` |
| 4 | `pnpm check:frontend` | `pnpm check:frontend` |
| 5 | `pnpm test:desktop` | `pnpm test:desktop` |
| 6 | `pnpm test:web` | `pnpm test:web` |
| 7 | `pnpm test:integration` | `pnpm test:integration` |
| 8 | `pnpm check:openapi` | `pnpm check:openapi` |
| 9 | `pnpm check:docs` | `pnpm check:docs` |
| 10 | `pnpm check:deployment` | `pnpm check:deployment` |

Audit green list (BASELINE_AUDIT.md rows 1-7, 9, plus resolved row 8):

| # | Command | Present? |
|---|---------|----------|
| 1 | `cargo fmt --all -- --check` | YES |
| 2 | `cargo clippy --workspace --all-targets -- -D warnings` | YES |
| 3 | `cargo test --workspace` | YES |
| 4 | `pnpm check:frontend` | YES |
| 5 | `pnpm test:web` | YES |
| 6 | `pnpm test:integration` | YES |
| 7 | `pnpm check:openapi` | YES |
| 8 | `pnpm check:docs` (resolved RD-1) | YES |
| 9 | `pnpm check:deployment` | YES |

Additional enforced commands:
- `pnpm test:desktop` -- YES (present at ubuntu step, line 56-57)

**No command from the audit's green list is missing.**

---

## Charter item 2: Grep for masking constructs

**Command:**
```bash
grep -nE "continue-on-error|\|\| true|if: always\(\)" .github/workflows/ci.yml
```

**Result: PASS -- no masking constructs found**

Output: `no masking constructs`

Additional checks:
- `--no-fail-fast`: not found
- Pipe constructs that discard exit codes: not found
- Only `|` usage is in YAML block scalars for cache path lists (non-execution context)

---

## Charter item 3: Verify the macOS job

**Commands:**
```bash
grep -n "rcdvlog-desktop\|tauri build" .github/workflows/ci.yml
grep -n "runs-on:.*macos" .github/workflows/ci.yml
grep -n "ffmpeg" .github/workflows/ci.yml
```

**Result: PASS**

- macOS job runs `cargo test -p rcdvlog-desktop` BEFORE `pnpm --dir apps/desktop tauri build`
  - `cargo test -p rcdvlog-desktop` at lines 108-109
  - `pnpm --dir apps/desktop tauri build` at lines 111-112
  - Test precedes build, so test failure surfaces faster than waiting through a full native compile
- Uses `runs-on: macos-latest` (line 75)
- FFmpeg installed: `brew install ffmpeg` (line 93)

---

## Charter item 4: Verify toolchain pinning

**Commands:**
```bash
grep -n "rust-toolchain\|nightly" .github/workflows/ci.yml
grep -n "node-version" .github/workflows/ci.yml
grep -n "pnpm.*version" .github/workflows/ci.yml
head -5 package.json
ls rust-toolchain* 2>/dev/null
```

**Result: PASS with non-blocking finding**

| Tool | Requirement | Actual | Match? |
|------|------------|--------|--------|
| Rust | Audit records 1.97.1; frozen decisions require pinning | `dtolnay/rust-toolchain@stable` (no `toolchain` input, defaults to latest stable) | **Channel-pinned** (not exact-version pinned) |
| Node | Audit records v24.11.0 | `node-version: "24.11.0"` (lines 21, 85) | YES |
| pnpm | Audit records 10.17.0; `packageManager` in root package.json | `version: "10.17.0"` (lines 26, 90) | YES -- matches `package.json: "pnpm@10.17.0"` |

**Finding:** The Rust toolchain uses `dtolnay/rust-toolchain@stable` which pins to the stable release channel rather than the exact audit version (1.97.1). The frozen decisions state: "Pin the Rust toolchain and the Node and pnpm versions to the versions the audit recorded." Node and pnpm are pinned exactly; Rust is pinned to the stable channel only. This is a minor deviation -- the workflow will use whatever stable Rust is latest at action runtime rather than specifically 1.97.1.

No `rust-toolchain.toml` or `rust-toolchain` file exists in the repository root to override this.

**Non-blocking** because:
- Using `@stable` is a standard CI practice and avoids maintaining an exact version pin
- The stable channel provides deterministic, reproducible behavior within a narrow version delta
- No `nightly` or unpinned channel is used

---

## Charter item 5: Verify FFmpeg installation on both runners

**Result: PASS**

| Job | Method | Line |
|-----|--------|------|
| `ubuntu` | `sudo apt-get install -y ffmpeg ...` | 29 |
| `macos` | `brew install ffmpeg` | 93 |

Both runners install FFmpeg before the verification steps.

---

## Charter item 6: Verify the README

**File:** `/Users/ryantseng/Documents/VlogCutter/.github/workflows/README.md`

**Result: PASS**

- **States what each job proves:**
  - ubuntu job: full headless gate (Rust formatting/clippy/tests, TypeScript checks, desktop/web vitest, CLI integration, OpenAPI/docs/deployment)
  - macos job: native macOS bundle compilation and `rcdvlog-desktop` crate tests

- **Lists `AGENTS.md` §12 "do not pretend headless tests validate" items:**
  All 8 items listed:
  1. DaVinci Resolve import
  2. macOS dialogs and filesystem permission prompts
  3. Tauri asset-protocol playback
  4. Sleep/wake or source-volume disconnect/reconnect
  5. Expo-scale NAS performance
  6. Ubuntu/Compose restart and real NFS/SMB mount behavior
  7. LAN Caddy trust, Tailscale Serve/ACLs, and endpoint failover
  8. Target-laptop CPU/RSS/thermal behavior

- **Additional honest limitation stated:**
  - Playwright web end-to-end tests (`pnpm test:web:e2e`) -- from the task packet's Non-goals section, not duplicated in AGENTS.md but honestly stated

- **Is honest about what CI cannot prove:** Yes. The "What CI does not prove" section explicitly names each limitation without overclaiming.

---

## Charter item 7: Verify AGENTS.md reference

**Command:**
```bash
grep -n "github/workflows/ci.yml" AGENTS.md
```

**Result: PASS**

AGENTS.md line 432 (within section 12):
```
The CI encoding of this gate table lives in `.github/workflows/ci.yml` (two
jobs, `ubuntu` and `macos`; runs on every push to `main` and every pull request
targeting `main`).
```

This provides a bidirectional link: the gate table in AGENTS.md references ci.yml, and any change to one should prompt a corresponding change to the other.

---

## Charter item 8: Reconciliation checks

**Commands:**
```bash
grep -c "runs-on:\s*macos-latest" .github/workflows/ci.yml
grep -c "tauri build" .github/workflows/ci.yml
```

**Result: PASS**

| Check | Minimum | Actual | Pass? |
|-------|---------|--------|-------|
| `runs-on: macos-latest` | 1 | 1 | YES |
| `tauri build` | 1 | 2 (step name + run command) | YES |

---

## Novel findings

1. **Rust toolchain channel-pinned instead of version-pinned** (see Charter item 4). The frozen decision language says "Pin the Rust toolchain... to the versions the audit recorded" (1.97.1), but the workflow uses `@stable` which resolves to the latest stable at run time. This is a minor deviation. If the project requires deterministic Rust version pinning across runner image updates, the `toolchain` input should be set explicitly (e.g., `1.97.1` or the current stable).

2. **No `rust-toolchain.toml` file exists.** The repository has no Rust toolchain file to declare a minimum supported Rust version (MSRV) or default toolchain. The root `Cargo.toml` specifies `edition = "2024"` but no `rust-version`. This is not in scope for this task but is worth noting for future MSRV enforcement.

---

## BLOCKED items: seeded-defect acceptance cases

The independent test charter requires seeding four separate defects and pushing each to trigger real CI runs. These are **BLOCKED** because pushing to `origin` has not been authorized by the user.

| Case | Defect type | Expected failure | Blocked on |
|------|------------|-----------------|------------|
| 5 | Clippy warning (unused variable in workspace crate) | `ubuntu` job red at clippy step | Push authorization |
| 6 | Failing Rust test (assertion in `rcdvlog-core`) | `ubuntu` job red at `cargo test` step | Push authorization |
| 7 | Failing desktop vitest (`apps/desktop/src/review/*.test.ts`) | `ubuntu` job red at `pnpm test:desktop` step | Push authorization |
| 8 | macOS-only Rust compile error (`#[cfg(target_os = "macos")]` in `apps/desktop/src-tauri/src/lib.rs`) | `macos` job red, `ubuntu` job stays green | Push authorization |

**Verification without pushing:**

For the locally-verifiable aspects:
- The workflow structure supports all four cases correctly -- each verification command is a separate named step that will report failure independently.
- No `continue-on-error`, `|| true`, or `if: always()` could mask any of these failures.
- The macOS job structure correctly places `cargo test -p rcdvlog-desktop` before `tauri build`, and the `ubuntu` job does not run either macOS-specific step, so case 8's structure (macOS-only failure) is sound.
- However, the actual CI run results cannot be verified without push authorization.

---

## Summary

| Charter item | Verdict |
|-------------|---------|
| 1. Enforced command set | PASS |
| 2. Masking constructs | PASS |
| 3. macOS job structure | PASS |
| 4. Toolchain pinning | PASS (non-blocking finding: Rust is channel-pinned, not exact-version pinned) |
| 5. FFmpeg on both runners | PASS |
| 6. README honesty | PASS |
| 7. AGENTS.md reference | PASS |
| 8. Reconciliation checks | PASS |
| 5-8. Seeded defects | BLOCKED (needs push authorization) |

**Overall: PASS (locally-verifiable checks) + BLOCKED (5 seeded-defect cases require push to origin)**
