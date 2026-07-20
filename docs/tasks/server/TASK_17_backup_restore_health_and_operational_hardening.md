# Task 17: Backup, Restore, Health, and Operational Hardening

**Status:** Planned
**Depends on:** Tasks 2, 8, and 9
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

## Verification

Run backup/restore tests entirely in temporary directories. Perform a disposable Compose restart and restore drill. Inspect backup permissions, manifest contents, and a generated diagnostic bundle manually.

## Handoff notes

Task 18 exposes read-only status and deliberately named admin mutations through stable JSON. Task 19 repeats the restore drill on the Ubuntu target.
