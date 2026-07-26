# Epic: Local-First Companion Server

**Status:** Planned
**Architecture:** [ARCHITECTURE.md](ARCHITECTURE.md)
**Owner:** RCD VlogCutter
**Target:** One Mac, one Ubuntu server, one user, Chrome

## Outcome

Deliver a companion server that acts as the durable origin-like replica for each desktop project, indexes immutable NAS media into a reusable global library, runs restart-safe media jobs, synchronizes current project state without blocking offline desktop work, and serves a secure Chrome review application over both the home LAN and Tailscale.

The completed epic must feel like one project appearing on two surfaces. The server is not a required backend for desktop saves, and the desktop is not a remote control for a server-owned project.

## User-visible capabilities

At epic completion, the user can:

1. Deploy RCD VlogCutter Server on the Ubuntu laptop with local-SSD state, read-only NAS originals, and a dedicated derived-media root.
2. Open the service in Chrome through trusted LAN HTTPS at home or Tailscale HTTPS away.
3. Pair the one Mac using a revocable device credential.
4. Configure and explicitly associate the Mac and server's differently mounted views of the same logical NAS root.
5. Scan configured NAS roots into a paginated global media library without modifying originals.
6. See quick-match candidates progress to full-hash-verified content identities.
7. Add verified server-library media to an existing desktop-created project.
8. Generate quality-1 thumbnails and current H.264 review proxies through a durable, restart-safe server queue.
9. Pair an existing desktop project after reviewing a baseline import preview.
10. Work on the desktop while offline and reconcile automatically after reconnecting.
11. Browse a replicated project, play server proxies, edit notes and labels, and remove or restore assets in Chrome.
12. See accurate node-relative original/proxy availability.
13. Download an explicitly requested server proxy to the Mac's local project cache.
14. Queue a Mac-targeted representation request when an original is available only to the Mac.
15. Monitor scans, jobs, synchronization, storage, and credentials through the web UI, and inspect all of those plus backups through JSON CLI.
16. Recover from server restart, expired work leases, interrupted synchronization, and a verified database backup.

## In scope

- Global server media library with project-scoped asset membership
- One server replica per desktop project
- Formal project and server schema migrations
- Sampled quick fingerprint plus background full-file BLAKE3 verification
- Logical storage roots, node-local mount bindings, and partial-root support
- Manual and scheduled NAS scans
- Durable server jobs with leases, retry, cancellation, progress, and ETA
- Existing quality-1 thumbnail and H.264 720p proxy recipes
- Immutable project operations and per-origin synchronization cursors
- Initial baseline adoption and continuous automatic reconciliation
- Current annotations: notes, free-form labels, `removed`, and rotation
- Synchronization of current fixed clock events, but no web chronology editor
- Server-origin add-to-project operation
- Authenticated media range delivery to Chrome
- Explicit Mac proxy download and minimal Mac-targeted job requests
- Chrome web application and incrementally shared React review components
- Bootstrap, browser sessions, desktop device credentials, and revocation
- LAN HTTPS through Caddy and tailnet HTTPS through Tailscale Serve
- Structured logs, health/readiness, backups, restore, and JSON diagnostics
- Automated and manual acceptance coverage
- Repository-wide documentation update as the final task

## Out of scope

- Additional users, Macs, servers, or peer-to-peer synchronization
- Public hosting or Tailscale Funnel
- Offline browser editing
- Web project creation or any project deletion
- Original upload, archive transfer, rename, deletion, or cleanup (original upload and
  archive transfer are taken up by the [Travel Ingest epic](../travel/EPIC.md))
- Bulk offline preparation and cache policy
- Durable local Mac worker queue or competitive cross-node scheduling
- Web chronology editing or conflict resolution
- Review-status/favorite states not currently in the product
- Trip context, chapters, events, scripts, edit plans, and Resolve export
- Contact sheets beyond quality-1 thumbnails
- Transcription, VLM, embeddings, semantic search, and other AI analysis
- Hardware transcoding
- MCP server

## Architectural constraints

- Originals are immutable and server original mounts are read-only.
- SQLite is canonical local state on each node, and live databases stay on local SSDs.
- SQLite files and SQLite changesets are not the synchronization protocol.
- Rust domain commands are shared by desktop, server, web adapters, and CLI.
- The desktop never waits for a network round trip to commit an ordinary local edit.
- The server never connects inbound to the Mac.
- The browser is online-only and talks only to the server.
- Absolute paths are node-local. Protocol state uses logical root IDs and relative paths.
- Cross-location content merges require verified full-file hashes.
- A representation is immutable and identified by verified input plus versioned recipe.
- Network retries, scan retries, job retries, and baseline retries are idempotent.
- Existing catalogs migrate forward; deletion and rebuild are not migration strategies.
- Read-only diagnostics must not migrate, scan, hash, generate media, or repair state.

## Delivery plan

### Foundation

