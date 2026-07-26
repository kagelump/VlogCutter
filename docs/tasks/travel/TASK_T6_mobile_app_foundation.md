# Task T6: Mobile App Foundation, Pairing, Sync, and Offline Notes

**Status:** Planned
**Depends on:** Companion-server Task 9, T5
**Unblocks:** T7

## Objective

Stand up `apps/mobile` as an Expo custom-development-build iPhone application that pairs
with the server, replicates a project locally, and supports offline annotation — before any
SD-card handling exists.

This is the "phone review without phone ingest" slice. It validates the mobile companion
without putting any original media at risk.

## User outcome

The user can open a project on their phone, browse its assets, read and write notes and
labels while offline, and see those edits appear on the desktop and web after reconnecting.

## Scope

- `apps/mobile` Expo project with a custom development build and pnpm workspace integration.
- Device-credential pairing through the existing pairing endpoints, stored in the iOS Keychain.
- Local SQLite replica of a project and a pending-operation queue.
- Sync client over the existing operation protocol with cursors and bounded pages.
- Offline note, label, and removal editing.
- Project list, asset list, and asset detail screens designed for touch.
- Server reachability and sync status surfacing.
- Trip project creation from the phone (calling T5's API).

## Out of scope

- SD-card access, inventory, or upload (T7, T8)
- Media playback and caching (T8)
- Chronology calibration
- Any desktop keyboard-oriented interaction model
- Android
- App Store distribution

## Why a custom development build

Expo Go cannot provide the security-scoped external-volume access and background transfer
behavior that T7 and T8 require. Establishing the custom build now — with its native module
surface and its build pipeline — avoids discovering in T7 that the foundation cannot carry
the feature.

## Design requirements

1. Authentication uses the existing pairing flow (`POST /api/v1/auth/pairing`,
   `POST /api/v1/auth/devices/exchange`). The phone is an ordinary paired native device;
   no new credential class is introduced.
2. Credentials live in the iOS Keychain and never in JavaScript-accessible storage, a URL,
   or a log.
3. The phone is a full sync participant using the existing operation protocol. It does not
   get a bespoke mutation API.
4. Local writes commit to local SQLite immediately and never block on the network.
5. Sync is idempotent across retry and converges after offline edits, exactly as the
   desktop's does.
6. Typed contracts come from `packages/project-client`. The phone must not hand-roll a
   parallel client.
7. UI is designed for touch and intermittent connectivity. Do not port desktop components
   wholesale; share contracts, models, mutation logic, and formatting rather than layout.
8. Connectivity state is always visible and unambiguous — the user must never wonder
   whether an edit was saved.
9. The app is usable with no network at all, including cold start from local state.
10. Project creation requires connectivity and says so plainly rather than queueing.

## Likely implementation areas

- `apps/mobile` new Expo application, native build configuration
- `packages/project-client` shared client consumed by the phone
- `core/rcdvlog-sync` any DTO adjustments needed for a third replica
- `pnpm-workspace.yaml`, root `package.json` workspace registration
- Sync convergence and offline-queue tests

## Acceptance criteria

- [ ] The app builds as an Expo custom development build and runs on a physical iPhone.
- [ ] Pairing completes through the existing endpoints and stores the credential in the Keychain.
- [ ] Credentials never appear in a URL, a log, or JavaScript-accessible storage.
- [ ] A project replicates to local SQLite and opens with no network.
- [ ] Notes, labels, and removals can be edited offline and queue locally.
- [ ] Queued edits converge on reconnect and appear on desktop and web.
- [ ] Duplicate delivery and interrupted sync do not duplicate state.
- [ ] The app cold-starts and is usable with the server unreachable.
- [ ] Connectivity and sync status are visible on every screen where edits are possible.
- [ ] A trip project can be created from the phone and appears on the server.
- [ ] Project creation with no connectivity fails clearly rather than queueing.
- [ ] Revoking the device credential on the server locks the phone out.
- [ ] Typed contracts are consumed from `packages/project-client`, not duplicated.
- [ ] Desktop and web behavior are unaffected by the phone's participation.

## Verification

Run against a real server over Tailscale, from a physical device. Simulator testing is
insufficient for Keychain and network-transition behavior. Test airplane-mode editing
followed by reconnection, and test credential revocation while the app is running. Assert
sync convergence with the same fixtures used for desktop sync tests so the three replicas
are held to one standard.

## Handoff notes

T7 adds card inventory on top of this foundation and needs the local operation queue to
already be reliable, because provisional assets are emitted as ordinary operations from the
phone. Do not defer sync correctness to T7.
