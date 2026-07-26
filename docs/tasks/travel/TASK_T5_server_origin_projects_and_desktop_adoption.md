# Task T5: Server-Origin Project Creation and Desktop Adoption

**Status:** Planned
**Depends on:** Companion-server Tasks 7, 8, and 10
**Unblocks:** T6, T7

## Objective

Allow a project to be created on the server by an authenticated device, and allow the
desktop to adopt a server-created project as a full local replica — the mirror image of the
existing desktop-to-server baseline import.

## Blocked on unbuilt work

This task depends on companion-server **Task 10**, which is genuinely not implemented. Task
10 owns desktop pairing, root mapping, and baseline adoption, and this task reuses its
machinery in reverse. T1–T4 are independent and should proceed first.

## User outcome

The user can start a trip project from their phone in a hotel room with no laptop present,
and on returning home pull that project onto the Mac where it behaves exactly like any
project they created there.

## Scope

- A `project.create` operation with server origin, callable by device credential.
- Server-side project replica creation without a prior desktop baseline.
- Origin node recorded on the project for diagnostics.
- Server API listing projects with no local replica on the requesting node.
- Desktop "Adopt server project": preview, then materialize a local catalog from the operation log.
- Amend the architecture invariant and its documentation.

## Out of scope

- Project deletion on any surface
- Multiple desktops adopting the same project
- Project transfer or origin reassignment
- Trip context, chapters, or events
- Card inventory and provisional assets (T7)

## Invariant amendment

The companion-server architecture states that project creation remains a desktop action.
That is amended to:

> Projects may be created on the desktop or on the server. Each project has exactly one
> replica per node that has adopted it.

Project deletion remains out of scope everywhere. Origin is recorded for diagnostics, not
for behavior: after adoption a server-origin project must be indistinguishable in behavior
from a desktop-origin one.

This task owns updating `docs/tasks/server/ARCHITECTURE.md` section 2 and the corresponding
authority table so the two documents do not contradict each other.

## Adoption flow

1. The desktop lists server projects for which it has no local replica.
2. The user selects one and sees a preview: name, origin node, asset count, operation
   count, storage roots referenced, and which of those roots the Mac can currently resolve.
3. On confirmation the desktop materializes a local catalog by replaying the operation log.
4. The desktop registers its sync cursor and thereafter reconciles normally.
5. Roots the Mac cannot resolve are surfaced as unresolved rather than silently dropped.

Adoption is idempotent and restart-safe. An interrupted adoption resumes or restarts
cleanly and never leaves a half-materialized catalog visible as a usable project.

## Design requirements

1. `project.create` is an ordinary immutable operation, not a side channel.
2. The server allocates a `projectId` that is stable and globally unique.
3. A device credential is sufficient authority to create a project; a browser session is
   also acceptable. No new credential class is introduced.
4. Creation requires connectivity. Annotation does not — that distinction is deliberate and
   must be reflected in client behavior, not worked around.
5. Adoption replays operations; it never transfers a SQLite file.
6. A half-materialized catalog is never presented as usable.
7. Adoption is idempotent; re-running it for an adopted project is a no-op.
8. After adoption, no code path may branch on project origin to decide behavior.
9. Existing desktop-to-server baseline import continues to work unchanged.
10. Storage roots the Mac cannot resolve appear as unresolved, and their assets remain
    useful through metadata and representations.

## Likely implementation areas

- `core/rcdvlog-core` `project.create` operation and origin field
- `core/rcdvlog-sync` project listing and adoption DTOs
- `core/rcdvlog-server/src/replicas.rs` server-origin replica creation
- `apps/server/src/lib.rs` project creation and unadopted-project listing routes
- `apps/desktop/src-tauri` adoption command and local catalog materialization
- `apps/desktop` adoption preview and progress UI
- `docs/tasks/server/ARCHITECTURE.md` invariant amendment

## Acceptance criteria

- [ ] A device credential can create a project on the server.
- [ ] The created project has a stable `projectId` and a recorded origin node.
- [ ] The server creates its replica without requiring a desktop baseline.
- [ ] The desktop lists server projects it has not adopted.
- [ ] The adoption preview reports assets, operations, roots, and root resolvability.
- [ ] Adoption materializes a local catalog by replaying operations, not by file transfer.
- [ ] An interrupted adoption never leaves a usable half-materialized project.
- [ ] Re-running adoption for an adopted project is a no-op.
- [ ] An adopted project supports every desktop operation a locally created project supports.
- [ ] No behavior branches on project origin after adoption.
- [ ] Existing desktop-to-server baseline import is unaffected.
- [ ] Unresolvable storage roots are surfaced, and their assets remain useful.
- [ ] Project deletion remains unavailable on every surface.
- [ ] The server architecture document no longer contradicts this behavior.

## Verification

Test adoption against a server project built entirely from synthetic operations, and assert
the resulting desktop catalog is equivalent to one built by applying the same operations
locally. Kill the desktop mid-adoption and assert recovery. Include a project referencing a
storage root the test Mac cannot mount.

## Handoff notes

T6 and T7 both assume a trip project already exists and was created from the phone. The
creation API shape defined here is what the phone calls first, before any card is touched.
