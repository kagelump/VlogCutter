# Task T7: Card Inventory and Provisional Assets

**Status:** Planned
**Depends on:** T2, T5, T6
**Unblocks:** T8

## Objective

Let the phone inventory an attached SD card and immediately create annotatable project
assets for every clip on it — before any bytes upload — then bind those assets to real
media objects as uploads complete.

This is the mechanism the entire "annotate while footage uploads" promise rests on.

## User outcome

Three minutes after inserting a card, every clip on it is visible as a project asset on the
phone, the web, and the desktop, and the user can start writing notes. Those notes are
still attached to the right clip hours later when its media finally arrives.

## Scope

- Security-scoped external-volume access and card enumeration on iOS.
- Card identity derivation matching T2's server-side definition.
- Provisional project asset creation as ordinary project operations.
- The media link operation, emittable by both server and desktop.
- Server-side emission of the link operation on ingest.
- Desktop-side emission of the link operation for locally ingested media.
- Unlinked-asset presentation on phone, web, and desktop.
- Re-inventory of a partially uploaded card.

## Out of scope

- Upload execution, queue policy, and prioritization (T8)
- Media playback and caching (T8)
- Reading media metadata from the card beyond filesystem attributes
- Writing anything to the SD card

## Provisional assets

At inventory time the phone emits ordinary project operations creating project assets that
have no `mediaObjectId` yet, carrying:

- filename and card-relative path
- byte size
- card session identity and card-relative capture sequence
- filesystem modification time
- camera folder (`DCIM/100MSDCF`)

These are not a travel-only entity. They are project assets awaiting media. They
synchronize through the existing operation protocol and support notes, labels, and removal
immediately.

## The link operation

When media arrives, a link operation binds an existing `projectAssetId` to a
`mediaObjectId`.

`projectAssetId` is allocated **by the phone at inventory time and never changes**. That
is why notes survive: they were always attached to the asset, not to the media. The
identity model already separates `projectAssetId` from `mediaObjectId`; this uses that
separation across time rather than only across nodes.

**The link operation must be emittable by the desktop as well as the server.** A trip whose
uploads did not finish would otherwise strand notes on assets that can never gain media.
On return, desktop card ingest emits the link for anything that never uploaded.

## Unlinked-asset presentation

During a trip, unlinked is the *common* state, not an edge case. Phone, web, and desktop
each need a coherent presentation of "this asset is known but its media has not arrived",
distinguishing it from "media exists but is not available on this node", which already has
its own meaning.

Minimum: the asset appears in lists, is annotatable, shows size and capture position,
displays transfer state, and does not present a broken player or a missing-file error.

## Card access on iOS

Card enumeration requires security-scoped access to an external volume. Enumeration must:

- Complete for thousands of files without exhausting memory
- Persist its results to local SQLite so it survives app restart and volume removal
- Never write to, rename, or delete anything on the card
- Handle the volume disappearing mid-enumeration by pausing, not failing

Re-inventorying a card the phone has seen before must recognize it by identity, add newly
shot footage as new assets, and leave existing assets and their upload progress untouched.

## Design requirements

1. `projectAssetId` is allocated by the phone and is stable from inventory through linking.
2. Provisional asset creation and linking are ordinary immutable operations.
3. The link operation is idempotent; replaying it changes nothing.
4. Linking an asset that is already linked to different media is a conflict, not a silent
   overwrite.
5. Notes, labels, and removal written before linking survive linking without rewrite.
6. Card identity derivation is shared between phone and server so both agree.
7. The card is opened read-only. Nothing is ever written to it.
8. Inventory of a large card is bounded in memory and resumable.
9. Inventory results persist locally and survive volume removal and app restart.
10. Deduped uploads (T3) still produce a link operation, so the asset gains media even
    though no new archive file was written.
11. An asset whose upload is abandoned remains a valid annotatable asset, not an error.

## Likely implementation areas

- `core/rcdvlog-core` provisional asset and link operations
- `core/rcdvlog-sync` operation DTOs
- `core/rcdvlog-server/src/transfer.rs` link emission on ingest
- `apps/desktop/src-tauri` link emission for locally ingested media
- `apps/desktop`, `apps/web`, `apps/mobile` unlinked-asset presentation
- `apps/mobile` external-volume access, enumeration, local persistence

## Acceptance criteria

- [ ] Inventorying a card creates a project asset for every media file on it.
- [ ] Provisional assets appear on phone, web, and desktop before any upload.
- [ ] Notes and labels can be written against a provisional asset immediately.
- [ ] Those notes survive linking, unchanged and unrewritten.
- [ ] `projectAssetId` is identical before and after linking.
- [ ] The server emits a link operation when an upload ingests.
- [ ] A deduped upload still produces a link operation.
- [ ] The desktop can emit a link operation for media it ingested from the card locally.
- [ ] Replaying a link operation is a no-op.
- [ ] Linking to different media than an existing link is a surfaced conflict.
- [ ] Re-inventorying a card adds new footage and preserves existing assets and progress.
- [ ] Card identity computed on the phone matches the server's derivation.
- [ ] Inventorying a card with thousands of files is memory-bounded and completes.
- [ ] Inventory survives app restart and volume removal.
- [ ] Nothing is written, renamed, or deleted on the SD card.
- [ ] Unlinked assets render coherently on all three surfaces with no broken player.
- [ ] An abandoned upload leaves a valid annotatable asset.

## Verification

Use a real SD card with real camera media and a card containing thousands of files. Test
volume removal mid-enumeration on a physical device. Assert card read-only behavior by
comparing a full checksum manifest of the card before and after a complete inventory and
upload cycle. Test note survival by writing notes at inventory time and asserting byte
equality after linking.

## Handoff notes

T8 layers the upload queue on top of this inventory. The inventory persistence schema
defined here is what the queue reads, so design it for the queue's access patterns:
filtering by state, sorting by policy, and paging by card.