| Task | Deliverable | Depends on |
| --- | --- | --- |
| [1](TASK_1_schema_and_domain_foundation.md) | Schema and domain foundation | — |
| [2](TASK_2_server_workspace_and_runtime.md) | Server workspace and runtime | 1 |
| [3](TASK_3_storage_roots_and_global_media_library.md) | Storage roots and global media library | 1, 2 |
| [4](TASK_4_durable_server_job_engine.md) | Durable server job engine | 2 |

### Archive and representations

| Task | Deliverable | Depends on |
| --- | --- | --- |
| [5](TASK_5_nas_scan_and_verified_content_identity.md) | NAS scanning and verified content identity | 3, 4 |
| [6](TASK_6_representation_store_media_jobs_and_streaming.md) | Representation store, media jobs, and streaming | 4, 5 |

### Project replication

| Task | Deliverable | Depends on |
| --- | --- | --- |
| [7](TASK_7_operation_backed_project_mutations.md) | Operation-backed project mutations | 1 |
| [8](TASK_8_sync_protocol_and_server_project_replicas.md) | Sync protocol and server project replicas | 2, 3, 7 |
| [9](TASK_9_authentication_and_secure_transports.md) | Authentication and secure LAN/Tailscale transport | 2 |
| [10](TASK_10_desktop_pairing_root_mapping_and_baseline.md) | Desktop pairing, root mapping, and baseline adoption | 8, 9 |
| [11](TASK_11_automatic_sync_and_conflict_handling.md) | Automatic incremental sync and conflict handling | 8, 10 |
| [12](TASK_12_remote_representation_exchange_and_mac_requests.md) | Remote representation exchange and Mac-targeted requests | 6, 11 |

### Chrome web product

| Task | Deliverable | Depends on |
| --- | --- | --- |
| [13](TASK_13_web_foundation_typed_client_and_shared_ui.md) | Web foundation, typed client, and shared UI seam | 9 |
| [14](TASK_14_web_library_project_browse_and_add.md) | Library/project browsing and add-to-project | 5, 8, 13 |
| [15](TASK_15_web_proxy_review_and_annotations.md) | Proxy review and annotation editing | 6, 7, 11, 13, 14 |
| [16](TASK_16_web_jobs_storage_sync_and_credential_admin.md) | Jobs, storage, sync, credential, and schedule administration | 4, 5, 9, 11, 13 |

### Operations and completion

| Task | Deliverable | Depends on |
| --- | --- | --- |
| [17](TASK_17_backup_restore_health_and_operational_hardening.md) | Backup, restore, health, and operational hardening | 2, 8, 9 |
| [18](TASK_18_cli_and_machine_readable_diagnostics.md) | CLI and machine-readable diagnostics | 5, 6, 8, 11, 17 |
| [19](TASK_19_end_to_end_security_scale_and_deployment_acceptance.md) | End-to-end, security, scale, and deployment acceptance | 12, 15, 16, 17, 18 |
| [20](TASK_20_update_all_documentation.md) | Repository-wide documentation update | 19 |

Tasks should land in dependency order. Tasks on separate branches may proceed concurrently only after their declared prerequisites have stabilized their contracts.

### Follow-on: Travel Ingest

The [Travel Ingest epic](../travel/EPIC.md) extends this work with phone-carried SD-card
ingest, a writable NAS archive root, a resumable original-media transfer protocol, and
server-origin project creation. It builds on this epic rather than replacing any part of it.

| Travel task | Depends on this epic's |
| --- | --- |
| [T1](../travel/TASK_T1_writable_archive_root_and_commit_path.md) Writable archive root | 3, 5 |
| [T2](../travel/TASK_T2_resumable_transfer_protocol.md) Resumable transfer protocol | 4 |
| [T3](../travel/TASK_T3_archive_ingest_and_dedupe.md) Archive ingest and dedupe | 5 |
| [T4](../travel/TASK_T4_mobile_proxy_recipe.md) Mobile proxy recipe | 6 |
| [T5](../travel/TASK_T5_server_origin_projects_and_desktop_adoption.md) Server-origin projects | 7, 8, **10** |
| [T6](../travel/TASK_T6_mobile_app_foundation.md) Mobile app foundation | 9 |

Two decisions in that epic amend commitments made here, and are recorded in
[its architecture document](../travel/ARCHITECTURE.md):

- **Project creation is no longer desktop-only.** Projects may be created on the desktop or
  the server; each project has one replica per node that has adopted it. Project deletion
  remains out of scope everywhere. Task 10's baseline machinery is reused in reverse.
- **A writable storage root is introduced.** It is a distinct logical root with
  `immutable_original: false` and a create-only write path. Existing NAS scan roots remain
  immutable and read-only, and the enforcement in `library.rs` is unchanged.

Task 10 is the one genuine prerequisite gap: travel T5 cannot start until it lands.

Several task files below carry stale `Status:` markers — Tasks 1–8, 11, 12, and 13 are
implemented in whole or in part despite being marked Planned, and Task 9's authentication
core exists. Trust the code and git history until Task 20 reconciles them.

