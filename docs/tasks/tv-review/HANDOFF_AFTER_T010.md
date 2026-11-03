# V01 handoff after T010 (2026-11-03)

**State:** T001–T009 are implemented. **T010 hit its stop condition:** the v2
positive run matched 0 windows. T011 has not started. Five decisions
below need the owner before work continues. Nothing that shipped changes v1
behaviour: every V01 feature is off unless explicitly set, and the v1 replay
of the real RC6 run is bit-identical.

Read with [EPIC.md](EPIC.md) and [ARCHITECTURE.md](ARCHITECTURE.md). The
architecture's revision notes in §6 and §7 record the owner-cycle decisions.

## 1. Branches and PRs

| Task | Branch | PR | Base | State |
|---|---|---|---|---|
| T001–T004, T007, T008 | (merged) | #248–#253 | main | merged |
| T005 speech mask | `claude/v01-t005-speech-mask` | #254 | main | CI green; **merge blocked** (see §5) |
| T006 gap windows | `claude/v01-t006-gap-windows` | #255 | T005 branch | local verification green; no CI yet |
| T009 recipe v2 | `claude/v01-t009-recipe-v2` | #256 | T006 branch | local verification green; no CI yet |
| Handoff (this file) | `claude/v01-t010-handoff` | draft | T009 branch | docs only |

**Merge order:** #254 → #255 → #256. CI runs only on PRs whose base is
`main`, so retarget each stacked PR after its parent merges and wait for green.

**Local verification on the handoff head** (`ef4ad22`, which includes #256;
pinned toolchain 1.97.1). Log: `../data/build-v01/verify-ef4ad22.log`.

- `cargo test -p rcdvlog-core --lib tv_`: 73 pass.
- All nine `tests/v01_*_independent.rs` suites pass.
- `cargo test -p rcdvlog-cli` passes.
- clippy `-D warnings` and fmt are clean.

## 2. Decisions taken in this cycle (recorded in ARCHITECTURE.md)

- **T005, effective mask weights (§6).** Silencing masked samples made the
  frames next to each mask span score a jump from silence as a full-scale
  onset; the independent 60% oracle failed with its true peak at 0.67. Any
  frame whose FFT window or 8-frame delta history reads a silenced sample is
  now weighted zero, and the sufficiency rule counts those weights. The
  fragmented 6 s burst scenario (0.4 s gaps) is now asserted
  `masked_insufficient`. The 12 s 60% scenario still matches.
