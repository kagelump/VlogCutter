#!/usr/bin/env python3
"""Validate an epic queue or select its next ready task."""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from bounded_task_common import ALLOWED_STATUSES, WorkflowError, load_epic, load_task, parse_queue, resolve_branch
from validate_task import validate as validate_task


def validate_epic(epic_path: Path) -> tuple[list[str], list[dict[str, object]], dict[str, object]]:
    text, meta = load_epic(epic_path)
    errors: list[str] = []
    if meta.get("schema_version") != 1:
        errors.append("epic schema_version must be 1")
    for key in ("epic_id", "title", "base_branch", "task_branch_template"):
        if not isinstance(meta.get(key), str) or not str(meta.get(key)).strip():
            errors.append(f"epic {key} must be a non-empty string")
    if meta.get("tasks_per_cycle") != 1:
        errors.append("tasks_per_cycle must be exactly 1")
    if meta.get("merge_requires_user_authorization") is not True:
        errors.append("merge_requires_user_authorization must be true")

    queue = parse_queue(epic_path, text)
    ids: set[str] = set()
    orders: set[int] = set()
    rows: list[dict[str, object]] = []
    for item in queue:
        if item.task_id in ids:
            errors.append(f"duplicate task ID: {item.task_id}")
        ids.add(item.task_id)
        if item.order in orders:
            errors.append(f"duplicate queue order: {item.order}")
        orders.add(item.order)
        if item.status not in ALLOWED_STATUSES:
            errors.append(f"{item.task_id}: unsupported epic status {item.status!r}")
        if not item.path.exists():
            errors.append(f"{item.task_id}: task file not found: {item.path}")
            task_meta = {}
        else:
            task_errors = validate_task(item.path)
            errors.extend(f"{item.task_id}: {message}" for message in task_errors)
            _, task_meta = load_task(item.path)
            if task_meta.get("task_id") != item.task_id:
                errors.append(f"{item.task_id}: packet task_id does not match epic row")
            if task_meta.get("epic_id") != meta.get("epic_id"):
                errors.append(f"{item.task_id}: packet epic_id does not match epic metadata")
            if task_meta.get("status") != item.status:
                errors.append(
                    f"{item.task_id}: epic status {item.status!r} != packet status {task_meta.get('status')!r}"
                )
            packet_deps = tuple(task_meta.get("depends_on", [])) if isinstance(task_meta.get("depends_on"), list) else ()
            if packet_deps != item.depends_on:
                errors.append(
                    f"{item.task_id}: epic dependencies {item.depends_on} != packet dependencies {packet_deps}"
                )
        rows.append(
            {
                "order": item.order,
                "task_id": item.task_id,
                "path": str(item.path),
                "title": item.title,
                "depends_on": list(item.depends_on),
                "status": item.status,
                "slug": task_meta.get("slug") if task_meta else None,
            }
        )

    for row in rows:
        for dependency in row["depends_on"]:
            if dependency not in ids:
                errors.append(f"{row['task_id']}: unknown dependency {dependency}")

    # Cycle detection.
    graph = {str(row["task_id"]): tuple(str(value) for value in row["depends_on"]) for row in rows}
    visiting: set[str] = set()
    visited: set[str] = set()

    def visit(node: str) -> None:
        if node in visited:
            return
        if node in visiting:
            errors.append(f"dependency cycle includes {node}")
            return
        visiting.add(node)
        for dep in graph.get(node, ()): visit(dep)
        visiting.remove(node)
        visited.add(node)

    for node in graph: visit(node)
    return errors, rows, meta


def select_next(rows: list[dict[str, object]], meta: dict[str, object], requested: str | None) -> dict[str, object] | None:
    by_id = {str(row["task_id"]): row for row in rows}
    candidates = sorted(rows, key=lambda row: int(row["order"]))
    if requested:
        if requested not in by_id:
            raise WorkflowError(f"requested task not found: {requested}")
        candidates = [by_id[requested]]
    for row in candidates:
        if row["status"] != "ready":
            continue
        deps = [by_id[str(dep)] for dep in row["depends_on"]]
        if all(dep["status"] == "complete" for dep in deps):
            template = str(meta["task_branch_template"])
            slug = str(row.get("slug") or str(row["task_id"]).lower())
            selected = dict(row)
            selected["base_branch"] = meta["base_branch"]
            selected["task_branch"] = resolve_branch(template, str(row["task_id"]), slug)
            return selected
    return None


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("epic", type=Path)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--validate", action="store_true")
    group.add_argument("--next", action="store_true")
    parser.add_argument("--task-id")
    args = parser.parse_args()
    try:
        errors, rows, meta = validate_epic(args.epic.resolve())
        if errors:
            for error in errors: print(f"ERROR: {error}", file=sys.stderr)
            return 1
        if args.validate:
            print(json.dumps({"valid": True, "epic": str(args.epic), "tasks": rows}, indent=2))
            return 0
        selected = select_next(rows, meta, args.task_id)
        if selected is None:
            state = "EPIC_COMPLETE" if all(row["status"] == "complete" for row in rows) else "EPIC_BLOCKED"
            print(json.dumps({"state": state, "selected": None}, indent=2))
            return 2 if state == "EPIC_BLOCKED" else 0
        print(json.dumps({"state": "TASK_READY", "selected": selected}, indent=2))
        return 0
    except WorkflowError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
