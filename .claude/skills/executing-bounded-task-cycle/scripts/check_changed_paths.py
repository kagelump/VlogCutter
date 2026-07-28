#!/usr/bin/env python3
"""Enforce role-specific changed-path ownership for a git range."""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

from bounded_task_common import WorkflowError, load_task, path_matches

ROLE_KEYS = {
    "contract-editor": ["contract_editor"],
    "implementer": ["implementer"],
    "independent-tester": ["independent_tester"],
    "integrator": ["contract_editor", "implementer", "independent_tester", "integrator"],
    "final": ["contract_editor", "implementer", "independent_tester", "integrator"],
}

# The cycle requires the integrator to reconcile planning state, and
# reconciliation_checks already name the exact files that edit must touch.
RECONCILIATION_ROLES = {"integrator", "final"}


def run_git(repo: Path, *args: str) -> str:
    completed = subprocess.run(
        ["git", *args], cwd=repo, check=False, text=True, capture_output=True
    )
    if completed.returncode != 0:
        raise WorkflowError(
            f"git {' '.join(args)} failed ({completed.returncode}): {completed.stderr.strip()}"
        )
    return completed.stdout


def reconciliation_paths(meta: dict) -> list[str]:
    """Paths the cycle protocol obliges the integrator to reconcile."""
    checks = meta.get("reconciliation_checks", [])
    if not isinstance(checks, list):
        raise WorkflowError("task reconciliation_checks is not a list")
    paths: list[str] = []
    for check in checks:
        if not isinstance(check, dict):
            continue
        value = check.get("path")
        if isinstance(value, str) and value.strip() and value.strip() not in paths:
            paths.append(value.strip())
    return paths


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--task", required=True, type=Path)
    parser.add_argument("--base", required=True)
    parser.add_argument("--head", required=True)
    parser.add_argument("--role", required=True, choices=sorted(ROLE_KEYS))
    parser.add_argument("--repo-root", type=Path, default=Path.cwd())
    parser.add_argument("--include-untracked", action="store_true")
    parser.add_argument("--json", action="store_true", dest="as_json")
    args = parser.parse_args()

    repo = args.repo_root.resolve()
    try:
        _, meta = load_task(args.task.resolve())
        ownership = meta.get("path_ownership")
        if not isinstance(ownership, dict):
            raise WorkflowError("task path_ownership is not an object")
        allowed: list[str] = []
        for role_key in ROLE_KEYS[args.role]:
            values = ownership.get(role_key, [])
            if not isinstance(values, list):
                raise WorkflowError(f"path_ownership.{role_key} is not a list")
            allowed.extend(str(value) for value in values)
        forbidden = [str(value) for value in meta.get("forbidden_paths", [])]
        reconciliation: list[str] = []
        if args.role in RECONCILIATION_ROLES:
            reconciliation = [
                value for value in reconciliation_paths(meta) if value not in allowed
            ]

        changed = {
            line.strip()
            for line in run_git(repo, "diff", "--name-only", f"{args.base}..{args.head}").splitlines()
            if line.strip()
        }
        if args.include_untracked:
            changed.update(
                line.strip()
                for line in run_git(repo, "ls-files", "--others", "--exclude-standard").splitlines()
                if line.strip()
            )

        violations: list[dict[str, str]] = []
        for path in sorted(changed):
            if any(path_matches(path, pattern) for pattern in forbidden):
                violations.append({"path": path, "reason": "matches forbidden path"})
            elif not any(
                path_matches(path, pattern) for pattern in (*allowed, *reconciliation)
            ):
                violations.append({"path": path, "reason": "not owned by selected role"})

        result = {
            "valid": not violations,
            "role": args.role,
            "base": args.base,
            "head": args.head,
            "allowed_patterns": allowed,
            "reconciliation_patterns": reconciliation,
            "forbidden_patterns": forbidden,
            "changed_paths": sorted(changed),
            "violations": violations,
        }
        if args.as_json or not result["valid"]:
            print(json.dumps(result, indent=2))
        else:
            print(f"VALID: {len(changed)} changed paths are owned by {args.role}")
        return 0 if not violations else 1
    except WorkflowError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
