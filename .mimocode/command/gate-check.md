---
description: Run the project verification gate — format check, clippy, Rust workspace tests, TypeScript check, and frontend tests. Use after every meaningful code change before commit or handoff. Accepts an optional scope argument: "backend" (Rust only), "full" (everything including CLI doctor), or omit for default (Rust + TypeScript).
---

# Verification Gate

Run the verification gate for RCD VlogCutter. This is the "definition of done" check from AGENTS.md §12 and §16.

## Scope

Determine scope from the argument `$ARGUMENTS`:

- `backend` or `rust` — Rust-only gate (fmt + clippy + workspace tests)
- `full` — Everything including CLI doctor and all frontend checks
- *(empty)* — Default: Rust + TypeScript check (the most common form)

## Default gate (no argument)

```bash
echo "=== cargo fmt ===" && \
cargo fmt --all --check 2>&1 && \
echo "=== cargo clippy ===" && \
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -1 && \
echo "=== cargo test ===" && \
cargo test --workspace 2>&1 | grep "test result" && \
echo "=== pnpm check ===" && \
pnpm --dir apps/desktop check 2>&1 | tail -3
```

## Backend-only gate (`backend` or `rust`)

```bash
echo "=== cargo fmt ===" && \
cargo fmt --all --check 2>&1 && \
echo "=== cargo clippy ===" && \
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -1 && \
echo "=== cargo test ===" && \
cargo test --workspace 2>&1 | grep "test result"
```

## Full gate (`full`)

```bash
echo "=== git diff ===" && \
git diff --check 2>&1 && \
echo "=== cargo fmt ===" && \
cargo fmt --all --check 2>&1 && \
echo "=== cargo clippy ===" && \
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -1 && \
echo "=== cargo test ===" && \
cargo test --workspace 2>&1 | grep "test result" && \
echo "=== pnpm check ===" && \
pnpm check 2>&1 | tail -1 && \
echo "=== pnpm test ===" && \
pnpm test 2>&1 | tail -4 && \
echo "=== CLI doctor ===" && \
cargo run -p rcdvlog-cli -- doctor --json 2>&1
```

## Additional desktop-specific checks

For UI changes, also run:

```bash
pnpm --dir apps/desktop test 2>&1 | tail -8
```

For browser/layout/scroll changes:

```bash
pnpm test:desktop:browser 2>&1 | tail -8
```

## Failure handling

- If `cargo fmt --check` fails: run `cargo fmt --all` to auto-fix, then re-run the gate.
- If `cargo clippy` fails: fix the warnings (they are errors with `-D warnings`).
- If tests fail: investigate and fix before proceeding.
- If `pnpm check` fails: fix TypeScript errors.
- Report each gate's result separately so failures are easy to locate.

## Reference

AGENTS.md §12 defines the full gate table. This command encodes the most common patterns observed in trajectory data (34 instances across 8+ sessions in 30 days).
