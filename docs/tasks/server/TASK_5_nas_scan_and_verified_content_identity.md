# Task 5: NAS Scan and Verified Content Identity

**Status:** Planned
**Depends on:** Tasks 3 and 4
**Unblocks:** Tasks 6, 14, 16, and 18

## Objective

Implement manual and scheduled server scans that populate the global library, extract technical metadata, and safely promote quick fingerprint candidates to full-file-verified media identities.

## User outcome

The Ubuntu server can index configured NAS folders in place, show progress and per-file failures, recognize media it has already seen, and establish trustworthy reusable content identities without modifying originals.

## Scope

- Add versioned handlers for root scan, media probe, quick fingerprint, and full-file BLAKE3 jobs.
- Add manual scan and configurable scheduled scan domain commands.
- Traverse only configured storage-root scopes and supported media extensions.
- Persist scan checkpoints/generations and incremental discovery metadata.
- Compute current sampled BLAKE3 plus file size for candidate matching.
- Compute full-file BLAKE3 in a background job before cross-location permanent merge.
- Probe media with ffprobe and supplement camera make/model through ExifTool with graceful degradation.
- Reconcile new, unchanged, unavailable, changed, moved/reappeared, duplicate, mismatched, and failed files.
- Emit paginated library records, progress, ETA, and per-file diagnostic evidence.

## Out of scope

- Creating project membership automatically
- Proxy or thumbnail generation
- Filesystem watcher
- Original relocation, rename, archive, deletion, or marker files
- Browser scan administration
- Scanning arbitrary API-supplied paths outside configured bindings

## Scan behavior

1. Resolve and validate a configured server root binding.
2. Create an observation generation for the requested full root or sub-scope.
3. Walk without following symlinks and enqueue discovered files incrementally.
4. Use size, modification metadata, and prior observation to skip provably unchanged work.
5. Compute quick fingerprint for new/changed candidates.
6. Register provisional media/storage identity and technical metadata.
7. Enqueue full hash where verified content identity is absent or ambiguous.
8. Complete the observation generation only after discovery bookkeeping is durable.
9. Mark prior unseen observations unavailable only for a successfully completed scope.

Full hashing records size and modification metadata before and after reading. A changing input produces a retryable `input_changed` result rather than a false identity.

## Design requirements

1. Original roots are opened read-only and never used for temp files.
2. The scanner never follows symlinks and validates containment again before every file job.
3. Supported extensions stay aligned with the Rust core unless intentionally versioned.
4. One bad file does not abort the scan.
5. ExifTool absence/failure degrades camera enrichment but not ffprobe-successful ingest.
6. Quick matches are candidates, not permanent cross-location merges.
7. Full hash plus size is required to merge files from different storage objects.
8. A full-hash mismatch splits or preserves separate media objects and records warning evidence.
9. The same logical root and relative path remains one storage object across Mac/Linux observations.
10. A rescan may discover the same full-hash media at a new path and preserve the old location as unavailable; it does not mutate the original or require explicit rename inference.
11. Scan scheduling is persisted, timezone-explicit, and disabled by default until configured.
12. Scan and hash concurrency follow Task 4 limits and remain configurable.
13. Full hash and probe results are immutable observations tied to file evidence and tool/version provenance.

## Likely implementation areas

- `core/rcdvlog-server` scanner, fingerprint handlers, schedules, and library reconciliation
- Shared probe/fingerprint primitives extracted from `core/rcdvlog-core`
- `apps/server` worker registration and internal routes
- Control-catalog migrations for scan checkpoints and media facts
- Real-media integration fixtures

## Acceptance criteria

- [ ] Manual scan imports supported media recursively from a configured temporary root.
- [ ] Repeating an unchanged scan is idempotent and avoids unnecessary full probes/hashes.
- [ ] Unsupported files, directories, and symlinks are skipped safely.
- [ ] Paths with spaces and non-ASCII filenames index correctly.
- [ ] Per-file probe/hash failures are recorded while other files succeed.
- [ ] Quick-equal/full-hash-equal files converge to one media object with two storage locations.
- [ ] A forced quick-fingerprint collision with different full content remains two media objects and surfaces a mismatch warning.
- [ ] A file changed during hashing does not commit a full hash.
- [ ] An interrupted scan generation does not mark previously known files missing.
- [ ] A later successful rescan changes node-relative availability without deleting records.
- [ ] ExifTool failure preserves ffprobe-derived metadata.
- [ ] Scheduled scans survive server restart and do not overlap the same root/scope unnecessarily.
- [ ] A read-only fixture root has no changed directory entries, metadata, or contents after scanning.

## Verification

Use generated media under temporary directories for automated tests. Include a scale test with thousands of empty metadata records and a smaller set of real FFmpeg media files. Do not point automated tests at `/Volumes` or a real NAS.

Manual Ubuntu acceptance enumerates the chosen NAS mount read-only before launching a small-scope scan.

## Handoff notes

Expose stable library query, verification, and scan-command services for media generation, the web app, and CLI diagnostics. Record task/recipe version keys for throughput history.
