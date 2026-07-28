# {{TASK_ID}} cycle report

## Identity

- Epic: `{{EPIC_FILE}}`
- Task: `{{TASK_FILE}}`
- Base: `{{BASE_BRANCH}}` at `{{BASE_SHA}}`
- Task branch: `{{TASK_BRANCH}}`
- Contract SHA: `{{CONTRACT_SHA_OR_NONE}}`
- Implementation base SHA: `{{IMPLEMENTATION_BASE_SHA}}`
- Implementation SHA: `{{IMPLEMENTATION_SHA}}`
- Reviewed/tested/gated subject SHA: `{{SUBJECT_SHA}}`
- Evidence commit SHA: `{{EVIDENCE_COMMIT_SHA_OR_NONE}}`

## Contract validation

{{CONTRACT_REPAIR_OR_NONE}}

## Roles

{{ROLE_TABLE}}

## Changed paths by role

{{PATH_TABLE}}

## Independent testing

- Verdict: `{{TEST_VERDICT}}`
- Novel findings: {{NOVEL_TEST_FINDINGS}}
- Commands and evidence: {{TEST_EVIDENCE}}

## Review

- Verdict: `{{REVIEW_VERDICT}}`
- Open P0/P1: `0/0`
- Findings: {{REVIEW_FINDINGS}}

## Final gate

{{GATE_RESULTS}}

## Planning reconciliation

{{RECONCILIATION_RESULTS}}

## Merge

{{MERGE_STATUS}}

## Workflow observations

- Scope pressure: {{SCOPE_PRESSURE}}
- Packet sufficiency: {{PACKET_SUFFICIENCY}}
- Role isolation: {{ROLE_ISOLATION}}
- Improvements before next cycle: {{IMPROVEMENTS}}

## Next task

`{{NEXT_TASK_OR_NONE}}` — identified only; not started.
