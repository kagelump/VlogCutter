#!/usr/bin/env python3
"""Validate one bounded task packet."""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from bounded_task_common import (
    ALLOWED_STATUSES,
    REQUIRED_TASK_SECTIONS,
    TASK_CLASSES,
    WorkflowError,
    load_task,
    patterns_provably_overlap,
    task_sections,
)

REQUIRED_KEYS = {
    "schema_version",
    "task_id",
    "epic_id",
    "title",
    "slug",
    "status",
    "task_class",
    "depends_on",
    "principal_invariant",
    "central_falsification",
    "architecture_decisions_frozen",
    "load_bearing_seams",
    "documents_by_role",
    "path_ownership",
    "forbidden_paths",
    "verification_commands",
    "reconciliation_checks",
}
REQUIRED_ROLES = {"contract_editor", "implementer", "independent_tester", "integrator"}


def _nonempty_string(value: Any) -> bool:
    return isinstance(value, str) and bool(value.strip())


def validate(path: Path) -> list[str]:
    text, meta = load_task(path)
    errors: list[str] = []

    missing = sorted(REQUIRED_KEYS - meta.keys())
    if missing:
        errors.append("missing metadata keys: " + ", ".join(missing))

    if meta.get("schema_version") != 1:
        errors.append("schema_version must be 1")
    if not _nonempty_string(meta.get("task_id")):
        errors.append("task_id must be a non-empty string")
    if not _nonempty_string(meta.get("epic_id")):
        errors.append("epic_id must be a non-empty string")
    if not _nonempty_string(meta.get("title")):
        errors.append("title must be a non-empty string")
    if not _nonempty_string(meta.get("slug")):
        errors.append("slug must be a non-empty string")
    if meta.get("status") not in ALLOWED_STATUSES:
        errors.append(f"unsupported status: {meta.get('status')!r}")
    if meta.get("task_class") not in TASK_CLASSES:
        errors.append(f"unsupported task_class: {meta.get('task_class')!r}")

    for key in ("depends_on", "load_bearing_seams", "forbidden_paths", "verification_commands", "reconciliation_checks"):
        if not isinstance(meta.get(key), list):
            errors.append(f"{key} must be a list")


    documents = meta.get("documents_by_role")
    required_document_roles = {"contract_editor", "implementer", "independent_tester", "reviewer", "integrator"}
    if not isinstance(documents, dict):
        errors.append("documents_by_role must be an object")
    else:
        missing_document_roles = sorted(required_document_roles - documents.keys())
        if missing_document_roles:
            errors.append("missing document routing roles: " + ", ".join(missing_document_roles))
        for role, values in documents.items():
            if not isinstance(values, list) or not all(_nonempty_string(item) for item in values):
                errors.append(f"documents_by_role.{role} must be a list of non-empty strings")

    if not _nonempty_string(meta.get("principal_invariant")):
        errors.append("principal_invariant must be a non-empty string")
    if not _nonempty_string(meta.get("central_falsification")):
        errors.append("central_falsification must be a non-empty string")

    frozen = meta.get("architecture_decisions_frozen")
    if not isinstance(frozen, bool):
        errors.append("architecture_decisions_frozen must be boolean")
    if meta.get("status") == "ready" and meta.get("task_class") != "foundation" and frozen is not True:
        errors.append("DESIGN_REQUIRED: ready non-foundation tasks must freeze architecture decisions")

    seams = meta.get("load_bearing_seams")
    if meta.get("task_class") == "implementation" and isinstance(seams, list) and len(seams) > 1:
        errors.append("implementation task crosses more than one load-bearing seam")

    ownership = meta.get("path_ownership")
    if not isinstance(ownership, dict):
        errors.append("path_ownership must be an object")
    else:
        missing_roles = sorted(REQUIRED_ROLES - ownership.keys())
        if missing_roles:
            errors.append("missing path ownership roles: " + ", ".join(missing_roles))
        owned_patterns: list[tuple[str, str]] = []
        for role, paths in ownership.items():
            if not isinstance(paths, list) or not all(_nonempty_string(item) for item in paths):
                errors.append(f"path_ownership.{role} must be a list of non-empty strings")
                continue
            for owned_path in paths:
                for previous_role, previous_pattern in owned_patterns:
                    if previous_role != role and patterns_provably_overlap(owned_path, previous_pattern):
                        errors.append(
                            f"ownership patterns overlap across roles: {previous_role}:{previous_pattern!r} and {role}:{owned_path!r}"
                        )
                owned_patterns.append((role, owned_path))
        if meta.get("task_class") == "implementation" and not ownership.get("implementer"):
            errors.append("implementation task must own at least one implementer path")
        if meta.get("task_class") == "implementation" and not ownership.get("independent_tester"):
            errors.append("implementation task must reserve an independent tester path")

    sections = task_sections(text)
    for heading in REQUIRED_TASK_SECTIONS:
        body = sections.get(heading)
        if body is None:
            errors.append(f"missing required section: ## {heading}")
        elif meta.get("status") == "ready":
            if not body.strip():
                errors.append(f"ready task section is empty: ## {heading}")
            if any(token in body.upper() for token in ("TBD", "TODO", "???")):
                errors.append(f"ready task contains unresolved placeholder in ## {heading}")

    for item in meta.get("reconciliation_checks", []) if isinstance(meta.get("reconciliation_checks"), list) else []:
        if not isinstance(item, dict) or not _nonempty_string(item.get("path")) or not _nonempty_string(item.get("regex")):
            errors.append("every reconciliation check must contain non-empty path and regex")

    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("task", type=Path)
    parser.add_argument("--json", action="store_true", dest="as_json")
    args = parser.parse_args()
    try:
        errors = validate(args.task.resolve())
    except WorkflowError as exc:
        errors = [str(exc)]
    result = {"task": str(args.task), "valid": not errors, "errors": errors}
    if args.as_json:
        print(json.dumps(result, indent=2))
    elif errors:
        for error in errors:
            print(f"ERROR: {error}", file=sys.stderr)
    else:
        print(f"VALID: {args.task}")
    return 0 if not errors else 1


if __name__ == "__main__":
    raise SystemExit(main())
