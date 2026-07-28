# Batch-merging a completed wave

Step 12 of the main cycle already covers merging *one* task's branch with
explicit authorization. This document covers merging *several* completed
task branches from the same wave — a distinct operation with its own failure
modes, because sibling branches that are each individually clean can still
conflict with each other in ways neither branch's own review caught.

This assumes `superpowers:using-git-worktrees`,
`superpowers:finishing-a-development-branch`, and
`superpowers:verification-before-completion` — read those first if you
haven't. This document does not re-explain what they already cover
(isolated-workspace mechanics, the single-branch merge/PR/keep-as-is menu,
"no completion claim without fresh evidence"); it covers what's specific to
merging *several* sibling branches together, which none of those anticipate.

Everything below was derived from actually doing this twice — once as a
disposable trial, once for real, merging into `main` — not from theory. False
starts are described alongside the fix, because the false start is what
makes the rule legible; "why" survives better than "do this."

## 0. Never trust the wave report's pass/fail claims

This is `superpowers:verification-before-completion`'s Iron Law
("no completion claims without fresh verification evidence"), applied to a
wave report specifically: a wave-completion report is prose written by an
agent summarizing many sub-cycles, and every "N tests, gated" line in it is a
claim to verify, not a fact already established. Concretely, before including
a branch in a merge batch:

```sh
git checkout <branch>
# run every command in the task packet's verification_commands, exactly
git checkout <base>
```

This caught a real case: a task reported "33 tests" and `APPROVE_WITH_FOLLOW_UP`
with one disclosed follow-up finding. Rerunning its suite in total isolation
— nothing else merged, nothing else touched — showed 3 of those 33 failing,
none of them the disclosed one. The fixture for its "happy path" test was
missing two of the seven files its own gate requires, so the "complete
evidence passes" test asserted something false. "33 tests" turned out to
mean "33 tests exist in the file," not "33 tests pass." Nobody had rerun the
command between drafting that sentence and reporting it.

A branch whose own verification_commands don't pass in isolation is not a
merge candidate. Route it back through the fix loop (step 8) as its own
task, on its own branch. Do not fix it inline as part of merging.

### Gotchas hit while actually running this loop

None of these are specific to wave-merging — they're generic shell and git
traps — but they cost real time doing this for real, across a dozen
branches, and are worth naming so the next run doesn't rediscover them:

- **A branch's isolation checkout leaves byproducts on disk, and some of
  them are invisible.** A test suite that writes fixtures into the repo tree
  at runtime (rather than a proper temp directory) leaves files behind after
  you check out the next branch. Before wiping what looks like a
  test-generated directory, confirm it actually is one:
  `git ls-files <dir>` — zero output means nothing there is tracked, safe to
  `git clean -fdxq` it; any output means real checked-in fixtures live
  alongside the generated ones, and a blanket clean would delete committed
  work along with the byproducts. The `-x` matters specifically when a
  `.gitignore` rule (a blanket `*.log`, for instance) is hiding files that
  plain `git clean -fd` (no `-x`) will silently skip — confirmed empirically
  more than once in this exact repo, where a blanket `*.log` rule quietly
  swallowed real, deliberately-committed fixture and evidence files in two
  different directories. Don't assume "nothing shown by `git status`" means
  "nothing there" — check `git check-ignore -v <path>` on any file you
  expect to see and don't.
- **Piping a git command through `tail`/`head`/`grep` for display loses its
  exit code.** `if git merge ... | tail -8; then` branches on `tail`'s exit
  status, not `git merge`'s — `tail` always succeeds, so the conditional
  always takes the success branch even when the merge conflicted. Capture
  the command's own exit code in a variable on the line that runs it, before
  piping anything for display.
- **In zsh (the macOS default), `status` is a read-only special variable.**
  `status=$?` fails silently in a way that aborted a merge loop mid-run in
  practice. Use a different name (`mstatus`, `rc`, anything not on zsh's
  reserved list) for exit-code variables in any loop driving this procedure.

