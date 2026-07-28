#!/usr/bin/env python3
"""Mechanically resolve an epic-queue Markdown table merge conflict.

Run with the file mid-conflict (git has already written conflict markers to
the working tree and staged all three sides at index stages 1/2/3). Reads
the *index stages*, not the marker-laden working-tree text — reading the
raw conflicted file and pattern-matching table rows out of it silently
duplicates rows (both the "ours" and "theirs" copy of a row look like valid
table rows to a naive line scanner). This has actually happened; it is why
this script exists instead of an inline shell one-liner.

Precondition this tool assumes: each sibling task branch in a wave only
edits the status column of *its own* row(s). If both sides changed the same
row differently, this is a real conflict this tool must not paper over, and
it refuses instead of guessing.
"""
from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

TASK_ID_RE = re.compile(r"E01-[A-Z0-9]+|E0[2-9]-[A-Z0-9]+")


class WorkflowError(ValueError):
    pass


def show_stage(repo: Path, stage: int, path: str) -> str:
    result = subprocess.run(
        ["git", "show", f":{stage}:{path}"], cwd=repo, capture_output=True, text=True
    )
    if result.returncode != 0:
        raise WorkflowError(f"could not read stage {stage} of {path}: {result.stderr.strip()}")
    return result.stdout


def row_task_id(line: str) -> str | None:
    if not line.startswith("| "):
        return None
    match = TASK_ID_RE.search(line)
    return match.group(0) if match else None


def index_rows(text: str) -> dict[str, str]:
    return {tid: line for line in text.splitlines() if (tid := row_task_id(line))}


def resolve(repo: Path, path: str) -> str:
    base = show_stage(repo, 1, path)
    ours = show_stage(repo, 2, path)
    theirs = show_stage(repo, 3, path)
    base_rows, ours_rows, theirs_rows = index_rows(base), index_rows(ours), index_rows(theirs)

    merged: list[str] = []
    for line in ours.splitlines():
        task_id = row_task_id(line)
        if task_id is None:
            merged.append(line)
            continue
        ours_row = ours_rows.get(task_id)
        theirs_row = theirs_rows.get(task_id)
        base_row = base_rows.get(task_id)
        if ours_row == base_row and theirs_row is not None:
            # only "theirs" touched this row - take their edit
            merged.append(theirs_row)
        elif theirs_row is not None and theirs_row != base_row and ours_row != theirs_row:
            raise WorkflowError(
                f"real conflict on row {task_id}: both branches edited it "
                f"differently (ours={ours_row!r} theirs={theirs_row!r}) — "
                "this needs human or agent judgment, not this script"
            )
        else:
            merged.append(line)

    trailing_newline = "\n" if ours.endswith("\n") else ""
    return "\n".join(merged) + trailing_newline


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
    print(f"resolved {args.path} from index stages 1/2/3 (base/ours/theirs)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
