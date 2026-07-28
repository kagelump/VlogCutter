# Task 10: Desktop Pairing, Root Mapping, and Baseline Adoption

**Status:** Implemented; native Tauri/two-machine acceptance remains external
**Depends on:** Tasks 8 and 9
**Unblocks:** Task 11

## Objective

Add the desktop workflow that pairs the Mac with the one home server, maps local paths to logical storage roots, previews an existing project baseline, and safely creates its one server replica.

## User outcome

The user can connect an existing project to the home server with clear evidence about what will be synchronized and how differently mounted NAS paths correspond, without changing or uploading original media.

## Scope

- Add global server connection settings with LAN and Tailscale endpoint aliases.
- Add desktop device pairing-code flow using Task 9 credentials.
- Add server identity verification and certificate/trust error handling.
- Add project pairing state that enforces one server per `projectId`.
- Add root mapping wizard for current legacy absolute asset paths.
- Support existing server root, new logical root, local-only root, and partial-root mapping.
- Generate and display baseline preview counts/evidence/conflicts.
- Confirm and upload the idempotent baseline.
- Persist pairing only after complete server acknowledgement.
- Add safe retry/resume and explicit abandon of an uncommitted pairing attempt.

## Out of scope

- Continuous background sync after baseline
- Server migration or unpair/re-pair destructive workflow
- Uploading originals or representations
- Automatically writing root markers
- Web project creation
- Multiple Macs or servers

## UX requirements

The flow must show:

- Server display name, stable node ID, LAN/tailnet endpoint, and connection status
- Selected project name/ID and whether a server replica already exists
- Asset, note, label, removal, clock-event, and representation counts
- Current legacy path prefixes
- Proposed logical roots and server-library candidate matches
- Evidence for “same storage root,” including relative path, size, quick fingerprint, and verified full hash where available
- Items that remain Mac-only or server-only
- No original-media upload in this operation
- Final success with baseline watermark and initial sync status

## Design requirements

1. Global settings gain Serde/TypeScript defaults so older settings remain valid.
2. Device secret remains in Keychain; project catalog stores server/node identity and non-secret pairing state.
3. The server certificate and returned server ID must match the intended pairing before state is persisted.
4. LAN and Tailscale URLs are aliases for the same stable server ID; endpoint switching cannot create a second pairing.
5. Root association is explicit. Suggested matches never auto-confirm.
6. Baseline preview is read-only on both nodes.
7. Baseline upload contains project data and manifests, never original/proxy blobs or arbitrary absolute paths.
8. An interrupted or rejected baseline leaves the desktop project unpaired and server without a registered partial replica.
9. A retry uses the same baseline ID/digest unless local project state changed, in which case the preview is regenerated.
10. Pairing an already-paired project to a different server fails with actionable guidance.
11. A legacy catalog containing multiple projects pairs only the selected project.
12. The user can cancel safely before final acknowledgement.

## Likely implementation areas

- `apps/desktop/src-tauri/src/lib.rs` connection, Keychain, root selection, baseline commands
- `apps/desktop/src/api.ts`, `types.ts`, `App.tsx`, and `styles.css`
- `core/rcdvlog-core` baseline/root projection
- `core/rcdvlog-sync` client handshake
- `core/rcdvlog-server` baseline preview/import services
- Browser mock behavior for layout only

## Acceptance criteria

- [ ] An older desktop settings file loads with no server configured.
- [ ] A valid pairing code stores a device credential in Keychain and non-secret endpoint metadata in settings.
- [ ] The UI recognizes LAN and tailnet endpoints as one server by stable ID.
- [ ] A Mac `/Volumes/...` binding and Linux `/mnt/...` binding map to one `storageRootId`.
- [ ] Partial-root mapping resolves only the intended subtree.
- [ ] Baseline preview counts match the selected project and does not mutate either catalog.
- [ ] User confirmation creates one server replica with matching materialized project state.
- [ ] Cancel, network loss, response loss, and restart are safe to retry.
- [ ] Changing local state after preview invalidates the stale preview/digest.
- [ ] A conflicting existing replica or different server pairing is rejected without overwrite.
- [ ] No media blobs transfer and no original path is modified.
- [ ] Desktop works normally before pairing and after a failed pairing attempt.
- [ ] Visible focus, keyboard access, error text, and minimum-window layout are verified.

## Verification

Add process-level tests using temporary desktop/server databases and fake HTTPS transport where appropriate. Use the browser mock for layout/focus only, then run a real Tauri-to-server smoke test for Keychain, certificate trust, and baseline transfer.

## Handoff notes

Persist enough state for Task 11 to begin reconciliation from the acknowledged baseline cursor. Do not start an automatic sync loop until the baseline transaction is complete.
