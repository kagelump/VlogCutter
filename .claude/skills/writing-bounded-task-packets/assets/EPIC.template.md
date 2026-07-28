# {{EPIC_ID}}: {{EPIC_TITLE}}

<!-- bounded-epic-metadata
{
  "schema_version": 1,
  "epic_id": "{{EPIC_ID}}",
  "title": "{{EPIC_TITLE}}",
  "base_branch": "main",
  "task_branch_template": "codex/{task_id_lower}-{slug}",
  "tasks_per_cycle": 1,
  "merge_requires_user_authorization": true
}
-->

## Outcome

{{EPIC_OUTCOME}}

## Frozen context

- Product contract: `{{PRODUCT_CONTRACT_PATH}}`
- Architecture contract: `{{ARCHITECTURE_CONTRACT_PATH}}`

## Queue

| Order | Task | Title | Depends on | Status |
| ---: | --- | --- | --- | --- |
| 10 | [{{TASK_ID}}](tasks/{{TASK_ID}}-{{TASK_SLUG}}.md) | {{TASK_TITLE}} | — | draft |

## Promotion rule

The epic is promoted only when every required implementation, QA, review, and
release-gate packet is complete. One task cycle never promotes unrelated work.
