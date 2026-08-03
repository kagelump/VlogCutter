# D01-T024: DaVinci Resolve 20/21 import acceptance

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T024",
  "epic_id": "D01",
  "title": "DaVinci Resolve 20/21 import acceptance",
  "slug": "resolve-import-acceptance",
  "status": "blocked",
  "task_class": "qa",
  "depends_on": ["D01-T022", "D01-T023"],
  "principal_invariant": "The exported OTIO timeline opens in DaVinci Resolve 20 or 21 with source references, in and out points, clip order, and clip names intact.",
  "central_falsification": "Import the exported timeline into the installed Resolve and compare every clip's resolved source file, timecode in and out, timeline position, and name against the edit plan read independently through the CLI.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["PRD.md", "AGENTS.md", "docs/tasks/desktop/EDIT_PLAN_MODEL.md"],
    "independent_tester": ["PRD.md", "AGENTS.md", "docs/tasks/desktop/EDIT_PLAN_MODEL.md"],
    "reviewer": ["PRD.md", "AGENTS.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "integrator": ["docs/tasks/desktop/EPIC.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T024-resolve-import-acceptance.md"],
    "implementer": [],
    "independent_tester": ["docs/tasks/desktop/ACCEPTANCE_RESOLVE.md", "docs/desktop/edit-plan-and-export.md"],
    "integrator": ["docs/task-cycles/D01-T024/**"]
  },
  "forbidden_paths": ["core/**", "apps/**", "packages/**", "scripts/**", ".github/**", "PRD.md", "AGENTS.md"],
  "verification_commands": [
    "pnpm --dir apps/desktop tauri build",
    "cargo test -p rcdvlog-core --test otio_golden"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T024\\].*\\| complete \\|"
    },
    {
      "path": "docs/tasks/desktop/ACCEPTANCE_RESOLVE.md",
      "regex": "(?s)## Verdict.*(accepted|rejected)"
    }
  ]
}
-->

## Current implementation checkpoint

No Resolve 20/21 import has been run. This manual gate remains blocked on the
OTIO golden/cross-process evidence in D01-T022 and the complete export workflow
in D01-T023. When unblocked, use an export produced through the shipped desktop
surface and record exact source, order, and range comparisons.

## Goal

Prove the handoff actually works: import the exported OTIO into the installed DaVinci
Resolve and verify the timeline is the one the user planned.

This gate is required, not optional. D01-T022 writes OTIO in pure Rust rather than
through the reference Python implementation, which means this repository owns schema
fidelity. Golden-file tests prove the writer is consistent with itself; only Resolve
proves it is consistent with the format.

## Principal invariant

This task is correct when the exported OTIO timeline opens in DaVinci Resolve 20 or 21
with source references, in and out points, clip order, and clip names intact.

## Inputs and outputs

Inputs:

- DaVinci Resolve 20 or 21, installed and usable on the target Mac, confirmed by the
  product owner on 2026-07-29.
- An exported `.otio` from a real plan built through the shipped UI, over real camera
  media — not a synthetic fixture, since Resolve's conform behavior depends on real
  codecs, real frame rates, and real file structure.
- The plan read independently through `rcdvlog` CLI output, as the comparison oracle.

Outputs:

- `docs/tasks/desktop/ACCEPTANCE_RESOLVE.md`: the Resolve version and build, the plan
  used, a per-clip comparison table, any adapter or conversion step required, a defect
  list, and a `## Verdict` of `accepted` or `rejected`.
- `docs/desktop/edit-plan-and-export.md`: the user-facing handoff procedure, including
  any conversion step the acceptance revealed.

## Frozen decisions

- Real Resolve, real media. A Resolve-compatible-looking JSON, an OTIO validator, or a
  third-party viewer is not evidence for this gate.
- Real camera media across the codecs `AGENTS.md` §14 names — Sony, DJI, iPhone,
  Insta360, H.264 and H.265 — not synthetic MPEG-4 fixtures. Conform behavior differs by
  codec and that is exactly what this gate tests.
- The comparison oracle is the CLI's view of the plan, not the exporter's own output.
  Comparing the export against itself proves nothing.
- If an adapter or conversion step turns out to be required, it is documented as part of
  the handoff rather than hidden. `PRD.md` §23 Milestone 6 explicitly allows for one, so
  needing it is not automatically a failure — leaving it undocumented is.
- Nothing is written into the source media tree, and Resolve's own media management must
  not be allowed to relocate or transcode originals during the test.
- A rejection is a valid outcome. It sends the schema back to D01-T022 with specifics.

## Owned paths

No production path is owned. The tester records what Resolve does; fixing the exporter is
a follow-up packet the contract editor creates from the defect list.

## Required interface

```md
## Verdict

Result: accepted | rejected

Resolve version: <exact version and build>
Plan: <revision id>, N clips, cameras and codecs present
Adapter or conversion required: none | <exact step>

| Clip | Source resolved | In matches | Out matches | Position matches | Name matches |
| ---: | --- | --- | --- | --- | --- |

- Clips fully correct: N of M
- Defects: RI-1 … RI-n
```

Accepting requires N equal to M. A partially correct timeline is a rejection, because a
finishing editor cannot use a timeline where some clips are silently wrong.

## Acceptance oracle

