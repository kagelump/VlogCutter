# Cycle evidence manifest schema v1

Store evidence under a task-owned evidence path, normally:

```text
docs/task-cycles/<TASK-ID>/cycle.json
```

Example:

```json
{
  "schema_version": 1,
  "task_id": "E01-T001",
  "epic_file": "planning/epics/E01/EPIC.md",
  "task_file": "planning/epics/E01/tasks/E01-T001-decoder-seam.md",
  "base_branch": "main",
  "base_sha": "...",
  "task_branch": "codex/e01-t001-decoder-seam",
  "contract_sha": null,
  "implementation_base_sha": "...",
  "implementation_sha": "...",
  "subject_sha": "...",
  "independent_tested_sha": "...",
  "reviewed_sha": "...",
  "gated_sha": "...",
  "evidence_commit_sha": null,
  "test_verdict": "PASS",
  "review_verdict": "APPROVE",
  "open_findings": {"p0": 0, "p1": 0, "p2": 0},
  "gate_commands": [
    {"command": "cargo test --workspace", "exit_code": 0}
  ],
  "source_hash_artifact": null,
  "merge": {
    "authorized": false,
    "merged": false,
    "merge_sha": null
  }
}
```

`subject_sha`, `independent_tested_sha`, `reviewed_sha`, and `gated_sha` must be
identical. `implementation_base_sha` is `contract_sha` when present, otherwise
`base_sha`; `implementation_sha` may be an ancestor of the subject. `evidence_commit_sha` may be
a descendant containing evidence-only changes.

## Gate command classification

Every entry in `gate_commands` needs a `command` and an integer `exit_code`.
Entries also accept an optional `classification`:

| Classification | Meaning | Required exit code |
| --- | --- | --- |
| `green` (default) | The command passed. | `0` |
| `real_defect` | The command failed and the failure is a code defect. | nonzero |
| `environmental` | The command failed only because a host tool or fixture is absent. | nonzero |

Omitting `classification` means `green`, so existing manifests keep the original
`exit_code=0` assertion unchanged.

`real_defect` and `environmental` exist for a task whose purpose is to *measure*
the gate, so they are accepted only when the task packet's `task_class` is `qa`.
For any other task class a failing gate command is still a failing cycle — a
classification cannot be used to wave one through. A non-green entry must also
carry a non-empty `finding` naming what was measured, normally the ledger id:

```json
{
  "command": "pnpm check:docs",
  "exit_code": 1,
  "classification": "real_defect",
  "finding": "RD-1: check-docs reports two unresolved documentation links."
}
```

Classification does not exempt a command from coverage: every entry in the
packet's `verification_commands` must still appear in `gate_commands`.
