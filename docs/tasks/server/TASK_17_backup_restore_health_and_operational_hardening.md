# Task 17: Backup, Restore, Health, and Operational Hardening

**Status:** Planned
**Depends on:** Tasks 2, 4, 8, and 9
**Unblocks:** Tasks 18 and 19

## Objective

Make the server recoverable and operable through verified database backups, explicit safe restore, deeper readiness/health, bounded retention, and restart-aware deployment behavior.

## User outcome

The home node can restart unattended, reports when storage or databases need attention, creates verified scheduled backups, and can be restored through an explicit process without risking the only live copy.

## Scope

- Add scheduled and manual backup jobs for the control catalog and all registered project replicas.
- Use SQLite online backup API or an equivalently consistent live snapshot mechanism.
- Add backup manifests with schema/protocol versions, per-project sync cursors, sizes, and checksums.
- Verify completed databases with integrity checks before marking a backup successful.
- Add configurable daily/weekly retention with defaults of 14 daily and 8 weekly restore points.
- Add local-only explicit restore inspection and execution commands.
- Add recovery/reseed diagnostics for missing project replicas and rebuildable library/representations.
- Expand health/readiness for databases, schema, disk space, mounts, derived root, backup root, workers, and clock sanity.
- Add Docker restart policy, graceful stop timeout, log rotation guidance, and startup recovery checks.
- Add redacted diagnostic bundle generation.

## Out of scope

- Backing up representation blobs by default
- Automatic destructive restore/failover
- Cloud backup service integration
- High availability or hot standby
- Original-media backup
- Browser restore button

## Backup set

A restore point contains:

- Control database snapshot
- One snapshot for every registered project replica
- Manifest with server ID, application/protocol/schema versions, project IDs, operation cursors, timestamps, sizes, and checksums
- Redacted configuration necessary to explain mount/root identities

It excludes plaintext credentials, one-time codes, original media, temp files, and derived blobs. Credential verifiers may remain in the control database, but restored sessions/devices can be explicitly revoked after recovery.

## Restore and synchronization identity

A restore rolls a project replica backward in time while the Mac keeps its full history and
its cursors. That interacts badly with the operation log, and the interaction must be handled
here rather than discovered during acceptance.

Server-origin sequences are allocated per replica from `operation_origin_sequences`. A
restored replica's counter is therefore behind the highest server-origin sequence the Mac has
already committed. If the restored server allocates from the stale counter, it emits a
*different* operation carrying an already-acknowledged `(originNodeId, originSequence)`, and
`Catalog::apply_operation_batch` on the Mac rejects the whole batch with "already acknowledged
with a different operation". Synchronization for that project wedges permanently and no
retry clears it.

The Mac-origin direction self-heals: the restored server's receive cursor for the Mac origin
also moves backward, and the Mac still holds those operations and resends them.

Required behavior after a restore is activated:

1. The restored replica is marked `sequence_recovery` for every paired origin. The server
   serves reads and accepts Mac operations but emits no new server-origin operation until
   recovery completes.
2. The first reconciliation learns the Mac's highest committed server-origin sequence, and the
   server advances its local allocator past that value before its next allocation. A restore
   point's manifest records the same watermark so recovery can also complete offline.
3. If the Mac reports a committed server-origin sequence the restored replica does not
   contain, the lost operations are surfaced as an explicit divergence with the affected
   range. They are not silently skipped, and the cursor is not force-advanced without a
   recorded decision.
4. A restored replica never reuses a `(originNodeId, originSequence)` pair for different
   content, and this is asserted by test rather than left to convention.
5. Restore also revalidates representation locations. A manifest whose blob is absent or
   size-mismatched after restore becomes unavailable and regenerable; it is never served as a
   successful response.

## Design requirements

1. Never copy a live WAL database with an ordinary filesystem copy.
2. Backup jobs write into a new staging directory and atomically publish the completed manifest last.
3. A partial/failed backup is not considered a restore point.
4. Retention operates only within the explicitly configured backup root and only on validated RCD backup directories.
5. Restore first validates manifest, checksums, schema compatibility, and SQLite integrity.
6. Restore writes to a new data path. Switching active data requires an offline explicit confirmation and preserves the previous data directory for recovery.
7. No restore command accepts `/`, a home directory, workspace root, original-media root, or unresolved variables as a destructive target.
8. Backup-root outage does not stop ordinary project/server work but readiness/status reports backup degradation.
9. Low local-SSD space blocks new media work before it threatens database durability.
10. Health checks are bounded and do not run full media scans or expensive integrity checks on every request.
11. Readiness distinguishes core unavailability from degraded optional mounts/backups.
12. Diagnostic bundles redact credentials, cookies, tokens, and unnecessary absolute paths.
13. The online backup API restarts its copy when a writer commits mid-snapshot. Backup runs under a bounded attempt/duration budget and reports a retryable failure instead of looping forever against a busy replica; it never takes a long write lock that stalls sync or media work.
14. Backups run through the Task 4 durable job engine, so a backup surviving restart, cancellation, and retry uses the same lease and idempotency rules as other server work.
15. Wall-clock sanity is a bound on HLC intake, not only a health string. A wall time beyond the configured skew bound is flagged and does not permanently advance the hybrid logical clock.

## Likely implementation areas

- `core/rcdvlog-server` backup domain, retention, health projections, and recovery analysis
- `apps/server` scheduler handlers, health routes, and local admin subcommands
- Control-catalog migrations for backup manifests/status
- `deploy/server/compose.yaml` restart/health/logging/volume rules
- Temporary restore fixtures and tests

## Acceptance criteria

- [ ] Manual and scheduled backups capture the control catalog and every project replica consistently.
- [ ] Completed manifest checksums and SQLite integrity checks pass.
- [ ] A simulated crash during backup leaves no published valid restore point.
- [ ] Default 14-daily/8-weekly retention deletes only validated expired backup sets within the configured backup root.
- [ ] Representation blobs and originals are excluded by default.
- [ ] Restore inspection is read-only and reports versions, projects, cursors, size, and integrity.
- [ ] Restore to a new temporary data root produces a usable server with matching materialized project state.
- [ ] Active data is never overwritten automatically; the prior directory remains recoverable after an explicit switch.
- [ ] Low disk, unavailable mounts, corrupt database, expired leases, stale backup, and clock skew have distinct health diagnostics.
- [ ] Container restart recovers expired jobs and serves acknowledged project operations.
- [ ] Diagnostic bundle contains required evidence and no known secrets.
- [ ] A project replica can be identified as reseedable from the Mac without silently replacing it.
- [ ] A restored replica advances its server-origin allocator past the Mac's committed sequence before emitting any new operation, and a two-replica test proves sync still converges after restore.
- [ ] Operations lost by restoring an older replica are reported as an explicit divergence range rather than skipped.
- [ ] A backup taken while a replica is being written either completes consistently or fails retryably within its budget, and never blocks sync.
- [ ] A representation whose blob is missing after restore reports unavailable and regenerable instead of returning a broken response.

## Verification

Run backup/restore tests entirely in temporary directories. Perform a disposable Compose restart and restore drill. Inspect backup permissions, manifest contents, and a generated diagnostic bundle manually.

## Handoff notes

Task 18 exposes read-only status and deliberately named admin mutations through stable JSON. Task 19 repeats the restore drill on the Ubuntu target.
