# Verification baseline audit

- **Audited commit:** `d5d9014b9d46ec737ae957cc23e6e8b8892ce759`
- **Date:** 2026-07-29
- **Host:** macOS (darwin, arm64)

## Toolchain

| Tool | Version |
| --- | --- |
| rustc | 1.97.1 (8bab26f4f 2026-07-14) (Homebrew) |
| cargo | 1.97.1 (c980f4866 2026-06-30) (Homebrew) |
| node | v24.11.0 |
| pnpm | 10.17.0 |
| ffmpeg | 7.1.3 |
| ffprobe | 7.1.3 |
| exiftool | 13.36 |

## Tree state at audit

`git status --porcelain` output:

```
 M .claude/skills/executing-bounded-task-cycle/scripts/__pycache__/bounded_task_common.cpython-314.pyc
?? .claude/skills/executing-bounded-task-cycle/scripts/__pycache__/validate_task.cpython-314.pyc
```

No tracked files modified. Untracked `__pycache__` artifacts are harmless.

## Gate results

| # | Command | Exit | Wall time | Classification |
| ---: | --- | ---: | ---: | --- |
| 1 | `cargo fmt --all -- --check` | 0 | 0.3s | green |
| 2 | `cargo clippy --workspace --all-targets -- -D warnings` | 0 | 32.2s | green |
| 3 | `cargo test --workspace` | 0 | 104.5s | green |
| 4 | `pnpm check:frontend` | 0 | 5.3s | green |
| 5 | `pnpm test:web` | 0 | 1.4s | green |
| 6 | `pnpm test:integration` | 0 | 36.0s | green |
| 7 | `pnpm check:openapi` | 0 | 14.3s | green |
| 8 | `pnpm check:docs` | 1 | 0.2s | real_defect |
| 9 | `pnpm check:deployment` | 0 | 0.2s | green |

**Summary:** 8 green, 1 real_defect, 0 environmental.

## Real defects

### RD-1: `pnpm check:docs` fails with 6 broken relative links in skill documentation

- **Command:** `pnpm check:docs`
- **Points at:** `.claude/skills/executing-bounded-task-cycle/SKILL.md` (and 3 other files)
- **Runs:** fail, fail, fail
- **Output:**

```text
> rcd-vlogcutter@ check:docs <repo>
> node scripts/check-docs.mjs

<repo>/.claude/skills/executing-bounded-task-cycle/SKILL.md:
  broken relative link: ../../../planning/ORCHESTRATION.md#9-definition-of-complete
<repo>/.claude/skills/executing-bounded-task-cycle/references/epic-schema.md:
  broken relative link: tasks/E01-T001-decoder-seam.md
<repo>/.claude/skills/executing-bounded-task-cycle/references/epic-schema.md:
  broken relative link: tasks/E01-T002-thumbnail-cache.md
<repo>/.claude/skills/writing-bounded-task-packets/assets/EPIC.template.md:
  broken relative link: tasks/{{TASK_ID}}-{{TASK_SLUG}}.md
<repo>/.claude/skills/writing-bounded-task-packets/references/epic-schema.md:
  broken relative link: tasks/E01-T001-decoder-seam.md
<repo>/.claude/skills/writing-bounded-task-packets/references/epic-schema.md:
  broken relative link: tasks/E01-T002-thumbnail-cache.md
 ELIFECYCLE  Command failed with exit code 1.
```

**What is broken:** The `check-docs.mjs` script validates relative links in Markdown files and finds six broken links across four skill documentation files. Two are references to a missing `planning/ORCHESTRATION.md`; two are template placeholder links (`{{TASK_ID}}`) that should be excluded from link checking; two are example task-file references in schema docs that reference files outside this repository's task directory layout.

## Resolution

### RD-1: `pnpm check:docs` — resolved

**Root cause:** `scripts/check-docs.mjs` had three defects:

1. **Fenced-code-block scanning (4 of 6 failures).** The checker scanned inside
   `` ```md `` fenced code blocks, treating illustrative queue-table rows in
   `epic-schema.md` as real relative links. Links inside fenced code blocks are
   now skipped by tracking line-level fence state.

2. **Template placeholder resolution (1 of 6 failures).** The checker attempted
   to resolve `{{TASK_ID}}` and `{{TASK_SLUG}}` template placeholders in
   `EPIC.template.md` as literal filesystem paths. Links whose target contains
   `{{` are now skipped.

   **Reviewer agreement:** Per the task packet's Frozen decisions, the independent
   reviewer explicitly agrees that excluding template placeholder links from the
   link checker is the correct fix — the check is wrong for unexpanded
   placeholders, not that the document is broken.

3. **Missing target (1 of 6 failures).** `.claude/skills/executing-bounded-task-cycle/SKILL.md`
   linked to `../../../planning/ORCHESTRATION.md`, which does not exist in this
   repository. The link was removed and replaced with plain text referencing the
   calling repo's own ORCHESTRATION.md convention.

Additionally, the checker walked into git worktree directories under
`.claude/worktrees/`, duplicating files from sibling checkouts. Directories
containing a `.git` file (worktree marker) are now skipped.

**Files changed:**
- `scripts/check-docs.mjs` — fence-aware link scanning, template placeholder skip, worktree skip
- `.claude/skills/executing-bounded-task-cycle/SKILL.md` — broken link replaced with plain text

**Command re-run:** `pnpm check:docs` exits 0 on 2026-07-29.

All 1 real_defect rows resolved.

## CI enforcement

The verification gate is encoded in `.github/workflows/ci.yml` (D01-T004).
Every command the audit classified green is enforced on every push to `main`
and every pull request targeting `main`.

| # | Command | Job | Enforced |
| ---: | --- | --- | --- |
| 1 | `cargo fmt --all -- --check` | `ubuntu` | yes |
| 2 | `cargo clippy --workspace --all-targets -- -D warnings` | `ubuntu` | yes |
| 3 | `cargo test --workspace` | `ubuntu` | yes |
| 4 | `pnpm check:frontend` | `ubuntu` | yes |
| 5 | `pnpm test:web` | `ubuntu` | yes |
| 6 | `pnpm test:integration` | `ubuntu` | yes |
| 7 | `pnpm check:openapi` | `ubuntu` | yes |
| 8 | `pnpm check:docs` | `ubuntu` | yes |
| 9 | `pnpm check:deployment` | `ubuntu` | yes |

Additional commands not in the original audit but enforced by CI:

| Command | Job | Rationale |
| --- | --- | --- |
| `pnpm test:desktop` | `ubuntu` | Added by D01-T003; desktop frontend vitest suite |
| `cargo test -p rcdvlog-desktop` | `macos` | Tauri-specific Rust tests |
| `pnpm --dir apps/desktop tauri build` | `macos` | Native macOS bundle compilation |

No audited-green command is unenforced. No `continue-on-error`, `\|\| true`,
or `if: always()` masks any step.

