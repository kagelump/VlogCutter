---
name: executing-bounded-task-cycle
description: Use when an epic file contains bounded task packets and an orchestrator must select and execute exactly one ready task through implementation, adversarial testing, review, exact-SHA gating, and planning reconciliation.
license: MIT
compatibility: Requires git, shell access, Python 3.11+, isolated worktree support, and subagent dispatch. Designed to complement an installed Superpowers skill set.
metadata:
  version: "0.1.0"
---

# Executing a Bounded Task Cycle

## Overview

Execute exactly one task selected from an epic queue. Preserve strict role
separation and close the cycle against one exact candidate SHA.

**Core principle:** a task is not complete when code works; it is complete when
the exact integrated tree is independently tested, reviewed, gated, and
reconciled with the planning ledger.

**REQUIRED BACKGROUND:** understand
`superpowers:subagent-driven-development`.

**REQUIRED SUB-SKILLS:** use these where applicable rather than duplicating
their mechanics:

- `superpowers:using-git-worktrees`
- `superpowers:test-driven-development`
- `superpowers:requesting-code-review`
- `superpowers:receiving-code-review`
- `superpowers:verification-before-completion`
- `superpowers:finishing-a-development-branch`

If required skills, git, shell execution, or genuinely isolated subagent
contexts are unavailable, stop and produce a handoff rather than pretending the
cycle was independently executed.

## Invocation contract

The user supplies one epic file, for example:

```text
Use executing-bounded-task-cycle with planning/epics/E01/EPIC.md.
```

Optional invocation overrides may specify:

- an exact task ID;
- a base branch differing from epic metadata;
- a task branch name;
- explicit authorization to merge after completion.

No override is inferred from historical branches, previous cycles, or nearby
planning prose.

## Non-negotiable constraints

1. Execute one task only.
2. Select from the epic; do not silently promote draft work.
3. Never commit directly to the base branch.
4. Use one canonical task branch created directly from the declared base SHA.
5. Give implementation agents only their permitted documents and one task.
6. Independent testing does not inherit implementer reasoning.
7. Integrate independent tests before final review.
8. Every fix invalidates previous test, review, and gate claims.
9. Reconcile planning before the final candidate is reviewed and gated.
10. Never merge without explicit authorization.

Read [cycle-state-machine.md](references/cycle-state-machine.md),
[task-packet-schema.md](references/task-packet-schema.md), and
[evidence-schema.md](references/evidence-schema.md).

## Cross-agent broadcast log

Every agent working this epic — primary, independent tester, reviewer,
integrator, contract editor, coordinator, any wave sub-orchestrator — shares
one append-only log for it:

```sh
python3 scripts/epic_broadcast.py tail --epic <epic-id>          # read
python3 scripts/epic_broadcast.py append --epic <epic-id> \
  --task <your-task-id> --role <your-role> \
  --tag <category> --message "<one or two sentences>"            # write
```

Read [broadcast-log.md](references/broadcast-log.md) before using it — the
distinction it draws matters more than the mechanics. In short: this is for
a fact discovered mid-task that changes what a *different*, isolated agent
should assume before it runs headfirst into the same thing — a defect found
outside your owned paths, a shared file you're about to touch that a sibling
task also owns, an environment surprise, a contract that turned out
ambiguous. It is never for implementation rationale, confidence claims, or
routine progress — an independent tester or reviewer reading it should come
away knowing "watch out for X," never "here's how the implementation works."
Using it to explain your approach to another role is the same violation as
handing that role your reasoning directly (constraint 6); the log's shared
visibility does not create an exception.

Every dispatched agent tails it once at the start of its work and once again
immediately before its final gate or reconciliation step — the second read
is where a late-breaking entry from a sibling task actually earns its keep.

## Process

### 1. Select one task

From this skill directory, run:

```sh
python3 scripts/epic_queue.py <epic-file> --validate
python3 scripts/epic_queue.py <epic-file> --next
```

When the user specifies a task ID, verify that it exists, is `ready`, and has
complete dependencies. Otherwise select the first ready task by queue order.

If nothing is ready, report either:

