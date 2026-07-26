# Task T3: Archive-to-Library Ingest and Content Dedupe

**Status:** Planned
**Depends on:** T2, companion-server Task 5
**Unblocks:** T8

## Objective

Register archived originals into the existing global media library as already-verified
media objects, deduplicate content that the library already holds, and enqueue
representation work — without scheduling a redundant hashing pass.

## User outcome

A clip that finishes uploading becomes a real library media object within seconds, and
footage the archive already contains is recognized instead of stored twice.

## Scope

- Transition transfer objects from `archived` to `ingested`.
- Register the archived file as a storage object beneath the archive root.
- Create or attach a media object using the already-verified full-file BLAKE3.
- Probe metadata through the existing media-facts path.
- Deduplicate against existing verified content in the library.
- Enqueue thumbnail and mobile-proxy jobs through the existing durable job engine.
- Report ingest outcome and errors through transfer diagnostics.

## Out of scope

- The mobile proxy recipe itself (T4)
- Project asset creation and linking (T7)
- Re-scanning or re-hashing existing scan roots
- Deletion or cleanup of duplicate archive files beyond declining to write them
- Perceptual or near-duplicate detection

## Verified-on-arrival ingest

The scan pipeline's normal flow computes a sampled quick fingerprint first and promotes to
a verified identity through a background full-hash job. A transfer-committed file has
already been hashed end to end by both the phone and the server before commit.

It must therefore enter the library **directly in the verified state**, and must not
schedule a full-hash job. Scheduling one would re-read every archived byte from the NAS for
no information gain, which on a travel-sized card is hours of pointless I/O.

## Dedupe

Before committing, the transfer pipeline checks the declared full-file hash against the
library's verified content.

| Situation | Behavior |
| --- | --- |
| Hash unknown to the library | Commit and register normally |
| Hash already verified in the library | Skip the archive write, attach the existing media object, mark the object `ingested` |
| Hash matches content that is currently unavailable | Attach the existing media object; record the new archive location as an additional storage observation |

Dedupe is exact-content only. Two visually similar clips are two media objects.

Because the phone declares the hash at `complete` time rather than at declare time, dedupe
short-circuits the *commit*, not the upload. A future optimization could declare hashes at
inventory time to skip the upload entirely; it is deliberately not in scope here, because
it would require the phone to read every byte of the card before uploading anything.

## Design requirements

1. Ingest is idempotent. Replaying it for an already-ingested object changes nothing.
2. Ingest runs after archive commit and never partially registers a file that failed to commit.
3. Registration reuses existing library identity code rather than reimplementing it.
4. No full-hash verification job is scheduled for transfer-ingested media.
5. Metadata probing failures mark the object ingested with a probe error, not failed —
   the bytes are safely archived and probing is retryable.
6. Representation jobs are enqueued per file as it lands, not batched per card.
7. Representation job enqueue failures are retryable and do not roll back ingest.
8. A deduped object leaves no orphan file beneath the archive root.
9. Ingest errors are visible in transfer diagnostics with stable structured codes.
10. Ingest work is bounded so a large card cannot saturate the job engine and starve
    interactive work.

## Likely implementation areas

- `core/rcdvlog-server/src/transfer.rs` ingest hook and state transition
- `core/rcdvlog-server/src/library.rs` verified-on-arrival registration path
- `core/rcdvlog-server/src/media_facts.rs` probe invocation
- `core/rcdvlog-server/src/representations.rs` job enqueue for freshly archived media
- Integration tests covering dedupe and idempotent replay

## Acceptance criteria

- [ ] A committed archive file registers as a media object in the verified state.
- [ ] No full-hash verification job is scheduled for transfer-ingested media.
- [ ] Metadata is probed and recorded through the existing media-facts path.
- [ ] Replaying ingest for an ingested object is a no-op.
- [ ] Uploading content the library already holds verified skips the archive write.
- [ ] A deduped upload attaches the existing media object and leaves no orphan file.
- [ ] Dedupe works when the existing copy is on an unavailable mount.
- [ ] Thumbnail and mobile-proxy jobs are enqueued per file as it lands.
- [ ] A probe failure marks the object ingested with a retryable probe error.
- [ ] A job-enqueue failure does not roll back ingest and is retryable.
- [ ] Ingesting a card's worth of files does not starve interactive server work.
- [ ] No scan root is read, written, or re-hashed by this path.

## Verification

Use generated media with known hashes. Assert absence of full-hash jobs by inspecting the
job queue, not by timing. Test dedupe against a library seeded through the ordinary scan
path to prove the two paths agree on identity. Test idempotency by replaying the ingest
hook directly. Verify bounded concurrency by enqueuing several hundred objects and
observing that interactive requests remain responsive.

## Handoff notes

T8 surfaces ingest state on the phone, so the diagnostics shape defined here becomes a
user-visible contract. Include enough detail to distinguish "waiting to ingest", "ingested,
probe failed", and "ingested, representations pending".
