#!/usr/bin/env python3
"""Shared parsers for the bounded-task workflow (standard library only)."""
from __future__ import annotations

import fnmatch
import json
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any

TASK_MARKER = "bounded-task-metadata"
EPIC_MARKER = "bounded-epic-metadata"

ALLOWED_STATUSES = {
    "draft",
    "ready",
    "in_progress",
    "code_complete",
    "integrated",
    "independently_tested",
    "reviewed",
    "gated",
    "complete",
    "blocked",
    "design_required",
}

TASK_CLASSES = {"foundation", "implementation", "integration", "qa", "review"}

REQUIRED_TASK_SECTIONS = [
    "Goal",
    "Principal invariant",
    "Inputs and outputs",
    "Frozen decisions",
    "Owned paths",
    "Required interface",
    "Acceptance oracle",
    "Independent test charter",
    "Review charter",
    "Non-goals",
    "Stop conditions",
    "Commands",
    "Handoff",
]


class WorkflowError(ValueError):
    """A deterministic workflow validation error."""


def _read(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except OSError as exc:
        raise WorkflowError(f"cannot read {path}: {exc}") from exc


def extract_json_comment(text: str, marker: str) -> dict[str, Any]:
    pattern = re.compile(
        rf"<!--\s*{re.escape(marker)}\s*(\{{.*?\}})\s*-->", re.DOTALL
    )
    match = pattern.search(text)
    if not match:
        raise WorkflowError(f"missing <!-- {marker} ... --> JSON metadata block")
    try:
        value = json.loads(match.group(1))
    except json.JSONDecodeError as exc:
        raise WorkflowError(f"invalid {marker} JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise WorkflowError(f"{marker} metadata must be a JSON object")
    return value


def load_task(path: Path) -> tuple[str, dict[str, Any]]:
    text = _read(path)
    return text, extract_json_comment(text, TASK_MARKER)


def load_epic(path: Path) -> tuple[str, dict[str, Any]]:
    text = _read(path)
    return text, extract_json_comment(text, EPIC_MARKER)


def task_sections(text: str) -> dict[str, str]:
    matches = list(re.finditer(r"^##\s+(.+?)\s*$", text, re.MULTILINE))
    sections: dict[str, str] = {}
    for index, match in enumerate(matches):
        start = match.end()
        end = matches[index + 1].start() if index + 1 < len(matches) else len(text)
        sections[match.group(1).strip()] = text[start:end].strip()
    return sections


@dataclass(frozen=True)
class EpicTask:
    order: int
    task_id: str
    path: Path
    title: str
    depends_on: tuple[str, ...]
    status: str
    line_number: int


QUEUE_HEADER = ["Order", "Task", "Title", "Depends on", "Status"]


def parse_queue(epic_path: Path, text: str) -> list[EpicTask]:
    lines = text.splitlines()
    header_index: int | None = None
    for index, line in enumerate(lines):
        cells = _table_cells(line)
        if cells == QUEUE_HEADER:
            header_index = index
            break
    if header_index is None:
        raise WorkflowError(
            "missing epic queue table with columns: " + " | ".join(QUEUE_HEADER)
        )
    if header_index + 1 >= len(lines) or not _is_separator_row(lines[header_index + 1]):
        raise WorkflowError("epic queue header must be followed by a Markdown separator row")

    tasks: list[EpicTask] = []
    for index in range(header_index + 2, len(lines)):
        line = lines[index]
        if not line.strip().startswith("|"):
            if tasks:
                break
            continue
        cells = _table_cells(line)
        if len(cells) != 5:
            raise WorkflowError(f"queue row {index + 1} must contain exactly five columns")
        try:
            order = int(cells[0])
        except ValueError as exc:
            raise WorkflowError(f"queue row {index + 1} has non-integer order") from exc
        link = re.fullmatch(r"\[([^\]]+)\]\(([^)]+)\)", cells[1])
        if not link:
            raise WorkflowError(f"queue row {index + 1} task must be a Markdown link")
        task_id, relative_path = link.groups()
        deps_cell = cells[3].strip()
        dependencies = () if deps_cell in {"", "-", "—"} else tuple(
            value.strip() for value in deps_cell.split(",") if value.strip()
        )
        tasks.append(
            EpicTask(
                order=order,
                task_id=task_id,
                path=(epic_path.parent / relative_path).resolve(),
                title=cells[2],
                depends_on=dependencies,
                status=cells[4].strip(),
                line_number=index + 1,
            )
        )
    if not tasks:
        raise WorkflowError("epic queue table contains no task rows")
    return tasks


def _table_cells(line: str) -> list[str]:
    stripped = line.strip()
    if not stripped.startswith("|") or not stripped.endswith("|"):
        return []
    return [cell.strip() for cell in stripped[1:-1].split("|")]


def _is_separator_row(line: str) -> bool:
    cells = _table_cells(line)
    return bool(cells) and all(re.fullmatch(r":?-{3,}:?", cell) for cell in cells)


def path_matches(path: str, pattern: str) -> bool:
    normalized = pattern.rstrip("/")
    if normalized.endswith("/**"):
        prefix = normalized[:-3].rstrip("/")
        return path == prefix or path.startswith(prefix + "/")
    return fnmatch.fnmatchcase(path, normalized)


def patterns_provably_overlap(left: str, right: str) -> bool:
    """Detect common ownership overlaps without pretending to solve glob intersection."""
    if left == right:
        return True
    left_has_glob = any(char in left for char in "*?[")
    right_has_glob = any(char in right for char in "*?[")
    if not left_has_glob:
        return path_matches(left, right)
    if not right_has_glob:
        return path_matches(right, left)
    if left.rstrip("/").endswith("/**") and right.rstrip("/").endswith("/**"):
        left_prefix = left.rstrip("/")[:-3].rstrip("/")
        right_prefix = right.rstrip("/")[:-3].rstrip("/")
        return (
            left_prefix == right_prefix
            or left_prefix.startswith(right_prefix + "/")
            or right_prefix.startswith(left_prefix + "/")
        )
    return False


def resolve_branch(template: str, task_id: str, slug: str) -> str:
    return template.format(task_id=task_id, task_id_lower=task_id.lower(), slug=slug)
