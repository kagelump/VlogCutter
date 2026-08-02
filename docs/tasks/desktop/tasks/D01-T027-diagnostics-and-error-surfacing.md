# D01-T027: Diagnostics and error surfacing

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T027",
  "epic_id": "D01",
  "title": "Diagnostics and error surfacing",
  "slug": "diagnostics-and-error-surfacing",
  "status": "draft",
  "task_class": "foundation",
  "depends_on": ["D01-T007"],
  "principal_invariant": "Every condition the user must know about is recorded as a durable typed diagnostic that survives relaunch and is readable through one surface, so no failure is discoverable only by reading a log or a database.",
  "central_falsification": "Trigger each producer — a rejected ingest file, a quarantined operation, an offline source volume — then quit, relaunch, and confirm every one is still reported with its subject and severity intact; a condition that vanishes on relaunch or never appears falsifies the surface.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": ["failure site -> diagnostic record -> reader"],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/ARCHITECTURE.md", "docs/tasks/desktop/CHRONOLOGY_MODEL.md"],
    "implementer": ["PRD.md", "AGENTS.md", "docs/tasks/desktop/ARCHITECTURE.md", "docs/tasks/desktop/CHRONOLOGY_MODEL.md"],
    "independent_tester": ["PRD.md", "docs/tasks/desktop/CHRONOLOGY_MODEL.md"],
    "reviewer": ["PRD.md", "AGENTS.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "integrator": ["docs/tasks/server/SCHEMA.md", "docs/tasks/desktop/EPIC.md"]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/desktop/tasks/D01-T027-diagnostics-and-error-surfacing.md",
      "docs/tasks/desktop/DIAGNOSTICS_MODEL.md"
    ],
    "implementer": [
      "core/rcdvlog-core/src/diagnostics/mod.rs",
      "core/rcdvlog-core/src/diagnostics/model.rs",
      "core/rcdvlog-core/tests/diagnostics.rs",
      "core/rcdvlog-core/tests/fixtures/diagnostics/**"
    ],
    "independent_tester": [
      "core/rcdvlog-core/tests/diagnostics_independent.rs",
      "core/rcdvlog-core/tests/fixtures/diagnostics-independent/**"
    ],
    "integrator": [
      "docs/tasks/server/SCHEMA.md",
      "docs/tasks/desktop/EPIC.md",
      "docs/task-cycles/D01-T027/**"
    ]
  },
  "forbidden_paths": ["apps/**", "packages/**", "core/rcdvlog-server/**", "core/rcdvlog-core/src/chronology/solver.rs", ".github/**", "PRD.md"],
  "verification_commands": [
    "cargo test -p rcdvlog-core --test diagnostics",
    "cargo test --workspace",
    "cargo clippy --workspace --all-targets -- -D warnings",
    "cargo fmt --all -- --check"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T027\\].*\\| complete \\|"
    },
    {
      "path": "docs/tasks/desktop/DIAGNOSTICS_MODEL.md",
      "regex": "(?s)## Frozen interface.*DiagnosticSeverity"
    },
    {
      "path": "docs/tasks/server/SCHEMA.md",
      "regex": "diagnostics"
    }
  ]
}
-->

## Goal

Give the application one durable place to record anything the user needs to know
about, and one reader to get it back out — so that a failure is never discoverable
only by reading a terminal log, a stderr line that scrolled past, or a SQLite
table by hand.

Today the desktop has three unrelated, transient error channels: a single `error`
string in `App.tsx`, a separate `baselineError` string, and a sync `lastError`
field. Anything that fails outside those three paths is silent. The smoke test in
`docs/tasks/desktop/SMOKE_LEDGER.md` demonstrates the gap directly: 14 of 434
files were rejected during ingest and the only reason anyone knows is that a
tester read the CLI's stdout. `quarantined_operations()` shipped in D01-T007 with
no consumer at all.

This packet builds the model and the recorder. Displaying diagnostics is D01-T015
(desktop) and D01-T017 (CLI); both are listed as consumers below and neither is
in this packet's scope.

## Principal invariant

This task is correct when every condition the user must know about is recorded as
a durable typed diagnostic that survives relaunch and is readable through one
surface, so no failure is discoverable only by reading a log or a database.

## Inputs and outputs

Inputs:

- The three existing ad-hoc error channels named above, as the set of things that
  must eventually route through one system.
- `ProjectCatalog::quarantined_operations` from D01-T007, which is a diagnostic
  producer with no reader.
- Ingest rejection sites in `core/rcdvlog-core/src/lib.rs` (ffprobe failure,
  unreadable file, unsupported container).