## Milestones

### Milestone A: Server sees the archive

Tasks 1–5 complete.

Exit criteria:

- The server starts from Compose and stores state on its SSD.
- Configured NAS roots cannot be written through the container.
- A scan survives restart and populates a paginated media library.
- Quick identity and full-hash verification are visible and tested.

### Milestone B: Server produces reusable review media

Task 6 complete.

Exit criteria:

- Current thumbnail and proxy recipes run through durable jobs.
- Outputs are validated, atomically committed, and identified by content/recipe.
- Chrome-compatible byte-range delivery passes integration tests.

### Milestone C: One project converges across Mac and server

Tasks 7–12 complete.

Exit criteria:

- Existing catalogs upgrade without data loss.
- Pairing shows and imports an explicit baseline.
- Desktop saves remain local while offline.
- Notes, labels, removal, rotation, clock events, asset additions, observations, and representation manifests converge after reconnect.
- Duplicate delivery and interrupted sync do not duplicate state.
- The Mac can fetch a server proxy and can receive a pending Mac-targeted request.

### Milestone D: Chrome is a useful secondary surface

Tasks 13–16 complete.

Exit criteria:

- The user can browse the global library and project assets in Chrome.
- The user can add library media to an existing project.
- Proxy seeking, note/label edits, and remove/restore work.
- Storage, jobs, schedules, sync, sessions, and credentials have usable administration views.

### Milestone E: Operable and documented release

Tasks 17–20 complete.

Exit criteria:

- Backups are verified and a disposable restore drill succeeds.
- CLI diagnostics are stable JSON and read-only where promised.
- Automated gates and the Ubuntu/LAN/Tailscale/NAS manual matrix pass.
- All repository documentation distinguishes implemented behavior from deferred work.

## Cross-cutting acceptance criteria

- [ ] No test or implementation writes, renames, or deletes source media.
- [ ] Original NAS roots are bind-mounted read-only in the supported deployment.
- [ ] No live SQLite file resides on or is synchronized through the NAS.
- [ ] The same logical NAS root resolves through different Mac and Linux paths.
- [ ] A sampled-fingerprint collision cannot merge distinct content after full verification.
- [ ] A project asset remains useful when its original is remote-only.
- [ ] Desktop annotation writes succeed while the server is unavailable.
- [ ] Sync is idempotent across timeout/retry and converges after offline edits.
- [ ] Unsafe chronology concurrency is surfaced rather than silently resolved.
- [ ] Server restart does not lose acknowledged operations or completed job results.
- [ ] Chrome can seek within a proxy without downloading the entire file first.
- [ ] LAN and Tailscale routes require application authentication and HTTPS.
- [ ] Secrets and absolute paths are not exposed unnecessarily through UI, logs, or URLs.
- [ ] APIs and CLIs use stable error codes and machine-readable contracts.
- [ ] Collections remain responsive with thousands of assets.
- [ ] Existing desktop keyboard, focus, offline playback, and project behavior do not regress.

## Testing gates

Each task runs its narrowest relevant checks while iterating. Before the epic is complete:

```bash
pnpm --dir apps/desktop build
pnpm --dir apps/web build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm test:integration
pnpm test:web:e2e
docker compose -f deploy/server/compose.yaml config
```

Exact script names may be introduced by the task that owns them, but the final documentation must describe the checked-in commands exactly.

Manual acceptance must use disposable catalogs and generated representations. The real NAS fixture is always enumerated read-only first and is never a test-output destination.

## Primary risks and mitigations

| Risk | Mitigation |
| --- | --- |
| Retrofitting sync onto state-replacement mutations | Task 7 introduces typed operations and compatibility adapters before network sync |
| Project/global-library identity confusion | Separate `projectAssetId`, `mediaObjectId`, and storage objects; verify cross-location content fully |
| Different NAS mount paths | Logical roots plus node-local bindings and partial-root prefixes |
| Unreliable network delivery | Per-origin sequences, idempotent IDs, bounded reconciliation, HTTP recovery from missed WebSockets |
| Server laptop resource saturation | One proxy and one hash worker by default; bounded scan/probe concurrency |
| NAS outage interpreted as deletion | Generation-based observations and explicit unavailable state |
| Authentication bypass through alternate route | Same app credentials on LAN and Tailscale; private backend; strict trusted-proxy/origin configuration |
| Server disk loss | Verified scheduled database backups plus reseed/rescan recovery paths |
| Large UI rewrite | Extract shared client/UI seams incrementally; keep separate web shell |
| Documentation becoming aspirational | Final task audits behavior and records unimplemented gaps explicitly |

## Definition of done

This epic is complete only when all 20 task files satisfy their acceptance criteria, Task 19 records the automated and manual acceptance evidence, and Task 20 updates the full repository documentation to match the implementation. A working browser mock, an unverified schema migration, or a server that requires the Mac to remain online does not constitute completion.