## 1. A `qa` or `review` task that found a real defect is not "complete"

If a task's own reconciliation never wrote `complete` to its epic-queue row
— check the row on that branch's own tip, not the report — it is not a merge
candidate, full stop, regardless of how much good work is on the branch. An
adversarial suite that finds a real P0 in production code is not supposed to
reconcile; per its own charter it reports and stops. This is correct
behavior, not a failure to clean up.

Concretely this happened: an adversarial XMP suite found that the ownership
check for foreign sidecars used `contains("tprs:Owner=\"TPRS\"")` — a
substring test, satisfied by a foreign file that merely contains that string
inside an XML comment, a CDATA block, or malformed XML that never becomes a
real attribute. Three of that branch's tests asserted the safe behavior and
failed against the real (pre-existing, already on the base branch) code. The
branch never touched `EPIC.md`. That absence is the signal: don't merge it,
and don't merge any later change that assumes the underlying defect is
fixed. Spin up a new `foundation`- or `implementation`-class task to fix the
actual production code, with its own independent test and review; only then
does the original adversarial branch's suite become something to merge (or
its cases get folded into the fix's own test task).

## 2. Simulate the whole intended order before touching anything real

```sh
python3 scripts/simulate_wave_merge.py \
  --base main --branches <branch1> <branch2> ... [--pairwise]
```

This runs `git merge-tree --write-tree` chained through `git commit-tree` to
walk the exact sequential merge you're about to perform, entirely against
the object database — no branch, ref, or working-tree file is touched. It
tells you, before any real merge, exactly which step conflicts and on which
files. Run it before starting, and rerun it any time the candidate list or
order changes.

Do not skip this because the branches "obviously" don't overlap — see the
next section. The reliable finding is inside a description of a diff, not an
educated guess: two branches touching different production code can still
collide on a shared file entirely outside either one's core work.

## 3. Every wave will produce EPIC.md conflicts — that's routine, not a signal

Every task's reconciliation step edits its own row in the epic's queue
table. Rows for adjacent tasks are close enough in the file that git's
default diff-context window overlaps between them, so merging task N after
task N-1 conflicts on `EPIC.md` almost every time — even though the two
edits touch completely different rows and there is nothing to actually
decide.

This is mechanically resolvable and a script exists for it:

```sh
git merge --no-ff <branch>
# on EPIC.md conflict:
python3 scripts/resolve_epic_queue_conflict.py planning/epics/<epic>/EPIC.md
git add planning/epics/<epic>/EPIC.md
git commit --no-edit
```

It reads git's index stages (`:1:`, `:2:`, `:3:` — base, ours, theirs)
directly, not the marker-laden working-tree file. **This distinction is not
cosmetic.** An earlier version of this exact resolution logic read
`open(path).read()` on the conflicted file and pattern-matched table rows
out of the raw text — which still contains both sides' copies of every
conflicted row, separated by `<<<<<<<`/`=======`/`>>>>>>>` markers that a
naive `line.startswith("| ")` scan doesn't know to skip. It produced a
queue table with every conflicted row duplicated — both statuses present,
neither removed. `epic_queue.py --validate` did not catch it (duplicate
task IDs are exactly the kind of thing it should reject; confirm that
independently before relying on it, since this gap may still be open). Only
running the full workspace test suite and manually inspecting the resulting
table caught it. If you write a variant of this resolution by hand instead
of using the script, read from index stages, not the working tree.

The script refuses (rather than guesses) if the same row was genuinely
edited differently by both sides — that case needs real judgment, not
mechanical resolution.

## 4. "Both sides only appended" conflicts are resolvable the same mechanical way

Two review packets appending independent sections to the end of
`REVIEW_REPORT.md` conflict for the identical reason as EPIC.md — both
diffs anchor at the same point (the end of the file, right after the prior
content) — but the correct resolution actually is "keep both, one after the
other," because neither side removed or altered anything the other side
touched.

