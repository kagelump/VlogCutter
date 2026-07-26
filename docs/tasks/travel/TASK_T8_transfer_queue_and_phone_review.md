# Task T8: Transfer Queue, Priority Policies, and Phone Review

**Status:** Planned
**Depends on:** T3, T4, T7
**Unblocks:** T9

## Objective

Give the phone a durable upload queue with real priority policies, a streaming read path
that never duplicates the card, and a review surface that plays arriving mobile proxies —
including offline.

## User outcome

The user chooses what uploads first, starts it before dinner, comes back to find some of it
done, pauses, moves to a new hotel, resumes, and reviews and annotates whatever has arrived
— all without ever waiting on the whole card.

## Scope

- Durable local upload queue backed by SQLite, surviving app restart.
- Streaming read path from the SD card into chunk uploads.
- Priority policies, pause, resume, and reorder.
- Per-card and per-object progress, throughput, and estimated remaining time.
- Network transition and card removal handling.
- Keep-awake mode and completion/failure notifications.
- Mobile proxy and thumbnail playback with an LRU-bounded media cache.
- Transfer and ingest state surfacing from T2 and T3.
- Storage-capacity warnings for the phone cache.

## Out of scope

- Any "safe to format" state
- Full duplication of card contents into phone storage
- Background upload daemons
- Automatic policy selection or learned prioritization
- Server-side remote diagnostics UI beyond what the transfer API already returns

## Streaming read path

```text
SD card file → bounded buffer → chunk upload
```

Never:

```text
SD card file → full copy in phone storage → chunk upload
```

A full local copy would demand enormous free phone storage and double all local I/O for no
benefit. The phone retains only queue and chunk state, thumbnails, selected proxies, and a
small rolling transfer buffer.

The phone computes the full-file BLAKE3 incrementally during the same read that feeds the
uploader, so verification costs no additional pass over the card.

## Priority policies

At minimum: everything, today only, smallest first, this folder only, and pinned first.
Pinning individual clips must be possible from the review surface, so the user can push
something they just annotated to the front of the queue.

The workflow must never depend on completing an entire card. Partial completion is the
expected outcome of any given night.

## iOS reality

Security-scoped external-volume access does not reliably survive backgrounding. Long
transfers are a foreground, screen-on, plugged-in activity with best-effort background
continuation. This is a platform limitation to be designed around honestly, not engineered
around.

Required behavior:

- An explicit keep-awake mode while uploading.
- Clear communication that overnight uploads need the app foregrounded and the phone charging.
- Clean pause on volume disappearance, with resume on reinsertion via card identity.
- Notifications on completion and on failure so the user need not watch the screen.
- Best-effort background continuation where the platform permits, never depended upon.

Because the resume protocol never retransmits committed chunks, an interrupted night costs
time, not data. Say so in the UI.

## Design requirements

1. Queue state is durable in local SQLite and survives app restart, crash, and force-quit.
2. The server's resume query is authoritative; local queue state is a cache that can always
   be rebuilt from it.
3. Only one object uploads at a time by default, with concurrency configurable and bounded.
4. Chunk uploads retry with bounded exponential backoff and never spin against a hard failure.
5. Pause is immediate and leaves committed chunks intact.
6. Reordering does not restart the in-flight object.
7. The card is opened read-only; nothing is ever written to it.
8. Progress reflects committed chunks, not bytes optimistically pushed.
9. The media cache has a hard size budget with LRU eviction and a visible current size.
10. Mobile proxies are fetched on demand and by explicit prefetch policy, never
    speculatively for a whole card — downstream bandwidth competes with uploads.
11. Cached proxies remain playable with no network.
12. No surface presents a "safe to format" state or otherwise implies the card is expendable.
13. Transfer, ingest, and representation state are distinguishable in the UI, so the user
    can tell "not uploaded" from "uploaded, not yet processed".

## Likely implementation areas

- `apps/mobile` queue engine, streaming reader, playback, cache, notifications
- `apps/mobile` native module surface for external-volume streaming and keep-awake
- `packages/project-client` transfer and representation contracts
- `core/rcdvlog-server/src/transfer.rs` resume query paging and indexing for large cards
- Queue state-machine unit tests and physical-device acceptance

## Acceptance criteria

- [ ] Queue state survives app restart, crash, and force-quit.
- [ ] Local queue state can be rebuilt entirely from the server's resume query.
- [ ] Uploads stream from the card without a full local copy.
- [ ] Peak phone storage use during a large card upload stays within a small bounded budget.
- [ ] The full-file hash is computed during the upload read, not in a separate pass.
- [ ] Each priority policy orders the queue as specified.
- [ ] A clip pinned from the review surface moves to the front without restarting the in-flight object.
- [ ] Pause is immediate and preserves committed chunks.
- [ ] Resume after pause retransmits no committed chunk.
- [ ] A network transition mid-upload resumes without data loss.
- [ ] Card removal pauses cleanly; reinsertion resumes via card identity.
- [ ] Retries use bounded backoff and stop against terminal errors.
- [ ] Progress reflects committed chunks only.
- [ ] Completion and failure notifications fire with the app backgrounded.
- [ ] Thumbnails and mobile proxies play, and seek, on the phone.
- [ ] Cached proxies play with no network.
- [ ] The media cache respects its size budget and evicts by LRU.
- [ ] The cache's current size is visible to the user.
- [ ] Proxies are never prefetched for an entire card automatically.
- [ ] Transfer, ingest, and representation states are distinguishable in the UI.
- [ ] No surface presents a "safe to format" state.
- [ ] Nothing is written, renamed, or deleted on the SD card.

## Verification

Physical device, real card, real reader, real camera media, and a bandwidth-limited
network. Simulator and fast-LAN testing cannot validate this task. Test across an actual
network change (wifi to cellular), an actual force-quit, and an actual card removal. Verify
card read-only behavior with a full checksum manifest before and after. Measure peak phone
storage during a multi-gigabyte upload and assert it against the declared budget.

## Handoff notes

T9 exercises this across multiple simulated nights. Anything this task leaves as
best-effort — particularly background continuation — must be documented as such so T9 tests
the promised behavior rather than the hoped-for behavior.
