# Backup and recovery

Backups contain a consistent online snapshot of the control catalog and every registered project replica, plus a manifest with versions, cursors, sizes, checksums, and redacted configuration. They exclude originals, derived blobs, temp files, plaintext credentials, and one-time codes. Defaults are one scheduled run per 86,400 seconds, 14 daily restore points, and 8 weekly restore points.

Backup jobs stage a new directory, use bounded SQLite online backup, verify integrity/checksums, and publish the manifest last. The copy is paced — a fixed number of pages per step, with a pause between steps — so a routine backup does not monopolise a live catalog. Pacing is abandoned when it stops paying: SQLite re-dirties pages written through another connection, so against a steadily-written catalog larger than one step the pause is long enough for the writer to undo each step and the copy never converges. After a run of steps that lowers no page count, the copy takes the whole database in one call instead. That briefly holds the source, which is the cost pacing was avoiding, but a backup that cannot finish is worse than one that briefly blocks. A partial set is not a restore point. Retention deletes only validated backup directories inside the configured backup root. Do not copy live SQLite or WAL files with filesystem tools.

Create or inspect backups remotely:

```bash
rcdvlog server --endpoint https://<server-host>:8443 backups create --idempotency-key <unique-key>
rcdvlog server --endpoint https://<server-host>:8443 backups list
rcdvlog server --endpoint https://<server-host>:8443 backups diagnose --backup <backup-id>
```

Restore is intentionally local, offline, and new-root-only:

1. Stop the server and preserve the current data directory.
2. Inspect the selected restore point under the configured backup root.
3. Apply it to a new, empty, explicit data root.
4. Start a disposable server against the new root and verify version/readiness, project state, and representation availability.
5. Switch deployment configuration only after that verification; never overwrite the old active directory.

```bash
rcdvlog-server admin restore inspect --restore-point /absolute/backup/restore-point
rcdvlog-server admin restore apply \
  --restore-point /absolute/backup/restore-point \
  --new-data-root /absolute/new-server-data \
  --confirm-new-data-root
```

After activation, adopted projects enter sequence recovery. Reconnect a Mac that retains its operation history. The server first replays exact contiguous retained server-origin operations and advances its allocator beyond acknowledged sequences before emitting anything new. Missing retained operations become an explicit divergence; do not edit cursors or accept an allocator advance until diagnostics prove that decision is appropriate. The deliberate command is:

```bash
rcdvlog-server admin recovery accept-sequence-divergence \
  --project-id <project-id> \
  --peer-node-id <mac-node-id> \
  --decision accept_allocator_advance
```

That decision can acknowledge lost server-origin edits, so preserve the old data root and inspect both replicas first. Missing or size-mismatched representation blobs become unavailable/regenerable rather than falsely ready.

Task 19 automated a pre-operation backup, backward restore, exact retained-operation replay, and a new server operation at sequence 2. The restore was not repeated in Docker Compose on the target Ubuntu/NAS topology.
