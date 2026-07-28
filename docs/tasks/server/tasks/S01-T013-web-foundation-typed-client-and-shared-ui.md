# S01-T013: Web Foundation, Typed Client, and Shared UI Seam

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "S01-T013",
  "epic_id": "S01",
  "title": "Web Foundation, Typed Client, and Shared UI Seam",
  "slug": "web-foundation-typed-client-and-shared-ui",
  "status": "complete",
  "task_class": "foundation",
  "depends_on": [
    "S01-T009"
  ],
  "principal_invariant": "The Chrome web application authenticates, consumes typed contracts shared with the desktop, and renders reusable review components — without duplicating domain logic or hand-rolling a parallel API client.",
  "central_falsification": "Load the web app in Chrome, authenticate through the browser session flow, and verify that project data rendered in the web app uses the same typed contracts (from packages/project-client) as the desktop — no hand-rolled fetch calls or duplicated type definitions.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Web app → auth endpoints (S01-T009)",
    "Web app → typed contracts (packages/project-client)"
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
      "docs/tasks/server/tasks/S01-T013-web-foundation-typed-client-and-shared-ui.md"
    ],
    "implementer": [
      "apps/web/src/**",
      "packages/project-client/**",
      "packages/review-ui/**"
    ],
    "independent_tester": [
      "apps/web/e2e/**"
    ],
    "integrator": [
      "package.json",
      "pnpm-workspace.yaml"
    ]
  },
  "forbidden_paths": [
    "apps/desktop/**",
    "apps/server/**",
    "core/**",
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
      "regex": "\\| 130 \\| \\[S01-T013\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Stand up the Chrome web application shell with authentication, typed API client
from shared packages, and incrementally extracted shared review UI components —
a separate web shell consuming the same contracts as the desktop.

## Principal invariant

This task is correct when the Chrome web application authenticates, consumes
typed contracts shared with the desktop, and renders reusable review components —
without duplicating domain logic or hand-rolling a parallel API client.

## Inputs and outputs

Inputs:
- S01-T009 authentication and browser sessions
- `packages/project-client` typed API contracts
- `packages/review-ui` shared review components

Outputs:
- `apps/web` React/TypeScript application with Vite build
- Browser-session authentication flow
- Typed HTTP client consuming `project-client` contracts
- Shared `review-ui` component integration
- Navigation shell, project list, and routing foundation
- Incrementally shared UI components extracted from desktop where applicable

## Frozen decisions

1. The browser is online-only and talks only to the server.
2. The web app consumes typed contracts from `packages/project-client` — no hand-rolled fetch.
3. Shared review components live in `packages/review-ui` and are consumed by web (now) and desktop (incrementally).
4. The web shell is separate from the desktop shell — share contracts and components, not app layout.
5. No domain logic is duplicated between web and desktop TypeScript.

## Owned paths

- **Implementer:** `apps/web/` (entire application), `packages/project-client/` (shared client), `packages/review-ui/` (shared components)
- **Independent tester:** `apps/web/` (unit, integration, e2e)
- **Integrator:** root `package.json`, `pnpm-workspace.yaml`

## Required interface

- Browser session auth flow
- Typed API client consumed by S01-T014, S01-T015, S01-T016
- Shared component API for `review-ui`

## Acceptance oracle

1. Web app builds and loads in Chrome.
2. Browser session authentication completes successfully.
3. Authenticated requests use typed contracts from `project-client`.
4. No hand-rolled fetch calls or duplicated type definitions.
5. Shared review components render correctly in the web app.
6. No domain logic is duplicated between web and desktop TypeScript.
7. Web app does not import from `apps/desktop`.

## Independent test charter

Try to falsify: "the web app duplicates nothing." Search for any fetch/Axios
call that bypasses the typed client. Search for any type definition in
`apps/web` that mirrors one in `packages/project-client`. Verify no import from
`apps/desktop`. Verify that the web app's authenticated client sends credentials
correctly and handles 401 by redirecting to login.

## Review charter

Risk domain: client architecture and contract integrity. Questions: (1) Does the
web app use typed contracts or hand-rolled fetch? (2) Are types duplicated? (3)
Does the web app import from desktop? (4) Is authentication enforced on every
data request? Severity: P0 for duplicated domain logic or hand-rolled auth-less
fetch; P1 for type duplication or desktop imports. Approval vocabulary:
`APPROVE`, `APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- Library browsing, project detail, or proxy review (S01-T014, S01-T015)
- Administration views (S01-T016)
- Offline support (browser is online-only)
- Extracting all desktop UI to shared components (incremental only)

## Stop conditions

Stop and escalate when:

- The web app needs to import from `apps/desktop` for a shared type
- A typed contract is missing from `project-client` that the web app needs
- The review-ui extraction becomes a rewrite of desktop components

## Commands

```sh
pnpm --dir apps/web build
pnpm --dir apps/web typecheck
pnpm test:web
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

Typed client, auth, and shared components are consumed by S01-T014 (library),
S01-T015 (review), and S01-T016 (admin). The extraction seam established here
is reused for each new web feature.
