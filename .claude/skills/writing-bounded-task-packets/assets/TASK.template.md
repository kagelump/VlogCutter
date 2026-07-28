# {{TASK_ID}}: {{TASK_TITLE}}

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "{{TASK_ID}}",
  "epic_id": "{{EPIC_ID}}",
  "title": "{{TASK_TITLE}}",
  "slug": "{{TASK_SLUG}}",
  "status": "draft",
  "task_class": "implementation",
  "depends_on": [],
  "principal_invariant": "{{ONE_SENTENCE_INVARIANT}}",
  "central_falsification": "{{ONE_SENTENCE_FALSIFICATION}}",
  "architecture_decisions_frozen": false,
  "load_bearing_seams": [],
  "documents_by_role": {
    "contract_editor": [],
    "implementer": [],
    "independent_tester": [],
    "reviewer": [],
    "integrator": []
  },
  "path_ownership": {
    "contract_editor": ["{{THIS_TASK_FILE}}"],
    "implementer": [],
    "independent_tester": [],
    "integrator": []
  },
  "forbidden_paths": [],
  "verification_commands": [],
  "reconciliation_checks": []
}
-->

## Goal

Describe the bounded outcome.

## Principal invariant

This task is correct when ...

## Inputs and outputs

Inputs:

- ...

Outputs:

- ...

## Frozen decisions

List decisions the implementation agent may rely on and may not redesign.

## Owned paths

Explain role ownership represented in metadata.

## Required interface

Specify only interfaces frozen by architecture.

## Acceptance oracle

State observable cases and expected behavior supplied independently of the
implementation.

## Independent test charter

Try to falsify the principal invariant through the real production seam. Use
different fixtures and assumptions from the implementer.

## Review charter

State the risk domain, questions, severity rules, and approval vocabulary.

## Non-goals

- ...

## Stop conditions

Stop and escalate when:

- ...

## Commands

```sh
# exact commands and expected result
```

## Handoff

List changed paths, commands and complete results, compatibility effects, known
limitations, and any stop condition encountered. The implementer does not
approve the task.