- Missing-volume detection, per `PRD.md` §21.2 launch-critical capabilities.
- The existing migration pattern in `core/rcdvlog-core/src/migrations.rs`
  (`PROJECT_CATALOG_MIGRATIONS`, currently 9 entries after D01-T007).

Outputs:

- Migration 10 adding the `diagnostics` table.
- `core/rcdvlog-core/src/diagnostics/model.rs` with the record types.
- `core/rcdvlog-core/src/diagnostics/mod.rs` with the recorder and the reader.
- Three producers wired: ingest rejection, quarantined operation, offline volume.
- `docs/tasks/desktop/DIAGNOSTICS_MODEL.md` — the frozen interface for D01-T015
  and D01-T017.
- `docs/tasks/server/SCHEMA.md` updated so the schema of record matches reality.

## Decision authority

This packet owns the decisions below and is the sole authority for them.

### Decision 1 — Durable, not transient

Diagnostics are rows behind migration 10, not in-memory state.

| Alternative | Verdict |
| --- | --- |
| Persisted rows in the project catalog | **Chosen.** A quarantined operation, an offline volume, and a rejected file are all still true after relaunch. An in-memory channel loses precisely the conditions that matter most. |
| In-memory event bus only | Rejected. Every condition this packet exists to surface outlives the process that noticed it. |
| Append to a log file | Rejected. Unqueryable, unacknowledgeable, and the user has to know it exists. |
| Reuse `operation_conflicts` | Rejected. That table has one specific meaning under the sync protocol; overloading it makes both surfaces ambiguous. |

Acceptance criterion: a diagnostic recorded before a quit is returned by the
reader after relaunch, with severity, subject, and message intact.

### Decision 2 — Deduplicated by stable key, with occurrence counting

A diagnostic carries a caller-supplied `dedupe_key`. Recording a diagnostic whose
`(project_id, dedupe_key)` already exists increments `occurrence_count` and
updates `last_seen_at` rather than inserting a second row.

Ingesting a folder with 400 unreadable sidecar files must produce one diagnostic
saying so, not 400 rows. A diagnostic surface that floods is a diagnostic surface
users learn to ignore, which is the same as not having one.

Acceptance criterion: recording the same `dedupe_key` 400 times yields exactly one
row with `occurrence_count` 400.

### Decision 3 — Acknowledgement is a user action, not a deletion

A diagnostic is acknowledged, never deleted by the application. Acknowledging sets
`acknowledged_at` and removes it from the default unacknowledged view. A
re-occurrence after acknowledgement clears `acknowledged_at` and resurfaces the
row, because the condition happened again.

Acceptance criterion: acknowledging then re-recording the same `dedupe_key`
returns the diagnostic to the unacknowledged set with its full occurrence history.

### Decision 4 — Not an operation, not synchronized

Diagnostics are local observations about one node's view of the world, not shared
project facts. They are not typed operations, do not carry HLC time, do not
participate in the revision guard, and are not synchronized to a peer.

"The source volume is offline" and "ffprobe could not read this file" are true of
this machine at this moment. Replicating them to the companion server would assert
them about a node that never observed them.

Acceptance criterion: no diagnostic appears in `project_operations`, and a
baseline export/import cycle carries no diagnostic rows.

## Frozen decisions

Inherited, not open for this packet to revisit:

- Existing catalogs migrate forward. Migration 10 is additive: it creates one
  table and touches no existing row.
- Timestamps are RFC 3339 strings in UTC, matching the existing convention.
- `AGENTS.md` §15: additive edits only to `lib.rs`. The new logic lives in the new
  `diagnostics` module.
- No diagnostic message embeds a secret, a credential, or a full absolute path to
  anything outside the project's own configured roots.

## Owned paths

The implementer owns the migration, the new module, and the additive producer call
sites. The independent tester owns a separate test file and fixture directory so
its catalogs cannot be the implementer's. The integrator owns the schema document
of record and the epic row.

Every surface is forbidden. This packet defines and records diagnostics; showing
them is D01-T015 and D01-T017. That boundary is what keeps this packet reviewable.

## Required interface

### Table, migration 10

```sql
CREATE TABLE IF NOT EXISTS diagnostics (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    dedupe_key TEXT NOT NULL,
    severity TEXT NOT NULL CHECK(severity IN ('error', 'warning', 'info')),
    kind TEXT NOT NULL,
    subject_type TEXT NOT NULL,
    subject_id TEXT NOT NULL DEFAULT '',
    message TEXT NOT NULL,
    context_json TEXT NOT NULL DEFAULT '{}',
    occurrence_count INTEGER NOT NULL DEFAULT 1,
    first_seen_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    acknowledged_at TEXT,
    UNIQUE(project_id, dedupe_key)
);

CREATE INDEX IF NOT EXISTS diagnostics_project_unacknowledged
ON diagnostics(project_id, acknowledged_at, last_seen_at);
```

