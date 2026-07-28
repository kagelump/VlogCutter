# S01-T014: Web Library/Project Browsing and Add-to-Project

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "S01-T014",
  "epic_id": "S01",
  "title": "Web Library/Project Browsing and Add-to-Project",
  "slug": "web-library-project-browse-and-add",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": [
    "S01-T005",
    "S01-T006",
    "S01-T008",
    "S01-T013"
  ],
  "principal_invariant": "The user can browse the paginated global media library in Chrome, browse project assets, add verified library media to an existing project, and see accurate node-relative original/proxy availability — all through the typed client with no hand-rolled API calls.",
  "central_falsification": "Browse the global library with multiple pages, verify pagination is deterministic, add a verified media object to an existing project, verify the asset appears in the project with correct availability indicators, and verify node-relative availability distinguishes 'available here', 'available on another node', and 'unavailable'.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Web library → storage root/library services (S01-T003, S01-T005)"
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
      "docs/tasks/server/tasks/S01-T014-web-library-project-browse-and-add.md"
    ],
    "implementer": [
      "apps/web/src/**",
      "packages/project-client/**",
      "packages/review-ui/**"
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
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/server/EPIC.md",
      "regex": "\\| 140 \\| \\[S01-T014\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Implement paginated global media library browsing in Chrome, project asset
browsing, server-origin add-to-project, and accurate node-relative availability
display.

## Principal invariant

This task is correct when the user can browse the paginated global media library
in Chrome, browse project assets, add verified library media to an existing
project, and see accurate node-relative original/proxy availability — all through
the typed client with no hand-rolled API calls.

## Inputs and outputs

Inputs:
- S01-T005 global media library with paginated queries
- S01-T006 representation store and streaming
- S01-T008 server project replicas and sync protocol
- S01-T013 web foundation and typed client

Outputs:
- Paginated global library view with thumbnails, metadata, and verification state
- Project asset list with node-relative availability indicators
- Add-to-project: select library media, add to existing project
- Server-origin add-to-project operation
- Availability display distinguishing known/here/remote/unavailable/mismatch
- Collections responsive with thousands of assets

## Frozen decisions

1. Library queries are cursor-paginated and deterministic.
2. Add-to-project is a server-origin operation (server adds library media to project).
3. Node-relative availability uses the model from S01-T003.
4. Thumbnails stream through the representation endpoints from S01-T006.
5. Collections must remain responsive with thousands of assets (pagination is not optional).

## Owned paths

- **Implementer:** `apps/web/src/` (library view, project view, add-to-project flow), `packages/project-client/` (library and project DTOs), `packages/review-ui/` (media cards, availability badges)
- **Independent tester:** `apps/web/` (e2e: browse, paginate, add-to-project)
- **Integrator:** no new integration paths

## Required interface

- Library listing API (paginated, with availability)
- Project asset listing API
- Add-to-project API
- Node-relative availability DTOs

## Acceptance oracle

1. Global library displays paginated, deterministic results.
2. Library items show thumbnails, metadata, and verification state.
3. Project assets display with accurate node-relative availability.
4. Add-to-project adds a verified library media object to an existing project.
5. Added asset appears in project immediately with correct availability.
6. Availability distinguishes known/here/remote/unavailable/mismatch.
7. Collections remain responsive with thousands of assets (pagination works).

## Independent test charter

Try to falsify: "availability is accurate." Create scenarios: media on server
only, media on Mac only, media on both, media on neither (unavailable mount),
media with fingerprint mismatch. Verify each displays the correct availability
state. Test pagination by seeding 1000+ library items and verifying all pages
are reachable and deterministic. Test add-to-project with media that has
representations and media without.

## Review charter

Risk domain: library browsing and availability accuracy. Questions: (1) Is
availability accurate for every node-relative state? (2) Is pagination
deterministic? (3) Does add-to-project work correctly? (4) Are collections
responsive at scale? Severity: P0 for incorrect availability (user can't find
their media) or broken pagination; P1 for add-to-project edge cases. Approval
vocabulary: `APPROVE`, `APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- Web project creation (deferred to T01-T005)
- Project deletion
- Inline editing of library metadata
- Bulk add or batch operations

## Stop conditions

Stop and escalate when:

- Availability display conflates "media exists but is remote" with "media is unverified"
- Pagination is non-deterministic (different page contents on repeated requests)
- Add-to-project needs desktop involvement (must be server-origin)
- Collections become unresponsive at 1000+ assets

## Commands

```sh
pnpm --dir apps/web build
pnpm --dir apps/web typecheck
pnpm test:web
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

Library browsing and add-to-project are consumed by S01-T015 (review with
context of how media arrived) and S01-T016 (storage administration).