- **T006 refinements (§6).**
  - Gap queries are capped at `min(20 s, reference − 1 s, movie)`.
  - References overlap by the longest *planned* query, so plans without a
    long gap query keep v1's reference windows.
  - Grid-identical gap queries are dropped before the 4096-query quota.
  - Receipts are checked against each query's own length.
  - **Fix found by T010 (`097d697`, on #255).** Gap query edges now snap to
    pairs of 8 kHz samples. On the real 48 kHz recording, an unaligned gap
    query failed the decoded-length check, and an odd 8 kHz length would
    put the anchor centre 3 native samples off the query centre. A 48 kHz
    end-to-end regression test now covers this.
  - The T005 oracle harness now scopes to the grid query and runs batches to
    completion. Its claims are unchanged.
- **T009 refinements (§7).**
  - Energy is taken over the same 16 bands, with nearest-rank percentiles.
  - The scoring weight is the minimum over the delta's 9 frames.
  - v1 is always spelled `recipe: None`.
  - The v1 digest input is unchanged; the v2 digest binds `frameEnergy`.

## 3. T010 results (real RC6 room recording)

Evidence is in `../data/editorial-review/rc6-passive-sync-v2/`, with
`run.py`, `summarize.py`, `summary.json`, per-scenario folders and `run.log`.
The CLI is pinned at `../data/build-v01/rcdvlog`, built from `515b20b`.
Provider calls: zero.

| Scenario (§10) | Result | Verdict |
|---|---|---|
| 1. v1 replay | Plan digest `555a42a5…` identical to the 2026-11-03 run; 31/31 unmatched; every best similarity identical (max diff 0) | pass |
| 2. v2 positive (v2 + distinctive 0.5/0.25/6 + transcript mask + consensus) | 0 matched, 2 ambiguous, 31 unmatched; 0 segments; 0/27 utterances mapped | **fail: stop condition** |
| 3a. Movie window 1000–1240 s only | 0 matches, 0 segments | pass (weak: see below) |
| 3b. Mic time-reversed | 0 matches, 0 segments | pass (weak) |
| 3c. RC7 as a different movie | Pre-check: RC6's clean 0–240 s audio matches RC7 in 3 segments (offsets 0, +53, +83 s), so **RC7 shares content and is not a valid control**. On the mic: 0 matches, 1 ambiguous | **invalid control** |
| 4. Seek splice (mic 0–90 s + 120–180 s) | First run crashed on the T006 alignment bug (preserved as `seek-failed-unaligned-gap-2026-11-03/`); re-run queued with the fixed CLI (`../data/build-v01/rcdvlog-fix`, `ef4ad22`) via `queue-seek.sh` → `seek.log` | pending |
| Ablation: v1 features + distinctive, no mask | **4 matched, all at −0.96 s** (1:18, 1:42, 1:48, 2:30); 0:06 and 0:12 ambiguous at −0.96 s (nearby runner-ups 0.485 and 0.434, margins 0.13 and 0.19 < 0.25); **0 consensus segments**; 1/27 utterances estimated | diagnostic: confirms Fix 2, exposes D5 |
| Ablation: v2, no mask | 0 matched, 7 ambiguous | diagnostic: v2 regresses on its own |

Only the seek re-run remains. When `seek.log` ends with `exit 0`, re-run
`python3 summarize.py`. Note that the seek control uses the v2 recipe and the
transcript mask, so it inherits both part-2 problems. Expect it to say little
until D1 and D2 are settled.

**3c pre-check purpose.** §10 requires proving that the other movie shares
none of RC6's first 4 minutes. `precheck_rc7` uses RC6's own clean movie audio
(0–240 s) as the "mic" against RC7. Any match there is real shared content,
so 3c must be read with those windows excluded.

### Why part 2 failed (two independent causes, neither a threshold)

1. **The speech mask came from the wrong source.** The mask was built from the
   Whisper transcript's utterance spans, padded ±0.25 s. Those spans tile the
   timeline from 16.5 s to 174 s, although detected speech is only 90.2 s.
   Every grid window from 0:12 to 2:54 became `masked_insufficient`, including
   the clean pauses where v1 had found the true offset.
2. **Recipe v2's energy weighting is degenerate on program audio.** Reference
   weights differ at every candidate offset. Where the movie is near-silent,
   almost no frames count, and a cosine over a few frames scores high by
   chance. In the 0:06–0:12 "woohoo" window, v1's best peak was the true offset
   (−0.96 s, 0.615). v2's best was at **+1902 s, a wrong position** (0.66);
   v2 is also wrong (+1733 s) in the 2:54 windows. The numpy replica
   (`v2_weight_probe.py`) shows the wrong-region peak at 0% effective
   support, and only 30% support even at the true position.
   - **Safety held:** those windows stayed ambiguous or unmatched, and no
     wrong anchor was emitted.

3. **Consensus never forms on real audio (T007, merged).** Each query votes
   only with its single best peak, and a run breaks whenever that peak falls
   in a different cluster. Speech-covered windows have a random best peak
   (≈0.28) at a random offset, so they split every run. In the v1+distinctive
   ablation, all 31 queries were "unsupported", despite 8 windows whose best
   peak is exactly −0.96 s and 4 Matched there. §8 step 4 intended only a
   window *Matched* at another offset to split a segment.

**Caveat on 3a/3b.** Because of cause 1, most control windows were never
actually scored. These passes say little about false positives; the
ablations are the meaningful check.

## 4. Decisions needed from the owner

- **D1. Speech-mask source.** Proposed: use voice-activity regions (the
  Silero speech regions the review-transcription pipeline already computes),
  not ASR utterance spans. Open question: are those regions persisted per
  session, or must the transcription job export them? This changes how
  callers build `speechMask`; the core API is unchanged.
- **D2. v2 weighting.** Options:
  - (a) Require a minimum effective support (e.g. ≥ 50% of the query's
    weighted frames at that offset) before a score counts.
  - (b) Use reference energy only as a hard gate with a fixed support floor.
  - (c) Drop energy weighting and keep only the robust median/±3 features.

  Decide using the "v2, no mask" ablation plus a fresh run of each option.
  Any change needs a new recipe string, because v2 caches already exist on
  disk.
- **D3. Ship order.** #254–#256 are opt-in and v1-safe, so merging them does
  not depend on D1 or D2. The alternative is to hold #256 until D2 is
  settled. Recommended: merge #254 and #255, and hold #256 until D2.
- **D5. Consensus run rule (T007).** Recommended: a run is broken only by a
  query that is Matched (or has a strong distinctive peak) at a different
  offset. Weak or unmatched queries neither support nor break it, and
  ambiguous queries whose best peak is in-cluster count as support. Keep the
  "≥ 3 supporting, ≥ 2 Matched" floor. On the ablation, this would join
  1:18–2:30 (4 Matched + 2 in-cluster ambiguous). Do not decide it from this
  recording alone: add synthetic seek/pause oracles first.
- **D4. Distinctive acceptance alone.** Should the first deliverable be
  "v1 features + distinctive + consensus", if the ablation confirms the
  2026-11-03 diagnosis (≈6 windows at −0.96 s)? That needs no D1/D2 work.
  **Update:** the ablation found 4 such windows (not 6), all correct, with no
  false anchor. It becomes useful once D5 lets them form a segment.

The §10 protocol forbids tuning thresholds to pass, so none was changed.
Ablations change only which features are enabled, and they must be reported
as diagnostics, not as an acceptance pass.

## 5. Environment and gotchas

- **Worktree.** All V01 work is in `/home/ry/project/vlogcutter-v01`. The main
  checkout `/home/ry/project/vlogcutter` has another agent's staged
  `AGENTS.md` change (Gemini guidance) that does not belong to V01; it was
  left untouched.
- **Toolchain.** `source ../data/build-v01/env.sh` (relative to this repo)
  sets the local `CARGO_HOME`/`RUSTUP_HOME` (1.97.1), the shared
  `CARGO_TARGET_DIR`, and the `native-root` pkg-config settings. Without
  them, `rcdvlog-cli` fails to build in `openssl-sys`.
- **Merging.** `gh pr merge` was denied by the auto-mode permission
  classifier ("merge without review"). A human must merge, or add a
  permission rule.
- **Machine load.** Other agents run heavy `cargo test` jobs on this host
  concurrently, so evidence runs take about 450 s per full-movie scenario.
- **Data.** Private caches under `rc6-passive-sync-v2/*/private-cache` hold
  derived soundtrack features. Keep them local, as with all review evidence.

## 6. Next steps

1. Let the seek re-run finish, re-run `summarize.py`, fill row 4 above, and
   write the evidence `REPORT.md`. Pick a genuinely unrelated movie for 3c
   (verify it with the same clean-audio pre-check).
2. Owner decides D1–D5. Suggested order: D5 (small, unlocks consensus with
   the already-correct v1+distinctive anchors), then D1, then D2.
3. Implement the chosen D1/D2 as a new packet (new recipe string for any
   weighting change), with the RC6 recording as its oracle plus the §10
   negative controls.
4. Re-run T010 in full, then T011: update `tv-soundtrack-matching.md`,
   `tv-review-local-session.md`, `tv-review-sync.md`, `agent-cli.md`
   (consensus command and `--consensus`), and the AGENTS.md §3 line. Mark
   the epic complete only after §10 passes.
