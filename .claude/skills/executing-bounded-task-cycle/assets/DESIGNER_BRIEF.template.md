# {{EPIC_ID}} designer brief

Written once, when the whole epic promotes — every wave merged, every queue
row `complete`, exit-gate evidence attached. Not written per wave and not a
duplicate of `EPIC.md`, `TEST_REPORT.md`, or `REVIEW_REPORT.md`; this exists
for what those don't surface on their own: patterns across the epic, and
explicit decisions only the senior designer can make before the next epic's
packets get written.

If a section has nothing to report, write "None." — an omitted section reads
as "not checked," not as "checked, nothing found."

## Identity

- Epic: `{{EPIC_ID}}` — `{{EPIC_TITLE}}`
- Promotion date: `{{DATE}}`
- Final integrated SHA: `{{FINAL_SHA}}`
- Waves: `{{WAVE_COUNT}}`, tasks: `{{TASK_COUNT}}`, cycles: `{{CYCLE_COUNT}}`

## Defects found during the epic

One row per defect discovered by any task in this epic, regardless of who
found it or whether it was fixed here. This is the epic's real defect
history — a table an auditor could use, not a narrative.

| Found by | Severity | Where | Status | Note |
| --- | --- | --- | --- | --- |
| {{TASK_ID}} | {{P0-P3}} | `{{FILE}}` | fixed-in-epic / deferred-new-task / still-open | {{ONE_LINE}} |

A `still-open` row is not acceptable at epic promotion unless the epic's own
exit gate explicitly tolerates it (e.g. a documented, accepted blocker per
the epic's own EPIC.md). If one exists, say so explicitly here and name the
tracking task.

## Cross-task discoveries

Defects or interactions that no single task's isolated verification could
have found — only visible once multiple branches were merged, or once two
tasks' outputs interacted. Call these out specifically: they're a category
of risk inherent to decomposing work into isolated tasks, not a flaw in any
one task, and the designer should know how often this epic hit that class of
issue when sizing the next one.

{{CROSS_TASK_DISCOVERIES_OR_NONE}}

## Process and report-integrity gaps

Places where a task's own report didn't match its own reality — an
undercounted test-failure, a fixture that didn't cover what it claimed to, a
verdict that didn't match its own stated findings, a wave-completion summary
that needed independent re-verification before it could be trusted. These
are signals about the *workflow*, not about any one task's code, and they're
exactly what the next epic's packet-writer needs to know to write tighter
acceptance oracles.

{{PROCESS_GAPS_OR_NONE}}

## Frozen-decision and contract friction

Anywhere an integration contract or a frozen architecture decision turned
out ambiguous, incomplete, or wrong during this epic — whether resolved by
narrow contract repair (step 2) or left as an open question. Include
anything a sibling task's stop condition escalated for this reason, even if
it was ultimately resolved without changing the contract.

{{CONTRACT_FRICTION_OR_NONE}}

## Broadcast log rollup

Pull the epic's full broadcast log and decide, per entry, whether it's
already reflected above or needs to be captured here before the log stops
being actively read:

```sh
python3 scripts/epic_broadcast.py tail --epic {{EPIC_ID}}
```

{{BROADCAST_LOG_ROLLUP_OR_NONE}}

## Decisions needed from the senior designer

The actual point of this document. Anything this epic's own scope and
authority could not resolve — a `design_required` outcome, a pattern
suggesting the next epic's scope or a foundation item's priority should
change, a tradeoff the coordinator is not authorized to make alone. State
each as a concrete question with enough context to answer it without
re-reading the whole epic.

{{DECISIONS_NEEDED_OR_NONE}}

## Workflow observations

- Task sizing: {{TASK_SIZING}}
- Path-ownership friction: {{PATH_OWNERSHIP_FRICTION}}
- Role isolation held / was tempting to break: {{ROLE_ISOLATION}}
- Improvements before the next epic's packets are written: {{IMPROVEMENTS}}
