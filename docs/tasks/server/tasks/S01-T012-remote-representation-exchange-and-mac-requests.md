# S01-T012: Remote Representation Exchange and Mac-Targeted Requests

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "S01-T012",
  "epic_id": "S01",
  "title": "Remote Representation Exchange and Mac-Targeted Requests",
  "slug": "remote-representation-exchange-and-mac-requests",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": [
    "S01-T006",
    "S01-T011"
  ],
  "principal_invariant": "The Mac can download a server-generated proxy to its local cache, and a Mac-targeted representation request is queued when an original is available only to the Mac — without blocking either node's normal operation.",
  "central_falsification": "Generate a proxy on the server for server-resident media, request it from the Mac, verify the proxy downloads to the Mac's local cache and is playable. Then queue a Mac-targeted representation request for Mac-only media, verify the job appears on the Mac, and verify the Mac's normal desktop operation is not blocked.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Proxy download → media streaming (S01-T006)"
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
      "docs/tasks/server/tasks/S01-T012-remote-representation-exchange-and-mac-requests.md"
    ],
    "implementer": [
      "core/rcdvlog-sync/src/**",
      "apps/desktop/src-tauri/**",
      "core/rcdvlog-server/src/**"
    ],
    "independent_tester": [
      "core/rcdvlog-sync/tests/**",
      "apps/desktop/tests/**"
    ],
    "integrator": []
  },
  "forbidden_paths": [
    "apps/web/**",
    "apps/mobile/**",
    "deploy/**"
  ],
  "verification_commands": [
    "cargo test -p rcdvlog-sync",
    "cargo test -p rcdvlog-server",
    "pnpm --dir apps/desktop build",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/server/EPIC.md",
      "regex": "\\| 120 \\| \\[S01-T012\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Allow the Mac to download server-generated proxies to its local cache and to
request representations for originals only the Mac can access — the bidirectional
representation flow that makes node-relative availability useful rather than
merely reported.

## Principal invariant

This task is correct when the Mac can download a server-generated proxy to its
local cache, and a Mac-targeted representation request is queued when an original
is available only to the Mac — without blocking either node's normal operation.

## Inputs and outputs

Inputs:
- S01-T006 representation store, media jobs, and streaming
- S01-T011 automatic sync and node-relative availability
- S01-T003 storage root model and availability observations

Outputs:
- Explicit Mac proxy download from server to local project cache
- Mac-targeted representation request queue
- Server-initiated job request for Mac-only originals
- Remote representation availability tracking
- Node-relative proxy availability in desktop UI

## Frozen decisions

1. Proxy download is explicit and user-initiated, not automatic.
2. A Mac-targeted request is a sync-payload message, not a server callback.
3. Representation generation on the Mac uses its own local FFmpeg.
4. Download does not block or delay project operation sync.
5. Remote representation availability is distinct from local availability in the UI.

## Owned paths

- **Implementer:** `core/rcdvlog-sync/` (exchange protocol DTOs), `apps/desktop/src-tauri/` (download, local cache, Mac-targeted job handling), `core/rcdvlog-server/src/` (exchange service)
- **Independent tester:** `core/rcdvlog-sync/tests/`, `apps/desktop/`
- **Integrator:** no new integration paths

## Required interface

- Proxy download API for desktop
- Mac-targeted request DTOs in sync protocol
- Remote availability indicators for desktop UI

## Acceptance oracle

1. Mac downloads a server proxy to local cache and plays it.
2. Downloaded proxy survives desktop restart.
3. Mac-targeted request is queued when original is Mac-only.
4. Request appears as a pending job on the Mac without blocking normal operation.
5. Mac generates the requested representation and the result is visible on server.
6. Node-relative proxy availability is correctly displayed.
7. Download does not block or delay project sync.

## Independent test charter

Try to falsify: "proxy download doesn't corrupt or block." Download a server
proxy, verify byte equality with the server's copy. Download while a sync is
in progress, verify sync is not delayed. Queue a Mac-targeted request, verify it
appears on the Mac and the Mac's normal operation continues. Test with a
server-resident proxy for media whose original is on the Mac (node-relative
availability).

## Review charter

Risk domain: representation integrity and node coordination. Questions: (1) Can a
downloaded proxy differ from the server's copy? (2) Can a download block sync? (3)
Can a Mac-targeted request be lost? (4) Is remote availability distinct from
local availability in the UI? Severity: P0 for corrupted downloads or sync
blocking; P1 for lost requests or confusing availability display. Approval
vocabulary: `APPROVE`, `APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- Automatic proxy prefetch or cache warming
- Bidirectional original media transfer (T01-T002 is travel-specific)
- Server-side Mac job execution
- Remote original access

## Stop conditions

Stop and escalate when:

- Proxy download needs to stream through the server's sync protocol
- Mac-targeted requests need the server to connect inbound to the Mac
- The exchange protocol conflates representation availability with original availability

## Commands

```sh
cargo test -p rcdvlog-sync
cargo test -p rcdvlog-server
pnpm --dir apps/desktop build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

Remote representation exchange completes the project replication milestone
(S01-T008 through S01-T012). Node-relative availability is consumed by S01-T015
(web review) for accurate proxy availability display.
