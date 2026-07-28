#!/usr/bin/env python3
"""Standard-library tests for the bounded-task workflow scripts.

Run from this directory:

    python3 test_bounded_task_scripts.py
"""
from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))

from check_changed_paths import reconciliation_paths  # noqa: E402
from verify_cycle import check_gate_commands  # noqa: E402

TASK_TEMPLATE = """# {task_id}: {title}

<!-- bounded-task-metadata
{metadata}
-->

## Goal

Placeholder.
"""


def task_metadata(**overrides: object) -> dict[str, object]:
    meta: dict[str, object] = {
        "schema_version": 1,
        "task_id": "E01-T001",
        "epic_id": "E01",
        "title": "Decoder seam",
        "slug": "decoder-seam",
        "status": "complete",
        "task_class": "implementation",
        "depends_on": [],
        "principal_invariant": "invariant",
        "central_falsification": "falsification",
        "architecture_decisions_frozen": True,
        "load_bearing_seams": [],
        "documents_by_role": {
            "contract_editor": [],
            "implementer": [],
            "independent_tester": [],
            "reviewer": [],
            "integrator": [],
        },
        "path_ownership": {
            "contract_editor": ["planning/epics/E01/tasks/E01-T001-decoder-seam.md"],
            "implementer": ["core/src/decoder.rs"],
            "independent_tester": ["core/tests/decoder_independent.rs"],
            "integrator": ["docs/task-cycles/E01-T001/**"],
        },
        "forbidden_paths": ["apps/**"],
        "verification_commands": ["cargo test --workspace"],
        "reconciliation_checks": [
            {
                "path": "planning/epics/E01/EPIC.md",
                "regex": "\\| \\[E01-T001\\].*\\| complete \\|",
            }
        ],
    }
    meta.update(overrides)
    return meta


def write_task(directory: Path, meta: dict[str, object]) -> Path:
    path = directory / f"{meta['task_id']}-{meta['slug']}.md"
    path.write_text(
        TASK_TEMPLATE.format(
            task_id=meta["task_id"],
            title=meta["title"],
            metadata=json.dumps(meta, indent=2),
        ),
        encoding="utf-8",
    )
    return path


