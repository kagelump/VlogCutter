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
> rcd-vlogcutter@ check:docs /Users/ryantseng/Documents/VlogCutter
> node scripts/check-docs.mjs

/Users/ryantseng/Documents/VlogCutter/.claude/skills/executing-bounded-task-cycle/SKILL.md:
  broken relative link: ../../../planning/ORCHESTRATION.md#9-definition-of-complete
/Users/ryantseng/Documents/VlogCutter/.claude/skills/executing-bounded-task-cycle/references/epic-schema.md:
  broken relative link: tasks/E01-T001-decoder-seam.md
/Users/ryantseng/Documents/VlogCutter/.claude/skills/executing-bounded-task-cycle/references/epic-schema.md:
  broken relative link: tasks/E01-T002-thumbnail-cache.md
/Users/ryantseng/Documents/VlogCutter/.claude/skills/writing-bounded-task-packets/assets/EPIC.template.md:
  broken relative link: tasks/{{TASK_ID}}-{{TASK_SLUG}}.md
/Users/ryantseng/Documents/VlogCutter/.claude/skills/writing-bounded-task-packets/references/epic-schema.md:
  broken relative link: tasks/E01-T001-decoder-seam.md
/Users/ryantseng/Documents/VlogCutter/.claude/skills/writing-bounded-task-packets/references/epic-schema.md:
  broken relative link: tasks/E01-T002-thumbnail-cache.md
 ELIFECYCLE  Command failed with exit code 1.
```

**What is broken:** The `check-docs.mjs` script validates relative links in Markdown files and finds six broken links across four skill documentation files. Two are references to a missing `planning/ORCHESTRATION.md`; two are template placeholder links (`{{TASK_ID}}`) that should be excluded from link checking; two are example task-file references in schema docs that reference files outside this repository's task directory layout.

## Environmental gaps

None. All commands ran without missing tools or fixtures.

## Real defect count

1