- `EPIC_COMPLETE`; or
- `EPIC_BLOCKED`, including the unresolved dependency or draft packet.

Do not start another task.

Before proceeding, tail the epic's broadcast log — a sibling task or a
previous cycle may have posted something that changes how this task should
be approached or whether it's still safe to select:

```sh
python3 scripts/epic_broadcast.py tail --epic <epic-id>
```

### 2. Validate task admission

Run:

```sh
python3 scripts/validate_task.py <task-file>
```

Then perform a senior semantic review:

- Is there one principal invariant?
- Does the central falsification exercise the real seam?
- Are architecture decisions actually frozen?
- Is path ownership exhaustive?
- Does the task fit one focused context?
- Are test and review charters independent of likely implementation details?

#### Narrow contract repair

A senior contract editor may repair a clerical or internal contradiction before
implementation. Give it only `documents_by_role.contract_editor` and commit that
correction separately. Record the resulting `contract_sha`; the implementation
diff begins at `contract_sha`, not the original base SHA.

The editor may not choose a new product behavior, persistence model,
concurrency policy, security boundary, or cross-component architecture. Such a
need changes task status to `design_required` and ends the cycle without
production code.

### 3. Establish branch topology

Invoke `superpowers:using-git-worktrees`.

The epic declares `base_branch` and `task_branch_template`. Resolve the template
with lowercase task ID and packet slug unless the user explicitly provides a
branch.

Before any edit, inspect the current checkout and base:

```sh
git branch --show-current
git rev-parse <base-branch>
git show-ref --verify refs/heads/<task-branch> || true
git rev-parse --show-toplevel
git status --short --untracked-files=all
```

Create the task branch directly from the recorded base SHA when absent. When it
already exists, treat it as a resume only when its merge base is the recorded
base SHA and its history belongs to this task. Then print and record:

```sh
git rev-parse <task-branch>
git merge-base <base-sha> <task-branch>
```

A branch with a different merge base or unrelated commits is a blocker, not a
reason to cherry-pick into a historical branch. `Work off main`
means branch from main, not commit on main.

Only the integration agent advances the canonical task branch. Other roles use
isolated worktrees and return commits or read-only findings.

### 4. Dispatch the primary agent

Route by `task_class`:

| Class | Primary role | Writable ownership |
| --- | --- | --- |
| `foundation` | senior foundation implementer | `implementer` |
| `implementation` | focused leaf implementation agent | `implementer` |
| `integration` | integration agent | `integrator` |
| `qa` | independent tester | `independent_tester` |
| `review` | independent reviewer | evidence paths in `integrator`; production is read-only |

The normal full pipeline below applies to `foundation`, `implementation`, and
`integration`. For `qa`, skip production implementation and treat the test
commit as the first candidate. For `review`, produce findings/evidence only;
code remedies become separate queued tasks rather than scope expansion.

For an implementation-style primary agent, construct a fresh context containing
only:

- documents in `documents_by_role.implementer`;
- the selected task packet;
- relevant source files needed to implement it;
- the exact base and task branch information.

Do not provide the full roadmap, future packets, coordinator reasoning,
independent-test design, or previous agent confidence claims.

Include the epic's broadcast-log commands
(`scripts/epic_broadcast.py tail`/`append --epic <epic-id> --task <task-id>
--role implementer`) in its instructions, with the read/write boundary from
[broadcast-log.md](references/broadcast-log.md): tail it before starting and
again before finishing; append only facts about shared state a sibling task
needs to know, never rationale for its own choices.

Require `superpowers:test-driven-development`.

The primary agent may change only the ownership column selected above. It must
stop on any packet stop condition or unexpected path/dependency need. A
foundation agent may make only the decision explicitly delegated by the packet;
it may not absorb adjacent architecture work.

Required handoff:

- status: `DONE`, `DONE_WITH_CONCERNS`, `NEEDS_CONTEXT`, or `BLOCKED`;
- commit SHA;
- exact changed paths;
- tests run with complete results;
- public interface changes;
- known limitations;
- explicit scope statement.

The implementer does not approve its work.