### Rust types

```rust
// core/rcdvlog-core/src/diagnostics/model.rs

pub enum DiagnosticSeverity { Error, Warning, Info }

/// What the diagnostic is about. `subject_id` is empty for `Project`.
pub enum DiagnosticSubject {
    Project,
    Asset { asset_id: String },
    Operation { operation_id: String },
    Volume { volume_hint: String },
    CaptureSequence { capture_sequence_id: String },
}

pub struct Diagnostic {
    pub id: String,
    pub dedupe_key: String,
    pub severity: DiagnosticSeverity,
    /// Stable machine-readable slug, e.g. `ingest.unreadable_media`.
    pub kind: String,
    pub subject: DiagnosticSubject,
    pub message: String,
    pub context: serde_json::Value,
    pub occurrence_count: u64,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub acknowledged_at: Option<String>,
}

/// What a producer supplies. Ids and timestamps are assigned by the recorder.
pub struct DiagnosticRecord {
    pub dedupe_key: String,
    pub severity: DiagnosticSeverity,
    pub kind: String,
    pub subject: DiagnosticSubject,
    pub message: String,
    pub context: serde_json::Value,
}

pub struct DiagnosticFilter {
    pub severity_at_least: Option<DiagnosticSeverity>,
    pub kind_prefix: Option<String>,
    pub include_acknowledged: bool,
}
```

```rust
// core/rcdvlog-core/src/lib.rs, additive
impl ProjectCatalog {
    pub fn record_diagnostic(&self, project_id: &str, record: DiagnosticRecord)
        -> Result<Diagnostic>;
    pub fn diagnostics(&self, project_id: &str, filter: &DiagnosticFilter)
        -> Result<Vec<Diagnostic>>;
    pub fn acknowledge_diagnostic(&self, project_id: &str, diagnostic_id: &str)
        -> Result<()>;
    pub fn diagnostic_counts(&self, project_id: &str)
        -> Result<DiagnosticCounts>;
}

pub struct DiagnosticCounts {
    pub errors: u64,
    pub warnings: u64,
    pub infos: u64,
}
```

`diagnostics()` returns rows ordered by `last_seen_at` descending, then `id`
ascending, so the order is total and deterministic. `DiagnosticFilter::default()`
returns unacknowledged rows of every severity.

### Producer kinds wired by this packet

| Kind | Severity | Subject | Dedupe key |
| --- | --- | --- | --- |
| `ingest.unreadable_media` | `warning` | `Asset` or `Project` | `ingest.unreadable_media:<containing folder>` |
| `sync.operation_quarantined` | `warning` | `Operation` | `sync.operation_quarantined:<operation_type>` |
| `media.volume_offline` | `error` | `Volume` | `media.volume_offline:<volume_hint>` |

Later packets add kinds. The `kind` namespace is `<area>.<condition>`, lowercase,
dot-separated, and stable once shipped — D01-T015 and D01-T017 key display
behaviour off it.

## Acceptance oracle

1. **Migration 10 is additive and order-preserving.** Open a schema-9 catalog with
   assets, migrate, and confirm the asset list and its order are byte-identical
   before and after, and that `diagnostics` is empty.
2. **A recorded diagnostic survives relaunch.** Record one, drop the catalog
   handle, reopen, and read it back with severity, subject, message, and context
   intact.
3. **Dedupe counts rather than duplicates.** Record the same `dedupe_key` 400
   times; exactly one row exists with `occurrence_count` 400, `first_seen_at`
   from the first and `last_seen_at` from the last.
4. **Distinct keys stay distinct.** Two records differing only in `dedupe_key`
   produce two rows.
5. **Acknowledgement hides, re-occurrence resurfaces.** Acknowledge a diagnostic,
   confirm it leaves the default view, re-record it, and confirm it returns with
   `acknowledged_at` NULL and `occurrence_count` incremented.
6. **Ingest rejection produces one diagnostic.** Ingest a folder containing real
   media plus 14 unreadable `._*` sidecar files; exactly one
   `ingest.unreadable_media` diagnostic exists, its `occurrence_count` is 14, and
   the real assets all ingested.
