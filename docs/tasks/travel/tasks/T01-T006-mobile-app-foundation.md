# T01-T006: Mobile App Foundation, Pairing, Sync, and Offline Notes

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "T01-T006",
  "epic_id": "T01",
  "title": "Mobile App Foundation, Pairing, Sync, and Offline Notes",
  "slug": "mobile-app-foundation",
  "status": "ready",
  "task_class": "foundation",
  "depends_on": [
    "T01-T005"
  ],
  "principal_invariant": "The phone pairs as an ordinary native device, replicates a project locally, supports offline note/label/removal editing, and converges on reconnect — validating the mobile companion without putting any original media at risk.",
  "central_falsification": "Pair a physical iPhone with the server, replicate a project, enable airplane mode, write notes/labels/removals, reconnect, and assert every edit appears on desktop and web with no duplication. Then revoke the device credential and assert the phone is locked out.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Mobile app → pairing endpoints (S01-T009)",
    "Mobile SQLite → sync protocol (S01-T008)",
    "Mobile app → T01-T005 project creation API"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ],
    "implementer": [
      "docs/tasks/travel/ARCHITECTURE.md",
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "independent_tester": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ],
    "reviewer": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ],
    "integrator": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/travel/tasks/T01-T006-mobile-app-foundation.md"
    ],
    "implementer": [
      "apps/mobile/src/**",
      "packages/project-client/**",
      "core/rcdvlog-sync/src/**"
    ],
    "independent_tester": [
      "apps/mobile/__tests__/**"
    ],
    "integrator": [
      "pnpm-workspace.yaml",
      "package.json"
    ]
  },
  "forbidden_paths": [
    "apps/desktop/src/**",
    "apps/web/src/**",
    "core/rcdvlog-server/src/transfer.rs",
    "deploy/**"
  ],
  "verification_commands": [
    "pnpm --dir apps/mobile typecheck",
    "cargo test -p rcdvlog-sync",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/travel/EPIC.md",
      "regex": "\\| 60 \\| \\[T01-T006\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Stand up `apps/mobile` as an Expo custom-development-build iPhone application
that pairs with the server, replicates a project locally, and supports offline
annotation — before any SD-card handling exists.

## Principal invariant

This task is correct when the phone pairs as an ordinary native device, replicates
a project locally, supports offline note/label/removal editing, and converges on
reconnect — validating the mobile companion without putting any original media at
risk.

## Inputs and outputs

Inputs:

- S01-T009 authentication and pairing endpoints
- S01-T008 sync protocol
- T01-T005 project creation API
- `packages/project-client` typed contracts

Outputs:

- `apps/mobile` Expo project with custom development build, pnpm workspace integration
- Device-credential pairing through existing endpoints, stored in iOS Keychain
- Local SQLite project replica and pending-operation queue
- Sync client over existing operation protocol with cursors and bounded pages
- Offline note, label, and removal editing
- Project list, asset list, and asset detail screens for touch
- Server reachability and sync status surfacing
- Trip project creation from phone

## Frozen decisions

1. Authentication uses existing pairing flow; the phone is an ordinary paired native device.
2. Credentials live in iOS Keychain, never in JavaScript-accessible storage, URLs, or logs.
3. The phone is a full sync participant using the existing operation protocol — no bespoke mutation API.
4. Local writes commit to local SQLite immediately and never block on the network.
5. Sync is idempotent across retry and converges after offline edits.
6. Typed contracts come from `packages/project-client`; no hand-rolled parallel client.
7. UI is for touch and intermittent connectivity; share contracts/models/mutation logic, not layout.
8. Connectivity state is always visible and unambiguous.
9. The app is usable with no network at all, including cold start from local state.
10. Project creation requires connectivity and says so plainly rather than queueing.

## Owned paths

- **Implementer:** `apps/mobile/` (entire new application), `packages/project-client/` (shared client consumed by phone), `core/rcdvlog-sync/` (any DTO adjustments for third replica)
- **Independent tester:** `apps/mobile/` sync convergence and offline-queue tests
- **Integrator:** `pnpm-workspace.yaml`, root `package.json` (workspace registration)

## Required interface

- Project creation API (calling T01-T005's endpoint)
- Sync client conforming to existing protocol
- Typed contracts from `packages/project-client`

## Acceptance oracle

1. The app builds as an Expo custom development build and runs on a physical iPhone.
2. Pairing completes through existing endpoints and stores credential in the Keychain.
3. Credentials never appear in a URL, log, or JavaScript-accessible storage.
4. A project replicates to local SQLite and opens with no network.
5. Notes, labels, and removals can be edited offline and queue locally.
6. Queued edits converge on reconnect and appear on desktop and web.
7. Duplicate delivery and interrupted sync do not duplicate state.
8. The app cold-starts and is usable with the server unreachable.
9. Connectivity and sync status are visible on every screen where edits are possible.
10. A trip project can be created from the phone and appears on the server.
11. Project creation with no connectivity fails clearly rather than queueing.
12. Revoking the device credential on the server locks the phone out.
13. Typed contracts are consumed from `packages/project-client`, not duplicated.
14. Desktop and web behavior are unaffected by the phone's participation.

## Independent test charter

Try to falsify: "the phone is a faithful sync participant." Run against a real
server over Tailscale from a physical device. Test airplane-mode editing followed
by reconnection. Test credential revocation while the app is running. Assert sync
convergence with the same fixtures used for desktop sync tests so the three
replicas are held to one standard. Test cold start with no network. Simulator
testing is insufficient for Keychain and network-transition behavior.

## Review charter

Risk domain: sync correctness and credential safety. Questions: (1) Can offline
edits be lost on reconnect? (2) Can duplicate delivery cause duplicate state? (3)
Do credentials ever appear in logs or JS storage? (4) Does the phone use the
existing sync protocol or a bespoke API? Severity: P0 for credential leakage or
sync data loss; P1 for duplicate state or bespoke API. Approval vocabulary:
`APPROVE`, `APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- SD-card access, inventory, or upload (T01-T007, T01-T008)
- Media playback and caching (T01-T008)
- Chronology calibration
- Desktop keyboard-oriented interaction model porting
- Android
- App Store distribution

## Stop conditions

Stop and escalate when:

- The phone needs a bespoke mutation API different from the sync protocol
- Credentials cannot be stored exclusively in the iOS Keychain
- The Expo custom build pipeline cannot be established
- Sync correctness must be deferred to T01-T007

## Commands

```sh
pnpm --dir apps/mobile typecheck
cargo test -p rcdvlog-sync
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

T01-T007 adds card inventory on top of this foundation and needs the local
operation queue to already be reliable, because provisional assets are emitted as
ordinary operations from the phone. Do not defer sync correctness to T01-T007.
