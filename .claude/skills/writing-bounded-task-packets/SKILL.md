---
name: writing-bounded-task-packets
description: Use when a senior designer has an approved product or architecture design and must decompose an epic into independently testable, junior-safe task packets before implementation.
license: MIT
compatibility: Requires filesystem access and Python 3.11+ for bundled validators. Designed to complement an installed Superpowers skill set.
metadata:
  version: "0.1.0"
---

# Writing Bounded Task Packets

## Overview

Turn an approved design into an ordered epic queue whose leaf tasks are small
enough for a focused implementation agent and precise enough for independent
falsification.

**Core principle:** the epic orders work; each linked task packet is the complete
unit of implementation authority.

**REQUIRED SUB-SKILL:** invoke `superpowers:writing-plans` for the initial
implementation decomposition. Then compile that result into this skill's epic
and packet format. The Superpowers plan is an intermediate design artifact, not
the executable unit of authority.

Do not duplicate its TDD, YAGNI, exact-file, or verification guidance. Add the
admission and governance rules below. Its 2–5 minute actions become steps inside
a bounded packet; they are not automatically separate epic rows. One packet
owns one test cycle and one meaningful reviewer gate.

## When to use

Use this skill when:

- product and architecture direction are approved;
- a senior designer is turning that direction into an epic;
- implementation agents have strong throughput but should not make system-level
  decisions;
- independent test and review roles need a stable external oracle;
- task size and scope are more important than maximizing parallelism.

Do not use it to discover product requirements. Resolve those first with the
appropriate design or brainstorming workflow.

## Inputs

Read only the documents needed to understand:

- product behavior and invariants;
- architecture boundaries and frozen decisions;
- current implementation state;
- existing epic and roadmap conventions;
- known risk areas and release gates.

Do not write implementation code while authoring packets.

## Output

Produce or update:

```text
<epic-directory>/EPIC.md
<epic-directory>/tasks/<TASK-ID>-<slug>.md
```

Use [epic-schema.md](references/epic-schema.md) and
[task-packet-schema.md](references/task-packet-schema.md).
Start from [EPIC.template.md](assets/EPIC.template.md) and
[TASK.template.md](assets/TASK.template.md).

## Process

### 1. Map the system before slicing

Identify:

- stable interfaces;
- load-bearing seams;
- senior foundation decisions;
- shared integration hot spots;
- independently observable outcomes;
- task dependencies.

Do not split by arbitrary file count or technical layer. Keep changes that must
be judged together in one packet; split where a reviewer could reject one unit
while approving its neighbor.

### 2. Separate foundation work from leaf work

Classify each packet before writing it:

| Class | Primary execution role |
| --- | --- |
| `foundation` | senior foundation implementer |
| `implementation` | focused leaf implementation agent |
| `integration` | integration agent for shared hot spots |
| `qa` | independent tester; no production changes |
| `review` | independent reviewer; findings/evidence only |

A task is senior foundation work when it chooses or changes:

- a persistence or revision model;
- concurrency and transaction policy;
- filesystem or security authority;
- public behavior semantics;
- cross-component dependency direction;
- a shared extension seam;
- release certification rules.

Create and order that foundation task before any leaf task depending on it.
Never hide an unresolved architecture choice inside an implementation packet.

### 3. State one principal invariant

Write one sentence completing:

> This task is correct when ...

If the sentence needs `and` to join unrelated claims, split the task.
Supporting preservation requirements may exist, but only one claim is the
center of the task.

### 4. Supply a central falsification

Write an externally supplied test idea that could prove the principal invariant
false. It must exercise the real public or production seam rather than merely a
new helper or symbol.

Weak:

> The new trait exists and has unit tests.

Strong:

> Inject a marked implementation through the public service, run the production
> workflow, and observe the mark in the derived output.

The implementation agent does not invent this oracle.

### 5. Apply the task-admission gate

An implementation packet is `ready` only if all are true:

1. one principal invariant;
2. independently falsifiable central claim;
3. no unresolved architecture decision;
4. no more than one load-bearing seam;
5. exhaustive, role-specific path ownership;
6. independent reviewer rejectability;
7. one focused implementation context;
8. explicit stop conditions.

If any criterion fails, split the task, create a foundation task, or leave it
`draft`. Never mark an aspirational packet `ready` to keep the queue moving.

### 6. Define role ownership

At minimum define paths for:

- contract editor;
- implementer;
- independent tester;
- integrator.

Shared manifests, migrations, module registries, generated indexes, CI files,
and planning reconciliation normally belong to the integrator.

Every forbidden path overrides every allowed path.

### 7. Write implementer-independent test and review charters

The independent test charter states:

- the claim to falsify;
- fixtures or data that differ from implementer fixtures;
- the real seam to exercise;
- required failure behavior;
- preservation checks;
- evidence required.

The review charter states:

- risk domain;
- exact questions to answer;
- severity rules;
- smallest acceptable remedy format;
- approval vocabulary.

Do not reveal expected implementation internals unless the architecture has
explicitly frozen them.

### 8. Add stop conditions

Stop conditions are task-specific. Include situations such as:

- needing an unowned production path;
- needing a new dependency or migration;
- changing a frozen public invariant;
- moving work across an architecture boundary;
- weakening the external oracle;
- discovering that the task cannot fit one context.

The required response is escalation, not opportunistic redesign.

### 9. Add planning reconciliation checks

For every canonical place that will still describe the task after completion,
add a regex check in metadata. Include:

- the epic queue row;
- linked foundation ledger entries;
- roadmap blocker statements when applicable;
- release-gate tracking entries.

This prevents code-complete work from leaving the queue stale.

### 10. Validate mechanically

From this skill directory, run:

```sh
python3 scripts/validate_task.py <task-file>
python3 scripts/epic_queue.py <epic-file> --validate
python3 scripts/epic_queue.py <epic-file> --next
```

Do not hand off an epic while any ready packet fails validation.

## Handoff to execution

End with the exact epic path and invocation:

```text
Use executing-bounded-task-cycle with <epic-path>.
Select and execute exactly one ready task. Do not start the following task.
```

Do not prescribe a task ID when queue ordering and dependencies already decide
it, unless the human explicitly overrides queue selection.

## Common mistakes

| Mistake | Correction |
| --- | --- |
| Task is a user story spanning database, backend, and UI | Split at stable seams while preserving independently testable vertical claims |
| Exact code is written but behavior oracle is vague | Freeze behavior and falsification before implementation detail |
| Path list says “related files” | Enumerate paths or globs exhaustively |
| Implementer owns shared manifests | Move them to integration ownership |
| Task requires a design judgment but is labeled implementation | Create a foundation task and dependency |
| Acceptance checks only symbol presence | Exercise the real production entry point |
| Epic says complete but roadmap says open | Add reconciliation regexes before handoff |
