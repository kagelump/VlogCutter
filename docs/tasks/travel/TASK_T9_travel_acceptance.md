# Task T9: End-to-End Travel Acceptance

**Status:** Planned
**Depends on:** T8
**Unblocks:** — (epic completion)

## Objective

Prove the travel workflow end to end under conditions resembling an actual trip: real
camera media, a constrained and unreliable network, multiple interrupted sessions, and a
return-home reconciliation.

## User outcome

The user can trust the workflow enough to actually leave the laptop at home.

## Scope

- A scripted multi-night trip simulation with real media and a throttled network.
- Return-home reconciliation acceptance.
- Security acceptance for the transfer surface.
- Scale acceptance for a travel-sized card.
- Documentation of measured throughput and realistic expectations.
- Recording of every behavior that remains best-effort.

## Out of scope

- Automating physical card insertion and removal
- Performance optimization work discovered during acceptance (file it, do not fix it here)
- Compute-first enrichment features

## Trip simulation

The scenario must include, in order:

1. Create a trip project from the phone with no Mac involved.
2. Insert a card with real camera media from at least two camera models.
3. Inventory it and confirm assets appear on phone, web, and desktop before any upload.
4. Write notes against clips that have not uploaded.
5. Start uploading under a throttled connection with a priority policy.
6. Interrupt: kill the network mid-chunk.
7. Interrupt: force-quit the app.
8. Interrupt: restart the server.
9. Interrupt: remove the card mid-transfer, then reinsert it.
10. Resume and confirm no committed chunk is retransmitted.
11. Review arrived clips through mobile proxies, including with the network disabled.
12. Shoot more footage onto the same card and re-inventory.
13. End the trip with uploads deliberately incomplete.
14. Return home, adopt the project on the desktop, and ingest the card locally.
15. Confirm uploaded media is not re-transferred and un-uploaded media links to its
    waiting provisional assets.
16. Confirm every note written during the trip is intact and attached to the right clip.

## Security acceptance

- [ ] Transfer endpoints reject requests without a valid device credential.
- [ ] Transfer endpoints are unreachable from outside the tailnet.
- [ ] The server listener remains loopback-only.
- [ ] Revoking the phone's credential immediately stops in-flight transfers.
- [ ] No credential appears in any URL, log line, or persisted request.
- [ ] Adversarial `cardRelativePath` values cannot escape the archive root.
- [ ] Non-transfer routes still enforce the 64 MB body limit.

## Scale acceptance

- [ ] A card with thousands of files inventories without exhausting phone memory.
- [ ] The server's resume query stays responsive for a card with thousands of objects.
- [ ] Ingesting a card's worth of files does not starve interactive server work.
- [ ] Project lists and asset lists remain responsive on the phone at that scale.

## Data-safety acceptance

- [ ] A full checksum manifest of the SD card is identical before and after the entire cycle.
- [ ] No file under any scan root is written, renamed, or deleted.
- [ ] No partial file ever appears beneath the archive root.
- [ ] No staging file is written to the NAS.
- [ ] Every archived file's hash matches the phone's declaration and the card's content.
- [ ] No surface presented a "safe to format" state at any point.

## Measurements to record

Acceptance must record, not merely assert:

| Measurement | Why |
| --- | --- |
| Sustained upload throughput at several throttled rates | Sets honest expectations for hotel wifi |
| Wall-clock time to inventory a large card | The user waits on this before annotating |
| Time from upload completion to reviewable mobile proxy | The core "annotate while uploading" promise |
| Mobile proxy size ratio versus the 720p proxy | Validates the bandwidth rationale |
| Peak phone storage during a large upload | Validates the no-full-copy design |
| Resume overhead after each interruption class | Validates the chunk ledger |

Publish these in the documentation so the workflow's real limits are stated rather than
discovered on a trip.

## Best-effort behaviors to document

Every behavior that is best-effort rather than guaranteed must be recorded explicitly,
with the observed reality rather than the intent. At minimum:

- iOS background upload continuation
- Behavior when the phone's screen locks during a transfer
- Behavior when iOS reclaims the external volume
- Behavior on a network that permits connection but throttles to near-zero

T9 tests the promised behavior, not the hoped-for behavior. If background continuation does
not survive an hour of backgrounding, that is the documented result.

## Acceptance criteria

- [ ] The full trip simulation completes with every interruption class exercised.
- [ ] No committed chunk is ever retransmitted after any interruption.
- [ ] Notes written before upload are intact and correctly attached after return.
- [ ] Return-home ingest re-transfers nothing that already uploaded.
- [ ] Un-uploaded media links to its waiting provisional assets on the desktop.
- [ ] Every security, scale, and data-safety criterion above passes.
- [ ] All measurements are recorded in the documentation.
- [ ] Every best-effort behavior is documented with observed results.
- [ ] Existing desktop offline behavior, keyboard contract, and project behavior do not regress.
- [ ] Existing web behavior does not regress.

## Verification

Manual, on real hardware: a physical iPhone, a physical card reader, real cards, the actual
Ubuntu server, and the actual NAS. Network throttling via a router-level or proxy-level
shaper, not by simulating latency in application code. Use disposable projects and a
scratch archive root; the real archive is never an acceptance destination.

The read-only real-media fixture is enumerated read-only only, exactly as with existing
acceptance.

## Handoff notes

Findings that are defects go back to the owning task. Findings that are limitations go into
the documentation. Do not fix performance problems inside acceptance — file them, because
an acceptance task that also changes behavior cannot honestly report on it.

Compute-first enrichment — transcription, scene extraction, visual summaries, chronology
suggestion — becomes reasonable to plan only once this task passes. Planning it earlier
would be speculative.
