# Task T1: Writable Archive Root and Atomic Commit Path

**Status:** Planned
**Depends on:** Companion-server Tasks 3 and 5
**Unblocks:** T2

## Objective

Introduce a writable archive storage root and a create-only, atomic file commit path, so
that originals arriving over the network have a durable destination that does not weaken
the immutability of existing NAS scan roots.

## User outcome

Footage uploaded from the road lands in a real, browsable location on the NAS, organized by
trip and card, while every existing archive folder remains as read-only as it is today.

## Scope

- Register an archive storage root as a logical root with `immutable_original: false`.
- Bind it `ReadWrite` on the server node and optionally `ReadOnly` on the Mac.
- Implement deterministic archive path construction from trip, card session, and card-relative path.
- Implement `cardRelativePath` validation and containment checking.
- Implement atomic commit: temp file in the destination directory, `fsync`, atomic rename.
- Add configuration and diagnostics for the archive root, including free-space reporting.
- Add a staging directory under the server data directory with a capacity floor.

## Out of scope

- The transfer protocol itself (T2)
- Media library registration (T3)
- Rename, overwrite, deletion, retention, or cleanup of archived files
- Migrating existing NAS content into the archive root
- Mac-side write access to the archive root

## Archive path construction

```text
<archiveRoot>/<tripSlug>/<cardSessionSlug>/<cardRelativePath>
```

- `tripSlug` derives from the trip project and is stable for that project's lifetime.
- `cardSessionSlug` is `<cardLabel>-<cardSessionShortId>`. The same physical card
  reinserted resumes into the same slug; a reformatted card gets a new one.
- `cardRelativePath` preserves camera folder structure so sidecars and clip groupings survive.

Slug construction must be deterministic, filesystem-safe on both ext4 and the NAS export,
and free of characters that require quoting.

## Path safety requirements

`cardRelativePath` originates on the phone and is untrusted. Reject before any filesystem use:

- Absolute paths, drive letters, and UNC prefixes
- Any `..` segment, any empty segment, any `.` segment
- Control characters and NUL
- Paths whose canonicalized destination falls outside the archive root

Containment must be re-checked **after** canonicalization, not only before. Symlinks
encountered during resolution are an error, not something to follow.

## Design requirements

1. Existing scan roots remain `immutable_original` and read-only; nothing in this task
   relaxes the existing enforcement in `library.rs`.
2. The archive root exposes no rename, overwrite, truncate, or delete API. Writes are
   create-only.
3. A destination that already exists is a conflict surfaced to the caller, never an overwrite.
4. Commit is atomic: a partial file is never visible under its final name.
5. A failed commit leaves at most one server-owned temp file with a known, recoverable name.
6. The temp file lives in the destination directory so the rename is same-filesystem.
7. Staging lives on the server SSD under the server data directory and never on the NAS.
8. When free space beneath staging or the archive root falls below a configured floor, the
   commit path refuses new work with a structured error code rather than failing partway.
9. Absolute archive paths are node-local and must never appear in a project operation, a
   synchronization payload, or a client-visible response.
10. Archive-root diagnostics report capacity and binding state without exposing absolute
    paths to non-administrative surfaces.

## Likely implementation areas

- `core/rcdvlog-server/src/library.rs` archive-root registration and binding rules
- `core/rcdvlog-server/src/storage_path.rs` path construction, validation, containment
- `core/rcdvlog-server/src/config.rs` archive root, staging directory, capacity floors
- `core/rcdvlog-server/src/foundation.rs` staging directory lifecycle
- `apps/server/src/lib.rs` archive diagnostics route
- `deploy/server/compose.yaml` writable archive bind mount

## Acceptance criteria

- [ ] An archive root registers with `immutable_original: false` and binds `ReadWrite` on the server.
- [ ] Registering a `ReadWrite` binding against an `immutable_original` root still fails.
- [ ] Archive paths are deterministic and identical across process restarts.
- [ ] A reinserted card resolves to the same `cardSessionSlug`; a reformatted card does not.
- [ ] Absolute, `..`, empty-segment, control-character, and symlink-escaping paths are all rejected.
- [ ] Containment is verified after canonicalization.
- [ ] A commit interrupted before rename leaves no file under the final name.
- [ ] A commit interrupted before rename leaves at most one recoverable temp file.
- [ ] An existing destination produces a conflict error, never an overwrite.
- [ ] No API beneath the archive root can rename, truncate, or delete.
- [ ] Below the configured capacity floor, commits are refused with a structured code before writing.
- [ ] No staging path resolves onto a NAS mount.
- [ ] No absolute path appears in any synchronization payload or non-administrative response.
- [ ] No test writes, renames, or deletes anything under a scan root.

## Verification

Use a temporary directory as the archive root and generated files. Test interruption by
failing between write and rename. Test containment with an adversarial path table including
Unicode normalization variants and symlinks planted inside the archive root. Verify the
capacity floor with a small filesystem image or an injected capacity probe rather than by
filling a real disk.

## Handoff notes

T2 consumes the commit path but never calls it until a file is fully staged and verified.
The commit function should accept an already-verified staged file and a destination
descriptor, and should not itself know anything about chunks or sessions.
