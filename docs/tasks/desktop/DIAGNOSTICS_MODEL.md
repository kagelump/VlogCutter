# Diagnostics model — frozen contract

Published by D01-T027
(`docs/tasks/desktop/tasks/D01-T027-diagnostics-and-error-surfacing.md`),
2026-08-03. This document freezes the diagnostic record model, the recorder and
reader interface, and the `kind` namespace for D01-T015 (desktop display) and
D01-T017 (CLI display). Both consume this contract and may not change it.

## Status

- Project catalog schema: 10 (migration 10, `diagnostics`).
- `docs/tasks/server/SCHEMA.md` is updated to match.

## Why this exists

Before this contract the desktop had three unrelated, transient error channels —
an `error` string in `App.tsx`, a separate `baselineError`, and a sync
`lastError` field — and anything failing outside them was silent. The smoke run
in `docs/tasks/desktop/SMOKE_LEDGER.md` shows the gap directly: 14 of 434 files
were rejected during ingest and the only reason anyone knew was that a tester
read the CLI's stdout. `quarantined_operations()` shipped in D01-T007 with no
consumer at all.

## Frozen interface

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

pub enum DiagnosticSeverity { Info, Warning, Error }

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
    pub kind: String,
    pub subject: DiagnosticSubject,
    pub message: String,
    pub context: serde_json::Value,
    pub occurrence_count: u64,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub acknowledged_at: Option<String>,
}

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

pub struct DiagnosticCounts { pub errors: u64, pub warnings: u64, pub infos: u64 }
```

`DiagnosticSeverity` is ordered `Info < Warning < Error`, so
`severity_at_least` is a simple comparison.

### Catalog interface

```rust
impl ProjectCatalog {
    pub fn record_diagnostic(&self, project_id: &str, record: DiagnosticRecord) -> Result<Diagnostic>;
    pub fn diagnostics(&self, project_id: &str, filter: &DiagnosticFilter) -> Result<Vec<Diagnostic>>;
    pub fn acknowledge_diagnostic(&self, project_id: &str, diagnostic_id: &str) -> Result<()>;
    pub fn diagnostic_counts(&self, project_id: &str) -> Result<DiagnosticCounts>;

    // Producers
    pub fn record_unreadable_media(&self, project_id: &str, containing_folder: &str, filename: &str) -> Result<Diagnostic>;
    pub fn record_quarantined_operation(&self, project_id: &str, operation_id: &str, operation_type: &str) -> Result<Diagnostic>;
    pub fn record_offline_volume(&self, project_id: &str, volume_hint: &str, affected_assets: u64) -> Result<Diagnostic>;
    pub fn detect_offline_volumes(&self, project_id: &str) -> Result<Vec<Diagnostic>>;
}
```

`diagnostics()` returns rows ordered by `last_seen_at` descending, then `id`
ascending, so the order is total and deterministic even when two conditions were
recorded in the same millisecond. `DiagnosticFilter::default()` returns
unacknowledged rows of every severity — what a user opening the app should see.

## Decisions

### Durable, not transient

Diagnostics are rows behind migration 10. A quarantined operation, an offline
volume, and a rejected file are all still true after relaunch; an in-memory
channel loses precisely the conditions that matter most.

### Deduplicated by stable key, with occurrence counting

Recording a diagnostic whose `(project_id, dedupe_key)` already exists
increments `occurrence_count` and refreshes `last_seen_at` rather than inserting
a second row. `first_seen_at` never moves. Ingesting a folder with 400
unreadable sidecar files produces one diagnostic saying so. A diagnostic surface
that floods is one users learn to ignore, which is the same as not having one.

### Acknowledgement is a user action, not a deletion

The application never deletes a diagnostic. Acknowledging sets
`acknowledged_at` and removes the row from the default view. A re-occurrence
clears `acknowledged_at` and resurfaces the row with its full occurrence
history, because the condition happened again.

### Not an operation, not synchronized

Diagnostics carry no HLC time, take no part in the revision guard, never appear
in `project_operations`, and are never synchronized to a peer. "The source
volume is offline" and "ffprobe could not read this file" are true of *this*
machine at *this* moment; replicating them would assert them about a node that
never observed them.

### Recording never fails the thing being observed

Every producer call site ignores a recording failure. An ingest must not fail
because a diagnostic could not be written, and a successfully applied operation
must never be rolled back by its own diagnostic. Quarantine diagnostics are
therefore recorded *after* the operation transaction commits.

## Kind namespace

`<area>.<condition>`, lowercase, dot-separated, and stable once shipped —
D01-T015 and D01-T017 key display behaviour off it.

| Kind | Severity | Subject | Dedupe key |
| --- | --- | --- | --- |
| `ingest.unreadable_media` | `warning` | `Project` | `ingest.unreadable_media:<containing folder>` |
| `sync.operation_quarantined` | `warning` | `Operation` | `sync.operation_quarantined:<operation_type>` |
| `media.volume_offline` | `error` | `Volume` | `media.volume_offline:<volume_hint>` |

Later packets add kinds; they never repurpose one.

## Forward compatibility

A `severity` or `subject_type` this build does not recognize still has to be
*visible*: an unknown severity reads back as `Error` and an unknown subject as
`Project`. Being wrong in the direction of showing the user something is the
safe direction.

## What consumers may not change

- The table, the index, and migration 10's additive-only rule.
- The record types, including field names and types.
- The reader's ordering contract and `DiagnosticFilter::default()` semantics.
- The dedupe, acknowledgement, and re-occurrence semantics.
- The rule that diagnostics are never operations and never synchronize.
- The `kind` strings already shipped above.
