#!/usr/bin/env python3
"""Verify exact-SHA closure and evidence ownership for a task cycle."""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Any

from bounded_task_common import WorkflowError, load_task, path_matches

REQUIRED_KEYS = {
    "schema_version",
    "task_id",
    "epic_file",
    "task_file",
    "base_branch",
    "base_sha",
    "task_branch",
    "contract_sha",
    "implementation_base_sha",
    "implementation_sha",
    "subject_sha",
    "independent_tested_sha",
    "reviewed_sha",
    "gated_sha",
    "evidence_commit_sha",
    "test_verdict",
    "review_verdict",
    "open_findings",
    "gate_commands",
    "merge",
}

GATE_CLASSIFICATIONS = {"green", "real_defect", "environmental"}
MEASUREMENT_TASK_CLASSES = {"qa"}


def _nonempty_string(value: Any) -> bool:
    return isinstance(value, str) and bool(value.strip())


def check_gate_commands(commands: Any, task_class: Any) -> tuple[list[str], set[str]]:
    """Validate gate evidence, allowing a qa audit to record a measured failure.

    Each entry may carry an optional ``classification``. It defaults to ``green``,
    which keeps the historical ``exit_code=0`` assertion. A task whose purpose is to
    measure the gate may instead classify an entry ``real_defect`` or
    ``environmental``, which records the nonzero exit as a finding rather than a
    failure of the cycle.
    """
    errors: list[str] = []
    names: set[str] = set()
    if not isinstance(commands, list) or not commands:
        return ["gate_commands must contain at least one command"], names

    for index, command in enumerate(commands):
        if not isinstance(command, dict):
            errors.append(f"gate_commands[{index}] must be an object")
            continue
        if _nonempty_string(command.get("command")):
            names.add(command["command"])
        else:
            errors.append(f"gate_commands[{index}] must contain a non-empty command")

        classification = command.get("classification", "green")
        if classification not in GATE_CLASSIFICATIONS:
            errors.append(
                f"gate_commands[{index}] classification must be one of "
                + ", ".join(sorted(GATE_CLASSIFICATIONS))
            )
            continue

        exit_code = command.get("exit_code")
        if not isinstance(exit_code, int) or isinstance(exit_code, bool):
            errors.append(f"gate_commands[{index}] must contain an integer exit_code")
            continue

        if classification == "green":
            if exit_code != 0:
                errors.append(
                    f"gate_commands[{index}] is green and must record exit_code=0; "
                    "classify a measured failure as real_defect or environmental"
                )
            continue

        if task_class not in MEASUREMENT_TASK_CLASSES:
            errors.append(
                f"gate_commands[{index}] classification {classification!r} requires task_class "
                + " or ".join(sorted(MEASUREMENT_TASK_CLASSES))
                + f", not {task_class!r}"
            )
        if exit_code == 0:
            errors.append(
                f"gate_commands[{index}] is classified {classification!r} but exited 0"
            )
        if not _nonempty_string(command.get("finding")):
            errors.append(
                f"gate_commands[{index}] classification {classification!r} requires a "
                "non-empty finding describing what was measured"
            )
    return errors, names


