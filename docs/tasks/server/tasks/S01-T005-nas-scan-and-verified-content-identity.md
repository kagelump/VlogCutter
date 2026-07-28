# S01-T005: NAS Scanning and Verified Content Identity

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "S01-T005",
  "epic_id": "S01",
  "title": "NAS Scanning and Verified Content Identity",
  "slug": "nas-scan-and-verified-content-identity",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": [
    "S01-T003",
    "S01-T004"
  ],
  "principal_invariant": "Every scan of a configured NAS root produces deterministic, paginated results with sampled quick fingerprints that promote to full-hash-verified content identities — and a sampled-fingerprint collision cannot merge distinct content after full verification.",
  "central_falsification": "Scan a NAS root with known content, assert every file appears in the paginated library, assert sampled fingerprints are produced, then deliberately create a fingerprint collision (two files with same sampled prefix but different full content) and verify that full-hash verification detects and surfaces the mismatch rather than merging them.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Scan scheduler → durable job engine (S01-T004)"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/NAS_SCAN_AND_CONTENT_IDENTITY.md"
    ],
    "implementer": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/NAS_SCAN_AND_CONTENT_IDENTITY.md",
      "docs/tasks/server/STORAGE_LIBRARY.md"
    ],
    "independent_tester": [
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "reviewer": [
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "integrator": [
      "docs/tasks/server/ARCHITECTURE.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/server/tasks/S01-T005-nas-scan-and-verified-content-identity.md"
    ],
    "implementer": [
      "core/rcdvlog-server/src/**"
    ],
    "independent_tester": [
      "core/rcdvlog-server/tests/**"
    ],
    "integrator": [
      "apps/server/src/lib.rs"
    ]
  },
  "forbidden_paths": [
    "core/rcdvlog-core/src/catalog/**",
    "apps/desktop/**",
    "apps/web/**",
    "deploy/**"
  ],
  "verification_commands": [
    "cargo test -p rcdvlog-server",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/server/EPIC.md",
      "regex": "\\| 50 \\| \\[S01-T005\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Implement NAS filesystem scanning as durable jobs, sampled quick fingerprinting,
background full-file BLAKE3 verification, and the two-stage content identity
promotion that prevents fingerprint collisions from silently merging distinct
content.

## Principal invariant

This task is correct when every scan of a configured NAS root produces
deterministic, paginated results with sampled quick fingerprints that promote to
full-hash-verified content identities — and a sampled-fingerprint collision
cannot merge distinct content after full verification.

## Inputs and outputs

Inputs:
- S01-T003 storage roots, bindings, and global media library
- S01-T004 durable job engine

Outputs:
- Scan jobs as durable job handlers (manual and scheduled)
- Filesystem walking with pagination, filtering, and resume
- Sampled quick fingerprint (first + last N bytes) for rapid identity
- Background full-file BLAKE3 verification jobs
- Two-stage identity promotion: quick match → verified or mismatch
- Verified content identity with explicit mismatch surfacing
- Generation-based scan observations integrated with S01-T003's availability model

## Frozen decisions

[Content derived from TASK_5 source document - scan, fingerprint, verification, and content identity infrastructure]

## Owned paths

- **Implementer:** `core/rcdvlog-server/src/` scan, fingerprint, and verification modules
- **Independent tester:** `core/rcdvlog-server/tests/` with temporary scan roots
- **Integrator:** `apps/server/src/lib.rs` scan trigger routes

## Required interface

- Scan initiation and status services
- Content identity types consumed by S01-T006 (representation jobs) and S01-T014 (web library)
- Fingerprint and verification result DTOs

## Acceptance oracle

1. Scanning a configured root produces deterministic, paginated results.
2. Quick fingerprints are computed without reading entire large files.
3. Full-hash verification promotes quick matches to verified.
4. A fingerprint collision is detected at full verification and surfaced as mismatch.
5. An incomplete scan generation does not mark prior observations missing.
6. Scans survive server restart through the durable job engine.
7. Read-only scan roots are never written to.
8. Scan concurrency is bounded and does not saturate the NAS.

## Independent test charter

Try to falsify: "fingerprint collisions are detected." Seed a scan root with two
files that share the same sampled prefix but differ in full content. Run a full
scan, assert both are initially quick-matched, then assert full verification
detects the mismatch and neither file is silently merged. Test scan restart by
killing the server mid-scan and verifying the resumed scan does not duplicate or
lose files.

## Review charter

Risk domain: content identity integrity. Questions: (1) Can a fingerprint
collision silently merge distinct content? (2) Can an incomplete scan invalidate
prior observations? (3) Is the scan root ever written to? (4) Does scan
concurrency saturate the NAS? Severity: P0 for silent content merging; P1 for
observation corruption or NAS saturation. Approval vocabulary: `APPROVE`,
`APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- Metadata extraction beyond what fingerprinting requires
- Representation generation
- Real-time filesystem watching
- Content relocation or marker-file creation

## Stop conditions

Stop and escalate when:

- Fingerprinting requires reading entire large files
- Full verification is skipped when quick fingerprints match
- Scan needs to write marker files into the NAS root
- Concurrency limits need per-NAS-share rather than per-server enforcement

## Commands

```sh
cargo test -p rcdvlog-server
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

Content identity types and verification states are consumed by S01-T006 for
representation job eligibility and S01-T014 for library browsing. Document the
fingerprint sampling strategy and collision detection guarantees.
