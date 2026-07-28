# T01-T001: Writable Archive Root and Atomic Commit Path

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "T01-T001",
  "epic_id": "T01",
  "title": "Writable Archive Root and Atomic Commit Path",
  "slug": "writable-archive-root-and-commit-path",
  "status": "ready",
  "task_class": "foundation",
  "depends_on": [],
  "principal_invariant": "A writable archive root accepts create-only commits through an atomic temp-file-and-rename path, while every existing scan root remains immutable and read-only — no API beneath the archive root can rename, overwrite, or delete.",
  "central_falsification": "Register an archive root with immutable_original:false, attempt to register a ReadWrite binding against an immutable_original scan root (must fail), commit a file atomically, then attempt to overwrite it (must fail), attempt to delete it (must fail), and verify a partial commit leaves no file under the final name.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Archive root → storage root model (S01-T003)",
    "Commit path → transfer pipeline (T01-T002)"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ],
    "implementer": [
      "docs/tasks/travel/ARCHITECTURE.md",
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/STORAGE_LIBRARY.md"
    ],
    "independent_tester": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ],
    "reviewer": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ],
    "integrator": [
      "docs/tasks/travel/ARCHITECTURE.md",
      "docs/tasks/server/ARCHITECTURE.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/travel/tasks/T01-T001-writable-archive-root-and-commit-path.md"
    ],
    "implementer": [
      "core/rcdvlog-server/src/library.rs",
      "core/rcdvlog-server/src/storage_path.rs",
      "core/rcdvlog-server/src/config.rs",
      "core/rcdvlog-server/src/foundation.rs"
    ],
    "independent_tester": [
      "core/rcdvlog-server/tests/**"
    ],
    "integrator": [
      "deploy/server/compose.yaml",
      "apps/server/src/lib.rs"
    ]
  },
  "forbidden_paths": [
    "core/rcdvlog-core/src/catalog/**",
    "apps/desktop/**",
    "apps/web/**",
    "apps/mobile/**"
  ],
  "verification_commands": [
    "cargo test -p rcdvlog-server",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/travel/EPIC.md",
      "regex": "\\| 10 \\| \\[T01-T001\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Introduce a writable archive storage root and a create-only, atomic file commit
path, so that originals arriving over the network have a durable destination that
does not weaken the immutability of existing NAS scan roots.

## Principal invariant

This task is correct when a writable archive root accepts create-only commits
through an atomic temp-file-and-rename path, while every existing scan root
remains immutable and read-only — no API beneath the archive root can rename,
overwrite, or delete.

## Inputs and outputs

Inputs:

- S01-T003 storage root model and binding types
- S01-T005 scan and verified content identity infrastructure
- Existing `library.rs` immutability enforcement

Outputs:

- Archive storage root registration (`immutable_original: false`, `ReadWrite` server binding)
- Deterministic archive path construction: `<archiveRoot>/<tripSlug>/<cardSessionSlug>/<cardRelativePath>`
- `cardRelativePath` validation and containment checking (post-canonicalization)
- Atomic commit: temp file in destination directory, `fsync`, atomic rename
- Staging directory under server data directory with capacity floor
- Archive-root diagnostics and free-space reporting

## Frozen decisions

1. Existing scan roots remain `immutable_original` and read-only — this task does not relax `library.rs` enforcement.
2. The archive root exposes no rename, overwrite, truncate, or delete API. Writes are create-only.
3. A destination that already exists is a conflict surfaced to the caller, never an overwrite.
4. Commit is atomic: a partial file is never visible under its final name.
5. A failed commit leaves at most one server-owned temp file with a known, recoverable name.
6. The temp file lives in the destination directory so the rename is same-filesystem.
7. Staging lives on the server SSD under server data directory, never on the NAS.
8. Below capacity floor, commits are refused with a structured error code before writing.
9. Absolute archive paths are node-local and never appear in sync payloads or client responses.
10. `cardRelativePath` originates on the phone and is untrusted; containment re-checked after canonicalization.

## Owned paths

- **Implementer:** `core/rcdvlog-server/src/library.rs` (archive-root registration), `core/rcdvlog-server/src/storage_path.rs` (path construction, validation, containment), `core/rcdvlog-server/src/config.rs` (archive root, staging, capacity floors), `core/rcdvlog-server/src/foundation.rs` (staging lifecycle)
- **Independent tester:** `core/rcdvlog-server/tests/` with temporary archive roots and adversarial path tables
- **Integrator:** `deploy/server/compose.yaml` (writable archive bind mount), `apps/server/src/lib.rs` (archive diagnostics route)

## Required interface

- `commit_staged_file(staged_path, destination_descriptor) -> Result<(), CommitError>` — consumed by T01-T002
- Archive root registration and binding configuration
- Capacity and free-space diagnostics

## Acceptance oracle

1. An archive root registers with `immutable_original: false` and binds `ReadWrite` on the server.
2. Registering a `ReadWrite` binding against an `immutable_original` root fails.
3. Archive paths are deterministic and identical across process restarts.
4. Absolute, `..`, empty-segment, control-character, and symlink-escaping paths are all rejected.
5. Containment is verified after canonicalization.
6. A commit interrupted before rename leaves no file under the final name.
7. An existing destination produces a conflict error, never an overwrite.
8. No API beneath the archive root can rename, truncate, or delete.
9. Below capacity floor, commits are refused with a structured code before writing.
10. No staging path resolves onto a NAS mount.
11. No absolute path appears in any synchronization payload or non-administrative response.
12. No test writes, renames, or deletes anything under a scan root.

## Independent test charter

Try to falsify: "the archive root is create-only." Use a temporary directory as
the archive root. Test every operation that would mutate an existing file (rename,
overwrite, truncate, delete) and assert each is rejected. Test interruption by
failing between write and rename, verify no partial file under the final name.
Test containment with adversarial paths: Unicode normalization variants, symlinks
planted inside the archive root, `../` traversal, absolute paths. Verify the
capacity floor with an injected capacity probe rather than by filling a real disk.

## Review charter

Risk domain: archive integrity and scan-root immutability. Questions: (1) Can any
operation write, rename, or delete under a scan root? (2) Can a path traversal
escape the archive root? (3) Can a partial commit leave a file under the final
name? (4) Can the capacity floor be bypassed? Severity: P0 for any scan-root
mutation or archive-root path escape; P1 for partial-file visibility or capacity
bypass. Approval vocabulary: `APPROVE`, `APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- The transfer protocol itself (T01-T002)
- Media library registration (T01-T003)
- Rename, overwrite, deletion, retention, or cleanup of archived files
- Migrating existing NAS content into the archive root
- Mac-side write access to the archive root

## Stop conditions

Stop and escalate when:

- Implementing create-only requires filesystem-level ACL changes
- Path canonicalization needs to follow symlinks across binding boundaries
- The archive root model requires changing S01-T003's storage root schema
- Staging capacity enforcement needs cross-process coordination

## Commands

```sh
cargo test -p rcdvlog-server
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

T01-T002 consumes the commit path but never calls it until a file is fully
staged and verified. The commit function should accept an already-verified staged
file and a destination descriptor, and should not itself know about chunks or
sessions.