```sh
python3 scripts/resolve_append_only_conflict.py <path>
```

This tool checks the precondition it needs before doing anything: both
`ours` and `theirs` must literally have `base` as a string prefix. If either
side changed something inside the shared prefix — not just appended after
it — the tool refuses rather than concatenating something that would
silently drop or duplicate content. Do not loosen this check to a fuzzy
line-diff match; a false "yes, append-only" here is exactly the failure
mode this document exists to prevent.

## 5. A shared file where both sides define the *same key* is not append-only — do not use the tool above

Three CI-gate tasks in one wave each independently added a `gates:` job to
`.github/workflows/ci.yml`, each anchored at the same point in the file.
Textually this looks identical to the REVIEW_REPORT.md case. It is not: a
YAML mapping key must be unique. Concatenating three `gates:` blocks
produces a file with three top-level keys of the same name — most YAML
tooling and GitHub Actions itself will not merge them into one job with
three steps; behavior is either a parse error or two of the three jobs
silently vanishing, and "silently" is the dangerous half of that sentence —
a CI gate whose steps quietly stopped running is worse than no gate,
because it looks green.

This class of conflict needs a real reconciliation, decided once, not
generated per-merge:

```yaml
gates:
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4
      with:
        fetch-depth: 0        # use the strictest requirement any single step needs
    - name: <first task's step name>
      run: <first task's command>
    - name: <second task's step name>
      run: <second task's command>
    # ...
```

There is no mechanical script for this step. Do it by hand or dispatch a
small reconciliation subagent scoped to exactly this file and exactly this
conflict — not a general-purpose fix-anything agent. Before merging,
sanity-check the result isn't accidentally still duplicated:

```sh
python3 -c "
import re
text = open('.github/workflows/ci.yml').read()
print('job-key occurrences:', len(re.findall(r'^  gates:', text, re.MULTILINE)))
"
```

Should print `1`.

## 6. A merge can surface a defect that no single branch's own tests would ever find

After merging six otherwise-clean adversarial-suite branches and two
review branches and two CI-gate branches, running the newly-wired evidence
gate for real (not a fixture, the actual `docs/phases/phase-1/` tree)
turned up a real absolute filesystem path (`/Users/<name>/...`) leaked into
a committed evidence log — produced by one branch's suite, only detectable
by another branch's gate, and only after both were present at once. Neither
branch's own isolated test run could have caught it: one produced the
artifact and had no reason to scan it for leaked paths, the other's tests
run against synthetic fixtures, not the real evidence tree.

A second, different-shaped version of the same lesson: the actual merge (not
a trial) turned up 131 `cargo fmt` diffs and 3 `cargo clippy -D warnings`
errors across the merged test files, with every individual branch's own
`verification_commands` passing throughout. Each branch's own gate was
scoped to its own test file (`cargo test -p tprs-core --test qa_whatever`) —
narrower than the full-workspace `cargo fmt --all --check` /
`cargo clippy --workspace --all-targets` the repository's real gate runs.
No branch was wrong to pass its own narrower check; the full-workspace tools
simply were never run against the *combination* until the merge did it.

Both examples are the same underlying point: **step 10 (full repository
gate) is not optional busywork after a batch merge — rerun it for real,
against the real tree, after every single merge step in the sequence, not
once at the end.** A defect introduced by merge N can hide behind merge
N+1's changes if you only check at the finish, and a per-branch gate that
was individually sufficient can still be narrower than the gate that
actually ships.

## 7. Finishing the batch: cleanup is not optional, and it's not automatic

Once every candidate is merged, the full gate passes, and — separately —
merging the integration branch into the declared base is authorized: that
final merge *is* `superpowers:finishing-a-development-branch`'s "Option 1:
merge locally" for a single branch (the integration branch), and that skill
already covers it — verify tests on the merged result, then clean up.

What it doesn't cover, because it's designed for one branch: **every source
branch that went into the wave also needs cleanup**, not just the
integration branch. In practice this step was skipped — the batch merge
landed, and cleanup only happened because it was asked for as a separate,
later request. Don't wait to be asked:

```sh
# after the integration branch is merged into base and pushed:
git branch --merged <base> | grep -E '<source-branch-pattern>'   # confirms each is a real ancestor before deleting
git branch -d <branch1> <branch2> ...   # -d refuses anything not actually merged — never -D here
git push origin --delete <branch1> <branch2> ...   # only for branches that were actually pushed; check first
```

`git branch --merged` is the safety check, not a formality — it only lists
branches whose tip is an ancestor of the target, so a `-d` (not `-D`) delete
right after it can't lose real, un-integrated work. Branches genuinely
outside this wave's scope (something that predates it, an unrelated
exploratory branch with commits not yet in the target) are not this
cleanup's business even if they're sitting in the same `git branch -a`
listing — report them as a separate, distinct observation rather than
folding them into the wave's cleanup silently.

## 8. When the coordinator does the merging directly instead of dispatching separate roles

The rest of this document, and the skill it belongs to, assume the roles
that touch a wave stay in separate contexts — an implementer doesn't review
its own work, an independent tester doesn't inherit the implementer's
reasoning. Sometimes a human explicitly authorizes the coordinator to do
batch-merge work directly instead of dispatching it out, for speed. That's a
legitimate choice, not a shortcut to hide.

It has a real cost, though: any fix the coordinator makes *while* doing the
merge — backfilling a branch's missing fixtures, redacting a leaked path,
running `cargo fmt`/`clippy` to close the gate — was written by the same
agent that is also verifying it, with no independent test or review pass.
That's exactly the correlated-error risk role separation exists to prevent,
accepted deliberately in exchange for speed. State it plainly in the final
report: which specific fixes were made by the coordinator itself, in-line,
with no independent check, versus which came from genuinely separate
branches or roles. A human reading the report needs to know which parts got
real independent scrutiny and which parts are "the same agent checked its
own work" before deciding how much to trust either.

## 9. Resuming an interrupted batch

If a batch merge stops partway — context runs out, the session ends, a
later run picks this up — the integration branch's own commit history is
the ledger; trust it over any prose summary or recollection, the same
principle `superpowers:subagent-driven-development` applies to its own
progress ledger. `git log --oneline <base>..<integration-branch>` shows
exactly which source branches are already merged and in what order. Don't
restart the batch from the beginning, and don't trust a stale mental list of
"what's done" — re-derive it from the branch itself before continuing.

## 10. Order of operations, summarized

1. For every candidate branch: confirm its epic-queue row is `complete` on
   its own tip (not "ready," not "blocked" — a `qa`/`review` task that
   stopped on a real finding stays out of the batch).
2. For every candidate branch: rerun its own `verification_commands` in
   isolation (§0), cleaning up test-run byproducts correctly between
   checkouts (§0's gotchas). Anything that fails comes out of the batch and
   goes back through the fix loop as its own task.
3. Run `simulate_wave_merge.py` against the surviving candidates in your
   intended order. Note every step it reports as conflicting.
4. Classify each reported conflict: routine queue-table row edit (§3),
   pure-append (§4), or same-key/semantic (§5). Do not assume; check.
5. Merge one branch at a time for real. After each merge: resolve any
   conflict per its classification, then rerun the full repository gate
   (step 10 of the main cycle) against the real resulting tree before
   merging the next branch — narrower per-branch gates can hide things the
   full gate catches (§6).
6. Only after every candidate is merged and the full gate passes on the
   final tree: report the batch, including anything excluded and why, and
   wait for explicit merge-to-base authorization per step 12. Batch-merging
   into an integration branch and merging that branch to the declared base
   are two different authorizations; do not conflate them.
7. Once the base merge is authorized and done: clean up every source branch
   in the wave, not just the integration branch (§7). This is part of
   finishing the batch, not a follow-up to wait for.
8. If any step above was done by the coordinator directly rather than a
   dispatched role, say so explicitly in the final report (§8).