7. **Quarantine produces a diagnostic.** Apply an operation of an unknown type;
   confirm a `sync.operation_quarantined` diagnostic exists naming the operation,
   and that `quarantined_operations()` still reports it — the diagnostic
   supplements that surface, it does not replace it.
8. **Diagnostics are not operations.** After exercising every producer, confirm
   `project_operations` contains no diagnostic row, no aggregate revision moved,
   and a baseline export/import round-trip carries no diagnostic.
9. **Reader order is total and deterministic.** Read 32 times across two opens
   and confirm an identical sequence every time, including rows sharing a
   `last_seen_at`.
10. **Filters are honest.** `severity_at_least: Error` returns only errors;
    `kind_prefix: "ingest."` returns only ingest kinds; `include_acknowledged`
    toggles acknowledged rows in and out.

## Independent test charter

Falsify the claim that diagnostics are durable, deduplicated, and complete.

- Build your own schema-9 catalogs by direct insert. Do not reuse the
  implementer's fixtures.
- Attack durability: record, kill the process without a clean close, reopen, and
  confirm the diagnostic is there. A diagnostic that only survives a graceful
  shutdown does not survive the crash it was recording.
- Attack dedupe: record the same key concurrently from two handles on one
  catalog and confirm one row with the correct total, not two rows or a lost
  update.
- Attack the migration: run migration 10 against a schema-9 catalog holding
  chronology facts from D01-T007 and confirm `chronology_facts()` returns an
  identical `ChronologyFacts` before and after, field for field.
- Attack completeness: find a failure path in ingest that produces no diagnostic
  and report it. The invariant is that the user is never left guessing, so a
  silent failure site is the finding this charter most wants.
- Confirm no diagnostic message or context contains an absolute path outside the
  project's configured roots, a credential, or a token.
- Confirm no source media file was created, modified, or deleted.

## Review charter

Risk domain: a surface that exists but is not trusted. The failure modes are a
diagnostic system that floods until users ignore it, one that loses the conditions
it was built to preserve, and one that leaks paths or secrets into a view the user
may screenshot.

Questions:

1. Is every producer's `dedupe_key` actually stable across runs, or does it embed
   a timestamp, a UUID, or a full path that makes every occurrence unique?
2. Does migration 10 touch any existing row?
3. Can a diagnostic be silently lost between the failure site and the row —
   swallowed `Result`, ignored error, recorded inside a transaction that later
   rolls back?
4. Is `kind` a closed, documented namespace, or can any call site invent a slug
   that D01-T015 cannot render?
5. Does any diagnostic carry a secret, a credential, or a path outside the
   project roots?
6. Is the reader's order genuinely total, including ties on `last_seen_at`?
7. Does recording a diagnostic ever fail the operation that produced it? It must
   not — a diagnostic is an observation, and failing the write because the note
   failed converts a warning into an outage.

Severity rules: a lost diagnostic, a diagnostic that fails its producing
operation, a migration that touches existing rows, or a leaked secret is blocking.
Message wording and missing `context` fields are non-blocking.

Smallest acceptable remedy: name the specific call site and the specific field.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Displaying diagnostics anywhere. D01-T015 (desktop) and D01-T017 (CLI).
- Retrofitting every existing failure site. Three producers are wired here; the
  rest arrive with the packets that own their call sites.
- Replacing `App.tsx`'s `error` / `baselineError` state. That is D01-T015's
  consolidation, and doing it here would touch a forbidden path.
- Synchronizing diagnostics to the companion server. Decision 4 excludes it.
- Log rotation, retention policy, or a diagnostics export bundle.
- Telemetry or any transmission off the machine.

## Stop conditions

Stop and escalate when:

- migration 10 cannot be made additive without recomputing an existing column;
- a producer call site cannot record a diagnostic without being able to fail the
  operation it observes;
- the dedupe rule cannot be satisfied for a producer because its condition has no
  stable identity, which means the `kind` is wrong and the contract editor must
  redefine it;
- wiring a producer would require editing a forbidden path;
- more than three producers turn out to be needed to make the acceptance oracle
  meaningful, which means this packet is too large and must be split.

## Commands

```sh
cargo test -p rcdvlog-core --test diagnostics
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

Expected result: all four exit 0, and the diagnostics suite covers all ten
acceptance oracle cases.

## Handoff

Report the migration number, the table shape as built, the three producer kinds
with their dedupe keys, the acceptance oracle results case by case, and the
independent tester's verdict. State explicitly whether any silent failure site was
found and left unwired, since that is the gap D01-T015 will inherit. Name the
`kind` namespace entries so D01-T015 and D01-T017 can key display behaviour off a
closed list.