def git(repo: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(["git", *args], cwd=repo, text=True, capture_output=True)
    if check and result.returncode != 0:
        raise WorkflowError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result


def is_commit(repo: Path, sha: str) -> bool:
    return git(repo, "cat-file", "-e", f"{sha}^{{commit}}", check=False).returncode == 0


def is_ancestor(repo: Path, ancestor: str, descendant: str) -> bool:
    return git(repo, "merge-base", "--is-ancestor", ancestor, descendant, check=False).returncode == 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--task", required=True, type=Path)
    parser.add_argument("--repo-root", type=Path, default=Path.cwd())
    parser.add_argument("--json", action="store_true", dest="as_json")
    args = parser.parse_args()
    repo = args.repo_root.resolve()
    try:
        manifest: dict[str, Any] = json.loads(args.manifest.read_text(encoding="utf-8"))
        _, task = load_task(args.task.resolve())
        errors: list[str] = []
        missing = sorted(REQUIRED_KEYS - manifest.keys())
        if missing:
            errors.append("missing manifest keys: " + ", ".join(missing))
        if manifest.get("schema_version") != 1:
            errors.append("manifest schema_version must be 1")
        if manifest.get("task_id") != task.get("task_id"):
            errors.append("manifest task_id does not match task packet")
        if task.get("status") != "complete":
            errors.append("task packet status must be complete")
        expected_task_file = str(args.task.resolve().relative_to(repo)) if args.task.resolve().is_relative_to(repo) else str(args.task.resolve())
        if manifest.get("task_file") != expected_task_file:
            errors.append(f"manifest task_file must be {expected_task_file!r}")

        subject = str(manifest.get("subject_sha", ""))
        exact_fields = ["independent_tested_sha", "reviewed_sha", "gated_sha"]
        for field in exact_fields:
            if manifest.get(field) != subject:
                errors.append(f"{field} must equal subject_sha")
        for field in ["base_sha", "implementation_sha", "subject_sha"] + exact_fields:
            value = str(manifest.get(field, ""))
            if not value or not is_commit(repo, value):
                errors.append(f"{field} is not a valid commit: {value!r}")
        contract_sha = manifest.get("contract_sha")
        implementation_base = str(manifest.get("implementation_base_sha", ""))
        expected_implementation_base = str(contract_sha or manifest.get("base_sha", ""))
        if implementation_base != expected_implementation_base:
            errors.append("implementation_base_sha must equal contract_sha when present, otherwise base_sha")
        if contract_sha:
            contract_sha = str(contract_sha)
            if not is_commit(repo, contract_sha):
                errors.append("contract_sha is not a valid commit")
            elif not is_ancestor(repo, str(manifest.get("base_sha", "")), contract_sha):
                errors.append("base_sha is not an ancestor of contract_sha")
        base_sha = str(manifest.get("base_sha", ""))
        if base_sha and subject and is_commit(repo, base_sha) and is_commit(repo, subject):
            if not is_ancestor(repo, base_sha, subject):
                errors.append("base_sha is not an ancestor of subject_sha")

        implementation = str(manifest.get("implementation_sha", ""))
        if implementation_base and implementation and is_commit(repo, implementation_base) and is_commit(repo, implementation):
            if not is_ancestor(repo, implementation_base, implementation):
                errors.append("implementation_base_sha is not an ancestor of implementation_sha")
        if implementation and subject and is_commit(repo, implementation) and is_commit(repo, subject):
            if not is_ancestor(repo, implementation, subject):
                errors.append("implementation_sha is not an ancestor of subject_sha")

        if manifest.get("test_verdict") != "PASS":
            errors.append("test_verdict must be PASS")
        if manifest.get("review_verdict") not in {"APPROVE", "APPROVE_WITH_FOLLOW_UP"}:
            errors.append("review_verdict must be APPROVE or APPROVE_WITH_FOLLOW_UP")
        findings = manifest.get("open_findings")
        if not isinstance(findings, dict) or findings.get("p0") != 0 or findings.get("p1") != 0:
            errors.append("open_findings must report p0=0 and p1=0")
        gate_errors, command_names = check_gate_commands(
            manifest.get("gate_commands"), task.get("task_class")
        )
        errors.extend(gate_errors)
        for required_command in task.get("verification_commands", []):
            if required_command not in command_names:
                errors.append(f"required verification command missing from gate evidence: {required_command}")

        evidence_sha = manifest.get("evidence_commit_sha")
        if evidence_sha:
            evidence_sha = str(evidence_sha)
            if not is_commit(repo, evidence_sha):
                errors.append("evidence_commit_sha is not a valid commit")
            elif subject and not is_ancestor(repo, subject, evidence_sha):
                errors.append("subject_sha is not an ancestor of evidence_commit_sha")
            else:
                changed = [
                    line.strip()
                    for line in git(repo, "diff", "--name-only", f"{subject}..{evidence_sha}").stdout.splitlines()
                    if line.strip()
                ]
                evidence_patterns = [str(value) for value in task.get("path_ownership", {}).get("integrator", [])]
                unowned = [path for path in changed if not any(path_matches(path, pattern) for pattern in evidence_patterns)]
                if unowned:
                    errors.append("post-subject evidence commit changes unowned paths: " + ", ".join(unowned))

        expected_branch_head = str(evidence_sha or subject)
        task_branch = str(manifest.get("task_branch", ""))
        branch_result = git(repo, "rev-parse", task_branch, check=False)
        if branch_result.returncode != 0:
            errors.append(f"task_branch does not resolve: {task_branch!r}")
        elif branch_result.stdout.strip() != expected_branch_head:
            errors.append(
                f"task_branch points to {branch_result.stdout.strip()}, expected {expected_branch_head}"
            )

        merge = manifest.get("merge")
        if not isinstance(merge, dict):
            errors.append("merge must be an object")
        elif merge.get("merged"):
            merge_sha = str(merge.get("merge_sha") or "")
            if not merge.get("authorized"):
                errors.append("merge is marked merged without authorization")
            if not merge_sha or not is_commit(repo, merge_sha):
                errors.append("merge_sha is not a valid commit")
            else:
                ancestor = str(evidence_sha or subject)
                if ancestor and not is_ancestor(repo, ancestor, merge_sha):
                    errors.append("reviewed/evidence tree is not an ancestor of merge_sha")

        output = {"valid": not errors, "manifest": str(args.manifest), "subject_sha": subject, "errors": errors}
        if args.as_json or errors:
            print(json.dumps(output, indent=2))
        else:
            print(f"VALID: cycle closes against subject SHA {subject}")
        return 0 if not errors else 1
    except (OSError, json.JSONDecodeError, WorkflowError) as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
