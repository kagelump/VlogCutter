# D01-T018: Antarctica footage chronology acceptance

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T018",
  "epic_id": "D01",
  "title": "Antarctica footage chronology acceptance",
  "slug": "antarctica-chronology-acceptance",
  "status": "blocked",
  "task_class": "qa",
  "depends_on": ["D01-T016", "D01-T017"],
  "principal_invariant": "The real Antarctica trip footage can be brought to a chronology the owner judges correct, using only the shipped calibration workflow.",
  "central_falsification": "Attempt the full correction on the real footage with no database edits and no CLI mutation; any clip the owner cannot place correctly through the shipped UI falsifies the workflow.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["PRD.md", "AGENTS.md", "docs/tasks/desktop/CHRONOLOGY_MODEL.md"],
    "independent_tester": ["PRD.md", "AGENTS.md", "docs/tasks/desktop/CHRONOLOGY_MODEL.md"],
    "reviewer": ["PRD.md", "AGENTS.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "integrator": ["docs/tasks/desktop/EPIC.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T018-antarctica-chronology-acceptance.md"],
    "implementer": [],
    "independent_tester": ["docs/tasks/desktop/ACCEPTANCE_CHRONOLOGY.md"],
    "integrator": ["docs/task-cycles/D01-T018/**"]
  },
  "forbidden_paths": ["core/**", "apps/**", "packages/**", "scripts/**", ".github/**", "PRD.md", "AGENTS.md"],
  "verification_commands": [
    "pnpm --dir apps/desktop tauri build",
    "cargo test --workspace"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T018\\].*\\| complete \\|"
    },
    {
      "path": "docs/tasks/desktop/ACCEPTANCE_CHRONOLOGY.md",
      "regex": "(?s)## Verdict.*(accepted|rejected)"
    }
  ]
}
-->

## Current implementation checkpoint

The automated corpus audit is complete and recorded in
`ACCEPTANCE_CHRONOLOGY.md`: 621 clips, three sequences, no unplaced clips or
contradictions, stable ordering, and byte-stable OTIO. The owner has not judged
the chronology, entered trip phases, or made required corrections through the
desktop. This gate is blocked until D01-T016 and D01-T017 are complete; the
manual verdict then remains the only work in this packet.

## Goal

Prove the chronology workflow on the footage it was designed for: a real trip with
multiple cameras, multiple cards, several timezones, and at least one date-line
crossing.

This is the Priority 1 release gate. Every automated test in this epic runs on
fixtures somebody in this repository wrote, which means they encode this
repository's assumptions about what footage looks like. Real footage is the only
evidence that those assumptions were right.

## Principal invariant

This task is correct when the real Antarctica trip footage can be brought to a
chronology the owner judges correct, using only the shipped calibration workflow.

## Footage path protocol

This packet is `ready`. The product owner has confirmed the corpus exists and is
mounted at gate time.

**The footage path is never recorded in this repository.** It is supplied by the
product owner at invocation, held in the executing agent's environment as
`$FOOTAGE_ROOT`, and referred to everywhere in this packet as `<footage>`. No
commit, no evidence file, no cycle report, and no acceptance document may contain
the literal path. `docs/tasks/desktop/ACCEPTANCE_CHRONOLOGY.md` describes the
corpus — clip counts, cameras, cards, date range, timezones crossed — without
naming where it lives.

The corpus is a three-camera Antarctica trip: a DJI Pocket 3, an Insta360 Ultra
Go, and a Sony A7C, in per-camera folders. It is the same corpus D01-T005's smoke
run drew from, and the same one whose filename shapes D01-T028's pattern table
was built against.

Do not substitute the large real corpus at `$RCDVLOG_ACCEPTANCE_MEDIA_ROOT`
for this gate. That fixture is valuable for scale acceptance, but the product owner
named the Antarctica pattern specifically because of its multi-timezone and
date-line structure, and a single-region shoot cannot exercise trip phases,
transit phases, or dual display.

## Inputs and outputs

Inputs:

- The Antarctica footage path, supplied by the product owner at gate time as
  `$FOOTAGE_ROOT` and never committed. See `## Footage path protocol` above.
- The owner's own knowledge of the trip: roughly where they were when, which camera
  clocks were wrong, and which clips they know the order of. This knowledge is the
  oracle; no automated check can supply it.
- A release build of the desktop app.

Outputs:

- `docs/tasks/desktop/ACCEPTANCE_CHRONOLOGY.md` containing: the corpus description
  (clip count, cameras, cards, date range, timezones crossed); the phases created;
  the corrections applied with their scopes; the clips that needed exceptions and
  why; every contradiction encountered and how it was resolved; a defect list; and a
  `## Verdict` section stating `accepted` or `rejected` with reasons.

## Frozen decisions

- **Real footage only.** Synthetic corpora are not evidence for this gate.
- **No database edits, no hand-edited JSON, no CLI mutation.** `PRD.md` §21 defines
  MVP success as completing a real project without them. Reaching for one is the
  failure this gate exists to detect, and must be recorded as a defect rather than
  worked around.
- **Read-only on originals.** Enumerate read-only first; macOS permission prompts
  may need the owner to click Allow and retry. The project catalog lives under the
  laptop-local projects root, never in the source tree, per `AGENTS.md` §13.
- **The footage is source media, not a test directory.** Nothing is written into it.
  No generated file, no catalog, no cache.
- **Start small.** Smoke one card before any all-media batch, and preserve partial
  successful work if a batch fails.
- **The owner is the oracle.** Where the owner cannot say what the correct order is,
  record that as unresolved rather than accepting whatever the solver produced.

## Owned paths

No production path is owned. This is an acceptance packet: it records what happened
and does not fix it. Defects found go to a follow-up packet the contract editor
creates.