1. Resolve imports the `.otio` without error, and its version and build are recorded.
2. The imported timeline has exactly one video track and the plan's clip count.
3. Every clip resolves to the correct original media file, by full path.
4. Every clip's in point matches the plan's `source_in_seconds` converted at the asset's
   frame rate, to the frame.
5. Every clip's out point matches `source_out_seconds`, to the frame.
6. Clip order in the timeline matches plan position order.
7. Every clip's name matches what the plan and export specified.
8. Audio is present and linked to its clip.
9. There are no transitions, no gaps, and no filler between clips.
10. Total timeline duration equals the sum of the plan's source range durations, to the
    frame.
11. Media with each of the codecs present in the corpus conforms correctly, with any
    codec-specific behavior recorded.
12. Re-exporting the same revision and re-importing produces the same timeline, with
    Resolve treating the clips as the same clips rather than new ones — this is what
    D01-T022's stable identifiers were for.
13. No original media file is created, modified, moved, or transcoded, verified by
    comparing the source file list, sizes, and modification times before and after.

## Independent test charter

Falsify the claim that the timeline matches the plan.

- Read the plan through `rcdvlog` CLI output and build the expected per-clip table
  yourself before opening Resolve, so the expectation is not shaped by what Resolve
  shows.
- Compare frame-accurately, not approximately. A one-frame in-point error is a real
  defect for a finishing editor and is invisible to a durations-only check.
- Verify source resolution by full path, not by filename. Two cards can contain
  `C0001.MP4`, and a conform that resolves to the wrong card is exactly the failure the
  capture-sequence model exists to prevent.
- Attack the awkward cases: a clip whose range starts at 0; a clip whose range ends at
  the media's last frame; a clip shorter than one second; the same asset used twice with
  different ranges; a file with a non-ASCII name; a path containing spaces; mixed frame
  rates in one timeline, including 23.976 and 29.97 alongside 30.
- Confirm the no-transition claim by inspecting the timeline in Resolve rather than
  inferring it from the OTIO source.
- Verify original immutability with a before-and-after file list. Resolve's media
  management can relocate files, so this check matters more here than elsewhere.
- Attempt the re-import stability check with a genuinely fresh Resolve project as well as
  the same project, since identifier stability behaves differently in each.

## Review charter

Risk domain: a handoff that appears to work but silently misplaces frames or resolves to
the wrong source — the worst possible failure, because the user only discovers it after
building an edit on it.

Questions:

1. Was the comparison built from the CLI's view of the plan, or from the export itself?
2. Was the in/out comparison frame-accurate or duration-approximate?
3. Was source resolution verified by full path rather than filename?
4. Were multiple real codecs exercised, or only one?
5. Is any required adapter or conversion step documented in the user-facing handoff?
6. Were originals provably untouched, including by Resolve's media management?
7. Does the verdict follow from the table — specifically, is a partially correct timeline
   being accepted?
8. Was the re-import stability check done in a fresh project as well as the original?

Severity rules: an unverified frame-accurate comparison, filename-only source
verification, an undocumented conversion step, any modification to originals, or
accepting a partially correct timeline is blocking. Missing codec coverage beyond the
first two is a non-blocking note with a follow-up.

Smallest acceptable remedy: re-run the specific comparison frame-accurately and record
the honest result.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Fixing the exporter. Defects go to a follow-up packet against D01-T022.
- Other finishing editors: Premiere, Final Cut, Avid.
- Other interchange formats: AAF, EDL, FCPXML.
- Round-tripping edits from Resolve back into the project.
- Colour, audio mixing, or anything past conform.
- Resolve performance or rendering.

## Stop conditions

Stop and escalate when:

- Resolve refuses the file outright, which sends the schema back to D01-T022 with the
  exact error;
- Resolve requires a conversion step that needs a Python OTIO adapter, which would
  reopen the pure-Rust decision and is the product owner's call;
- Resolve relocates or transcodes originals as part of import and it cannot be prevented
  — stop before running it over real footage;
- frame accuracy cannot be verified because Resolve's displayed timecode cannot be
  mapped to source seconds for a codec in the corpus;
- the installed Resolve turns out to be a version other than 20 or 21.

## Commands

```sh
pnpm --dir apps/desktop tauri build
cargo test -p rcdvlog-core --test otio_golden
```

Then, with `<media>` replaced by the corpus path:

```sh
find "<media>" -type f -exec stat -f "%N %z %m" {} \; | sort > /tmp/resolve-before.txt
find "<media>" -type f -exec stat -f "%N %z %m" {} \; | sort > /tmp/resolve-after.txt
diff /tmp/resolve-before.txt /tmp/resolve-after.txt && echo "originals untouched"
```

Expected result: the golden test passes, the build succeeds, and the `diff` reports no
differences.

## Handoff

Report the Resolve version and build, the plan and corpus used with its codecs, the
full per-clip comparison table, any adapter or conversion step with its exact procedure,
the re-import stability result in both a fresh and an existing project, the immutability
evidence, the `RI-n` defect list, and the verdict with its reasons. List any stop
condition encountered. The tester does not approve this task and does not fix what it
finds.

## Promotion note

`draft` until D01-T023 completes, so the export under test is one produced through the
shipped UI rather than a test harness. The contract editor confirms the corpus covers at
least two real codecs before promoting.
