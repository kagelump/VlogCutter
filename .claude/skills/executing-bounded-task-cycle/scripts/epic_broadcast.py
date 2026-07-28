#!/usr/bin/env python3
"""Append-only broadcast log shared by every agent working one epic.

Not a findings channel and not a status channel — cycle.json, EPIC.md, and
REVIEW_REPORT.md already cover "what did you decide" and "what's the
verdict." This is for the narrower thing neither of those covers: a fact
discovered mid-task that changes what a *different*, isolated agent should
assume, and that would otherwise only surface once that agent runs headfirst
into it. See references/broadcast-log.md for what belongs here and what
doesn't — the isolation between roles (independent testing must not inherit
implementer reasoning) is intentional and this log must not be used to route
around it.

Lives outside the repo (default: under ~/.claude), so it is reachable by
every isolated worktree of the same repo without being a tracked file and
without colliding with another repo that happens to reuse an epic ID like
"E01". The path is a deterministic function of the repo's *common* git dir
(stable across worktrees, unlike --show-toplevel, which differs per
worktree) plus the epic ID — any agent can recompute it independently with
`epic_broadcast.py path --epic <id>`; nothing needs to hand the path down
out of band, though doing so is also fine.

Format is JSON Lines: one JSON object per line, newline-terminated. Appends
use a single os.write() of the complete line so concurrent writers from
different processes cannot interleave partial lines (relies on POSIX
O_APPEND write atomicity for writes under PIPE_BUF — true for any message
this tool will realistically produce; see references/broadcast-log.md for
the atomicity argument and how it was verified before being trusted here).
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path


class WorkflowError(ValueError):
    pass


def git(repo: Path, *args: str) -> str:
    result = subprocess.run(["git", *args], cwd=repo, capture_output=True, text=True)
    if result.returncode != 0:
        raise WorkflowError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout.strip()


def repo_identity(repo_root: Path) -> str:
    """Stable across every worktree of one repo; distinct across repos."""
    common_dir = Path(git(repo_root, "rev-parse", "--git-common-dir"))
    if not common_dir.is_absolute():
        common_dir = (repo_root / common_dir).resolve()
    else:
        common_dir = common_dir.resolve()
    canonical_root = common_dir.parent  # .git's parent is the one true repo root
    digest = hashlib.sha256(str(canonical_root).encode("utf-8")).hexdigest()[:16]
    slug = re.sub(r"[^a-zA-Z0-9_.-]+", "-", canonical_root.name) or "repo"
    return f"{slug}-{digest}"


def log_path(repo_root: Path, epic_id: str, log_root: Path) -> Path:
    return log_root / "epic-logs" / repo_identity(repo_root) / f"{epic_id}.jsonl"


def append_entry(path: Path, entry: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    line = (json.dumps(entry, sort_keys=True) + "\n").encode("utf-8")
    if len(line) > 4096:
        raise WorkflowError(
            f"entry is {len(line)} bytes, over the 4096-byte atomic-append "
            "budget this tool assumes — shorten the message, this is a "
            "broadcast, not a report"
        )
    fd = os.open(path, os.O_APPEND | os.O_CREAT | os.O_WRONLY, 0o644)
    try:
        os.write(fd, line)
    finally:
        os.close(fd)


def read_entries(path: Path, since: str | None) -> list[dict]:
    if not path.exists():
        return []
    entries = []
    with path.open("r", encoding="utf-8") as f:
        for line_number, line in enumerate(f, start=1):
            line = line.strip()
            if not line:
                continue
            try:
                entries.append(json.loads(line))
            except json.JSONDecodeError as exc:
                raise WorkflowError(f"{path}:{line_number}: corrupt entry: {exc}") from exc
    if since:
        entries = [e for e in entries if e.get("timestamp", "") >= since]
    return entries


def cmd_path(args: argparse.Namespace) -> int:
    path = log_path(args.repo_root.resolve(), args.epic, args.log_root)
    print(path)
    return 0


def cmd_append(args: argparse.Namespace) -> int:
    path = log_path(args.repo_root.resolve(), args.epic, args.log_root)
    entry = {
        "timestamp": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "epic_id": args.epic,
        "task_id": args.task,
        "role": args.role,
        "tag": args.tag,
        "message": args.message,
        "relevant_paths": args.paths or [],
    }
    append_entry(path, entry)
    print(f"appended to {path}")
    return 0


def cmd_tail(args: argparse.Namespace) -> int:
    path = log_path(args.repo_root.resolve(), args.epic, args.log_root)
    entries = read_entries(path, args.since)
    if args.count:
        entries = entries[-args.count:]
    if args.as_json:
        print(json.dumps(entries, indent=2))
    else:
        if not entries:
            print(f"(no entries at {path})")
        for e in entries:
            paths = f" [{', '.join(e['relevant_paths'])}]" if e.get("relevant_paths") else ""
            print(f"{e['timestamp']} {e['task_id']}/{e['role']} [{e['tag']}]: {e['message']}{paths}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--repo-root", type=Path, default=Path.cwd())
    parser.add_argument("--log-root", type=Path,
                         default=Path.home() / ".claude" / "bounded-task-workflow",
                         help="default: ~/.claude/bounded-task-workflow — override only for testing")
    sub = parser.add_subparsers(dest="command", required=True)

    p_path = sub.add_parser("path", help="print this epic's log file path")
    p_path.add_argument("--epic", required=True)
    p_path.set_defaults(func=cmd_path)

    p_append = sub.add_parser("append", help="append one broadcast entry")
    p_append.add_argument("--epic", required=True)
    p_append.add_argument("--task", required=True, help="your task ID, or 'coordinator'")
    p_append.add_argument("--role", required=True,
                           choices=["implementer", "independent_tester", "reviewer",
                                    "integrator", "contract_editor", "coordinator"])
    p_append.add_argument("--tag", required=True,
                           help="short free-text category, e.g. shared-file-collision, "
                                "defect-outside-scope, environment-surprise, contract-ambiguity")
    p_append.add_argument("--message", required=True)
    p_append.add_argument("--paths", nargs="*", default=[],
                           help="repo-relative paths this entry is about, if any")
    p_append.set_defaults(func=cmd_append)

    p_tail = sub.add_parser("tail", help="read entries")
    p_tail.add_argument("--epic", required=True)
    p_tail.add_argument("--since", help="ISO 8601 UTC timestamp, e.g. 2026-07-27T00:00:00Z")
    p_tail.add_argument("--count", type=int, help="only the last N entries")
    p_tail.add_argument("--json", action="store_true", dest="as_json")
    p_tail.set_defaults(func=cmd_tail)

    args = parser.parse_args()
    try:
        return args.func(args)
    except WorkflowError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