### 5. Integrate a candidate

The integration agent inspects the returned commit, rejects unrelated changes,
and applies only compatible work to the canonical task branch.

Run path enforcement:

```sh
python3 scripts/check_changed_paths.py \
  --task <task-file> --base <implementation-base-sha> --head <implementation-sha> \
  --role implementer
```

The integrator alone owns shared hot spots listed in
`path_ownership.integrator`.

Set `implementation_base_sha` to `contract_sha` when contract repair occurred,
otherwise to `base_sha`. Record `implementation_sha` after leaf implementation.
Integrator-owned wiring may then create the first integrated candidate.

### 6. Dispatch the independent tester

Create a fresh context containing:

- only documents in `documents_by_role.independent_tester`;
- the task packet, especially its independent test charter;
- candidate SHA and public interface;
- independently owned test paths.

Do not provide implementer reasoning or tests as the primary oracle. Its
epic-broadcast-log instructions are the same read/write boundary as the
primary agent's (`--role independent_tester`): tailing it for facts about
shared state is fine and expected; the log is still not a channel for
implementer reasoning to reach this role by another route.

Standing instruction:

> Try to falsify the principal invariant through the real production seam. Use
> different fixtures, identifiers, and assumptions from the implementer.

The tester commits only paths in `path_ownership.independent_tester` and reports
`PASS`, `FAIL`, or `BLOCKED` with commands and evidence.

Before integration, enforce the tester's commit range with
`check_changed_paths.py --role independent-tester`. Integrate the independent
test commit before review, then rerun its tests on the canonical task branch and
record that candidate SHA. Fix lint or harness issues through the tester or
integrator role, never by weakening assertions.

### 7. Dispatch final-candidate review

Only after independent tests are integrated, dispatch an independent reviewer
using `superpowers:requesting-code-review` as the base pattern.

The reviewer receives:

- the task packet and only documents in `documents_by_role.reviewer`;
- base SHA;
- exact current candidate SHA;
- independent test evidence;
- no implementer reasoning.

Same broadcast-log boundary applies (`--role reviewer`): read for shared-state
facts, never as a substitute for reading the diff itself.

It answers every question in the task review charter. Each finding includes:

- severity;
- file and line;
- violated invariant or contract;
- concrete failure scenario;
- smallest acceptable remedy.

Verdict is exactly `APPROVE`, `APPROVE_WITH_FOLLOW_UP`, or `REJECT`.
No P0 or P1 may remain.

### 8. Fix loop

Evaluate feedback with `superpowers:receiving-code-review`.

Accepted changes return to the correct owning role as a bounded fix packet.
After any code, test, contract, or planning change:

1. integrate a new candidate SHA;
2. rerun independent tests against it;
3. rerun final-candidate review against it.

Do not carry approval across SHAs.

### 9. Reconcile planning state

Before the final gate, update:

- task metadata status to `complete`;
- epic queue row status to `complete`;
- foundation or roadmap entries named by reconciliation checks;
- stale blocker prose that asserts the completed behavior is absent.

Run:

```sh
python3 scripts/epic_queue.py <epic-file> --validate
python3 scripts/reconcile_planning_state.py --task <task-file> --repo-root .
```

Planning changes are part of the candidate. After reconciliation creates a new
candidate SHA, rerun independent tests on the canonical branch and dispatch a
new final-candidate review against that exact reconciled SHA before the final
gate. The earlier review is preliminary and cannot supply the final approval. Do not postpone planning updates
until after review, gate, or report.

### 10. Run final path and repository gates

Run final ownership enforcement:

```sh
python3 scripts/check_changed_paths.py \
  --task <task-file> --base <base-sha> --head <candidate-sha> \
  --role final --include-untracked
```

Run every command in `verification_commands`, plus repository-standard format,
lint, test, build, status, and sensitive-content checks.

Use `superpowers:verification-before-completion`. Capture commands, exit codes,
and artifacts. A claimed pass without reproducible command evidence is not a
pass.

Set:

```text
subject_sha = independent_tested_sha = reviewed_sha = gated_sha
```

### 11. Write and verify evidence

