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