class GateCommandTests(unittest.TestCase):
    """Gap 1: a qa audit must be able to record a measured failure."""

    def test_legacy_entries_without_classification_still_pass(self) -> None:
        errors, names = check_gate_commands(
            [{"command": "cargo test --workspace", "exit_code": 0}], "implementation"
        )
        self.assertEqual(errors, [])
        self.assertEqual(names, {"cargo test --workspace"})

    def test_legacy_nonzero_exit_without_classification_still_fails(self) -> None:
        errors, _ = check_gate_commands(
            [{"command": "pnpm check:docs", "exit_code": 1}], "implementation"
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("must record exit_code=0", errors[0])

    def test_qa_task_may_record_a_real_defect(self) -> None:
        errors, names = check_gate_commands(
            [
                {"command": "cargo test --workspace", "exit_code": 0},
                {
                    "command": "pnpm check:docs",
                    "exit_code": 1,
                    "classification": "real_defect",
                    "finding": "RD-1: check-docs.mjs reports two broken links.",
                },
            ],
            "qa",
        )
        self.assertEqual(errors, [])
        self.assertEqual(names, {"cargo test --workspace", "pnpm check:docs"})

    def test_qa_task_may_record_an_environmental_failure(self) -> None:
        errors, _ = check_gate_commands(
            [
                {
                    "command": "pnpm test:web:e2e",
                    "exit_code": 1,
                    "classification": "environmental",
                    "finding": "Playwright browsers are not installed on this host.",
                }
            ],
            "qa",
        )
        self.assertEqual(errors, [])

    def test_non_qa_task_may_not_classify_away_a_failure(self) -> None:
        errors, _ = check_gate_commands(
            [
                {
                    "command": "pnpm check:docs",
                    "exit_code": 1,
                    "classification": "real_defect",
                    "finding": "RD-1",
                }
            ],
            "implementation",
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("requires task_class qa", errors[0])

    def test_non_green_classification_requires_a_finding(self) -> None:
        errors, _ = check_gate_commands(
            [{"command": "pnpm check:docs", "exit_code": 1, "classification": "real_defect"}],
            "qa",
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("non-empty finding", errors[0])

    def test_non_green_classification_requires_a_nonzero_exit(self) -> None:
        errors, _ = check_gate_commands(
            [
                {
                    "command": "pnpm check:docs",
                    "exit_code": 0,
                    "classification": "real_defect",
                    "finding": "RD-1",
                }
            ],
            "qa",
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("but exited 0", errors[0])

    def test_unknown_classification_is_rejected(self) -> None:
        errors, _ = check_gate_commands(
            [{"command": "pnpm check:docs", "exit_code": 1, "classification": "flaky"}], "qa"
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("classification must be one of", errors[0])

    def test_missing_or_non_integer_exit_code_is_rejected(self) -> None:
        errors, _ = check_gate_commands([{"command": "pnpm check:docs"}], "qa")
        self.assertEqual(len(errors), 1)
        self.assertIn("integer exit_code", errors[0])
        errors, _ = check_gate_commands(
            [{"command": "pnpm check:docs", "exit_code": True}], "qa"
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("integer exit_code", errors[0])

    def test_empty_or_missing_gate_commands_is_rejected(self) -> None:
        for value in ([], None, "cargo test"):
            errors, names = check_gate_commands(value, "qa")
            self.assertEqual(errors, ["gate_commands must contain at least one command"])
            self.assertEqual(names, set())

    def test_missing_command_name_is_rejected(self) -> None:
        errors, names = check_gate_commands([{"exit_code": 0}], "qa")
        self.assertEqual(len(errors), 1)
        self.assertIn("non-empty command", errors[0])
        self.assertEqual(names, set())


class ReconciliationPathTests(unittest.TestCase):
    """Gap 2: the required planning reconciliation edit is not an ownership violation."""

    def test_paths_are_extracted_and_deduplicated(self) -> None:
        meta = task_metadata(
            reconciliation_checks=[
                {"path": "planning/epics/E01/EPIC.md", "regex": "a"},
                {"path": " planning/epics/E01/EPIC.md ", "regex": "b"},
                {"path": "planning/ROADMAP.md", "regex": "c"},
                {"regex": "no path"},
                "not an object",
                {"path": "  ", "regex": "blank"},
            ]
        )
        self.assertEqual(
            reconciliation_paths(meta),
            ["planning/epics/E01/EPIC.md", "planning/ROADMAP.md"],
        )

    def test_missing_reconciliation_checks_yields_no_paths(self) -> None:
        self.assertEqual(reconciliation_paths({}), [])


class CheckChangedPathsCliTests(unittest.TestCase):
    """End-to-end runs of check_changed_paths.py against a scratch repository."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "test@example.com")
        self.git("config", "user.name", "Test")
        (self.repo / "planning/epics/E01/tasks").mkdir(parents=True)
        (self.repo / "core/src").mkdir(parents=True)
        (self.repo / "planning/epics/E01/EPIC.md").write_text("| [E01-T001](x) | ready |\n")
        (self.repo / "core/src/decoder.rs").write_text("// decoder\n")
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "base")
        self.base = self.git("rev-parse", "HEAD").strip()

    def git(self, *args: str) -> str:
        return subprocess.run(
            ["git", *args], cwd=self.repo, check=True, text=True, capture_output=True
        ).stdout

    def run_check(self, task: Path, role: str) -> tuple[int, dict]:
        completed = subprocess.run(
            [
                sys.executable,
                str(SCRIPTS / "check_changed_paths.py"),
                "--task", str(task),
                "--base", self.base,
                "--head", "HEAD",
                "--role", role,
                "--repo-root", str(self.repo),
                "--json",
            ],
            check=False,
            text=True,
            capture_output=True,
        )
        return completed.returncode, json.loads(completed.stdout)

    def commit_reconciliation(self) -> None:
        (self.repo / "planning/epics/E01/EPIC.md").write_text("| [E01-T001](x) | complete |\n")
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "reconcile")

    def test_final_role_allows_the_required_epic_row_edit(self) -> None:
        task = write_task(self.repo / "planning/epics/E01/tasks", task_metadata())
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "packet")
        self.commit_reconciliation()
        code, result = self.run_check(task, "final")
        self.assertEqual(code, 0, result)
        self.assertEqual(result["violations"], [])
        self.assertEqual(result["reconciliation_patterns"], ["planning/epics/E01/EPIC.md"])

    def test_implementer_role_still_rejects_the_epic_row_edit(self) -> None:
        task = write_task(self.repo / "planning/epics/E01/tasks", task_metadata())
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "packet")
        self.commit_reconciliation()
        code, result = self.run_check(task, "implementer")
        self.assertEqual(code, 1)
        self.assertEqual(result["reconciliation_patterns"], [])
        self.assertIn(
            {"path": "planning/epics/E01/EPIC.md", "reason": "not owned by selected role"},
            result["violations"],
        )

    def test_forbidden_path_still_wins_over_reconciliation_allowance(self) -> None:
        meta = task_metadata(
            forbidden_paths=["apps/**", "planning/epics/E01/EPIC.md"],
        )
        task = write_task(self.repo / "planning/epics/E01/tasks", meta)
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "packet")
        self.commit_reconciliation()
        code, result = self.run_check(task, "final")
        self.assertEqual(code, 1)
        self.assertIn(
            {"path": "planning/epics/E01/EPIC.md", "reason": "matches forbidden path"},
            result["violations"],
        )

    def test_unowned_path_is_still_a_violation_under_final(self) -> None:
        task = write_task(self.repo / "planning/epics/E01/tasks", task_metadata())
        (self.repo / "core/src/unowned.rs").write_text("// nope\n")
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "packet plus stray")
        code, result = self.run_check(task, "final")
        self.assertEqual(code, 1)
        self.assertIn(
            {"path": "core/src/unowned.rs", "reason": "not owned by selected role"},
            result["violations"],
        )


if __name__ == "__main__":
    unittest.main()
