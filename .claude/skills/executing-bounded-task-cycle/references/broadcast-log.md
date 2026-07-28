# The epic broadcast log

Every agent working on one epic — implementer, independent tester, reviewer,
integrator, contract editor, coordinator, and any sub-orchestrator dispatched
for a wave — shares one append-only log for that epic. It exists for exactly
one purpose: a fact discovered mid-task that changes what a *different*,
isolated agent should assume, and that agent has no other way to find out
before running headfirst into it.

It is not a status channel and not a findings channel. Those already exist —
`cycle.json`/`EPIC.md` for status, `REVIEW_REPORT.md`/a task's findings for
severity-rated defects. If something belongs in one of those, put it there.
The broadcast log is for the narrower, more urgent case where waiting for
those to be written and read at their normal point in the cycle would waste
another agent's time or cause a real collision.

## Where it lives and why

```sh
python3 scripts/epic_broadcast.py path --epic <epic-id>
```

Defaults to `~/.claude/bounded-task-workflow/epic-logs/<repo-identity>/<epic-id>.jsonl`.
`<repo-identity>` is derived from `git rev-parse --git-common-dir`, not
`--show-toplevel` — the common dir is identical across every worktree of one
repo, while `--show-toplevel` returns a different path per worktree. Verify
this yourself if it matters to you (`git worktree add` a throwaway worktree,
run both commands from inside it) rather than trusting this document; it was
checked empirically before being written down here, and it's the kind of
assumption that's easy to get backwards.

Living outside the repo, under the user's home directory, means every
isolated worktree — which is a genuinely separate directory on disk — can
still reach the same file, and the log is never accidentally staged,
committed, or subject to the path-ownership rules that govern the actual
patch.

The log is cumulative for the epic's entire lifetime. It is not reset per
task, per wave, or per orchestrator invocation — only a new epic ID starts a
new file. It is not archived or committed automatically; the coordinator's
end-of-epic brief (below) is the mechanism for pulling anything durable out
of it before the epic — and interest in its log — closes out.

## What belongs in an entry, and what doesn't

Belongs:

- **A defect found outside your owned paths that you are not fixing.** An
  adversarial test discovers a real bug in production code it doesn't own —
  broadcast it immediately, don't wait for your own task's review to surface
  it days later. (Concrete case this was built for: an XMP adversarial suite
  found a substring-based ownership check that would let a crafted file
  overwrite a foreign sidecar. That's a fact every sibling task touching
  `xmp.rs`, and the coordinator, needed immediately — not after that task's
  own cycle finished.)
- **You are about to touch a shared file that other tasks in this wave also
  own.** Say so before you do it, not after. (Concrete case: three CI-gate
  tasks each independently added a `gates:` job to `ci.yml` under the same
  key — a real collision that a one-line broadcast from any of the three
  would have flagged before the other two wasted the same motion.)
- **An environment or toolchain surprise unrelated to your task's logic.** A
  missing binary, an unexpected git state, a fixture that reveals something
  true about the repo that isn't specific to what you're implementing.
- **A frozen decision that turned out to be ambiguous or wrong** in a way
  that affects more than your task. Still escalate through the packet's own
  stop conditions for your task; also broadcast it so a sibling task doesn't
  independently hit the same ambiguity and resolve it differently.

Does not belong — and putting it here would defeat the point of role
separation, not just clutter the log:

- **Your implementation rationale.** An independent tester must not inherit
  why the implementer built something a particular way — that's exactly the
  correlated-reasoning failure mode role separation exists to prevent. "I
  chose X because Y" is not a broadcast entry.
- **Confidence claims.** "This should be fine" or "I'm pretty sure this
  covers it" is not a fact about shared state; it's exactly the kind of
  self-assessment a reviewer or tester must form independently, not inherit.
- **Routine progress.** "Started task," "still working," "tests passing so
  far" — that's what the cycle's own status fields are for. If every agent
  narrates its progress here, the signal-to-noise ratio collapses and nobody
  reads it, which defeats an urgent finding posted an hour later.
- **Anything that would let a reviewer infer what the implementer did before
  reading the diff.** If in doubt, ask: would reading this before forming my
  own approach make my approach more correlated with the implementer's, or
  strictly better-informed about a fact neither of us controls? Only the
  second is a broadcast entry.

A useful test: an independent tester who reads the whole log before starting
should come away knowing "watch out for X" or "Y is shared state you should
check," never "here's how the implementation works, so testing is easier."

## Reading it

```sh
python3 scripts/epic_broadcast.py tail --epic <epic-id> [--since <ISO8601>] [--count N]
```

Every dispatched agent — primary, independent tester, reviewer — should tail
the log twice: once at the start of its work (skim recent entries, especially
anything naming a path it's about to touch), and once immediately before its
final gate or reconciliation step (a sibling task may have posted something
relevant while this one was in flight). Neither read should be treated as
optional busywork; the whole value of the mechanism is in the second read
catching something the first one couldn't have.

## Writing to it

```sh
python3 scripts/epic_broadcast.py append \
  --epic <epic-id> --task <your-task-id-or-coordinator> --role <your-role> \
  --tag <short-free-text-category> --message "<one or two sentences>" \
  [--paths <repo-relative-path> ...]
```

`--tag` is free text, not a closed enum — but keep it short and reusable
(`shared-file-collision`, `defect-outside-scope`, `environment-surprise`,
`contract-ambiguity` are the ones seen so far). Keep `--message` to what
someone skimming a tail needs to act on; the full story belongs in the
finding, the packet, or the report, not here. The tool enforces a 4096-byte
budget per entry for exactly this reason, and because that is also the
practical limit past which POSIX append atomicity for concurrent writers
stops being guaranteed — this was verified directly (forty processes
appending simultaneously, checked for zero corrupted or interleaved lines)
rather than assumed, and the same margin is why the limit exists.