## Required interface

The acceptance document's `## Verdict` section is the machine-checked output:

```md
## Verdict

Result: accepted | rejected

- Clips whose final placement the owner judges correct: N of M
- Corrections required: N epochs, N exceptions, N phases, N constraints
- Contradictions encountered: N hard, N soft
- Database edits or CLI mutations required: N  (must be 0 to accept)
- Defects: AD-1 … AD-n
```

Accepting with a nonzero database-edit count is not available. That count being
zero is what `PRD.md` §21 means by MVP success.

## Acceptance oracle

1. The footage ingests with an asset count the owner confirms matches the trip.
2. Capture-sequence proposals separate the real cards, including any two cards that
   reuse counters. The owner confirms the split matches physical reality.
3. Trip phases can be created covering the whole trip, including at least one
   transit phase and the date-line crossing, and local times display as the owner
   expects for each region.
4. Every camera whose clock was wrong can be corrected with bounded epochs, and the
   correction affects only the clips the owner intended.
5. Clips that genuinely break their sequence can be placed with exceptions.
6. Where the owner knows two clips' relative order, before/after placement records
   it, and any contradiction it creates is reported understandably.
7. The final total order is one the owner judges correct, clip by clip for a sample
   they choose, and they can explain any clip's time through the explain surface.
8. The review queue never loses the owner's place across any correction.
9. Desktop and CLI agree on the final order for the real catalog.
10. No original media file is created, modified, or deleted, verified by comparing
    the source file list, sizes, and modification times before and after.
11. Zero database edits and zero CLI mutations were needed.
12. Performance is acceptable on the real corpus: calibration renders and impact
    previews stay responsive at the trip's real asset count.

## Independent test charter

Falsify the claim that the workflow was sufficient.

- Re-derive the corpus facts independently: count media files by extension under the
  footage path with `find`, and compare against the app's reported asset count.
  A silent skip during ingest is a realistic and serious failure.
- Verify original immutability with a before-and-after file list including sizes and
  modification times, not by trusting the app.
- Independently re-run the final order comparison through the CLI and diff against
  the desktop's order.
- Re-run at least three of the owner's corrections from a fresh project on the same
  footage, and confirm the same result. A correction that only works once is a
  defect.
- Attack the areas real footage stresses that fixtures do not: clips whose metadata
  timezone is present and wrong; clips with no timestamp at all; two cameras
  recording the same moment; a card whose counter wrapped; files a previous tool
  re-encoded; unusually long or short clips.
- Confirm the recorded defect count is not understated by walking the session
  narrative for moments where the owner worked around something without filing it.
- Confirm no catalog, cache, or export was written under the footage path.

## Review charter

Risk domain: accepting a workflow that only works on data this repository invented.

Questions:

1. Is the corpus genuinely multi-timezone with a date-line crossing, or was a
   simpler shoot substituted?
2. Was the asset count independently verified against the filesystem?
3. Were any database edits, JSON edits, or CLI mutations used, and were they recorded
   honestly rather than treated as setup?
4. Did the owner actually judge the final order, or was solver output assumed
   correct?
5. Were originals provably untouched?
6. Is every workaround recorded as a defect?
7. Does the verdict follow from the evidence, or is a rejection being softened?

Severity rules: substituted corpus, unverified asset count, an unrecorded database
edit, any modification to originals, or a verdict unsupported by the evidence is
blocking. Missing performance numbers are non-blocking.

Smallest acceptable remedy: re-run the specific check and record the honest result.
A rejection is a valid and useful outcome of this packet.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Fixing any defect found. The contract editor creates a follow-up packet.
- Scale acceptance over thousands of assets, which is separate periodic manual
  acceptance per `AGENTS.md` §13.
- Edit plan, preview render, or export, which D01-T024 covers.
- Server pairing or sync.
- Improving the footage's metadata or re-encoding anything.

## Stop conditions

Stop and escalate when:

- the footage path has not been supplied at invocation, or the corpus is not
  mounted, in which case stop and ask rather than substituting a fixture;
- the literal footage path would have to appear in any committed file to make the
  evidence reproducible — record the corpus description instead and say so;
- the volume cannot be mounted or macOS denies access and the prompt does not
  appear;
- ingest would write into the footage tree — stop immediately, since that breaches
  the repository's core invariant;
- a correction the owner needs cannot be expressed at all, which is a design gap
  rather than a defect and needs a new foundation packet;
- the app crashes in a way that loses committed work;
- the owner cannot determine the correct order for a substantial part of the trip,
  making the oracle unavailable for those clips — record them as unresolved and
  continue with the rest.

## Commands

```sh
pnpm --dir apps/desktop tauri build
cargo test --workspace
```

Corpus verification and immutability, with `<footage>` replaced by the supplied path:

```sh
find "<footage>" -type f \( -iname "*.mp4" -o -iname "*.mov" -o -iname "*.mts" -o -iname "*.insv" \) | wc -l
find "<footage>" -type f -exec stat -f "%N %z %m" {} \; | sort > /tmp/antarctica-before.txt
find "<footage>" -type f -exec stat -f "%N %z %m" {} \; | sort > /tmp/antarctica-after.txt
diff /tmp/antarctica-before.txt /tmp/antarctica-after.txt && echo "originals untouched"
```

Expected result: the file count matches the app's asset count, and the `diff`
reports no differences.

## Handoff

Report the corpus description, the phases and corrections applied, the clips needing
exceptions with reasons, every contradiction and its resolution, the desktop/CLI
order comparison, the immutability evidence, the database-edit count, the
performance observations, the `AD-n` defect list, and the verdict with its reasons.
State explicitly whether any correction the owner wanted could not be expressed.
List any stop condition encountered. The tester does not approve this task and does
not fix what it finds.
