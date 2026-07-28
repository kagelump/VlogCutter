# Troubleshooting and diagnostics

Start with stable JSON and keep stored state, live observation, derived availability, warnings, and recommended mutations separate.

```bash
rcdvlog server --endpoint https://<server-host>:8443 status
rcdvlog server --endpoint https://<server-host>:8443 storage list
rcdvlog server --endpoint https://<server-host>:8443 scans list
rcdvlog server --endpoint https://<server-host>:8443 jobs list
rcdvlog server --endpoint https://<server-host>:8443 sync status
```

Use `library diagnose`, `storage diagnose`, `scans diagnose`, `jobs diagnose`, `sync diagnose`, `representations status`, and `backups diagnose` with the stable ID shown by a list command. Read-only diagnostics do not migrate, enqueue, hash broadly, repair, or create cache output. Remote errors include a `requestId` that can be correlated with server logs.

For desktop screenshots, begin with:

```bash
rcdvlog desktop last-opened
rcdvlog --catalog /absolute/project/catalog.sqlite3 catalog inspect
rcdvlog --catalog /absolute/project/catalog.sqlite3 project diagnose --project <project-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 clips diagnose --asset <asset-id>
```

The last-opened record is a persisted breadcrumb, not live IPC; compare its timestamp and IDs with the screenshot.

## Common symptoms

| Symptom | Evidence and action |
| --- | --- |
| `/ready` is `503` | Read its stable check codes. Verify local SSD free space, derived/backup permissions, declared original mounts, tool availability, clock sanity, and schema compatibility. `/health` only proves liveness. |
| `invalid_host`, `origin_mismatch`, or `invalid_forwarded_headers` | Check the Caddy/Tailscale hostname, proxy headers, and `RCDVLOG_LAN_HOSTNAME`; never disable validation or trust arbitrary proxy CIDRs. |
| `rate_limited` | Stop repeated credential exchanges and wait for the one-minute window. Confirm endpoint/Host and create a fresh one-time code if the old code expired. |
| Root stays unavailable | Verify it is beneath an allowed original mount, mounted read-only, enabled, verified, and explicitly confirmed with matching association evidence. |
| `source_not_verified` | Complete full hashing; quick identity alone cannot create a shared representation. |
| Proxy returns `404`/unavailable | Diagnose its manifest/location and source mount. Regenerate if the blob is absent or invalid; do not hand-edit a path. |
| Job remains running after a crash | Let the lease expire and restart recovery claim it. Use diagnose before retry/cancel; never edit job rows. |
| Sync is Attention | Diagnose per-origin gaps/conflicts, credential state, protocol/schema compatibility, and restore recovery. Do not force cursors. |
| Browser updates stop | Durable 5-second polling should recover from WebSocket loss. Check `/ready`, authentication, Origin, and the browser console before reloading. |
| NAS disconnects mid-work | Successful items remain committed. The current item fails/retries safely; restore the same logical binding and rerun/inspect the durable job. |

With the server stopped, create a redacted local bundle in a new output directory:

```bash
rcdvlog-server admin diagnostics bundle --output-directory /absolute/new/diagnostic-directory
```

Inspect it before sharing. It intentionally omits secrets and unnecessary absolute paths, but an operator should still treat operational metadata as private.
