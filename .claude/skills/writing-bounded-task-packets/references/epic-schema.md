# Epic queue schema v1

The orchestrator is pointed at one Markdown epic file. The epic owns ordering,
dependencies, queue status, and branch policy; linked packets own implementation
authority.

## Metadata block

```md
<!-- bounded-epic-metadata
{
  "schema_version": 1,
  "epic_id": "E01",
  "title": "Safe JPEG project",
  "base_branch": "main",
  "task_branch_template": "codex/{task_id_lower}-{slug}",
  "tasks_per_cycle": 1,
  "merge_requires_user_authorization": true
}
-->
```

## Queue table

Use this exact column order:

```md
| Order | Task | Title | Depends on | Status |
| ---: | --- | --- | --- | --- |
| 10 | [E01-T001](tasks/E01-T001-decoder-seam.md) | Decoder seam | — | ready |
| 20 | [E01-T002](tasks/E01-T002-thumbnail-cache.md) | Thumbnail cache | E01-T001 | draft |
```

Rules:

- Order is a unique integer and controls selection.
- Task IDs are unique.
- Dependencies name task IDs in the same epic, comma-separated.
- `—` or `-` means no dependency.
- The row status must match the packet metadata status.
- A task is selectable only when its status is `ready` and every dependency is
  `complete`.
- One invocation selects exactly one task.
- If no task is selectable, report whether the epic is complete or blocked.
- The executor must not silently promote a `draft` task to `ready`.
