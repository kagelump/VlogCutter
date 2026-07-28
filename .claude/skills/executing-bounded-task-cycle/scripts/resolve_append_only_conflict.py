#!/usr/bin/env python3
"""Mechanically resolve a conflict where both sides purely *appended*
content after a shared base prefix — e.g. two review packets each adding
their own new section to the end of REVIEW_REPORT.md.

Safe only when neither side edited or removed anything the other side also
touched. Verifies that precondition directly (both "ours" and "theirs" must
literally start with "base" as a string prefix) and refuses rather than
guessing if it doesn't hold — do not loosen this check to "mostly similar"
or line-diff-based matching. A false positive here silently discards real
content.

Not safe for files where the append target defines named keys that must
stay unique (e.g. a YAML `gates:` job two branches both add) — a
duplicate-key collision needs real reconciliation, not concatenation. This
tool does not attempt to detect that case; the caller must know which kind
of conflict it has before reaching for this tool.
"""
from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path


class WorkflowError(ValueError):
    pass


def show_stage(repo: Path, stage: int, path: str) -> str:
    result = subprocess.run(
        ["git", "show", f":{stage}:{path}"], cwd=repo, capture_output=True, text=True
    )
    if result.returncode != 0:
        raise WorkflowError(f"could not read stage {stage} of {path}: {result.stderr.strip()}")
    return result.stdout


def resolve(repo: Path, path: str) -> str:
    base = show_stage(repo, 1, path)
    ours = show_stage(repo, 2, path)
    theirs = show_stage(repo, 3, path)

    if not ours.startswith(base):
        raise WorkflowError(
            f"'ours' is not base+append for {path} — not a pure append, "
            "needs real reconciliation, not this tool"
        )
    if not theirs.startswith(base):
        raise WorkflowError(
            f"'theirs' is not base+append for {path} — not a pure append, "
            "needs real reconciliation, not this tool"
        )

    return base + ours[len(base):] + theirs[len(base):]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("path", help="conflicted file, relative to repo root")
    parser.add_argument("--repo-root", type=Path, default=Path.cwd())
    args = parser.parse_args()
    repo = args.repo_root.resolve()
    try:
        resolved = resolve(repo, args.path)
    except WorkflowError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1
    (repo / args.path).write_text(resolved, encoding="utf-8")
    print(f"resolved {args.path}: base + ours-addition + theirs-addition")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
