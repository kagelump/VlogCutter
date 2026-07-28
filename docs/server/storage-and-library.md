# Storage roots, scans, and the global library

A logical storage root has one stable `storageRootId`; each node keeps its own absolute binding. Thus `/Volumes/Trip Archive` on macOS and `/media/originals` on Linux can describe the same root without an absolute path entering project operations or sync DTOs. A binding can cover the whole root or a declared `rootSubpath`.

The server's original roots are immutable. Compose bind-mounts them read-only, and domain APIs cannot create, rename, rewrite, or delete original files. Derived output and backups use separate writable roots. The future travel archive is planned as another logical root with a create-only write path; it is not implemented by the companion-server epic.

## Binding lifecycle and availability

Chrome administration can create/rename/disable logical roots and configure bindings only beneath `RCDVLOG_ALLOWED_ORIGINAL_MOUNTS`. It cannot browse arbitrary paths. A binding is usable only when root and binding are enabled, the mount is verified, association evidence is displayed, and the user explicitly confirms it. Immutable roots reject read-write bindings.

Availability is node-relative: `known`, `accessible_here`, `accessible_on_another_node`, `temporarily_unavailable`, `missing_after_completed_observation`, or `fingerprint_mismatch`. A partial mount affects only its subtree. Disabling a root or losing a mount preserves identity, observations, project membership, and representations.

## Scanning and content identity

Scans walk supported media below a confirmed root without following symlinks. Discovery creates durable child work for metadata probing, quick identity, and full identity. A completed observation generation publishes availability atomically; in-progress or abandoned generations do not make files disappear.

Quick identity uses sampled BLAKE3 evidence for fast candidate matching. Verified global identity requires full-file BLAKE3 plus byte length. Project membership is explicit; finding a global media object never silently adds it to a project. Chrome can preview and add verified library items to an adopted project, producing ordinary project operations that later synchronize to the Mac.

Manual scans require an idempotency key. Scheduled scans store an IANA time zone, coalesce downtime into at most one catch-up run, and never overlap the same active scope. Per-file failures remain inspectable without discarding successful work.

Use bounded CLI pages and focused diagnostics:

```bash
rcdvlog server --endpoint https://<server-host>:8443 storage list
rcdvlog server --endpoint https://<server-host>:8443 storage diagnose --root <root-id>
rcdvlog server --endpoint https://<server-host>:8443 scans request --root <root-id> --idempotency-key <unique-key>
rcdvlog server --endpoint https://<server-host>:8443 scans diagnose --scan <scan-id>
rcdvlog server --endpoint https://<server-host>:8443 library list --limit 50
rcdvlog server --endpoint https://<server-host>:8443 library diagnose --media-object <media-object-id>
```

The verified automated boundary covers generated media, missing mounts, path traversal/symlink escape, spaces/non-ASCII paths, restart/cancel behavior, a 2,000-row library, and a 2,000-row scan history. Physical NFS/SMB outage timing and NAS throughput remain unrun release-topology checks.
