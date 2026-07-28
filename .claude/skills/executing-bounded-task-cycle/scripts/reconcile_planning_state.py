#!/usr/bin/env python3
"""Verify task completion is reflected in every declared planning location."""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

from bounded_task_common import WorkflowError, load_task


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--task", required=True, type=Path)
    parser.add_argument("--repo-root", type=Path, default=Path.cwd())
    parser.add_argument("--json", action="store_true", dest="as_json")
    args = parser.parse_args()
    repo = args.repo_root.resolve()
    task_path = args.task.resolve()
    try:
        _, meta = load_task(task_path)
        errors: list[str] = []
        if meta.get("status") != "complete":
            errors.append(f"task metadata status is {meta.get('status')!r}, expected 'complete'")
        results: list[dict[str, object]] = []
        for check in meta.get("reconciliation_checks", []):
            if not isinstance(check, dict):
                errors.append("invalid reconciliation check object")
                continue
            relative = Path(str(check.get("path", "")))
            target = relative if relative.is_absolute() else repo / relative
            pattern = str(check.get("regex", ""))
            if not target.exists():
                matched = False
                detail = "file does not exist"
            else:
                content = target.read_text(encoding="utf-8")
                try:
                    matched = re.search(pattern, content, re.MULTILINE) is not None
                    detail = "matched" if matched else "regex did not match"
                except re.error as exc:
                    matched = False
                    detail = f"invalid regex: {exc}"
            results.append({"path": str(target), "regex": pattern, "matched": matched, "detail": detail})
            if not matched:
                errors.append(f"reconciliation failed for {target}: {detail}")
        output = {"valid": not errors, "task": str(task_path), "checks": results, "errors": errors}
        if args.as_json or errors:
            print(json.dumps(output, indent=2))
        else:
            print(f"VALID: {len(results)} planning reconciliation checks matched")
        return 0 if not errors else 1
    except (WorkflowError, OSError) as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
