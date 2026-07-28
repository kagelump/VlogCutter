# S01-T015: Web Proxy Review and Annotation Editing

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "S01-T015",
  "epic_id": "S01",
  "title": "Web Proxy Review and Annotation Editing",
  "slug": "web-proxy-review-and-annotations",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": [
    "S01-T006",
    "S01-T007",
    "S01-T011",
    "S01-T013",
    "S01-T014"
  ],
  "principal_invariant": "The user can play server proxies with seeking in Chrome, edit notes and labels, remove and restore assets, and those edits synchronize to the desktop through the existing operation protocol — without a parallel mutation path.",
  "central_falsification": "Play a server proxy in Chrome, seek to the middle of a clip, verify the byte-range request fetches only the requested range. Edit a note on web, verify it appears on desktop after sync. Remove an asset on web, verify it is marked removed on desktop. Restore it on web, verify it reappears on desktop.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Web player → media streaming (S01-T006)"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "implementer": [
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "independent_tester": [
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "reviewer": [
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "integrator": [
      "docs/tasks/server/ARCHITECTURE.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/server/tasks/S01-T015-web-proxy-review-and-annotations.md"
    ],
    "implementer": [
      "apps/web/src/**",
      "packages/review-ui/**",
      "packages/project-client/**"
    ],
    "independent_tester": [
      "apps/web/e2e/**"
    ],
    "integrator": []
  },
  "forbidden_paths": [
    "apps/desktop/**",
    "core/**",
    "apps/server/**",
    "deploy/**"
  ],
  "verification_commands": [
    "pnpm --dir apps/web build",
    "pnpm --dir apps/web typecheck",
    "pnpm test:web",
    "pnpm test:web:e2e",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/server/EPIC.md",
      "regex": "\\| 150 \\| \\[S01-T015\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Implement proxy playback with seeking in Chrome, plus note, label, removal, and
restore editing that synchronizes to the desktop through the existing operation
protocol — not through a parallel mutation API.

## Principal invariant

This task is correct when the user can play server proxies with seeking in
Chrome, edit notes and labels, remove and restore assets, and those edits
synchronize to the desktop through the existing operation protocol — without a
parallel mutation path.

## Inputs and outputs

Inputs:
- S01-T006 representation streaming with byte-range support
- S01-T007 operation-backed project mutations
- S01-T011 automatic sync and conflict handling
- S01-T013 web foundation and typed client
- S01-T014 library/project browsing

Outputs:
- Proxy player with seeking (byte-range requests) in Chrome
- Note and label editing through typed operations
- Remove and restore asset operations
- Annotations synchronized through existing sync protocol (not a parallel API)
- Node-relative proxy availability in player UI
- Rotation display respecting orientation metadata

## Frozen decisions

1. Annotations use the existing typed operation model — no parallel mutation API.
2. Proxy seeking uses byte-range requests through S01-T006's media endpoints.
3. The browser is online-only; proxy playback requires server connectivity.
4. Edits emit operations that sync through S01-T011 — same path as desktop edits.
5. No web chronology editor; current fixed clock events only.

## Owned paths

- **Implementer:** `apps/web/src/` (player, annotation editing), `packages/review-ui/` (shared player and annotation components), `packages/project-client/` (annotation operation DTOs)
- **Independent tester:** `apps/web/` (e2e: play, seek, edit, sync)
- **Integrator:** no new integration paths

## Required interface

- Proxy streaming endpoints (from S01-T006)
- Annotation operation API (through sync protocol)
- Proxy availability indicators (from S01-T012)

## Acceptance oracle

1. Proxy plays in Chrome with seeking through byte-range requests.
2. Seeking fetches only the requested byte range (verify in network tab).
3. Note edited on web appears on desktop after sync.
4. Label edited on web appears on desktop after sync.
5. Asset removed on web is marked removed on desktop.
6. Restored asset reappears on desktop.
7. No parallel mutation API — web edits travel through the same operation protocol as desktop edits.
8. Proxy player respects orientation metadata.
9. Annotations survive page refresh.

## Independent test charter

Try to falsify: "web annotations use a parallel path." Trace a web-initiated note
edit through the system: verify it emits the same operation type as a desktop
note edit, travels through the same sync protocol, and produces the same state.
Test seeking across the full duration of a proxy — verify each byte-range request
returns the correct range. Test concurrent edits from web and desktop and verify
conflict surfacing (S01-T011).

## Review charter

Risk domain: annotation integrity and playback correctness. Questions: (1) Do web
annotations use the operation protocol or a parallel API? (2) Does proxy seeking
work correctly across the full duration? (3) Do web edits survive page refresh?
(4) Are concurrent web+desktop edits handled correctly? Severity: P0 for parallel
mutation API or broken seeking; P1 for lost edits on refresh or incorrect
concurrent handling. Approval vocabulary: `APPROVE`,
`APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- Web project creation
- Offline browser editing (browser is online-only)
- Chronology editor or conflict resolution UI
- Favorites or review-status states not in current product

## Stop conditions

Stop and escalate when:

- Web annotations need a separate mutation API from the sync protocol
- Proxy seeking requires loading the entire file into browser memory
- An edit cannot be represented as an existing typed operation
- The web UI needs to store annotations locally (browser is online-only)

## Commands

```sh
pnpm --dir apps/web build
pnpm --dir apps/web typecheck
pnpm test:web
pnpm test:web:e2e
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

Web review and annotations are the primary Chrome product surface. Consumed by
S01-T019 (end-to-end acceptance).
