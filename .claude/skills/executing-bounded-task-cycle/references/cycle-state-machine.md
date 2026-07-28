# One-task cycle state machine

## Canonical sequence

```text
ready
  -> contract_validated
  -> branch_verified
  -> implementing
  -> code_complete
  -> integrated
  -> independently_tested
  -> preliminary_reviewed
  -> reconciled
  -> final_independently_tested
  -> final_reviewed
  -> gated
  -> evidence_written
  -> complete
```

A fix after independent testing or review returns to `integrated`, then requires
independent tests and review again against the new candidate SHA.

## Contract repair

The coordinator may dispatch a senior contract editor before implementation only
for narrow inconsistencies that do not choose new product or architecture
behavior, such as:

- owned paths contradicting a frozen required interface;
- a stale stop condition that forbids the already-approved seam;
- an acceptance case naming the wrong public entry point;
- missing explicit branch or evidence wording.

If repair would choose a persistence model, concurrency policy, public behavior,
security boundary, or cross-component architecture, set the task to
`design_required`, record the issue, and stop without production changes.

## Branch topology

Before any edit, print and record:

- current branch;
- base branch and SHA;
- canonical task branch and SHA;
- merge base;
- current worktree path.

`work off main` means create the task branch directly from main; it never means
commit directly to main.

There is one canonical task branch. Agents may use isolated worktrees and return
commits, but only the integration agent moves the canonical task branch.
Historical salvage branches are never inferred as active targets.

## Candidate SHA rule

The exact candidate SHA must satisfy:

```text
independent_tested_sha == reviewed_sha == gated_sha == subject_sha
```

Independent test code must already be integrated before the final reviewer sees
the candidate. Any fix changes the SHA and invalidates prior test/review/gate
claims until rerun.

An evidence-only commit may follow `subject_sha`. If so, every changed path after
`subject_sha` must be owned evidence output, and the manifest records both the
subject SHA and evidence commit SHA.

## Completion

A task is not cycle-complete merely because production code works. Completion
also requires:

- no P0 or P1 findings;
- all required commands pass;
- task packet and epic row both say `complete`;
- every reconciliation regex matches;
- no untracked test or evidence file;
- final report names the exact subject SHA;
- the next task is identified but not started.

## Merge policy

Never merge to the base branch without explicit authorization. When merge is
pre-authorized, finish evidence first, merge second, rerun the base-branch smoke
gate, and report both the reviewed subject SHA and resulting merge SHA. Do not
rewrite historical evidence merely to embed a self-referential merge SHA.