Create `cycle.json` using [CYCLE.template.json](assets/CYCLE.template.json) and a
human report using [CYCLE_REPORT.template.md](assets/CYCLE_REPORT.template.md).

Run:

```sh
python3 scripts/verify_cycle.py \
  --manifest <cycle.json> --task <task-file> --repo-root .
```

If committing evidence changes HEAD, record the evidence commit separately. Do
not pretend the evidence commit itself was the reviewed subject; verify that all
post-subject changes are evidence-owned paths only.

### 12. Finish without starting the next task

Identify the next ready task with `epic_queue.py --next`, but do not execute it.

Invoke `superpowers:finishing-a-development-branch`.

- Without explicit merge authorization: leave the canonical task branch ready.
- With explicit authorization: merge only after evidence is complete, rerun the
  base-branch smoke gate, and report both subject SHA and merge SHA.

### 13. Batch-merging a completed wave

Step 12 covers merging *one* task's branch. Merging several completed
branches from the same wave is a distinct operation — sibling branches that
are each individually clean can still conflict with each other in ways
neither branch's own review caught, and a wave-completion report's pass/fail
claims are prose to verify, not facts.

Read [wave-merge.md](references/wave-merge.md) before doing this — it builds
on `superpowers:using-git-worktrees`, `superpowers:finishing-a-development-branch`,
and `superpowers:verification-before-completion` rather than re-explaining
them, and covers only what's specific to merging several sibling branches
together. Do not improvise a shortcut version of it; every rule there was
written because a plausible-looking shortcut produced a real defect once (a
duplicated queue row from reading a conflict-marked file as if it were valid
content; a "33 tests" claim that meant "33 tests exist," not "33 pass";
three CI jobs silently colliding under one YAML key; source branches that sat
unmerged and undeleted on the remote until asked about separately). In short:

1. Re-verify every candidate branch's own `verification_commands` in
   isolation — do not trust the report.
2. Exclude any branch whose epic-queue row isn't `complete` on its own tip.
   A `qa`/`review` task that stopped on a real finding is not a merge
   candidate; the finding needs its own remediation task first.
3. Simulate the full intended merge order before touching anything real:

   ```sh
   python3 scripts/simulate_wave_merge.py --base <base> --branches <b1> <b2> ...
   ```

