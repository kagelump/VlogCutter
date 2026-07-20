# Task 20: Update All Documentation

**Status:** Planned
**Depends on:** Task 19
**Unblocks:** Companion-server epic completion

## Objective

Update every repository document and user/developer contract to describe the implemented companion server accurately, using Task 19 acceptance evidence and clearly separating completed behavior from deferred architecture.

## User outcome

A user or future agent can install, operate, diagnose, extend, and safely test the desktop/server system without relying on stale MVP assumptions or treating planned capabilities as implemented.

## Scope

- Audit and update `PRD.md` product decisions, current status, and future/deferred sections.
- Update `AGENTS.md` repository map, authority rules, invariants, current implementation/gaps, architecture, workflows, commands, test matrix, safety, and definition of done.
- Update `README.md` setup, supported topology, fast-start, and high-level capabilities.
- Update `docs/agent-cli.md` with final server/admin commands, JSON envelopes, fields, errors, progress, and read-only guarantees.
- Update `docs/batch-estimation.md` with server worker/task keys and node-scoped rate history.
- Update `docs/integration-testing.md` with automated/manual server, Chrome, LAN, Tailscale, NAS, backup, and security boundaries.
- Add focused deployment, synchronization, storage-root, job, API, security, backup/recovery, and troubleshooting documents where the implemented system needs them.
- Update checked-in CLI `--help`, example config, Compose comments, API schema descriptions, and UI help text.
- Reconcile `ARCHITECTURE.md`, `EPIC.md`, and task statuses with actual implementation and accepted deviations.
- Add links among documents and remove or mark superseded guidance.

## Out of scope

- Adding new product behavior under the guise of documentation
- Claiming unrun manual acceptance passed
- Rewriting historical notes as if they were current contracts
- Copying secrets, real absolute private paths, or volatile local output into examples

## Required documentation set

At minimum create or update focused documents for:

- Ubuntu/Docker Compose installation and upgrades
- NFS-preferred/SMB-supported host mounting and read-only original bind mounts
- LAN Caddy CA setup and Tailscale Serve setup
- Browser bootstrap, desktop pairing, credential rotation, and revocation
- Logical storage roots, node bindings, partial mounts, and availability
- Project baseline adoption, incremental sync, conflict behavior, and protocol compatibility
- Global library scanning and two-stage content identity
- Durable jobs, schedules, retry/cancel, recipes, and resource defaults
- Representation storage, Chrome streaming, and Mac proxy download
- Backup schedule, retention, inspection, restore, and recovery
- Server/web/desktop development and testing commands
- Troubleshooting and machine-readable diagnostics

## Design requirements

1. Current implementation facts and future plans are labeled distinctly.
2. User request and accepted companion-server decisions supersede older “future node/cross-device deferred” text where implementation is complete.
3. Original immutability, local SQLite placement, operation sync, and node-local paths are consistent in every document.
4. Commands, paths, environment variables, ports, schema/protocol/recipe versions, and error codes match checked-in behavior exactly.
5. Examples use placeholders or disposable paths and preserve spaces/non-ASCII handling.
6. No documentation tells users to copy live SQLite/WAL files, put catalogs on NAS, expose Funnel, disable security globally, or write into original roots.
7. Manual acceptance boundaries and known limitations remain explicit.
8. `AGENTS.md` remains an operational handoff rather than a narrative design document.
9. `README.md` stays concise and links to focused documents.
10. The architecture file records material implementation deviations and their rationale instead of silently rewriting accepted decisions.
11. Task files reflect final completion status only when their acceptance criteria genuinely passed.
12. Markdown links, headings, code blocks, and commands are validated.

## Documentation audit checklist

- [ ] Search for stale statements that Linux server, synchronization, durable jobs, web review, auth, or global media identity do not exist.
- [ ] Search for old single-path asset assumptions and project-local-only proxy assumptions.
- [ ] Search for old task keys/recipe versions and update only where implementation changed them.
- [ ] Search for direct `invoke`/HTTP examples that bypass the intended client/domain boundaries.
- [ ] Search for unsupported AI, edit-planning, trip-context, cleanup, multi-user, and original-transfer claims.
- [ ] Confirm repository tree diagrams include new apps, crates, packages, deploy files, and docs.
- [ ] Confirm setup covers both LAN and Tailscale, not one as an undocumented fallback.
- [ ] Confirm backup/restore warnings and destructive-action boundaries are prominent.
- [ ] Confirm all CLI examples use explicit catalog/server selection and stable JSON.
- [ ] Confirm testing tables distinguish mocks, automated processes, Chrome, Tauri, Ubuntu, NAS, and Tailscale.

## Acceptance criteria

- [ ] Every required repository document is audited and updated or explicitly confirmed unaffected.
- [ ] `PRD.md` and `AGENTS.md` accurately separate implemented server behavior from remaining gaps.
- [ ] A fresh Ubuntu setup can follow the deployment documentation without undocumented steps.
- [ ] A fresh Mac can trust LAN TLS, pair, adopt a project, and use Tailscale by following the docs.
- [ ] CLI/API/config examples pass against the final implementation or a checked validation script.
- [ ] Sync/conflict/storage/representation docs match code-level identities and semantics.
- [ ] Backup and restore documentation matches the verified Task 19 drill.
- [ ] No document claims an unrun test or deferred feature is complete.
- [ ] No secret or private real-project path is introduced.
- [ ] Markdown link and formatting checks pass.
- [ ] `rg` audit finds no known stale server-deferred or unsafe database/media guidance outside clearly historical context.
- [ ] `ARCHITECTURE.md`, `EPIC.md`, and all task statuses match the actual delivered scope.

## Verification

Run all documentation/command/schema generators and link checks added by the implementation. Re-run representative setup, CLI, Compose validation, and test commands exactly as documented. Have one review pass read only the docs and compare each implementation claim against Task 19 evidence.

## Epic completion

Only after this task passes may the companion-server epic be marked complete. If implementation deviates materially from `ARCHITECTURE.md`, record the accepted deviation, update dependent documentation, and leave unimplemented task criteria visibly incomplete.
