#!/usr/bin/env python3
"""Simulate sequential-merging a wave of task branches, without touching the
working tree, any ref, or the index.

A completed cycle's branch is only 'clean' in isolation. Two sibling branches
in the same wave can independently add the same key to a shared file (a new
`gates:` job block in ci.yml, an appended section in REVIEW_REPORT.md, an
edited row in EPIC.md) and *each* be conflict-free against base while still
conflicting against each other. The only way to know before a real merge is
to simulate the real merge order with git's own tree-merging machinery.

Uses `git merge-tree --write-tree` (real three-way merge, base auto-selected
via merge-base) plus `git commit-tree` to chain simulated commits so each
step's simulated result becomes the next step's left-hand side. No branch,
ref, or working-tree file is created or modified; simulated commits are
unreferenced objects, invisible to `git log` and normal `git gc` cadence.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

# git's merge-tree conflict messages are human-readable, not a stable porcelain
# format, but "CONFLICT (<kind>): ... in <path>" has been consistent across
# recent git releases. Treated as best-effort extraction for a human-readable
# summary; `raw_messages` below carries the untouched text as the source of truth.
CONFLICT_PATH_RE = re.compile(r"^CONFLICT \([^)]+\): .*? in (.+)$", re.MULTILINE)


class WorkflowError(ValueError):
    pass


def git(repo: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        ["git", *args], cwd=repo, text=True, capture_output=True
    )
    if check and result.returncode not in (0, 1):
        raise WorkflowError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result


def resolve(repo: Path, ref: str) -> str:
    result = git(repo, "rev-parse", "--verify", ref)
    if result.returncode != 0:
        raise WorkflowError(f"cannot resolve ref: {ref}")
    return result.stdout.strip()


def merge_tree(repo: Path, left: str, right: str) -> tuple[bool, str, list[str], str]:
    """Returns (clean, tree_oid, conflicted_paths, raw_messages).

    Deliberately avoids `--name-only -z`: its conflicted-path output
    interleaves stage listings and human messages in a way that isn't a
    documented stable format. Plain `--write-tree` output is: tree oid on
    line 1, then (only on conflict) git's own "Auto-merging <path>" /
    "CONFLICT (<kind>): ... in <path>" messages, which we keep verbatim as
    the source of truth and only best-effort-parse for a path list.
    """
    result = git(repo, "merge-tree", "--write-tree", left, right)
    lines = result.stdout.splitlines()
    tree_oid = lines[0].strip() if lines else ""
    raw_messages = "\n".join(lines[1:]).strip()
    conflicted = CONFLICT_PATH_RE.findall(raw_messages)
    return result.returncode == 0, tree_oid, conflicted, raw_messages


def commit_tree(repo: Path, tree_oid: str, parents: list[str], message: str) -> str:
    args = ["commit-tree", tree_oid]
    for parent in parents:
        args += ["-p", parent]
    args += ["-m", message]
    result = subprocess.run(
        ["git", *args], cwd=repo, text=True, capture_output=True,
        env={"GIT_AUTHOR_NAME": "simulate-wave-merge", "GIT_AUTHOR_EMAIL": "noreply@localhost",
             "GIT_COMMITTER_NAME": "simulate-wave-merge", "GIT_COMMITTER_EMAIL": "noreply@localhost",
             "PATH": "/usr/bin:/bin:/usr/local/bin"},
    )
    if result.returncode != 0:
        raise WorkflowError(f"git commit-tree failed: {result.stderr.strip()}")
    return result.stdout.strip()


def simulate(repo: Path, base: str, branches: list[str]) -> dict:
    base_sha = resolve(repo, base)
    integration = base_sha
    steps = []
    stopped_at: str | None = None

    for branch in branches:
        branch_sha = resolve(repo, branch)
        clean, tree_oid, conflicted, raw_messages = merge_tree(repo, integration, branch_sha)
        step = {
            "branch": branch,
            "branch_sha": branch_sha,
            "merged_into": integration,
            "clean": clean,
            "conflicted_paths": conflicted,
        }
        if not clean:
            step["raw_messages"] = raw_messages
        steps.append(step)
        if not clean:
            stopped_at = branch
            break
        integration = commit_tree(
            repo, tree_oid, [integration, branch_sha],
            f"simulate: merge {branch}",
        )
        step["simulated_commit"] = integration

    return {
        "base": base,
        "base_sha": base_sha,
        "requested_order": branches,
        "steps": steps,
        "all_clean": stopped_at is None,
        "stopped_at": stopped_at,
        "final_simulated_tree": integration if stopped_at is None else None,
    }


def pairwise_matrix(repo: Path, branches: list[str]) -> dict:
    """Independent of order: which pairs collide with each other at all.

    Cheaper way to see the shape of the problem before picking an order:
    two branches that don't collide pairwise can be merged in either
    relative order; two that do will need explicit reconciliation no
    matter which one goes first.
    """
    shas = {b: resolve(repo, b) for b in branches}
    matrix: dict[str, dict[str, object]] = {}
    for i, left in enumerate(branches):
        for right in branches[i + 1 :]:
            clean, _tree, conflicted, _raw = merge_tree(repo, shas[left], shas[right])
            if not clean:
                matrix.setdefault(left, {})[right] = conflicted or ["(path parse failed — see raw_messages by rerunning without --pairwise on this pair)"]
    return matrix


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True, help="integration base, e.g. main")
    parser.add_argument("--branches", nargs="+", required=True,
                         help="branch names in the order you intend to merge them")
    parser.add_argument("--repo-root", type=Path, default=Path.cwd())
    parser.add_argument("--pairwise", action="store_true",
                         help="also report all pairwise collisions regardless of order")
    args = parser.parse_args()
    repo = args.repo_root.resolve()

    try:
        report = simulate(repo, args.base, args.branches)
        if args.pairwise:
            report["pairwise_collisions"] = pairwise_matrix(repo, args.branches)
        print(json.dumps(report, indent=2))
        return 0 if report["all_clean"] else 1
    except WorkflowError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