4. Classify every conflict the simulation reports — routine queue-table edit,
   pure append, or same-key/semantic collision — and resolve accordingly.
   `scripts/resolve_epic_queue_conflict.py` and
   `scripts/resolve_append_only_conflict.py` handle the first two
   mechanically (both refuse rather than guess if their precondition
   doesn't actually hold); same-key collisions need real reconciliation,
   decided once, not generated per merge.
5. Merge one branch at a time. Rerun the full repository gate (step 10)
   against the real resulting tree after *every* merge, not once at the end
   — a merge can surface a defect neither branch's own isolated tests could
   have found.
6. Batch-merging into an integration branch and merging that branch to the
   declared base are two different authorizations. Report the completed,
   gated batch and wait for explicit authorization before the second one,
   same as step 12. When authorized, that final merge is
   `superpowers:finishing-a-development-branch`'s "merge locally" for the
   integration branch — use it for that step rather than improvising.
7. That skill's cleanup only covers the one branch it merges. Once the base
   merge lands, delete every *source* branch in the wave too — verify each
   with `git branch --merged <base>` before `-d`, never `-D`. This step was
   missed running the procedure for real and only got done because it was
   asked for separately; it belongs at the end of the batch, not after a
   follow-up request.

If any step above was performed by the coordinator directly rather than a
dispatched role — a real, sometimes explicitly-authorized tradeoff for
speed — say so plainly in the final report: which specific fixes had no
independent test or review pass because the same agent doing the merge also
made them.

The three-way `ci.yml` collision in §5 of wave-merge.md is exactly what a
`shared-file-collision` broadcast entry, posted by the first task to touch
that file, would have flagged for the other two before they repeated the
same motion. When dispatching parallel primary agents within one wave, tell
each one which shared paths (per the epic's path-ownership hotspots) are
worth a pre-emptive broadcast the moment it's about to write one.

### 14. Write an epic completion brief

Distinct from step 13 and from every per-cycle report: this runs once, when
the *entire* epic promotes — every wave merged, every queue row `complete`,
exit-gate evidence attached per the calling repo's ORCHESTRATION.md §9 definition-of-complete
if the calling repo has one. Not after a wave; a wave completing is not an
epic completing.

Write `docs/phases/phase-N/DESIGNER_BRIEF.md` (or wherever the calling
repo's convention places phase evidence) using
[DESIGNER_BRIEF.template.md](assets/DESIGNER_BRIEF.template.md). Its point is
narrower than a status report: surface what the epic's own scope and
authority could not resolve, and what only shows up in hindsight across many
cycles rather than in any one of them —

- every defect found during the epic, regardless of who found it or whether
  it got fixed here, as a flat table a later audit could use directly;
- defects or interactions that no single task's isolated verification could
  have found — only visible once branches were merged or two tasks'
  outputs interacted (this is a real, recurring category, not a one-off —
  size the next epic's expectations accordingly);
- places a frozen contract turned out ambiguous or wrong, whether resolved
  by narrow contract repair or left open;
- process and report-integrity gaps — anywhere a task's own report didn't
  match its own reality, which is exactly the kind of thing that makes the
  next epic's packet-writer tighten its acceptance oracles;
- a rollup of the epic's broadcast log (`epic_broadcast.py tail --epic
  <epic-id>`) — decide per entry whether it's already reflected elsewhere or
  needs to be captured here before the log stops being actively read;
- explicit decisions that need the senior designer, stated as concrete
  questions with enough context to answer without re-reading the whole
  epic — this section is the actual reason the document exists.

A brief with every section reading "None." for an epic that had any real
friction is a sign the brief was written from memory instead of from the
actual cycle evidence, findings, and broadcast log. Pull from those sources,
don't reconstruct from recollection.

## Final report

Report:

- epic and selected task;
- base SHA, task branch, implementation SHA, subject SHA, evidence commit SHA;
- contract repair, if any;
- agents and isolated roles;
- changed paths by role;
- independent test verdict and novel findings;
- review verdict and findings;
- full gate evidence;
- planning reconciliation result;
- merge result, if authorized;
- workflow observations;
- next task, without starting it.

Never call an intermediate SHA “final.”

## Common rationalizations to reject

| Rationalization | Response |
| --- | --- |
| “The tree is identical, so branch choice does not matter” | Correct branch topology before further integration; record exact refs |
| “The reviewer saw the production code, so a later test commit is harmless” | Review the final candidate containing independent tests |
| “Planning updates are documentation-only” | They are authoritative queue state and part of cycle closure |
| “A boolean says orientation was applied” | Use a fixture where zero, one, and two applications differ observably |
| “The full gate passed in another agent’s message” | Capture commands against the exact subject SHA |
| “This tiny extra file is obviously in scope” | Unowned paths trigger integration rejection or packet repair |
| “The wave report says N tests passed” | Rerun that branch’s verification_commands yourself before merging it |
| “These branches obviously don’t overlap” | Run `simulate_wave_merge.py`; a shared file outside either branch’s core work still collides |
| “Both sides just add a block, I’ll concatenate” | Confirm it’s append-only first — a shared key (e.g. a YAML job name) needs real reconciliation, not concatenation |
| “I’ll read the conflicted file and fix up the rows” | Read git’s index stages, not the marker-laden working-tree file — conflict markers look like valid content to a naive line scan |
| “I’ll post my reasoning to the broadcast log so the tester understands my approach” | That’s implementer reasoning reaching an isolated role by another route — constraint 6 applies to the log too |
| “Nobody reads the broadcast log anyway” | Every dispatched agent is required to tail it at start and before finishing — if that isn’t happening, fix the dispatch, don’t skip writing to it |
| “The epic brief has nothing surprising, I’ll mark every section None” | Pull from cycle evidence, findings, and the broadcast log before writing it — an epic with real friction and an all-None brief means it was written from memory |
