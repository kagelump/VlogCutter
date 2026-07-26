# Task 14a: Library and Project Query Surface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the server an indexed, cursor-paginated, deterministic read surface over the global media library and replicated project assets, exposed as authenticated HTTP endpoints with a checked OpenAPI contract.

**Architecture:** Filename and camera facts live in `media_probe_facts` keyed by `storage_object_id`, and one media object may have several storage objects, so no sort over those fields is deterministic today. This plan adds `media_search_index`, a rebuildable one-row-per-media-object projection refreshed inside the same transaction as the writes that feed it, with one composite index per supported sort/filter combination. Queries reject any combination that has no index rather than falling back to a scan. Availability stays derived: it is computed only for the rows on an already-bounded page.

**Tech Stack:** Rust (rusqlite, serde, thiserror, axum), SQLite, TypeScript client generation via `scripts/sync-openapi.mjs`.

## Global Constraints

- This is plan 1 of 3 for [Task 14](../../tasks/server/TASK_14_web_library_project_browse_and_add.md). Plan 2 adds the add-to-project command; plan 3 adds the Chrome UI. Do not implement either here.
- Crate names: the library crate is `rcdvlog-server-core` (at `core/rcdvlog-server/`), the binary crate is `rcdvlog-server` (at `apps/server/`).
- Control-catalog schema version is currently 7. This plan adds migration 8 and no other migration.
- Migrations are numbered, contiguous, and individually transactional. Any migration that has reached `main` is immutable. Migration 8 is introduced by this branch and has not shipped, so it may be amended within this branch until merge — Task 3 Step 6 depends on that.
- Absolute filesystem paths must never appear in any DTO this plan adds. Root-relative paths and stable IDs only.
- Every collection endpoint is cursor-paginated with an explicit limit. `MAX_PAGE_SIZE` is 200 and stays 200.
- All new query methods open the catalog with `open_read_only_connection`.
- New HTTP routes go on the authenticated `internal` router in `apps/server/src/lib.rs:505`, never the `public` router.
- Error envelopes reuse the existing `ErrorResponse { requestId, code }` shape and stable string codes.
- Run `cargo fmt --all` before every commit. `cargo clippy --workspace --all-targets -- -D warnings` must stay clean.
- Work on a branch off `main`: `git switch -c task-14a-library-query-surface`.

---

### Task 1: Search-index migration with backfill

**Files:**
- Modify: `core/rcdvlog-server/src/catalog.rs:24-67` (migration registry), and add the migration function beside `create_authentication_credentials_sessions`
- Test: `core/rcdvlog-server/src/catalog.rs` (in the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: control-catalog table `media_search_index` and migration ID 8 named `library_search_index`. Task 2 refreshes rows in it; Tasks 3–5 read from it.

- [ ] **Step 1: Write the failing upgrade test**

Add to the `tests` module in `core/rcdvlog-server/src/catalog.rs`, following the existing upgrade tests (`storage_library_upgrade_preserves_server_identity_and_project_registry` at line 1077 is the model):

```rust
#[test]
fn library_search_index_migration_backfills_existing_media() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("control.sqlite3");
    let mut connection = rusqlite::Connection::open(&path).unwrap();
    run_sqlite_migrations(&mut connection, &CONTROL_CATALOG_MIGRATIONS[..7]).unwrap();

    connection
        .execute_batch(
            r#"
            INSERT INTO storage_roots(storage_root_id, display_name, immutable_original, enabled, created_at, updated_at)
                VALUES ('root_a', 'Main NAS', 1, 1, '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z');
            INSERT INTO media_objects(media_object_id, size_bytes, quick_fingerprint, full_content_hash, verification_state, created_at, updated_at)
                VALUES ('media_a', 1024, 'quick', NULL, 'provisional', '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z');
            INSERT INTO storage_objects(storage_object_id, storage_root_id, relative_path, size_bytes, created_at, updated_at)
                VALUES ('object_a', 'root_a', 'Trips/2025/Sony/C0103.MP4', 1024, '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z');
            INSERT INTO media_object_locations(media_object_id, storage_object_id, verification_state, linked_at, updated_at)
                VALUES ('media_a', 'object_a', 'provisional', '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z');
            INSERT INTO media_probe_facts(storage_object_id, recipe_key, recipe_version, probe_json, camera_make, camera_model, captured_at)
                VALUES ('object_a', 'probe', '1', '{}', 'Sony', 'ILCE-7M4', '2026-07-20T00:00:00Z');
            "#,
        )
        .unwrap();

    run_sqlite_migrations(&mut connection, CONTROL_CATALOG_MIGRATIONS).unwrap();

    let row: (String, String, String, Option<String>, u32) = connection
        .query_row(
            "SELECT media_object_id, filename, filename_folded, camera_model, storage_location_count
             FROM media_search_index WHERE media_object_id = 'media_a'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap();
    assert_eq!(row.0, "media_a");
    assert_eq!(row.1, "C0103.MP4");
    assert_eq!(row.2, "c0103.mp4");
    assert_eq!(row.3.as_deref(), Some("ILCE-7M4"));
    assert_eq!(row.4, 1);
}
```

- [ ] **Step 2: Run the test and confirm it fails**

Run: `cargo test -p rcdvlog-server-core library_search_index_migration_backfills_existing_media`
Expected: FAIL — `no such table: media_search_index`.

- [ ] **Step 3: Register migration 8**

Append to `CONTROL_CATALOG_MIGRATIONS` in `core/rcdvlog-server/src/catalog.rs`, after the id-7 entry:

```rust
    SqliteMigration {
        id: 8,
        name: "library_search_index",
        source_identity: "server-control/0008-library-search-index/20260727",
        apply: create_library_search_index,
    },
```

- [ ] **Step 4: Implement the migration**

Add this function to `core/rcdvlog-server/src/catalog.rs`, next to the other `create_*` migration functions.

The `filename` is the last `/`-separated component of `relative_path`; SQLite has no `rsplit`, so the backfill uses the `replace`/`instr` idiom on the reversed string. The primary storage object is the lowest `storage_object_id` for determinism.

```rust
/// One-row-per-media-object projection supporting deterministic library sort
/// and filter. Every column is derived from other control-catalog tables, so
/// the table is fully rebuildable and never an authority.
fn create_library_search_index(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE media_search_index (
            media_object_id TEXT PRIMARY KEY REFERENCES media_objects(media_object_id),
            primary_storage_object_id TEXT REFERENCES storage_objects(storage_object_id),
            primary_storage_root_id TEXT REFERENCES storage_roots(storage_root_id),
            filename TEXT NOT NULL DEFAULT '',
            filename_folded TEXT NOT NULL DEFAULT '',
            camera_make TEXT,
            camera_model TEXT,
            camera_folded TEXT NOT NULL DEFAULT '',
            verification_state TEXT NOT NULL,
            size_bytes INTEGER,
            created_at TEXT NOT NULL,
            storage_location_count INTEGER NOT NULL DEFAULT 0,
            project_membership_count INTEGER NOT NULL DEFAULT 0,
            refreshed_at TEXT NOT NULL
        );

        CREATE INDEX media_search_created
            ON media_search_index(created_at DESC, media_object_id ASC);
        CREATE INDEX media_search_filename
            ON media_search_index(filename_folded ASC, media_object_id ASC);
        CREATE INDEX media_search_size
            ON media_search_index(size_bytes DESC, media_object_id ASC);
        CREATE INDEX media_search_state_created
            ON media_search_index(verification_state, created_at DESC, media_object_id ASC);
        CREATE INDEX media_search_root_created
            ON media_search_index(primary_storage_root_id, created_at DESC, media_object_id ASC);
        CREATE INDEX media_search_camera_created
            ON media_search_index(camera_folded, created_at DESC, media_object_id ASC);

        INSERT INTO media_search_index (
            media_object_id, primary_storage_object_id, primary_storage_root_id,
            filename, filename_folded, camera_make, camera_model, camera_folded,
            verification_state, size_bytes, created_at,
            storage_location_count, project_membership_count, refreshed_at
        )
        SELECT
            m.media_object_id,
            p.storage_object_id,
            p.storage_root_id,
            COALESCE(p.filename, ''),
            LOWER(COALESCE(p.filename, '')),
            p.camera_make,
            p.camera_model,
            LOWER(TRIM(COALESCE(p.camera_make, '') || ' ' || COALESCE(p.camera_model, ''))),
            m.verification_state,
            m.size_bytes,
            m.created_at,
            (SELECT COUNT(*) FROM media_object_locations l WHERE l.media_object_id = m.media_object_id),
            (SELECT COUNT(*) FROM project_media_memberships pm WHERE pm.media_object_id = m.media_object_id),
            m.updated_at
        FROM media_objects m
        LEFT JOIN (
            SELECT
                mol.media_object_id AS media_object_id,
                so.storage_object_id AS storage_object_id,
                so.storage_root_id AS storage_root_id,
                REPLACE(so.relative_path, RTRIM(so.relative_path, REPLACE(so.relative_path, '/', '')), '') AS filename,
                pf.camera_make AS camera_make,
                pf.camera_model AS camera_model
            FROM media_object_locations mol
            JOIN storage_objects so ON so.storage_object_id = mol.storage_object_id
            LEFT JOIN media_probe_facts pf ON pf.storage_object_id = so.storage_object_id
            GROUP BY mol.media_object_id
            HAVING so.storage_object_id = MIN(so.storage_object_id)
        ) p ON p.media_object_id = m.media_object_id;
        "#,
    )?;
    Ok(())
}
```

- [ ] **Step 5: Run the test and confirm it passes**

Run: `cargo test -p rcdvlog-server-core library_search_index_migration_backfills_existing_media`
Expected: PASS.

- [ ] **Step 6: Confirm no existing migration test regressed**

Run: `cargo test -p rcdvlog-server-core catalog::`
Expected: PASS. Some existing tests assert the current schema version; update any that hardcode `7` to `8` and note the change in the commit message.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add core/rcdvlog-server/src/catalog.rs
git commit -m "feat(server): add library search index migration with backfill"
```

---

### Task 2: Search-index refresh and rebuild

**Files:**
- Create: `core/rcdvlog-server/src/search_index.rs`
- Modify: `core/rcdvlog-server/src/lib.rs` (add `mod search_index;`)
- Modify: `core/rcdvlog-server/src/library.rs:969-1014` (`link_media_object_location`), and the media-object create/update paths
- Modify: `core/rcdvlog-server/src/scan_handler.rs:1135` (probe-fact write path)

**Interfaces:**
- Consumes: `media_search_index` from Task 1.
- Produces:
  - `pub(crate) fn refresh_media_search_row(tx: &Transaction<'_>, media_object_id: &str) -> rusqlite::Result<()>`
  - `pub fn rebuild_media_search_index(&self) -> Result<u64, StorageLibraryError>` on `StorageLibrary`

- [ ] **Step 1: Write the failing tests**

Create `core/rcdvlog-server/src/search_index.rs` with a test module at the bottom. Use the existing `library.rs` test helpers as the model for building fixtures.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::CONTROL_CATALOG_MIGRATIONS;
    use rcdvlog_core::run_sqlite_migrations;

    fn control(path: &std::path::Path) -> rusqlite::Connection {
        let mut connection = rusqlite::Connection::open(path).unwrap();
        run_sqlite_migrations(&mut connection, CONTROL_CATALOG_MIGRATIONS).unwrap();
        connection
    }

    fn seed(connection: &rusqlite::Connection) {
        connection
            .execute_batch(
                r#"
                INSERT INTO storage_roots(storage_root_id, display_name, immutable_original, enabled, created_at, updated_at)
                    VALUES ('root_a', 'Main NAS', 1, 1, '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z');
                INSERT INTO media_objects(media_object_id, size_bytes, verification_state, created_at, updated_at)
                    VALUES ('media_a', 1024, 'provisional', '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z');
                INSERT INTO storage_objects(storage_object_id, storage_root_id, relative_path, size_bytes, created_at, updated_at)
                    VALUES ('object_b', 'root_a', 'Trips/B.MP4', 1024, '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z'),
                           ('object_a', 'root_a', 'Trips/A.MP4', 1024, '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z');
                INSERT INTO media_object_locations(media_object_id, storage_object_id, verification_state, linked_at, updated_at)
                    VALUES ('media_a', 'object_b', 'provisional', '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z'),
                           ('media_a', 'object_a', 'provisional', '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z');
                "#,
            )
            .unwrap();
    }

    #[test]
    fn refresh_picks_the_lowest_storage_object_as_primary() {
        let directory = tempfile::tempdir().unwrap();
        let mut connection = control(&directory.path().join("control.sqlite3"));
        seed(&connection);
        let tx = connection.transaction().unwrap();
        refresh_media_search_row(&tx, "media_a").unwrap();
        tx.commit().unwrap();

        let (primary, filename, count): (String, String, u32) = connection
            .query_row(
                "SELECT primary_storage_object_id, filename, storage_location_count
                 FROM media_search_index WHERE media_object_id = 'media_a'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(primary, "object_a");
        assert_eq!(filename, "A.MP4");
        assert_eq!(count, 2);
    }

    #[test]
    fn refresh_is_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let mut connection = control(&directory.path().join("control.sqlite3"));
        seed(&connection);
        for _ in 0..3 {
            let tx = connection.transaction().unwrap();
            refresh_media_search_row(&tx, "media_a").unwrap();
            tx.commit().unwrap();
        }
        let rows: u32 = connection
            .query_row("SELECT COUNT(*) FROM media_search_index", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn rebuild_restores_a_truncated_index() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("control.sqlite3");
        let mut connection = control(&path);
        seed(&connection);
        let tx = connection.transaction().unwrap();
        refresh_media_search_row(&tx, "media_a").unwrap();
        tx.commit().unwrap();
        connection.execute("DELETE FROM media_search_index", []).unwrap();
        drop(connection);

        let library = ControlCatalog::open_or_create(path.parent().unwrap(), &config.diagnostics())
            .unwrap()
            .storage_library();
        let repaired = library.rebuild_media_search_index().unwrap();
        assert_eq!(repaired, 1);
    }
}
```

`crate::test_support` does not exist. Build the library the way `library.rs` does: open a `ControlCatalog` over a temporary data directory via `ControlCatalog::open_or_create` and call `.storage_library()` on it (`library.rs:1968-1990`). Inline that here rather than adding a shared helper module.

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cargo test -p rcdvlog-server-core search_index::`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Implement the refresh module**

Write `core/rcdvlog-server/src/search_index.rs`:

```rust
use rusqlite::{Transaction, params};

/// Recomputes one media object's search projection from its authoritative
/// source tables. Safe to call repeatedly and safe to call for a media object
/// that has no locations yet.
pub(crate) fn refresh_media_search_row(
    tx: &Transaction<'_>,
    media_object_id: &str,
) -> rusqlite::Result<()> {
    let primary: Option<(String, String, String)> = tx
        .query_row(
            r#"
            SELECT so.storage_object_id, so.storage_root_id, so.relative_path
            FROM media_object_locations mol
            JOIN storage_objects so ON so.storage_object_id = mol.storage_object_id
            WHERE mol.media_object_id = ?1
            ORDER BY so.storage_object_id ASC
            LIMIT 1
            "#,
            [media_object_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .ok();

    let (primary_object, primary_root, filename) = match &primary {
        Some((object, root, relative_path)) => (
            Some(object.as_str()),
            Some(root.as_str()),
            relative_path.rsplit('/').next().unwrap_or("").to_string(),
        ),
        None => (None, None, String::new()),
    };

    let camera: Option<(Option<String>, Option<String>)> = match primary_object {
        Some(object) => tx
            .query_row(
                "SELECT camera_make, camera_model FROM media_probe_facts
                 WHERE storage_object_id = ?1
                 ORDER BY captured_at DESC LIMIT 1",
                [object],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok(),
        None => None,
    };
    let (camera_make, camera_model) = camera.unwrap_or((None, None));
    let camera_folded = format!(
        "{} {}",
        camera_make.as_deref().unwrap_or(""),
        camera_model.as_deref().unwrap_or("")
    )
    .trim()
    .to_lowercase();

    tx.execute(
        r#"
        INSERT INTO media_search_index (
            media_object_id, primary_storage_object_id, primary_storage_root_id,
            filename, filename_folded, camera_make, camera_model, camera_folded,
            verification_state, size_bytes, created_at,
            storage_location_count, project_membership_count, refreshed_at
        )
        SELECT
            m.media_object_id, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
            m.verification_state, m.size_bytes, m.created_at,
            (SELECT COUNT(*) FROM media_object_locations l WHERE l.media_object_id = m.media_object_id),
            (SELECT COUNT(*) FROM project_media_memberships pm WHERE pm.media_object_id = m.media_object_id),
            ?9
        FROM media_objects m
        WHERE m.media_object_id = ?1
        ON CONFLICT(media_object_id) DO UPDATE SET
            primary_storage_object_id = excluded.primary_storage_object_id,
            primary_storage_root_id = excluded.primary_storage_root_id,
            filename = excluded.filename,
            filename_folded = excluded.filename_folded,
            camera_make = excluded.camera_make,
            camera_model = excluded.camera_model,
            camera_folded = excluded.camera_folded,
            verification_state = excluded.verification_state,
            size_bytes = excluded.size_bytes,
            storage_location_count = excluded.storage_location_count,
            project_membership_count = excluded.project_membership_count,
            refreshed_at = excluded.refreshed_at
        "#,
        params![
            media_object_id,
            primary_object,
            primary_root,
            filename,
            filename.to_lowercase(),
            camera_make,
            camera_model,
            camera_folded,
            crate::library::now(),
        ],
    )?;
    Ok(())
}
```

If `crate::library::now()` is private, make it `pub(crate)` in `library.rs` rather than duplicating the timestamp format.

- [ ] **Step 4: Add the module declaration**

In `core/rcdvlog-server/src/lib.rs`, add `mod search_index;` next to the other module declarations.

- [ ] **Step 5: Add the rebuild method**

Add to `impl StorageLibrary` in `core/rcdvlog-server/src/library.rs`:

```rust
    /// Rebuilds every search-projection row from authoritative tables and
    /// returns the number of media objects refreshed. Used after restore,
    /// after an interrupted scan, and by diagnostics.
    pub fn rebuild_media_search_index(&self) -> Result<u64, StorageLibraryError> {
        let mut connection = open_writable_connection(&self.database_path)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids: Vec<String> = {
            let mut statement = tx.prepare("SELECT media_object_id FROM media_objects")?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        tx.execute(
            "DELETE FROM media_search_index WHERE media_object_id NOT IN (SELECT media_object_id FROM media_objects)",
            [],
        )?;
        for id in &ids {
            crate::search_index::refresh_media_search_row(&tx, id)?;
        }
        tx.commit()?;
        Ok(ids.len() as u64)
    }
```

- [ ] **Step 6: Run the tests and confirm they pass**

Run: `cargo test -p rcdvlog-server-core search_index::`
Expected: PASS.

- [ ] **Step 7: Wire refresh into the write paths**

Three call sites must refresh the affected media object inside their existing transaction:

1. `library.rs:986-1002` `link_media_object_location` currently runs a bare `connection.execute`. Replace that call with a transaction so the link and its projection commit together:

```rust
        let mut connection = open_writable_connection(&self.database_path)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            r#"
            INSERT INTO media_object_locations (
                media_object_id, storage_object_id, verification_state, linked_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?4)
            ON CONFLICT(media_object_id, storage_object_id) DO UPDATE SET
                verification_state = excluded.verification_state,
                updated_at = excluded.updated_at
            "#,
            params![
                media_object_id.as_str(),
                storage_object_id.as_str(),
                verification_state.as_str(),
                now,
            ],
        )?;
        crate::search_index::refresh_media_search_row(&tx, media_object_id.as_str())?;
        tx.commit()?;
```

The existing read-back `query_row` after this block stays as it is.

2. `library.rs` — every site that changes a media object's `verification_state`, `size_bytes`, or `full_content_hash`. Find them with `rg 'UPDATE media_objects' core/rcdvlog-server/src/`. Each already runs inside a transaction or a single `execute`; give each one the same treatment as site 1, adding `refresh_media_search_row(&tx, media_object_id)` before the commit. Do not skip the mismatch-marking path — a media object that becomes `mismatch` must stop matching a `verificationState=verified` filter immediately.

3. `scan_handler.rs:1135` — after `crate::media_facts::upsert_probe_fact(tx, &fact)`, resolve the media object linked to that storage object and refresh it:

```rust
    if let Some(media_object_id) = tx
        .query_row(
            "SELECT media_object_id FROM media_object_locations WHERE storage_object_id = ?1",
            [fact.storage_object_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        crate::search_index::refresh_media_search_row(tx, &media_object_id)?;
    }
```

- [ ] **Step 8: Write the write-path integration test**

Add to the `library.rs` test module:

```rust
#[test]
fn linking_a_location_refreshes_the_search_projection() {
    let fixture = fixture();
    let library = &fixture.library;
    let root = create_root(&library, "root_a");
    let media = create_media(&library, "media_a");
    let object = create_object(&library, "object_a", &root, "Trips/C0103.MP4");
    link(&library, &media, &object);

    let connection = open_read_only_connection(library.database_path()).unwrap();
    let filename: String = connection
        .query_row(
            "SELECT filename FROM media_search_index WHERE media_object_id = ?1",
            [media.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(filename, "C0103.MP4");
}
```

Reuse the existing `library.rs` test helpers verbatim: `fixture()` returns a `Fixture` with a `.library` field (`library.rs:1962-1968`), and `create_root`, `create_media`, `create_object`, and `link` already exist in that module (`library.rs:2090-2126`). Do not add new fixture helpers.

- [ ] **Step 9: Run the full crate tests**

Run: `cargo test -p rcdvlog-server-core`
Expected: PASS.

- [ ] **Step 10: Commit**

```bash
cargo fmt --all
git add core/rcdvlog-server/src/search_index.rs core/rcdvlog-server/src/lib.rs core/rcdvlog-server/src/library.rs core/rcdvlog-server/src/scan_handler.rs
git commit -m "feat(server): maintain and rebuild the library search projection"
```

---

### Task 3: Query model, supported-combination matrix, and cursors

**Files:**
- Create: `core/rcdvlog-server/src/library_query.rs`
- Modify: `core/rcdvlog-server/src/lib.rs` (add `pub mod library_query;`)

**Interfaces:**
- Consumes: `media_search_index` and its indexes from Task 1.
- Produces:

```rust
pub enum LibrarySort { CreatedDesc, FilenameAsc, SizeDesc }
pub struct LibraryFilters {
    pub verification_state: Option<GlobalMediaVerificationState>,
    pub storage_root_id: Option<StorageRootId>,
    pub camera_folded: Option<String>,
    pub filename_prefix: Option<String>,
}
pub struct LibraryQuery { pub limit: u32, pub sort: LibrarySort, pub filters: LibraryFilters, pub after: Option<String> }
pub struct LibraryQueryPlan { pub sql: String, pub bindings: Vec<rusqlite::types::Value> }
pub fn plan_library_query(query: &LibraryQuery) -> Result<LibraryQueryPlan, LibraryQueryError>;
pub fn encode_cursor(sort: LibrarySort, filters: &LibraryFilters, key: &CursorKey) -> String;
pub fn decode_cursor(sort: LibrarySort, filters: &LibraryFilters, raw: &str) -> Result<CursorKey, LibraryQueryError>;
```

`LibraryQueryError::UnsupportedCombination` and `LibraryQueryError::CursorMismatch` both map to the stable code `invalid_query`.

- [ ] **Step 1: Write the failing tests**

Create `core/rcdvlog-server/src/library_query.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn filters() -> LibraryFilters {
        LibraryFilters { verification_state: None, storage_root_id: None, camera_folded: None, filename_prefix: None }
    }

    #[test]
    fn created_sort_with_state_filter_is_supported() {
        let query = LibraryQuery {
            limit: 50,
            sort: LibrarySort::CreatedDesc,
            filters: LibraryFilters {
                verification_state: Some(GlobalMediaVerificationState::Verified),
                ..filters()
            },
            after: None,
        };
        let plan = plan_library_query(&query).unwrap();
        assert!(plan.sql.contains("ORDER BY created_at DESC, media_object_id ASC"));
    }

    #[test]
    fn filename_sort_with_root_filter_is_rejected() {
        let query = LibraryQuery {
            limit: 50,
            sort: LibrarySort::FilenameAsc,
            filters: LibraryFilters {
                storage_root_id: Some(StorageRootId::parse("root_a").unwrap()),
                ..filters()
            },
            after: None,
        };
        assert!(matches!(
            plan_library_query(&query),
            Err(LibraryQueryError::UnsupportedCombination { .. })
        ));
    }

    #[test]
    fn a_cursor_from_another_sort_is_rejected() {
        let key = CursorKey::Created { created_at: "2026-07-20T00:00:00Z".into(), media_object_id: "media_a".into() };
        let raw = encode_cursor(LibrarySort::CreatedDesc, &filters(), &key);
        assert!(matches!(
            decode_cursor(LibrarySort::FilenameAsc, &filters(), &raw),
            Err(LibraryQueryError::CursorMismatch)
        ));
    }

    #[test]
    fn a_cursor_from_another_filter_set_is_rejected() {
        let key = CursorKey::Created { created_at: "2026-07-20T00:00:00Z".into(), media_object_id: "media_a".into() };
        let raw = encode_cursor(LibrarySort::CreatedDesc, &filters(), &key);
        let changed = LibraryFilters {
            verification_state: Some(GlobalMediaVerificationState::Verified),
            ..filters()
        };
        assert!(matches!(
            decode_cursor(LibrarySort::CreatedDesc, &changed, &raw),
            Err(LibraryQueryError::CursorMismatch)
        ));
    }

    #[test]
    fn a_substring_search_is_rejected_in_favour_of_prefix() {
        let query = LibraryQuery {
            limit: 50,
            sort: LibrarySort::FilenameAsc,
            filters: LibraryFilters { filename_prefix: Some("*c010".into()), ..filters() },
            after: None,
        };
        assert!(matches!(
            plan_library_query(&query),
            Err(LibraryQueryError::UnsupportedCombination { .. })
        ));
    }
}
```

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cargo test -p rcdvlog-server-core library_query::`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Implement the query planner**

The supported matrix is checked explicitly, so an unsupported request cannot silently become a scan:

| Sort | Allowed filters | Index used |
| --- | --- | --- |
| `CreatedDesc` | none, or exactly one of `verificationState` / `storageRootId` / `cameraFolded` | `media_search_created`, `media_search_state_created`, `media_search_root_created`, `media_search_camera_created` |
| `FilenameAsc` | none, or `filenamePrefix` | `media_search_filename` |
| `SizeDesc` | none | `media_search_size` |

Write `core/rcdvlog-server/src/library_query.rs`:

```rust
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rcdvlog_core::{GlobalMediaVerificationState, StorageRootId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// `library.rs` has its own private `MAX_PAGE_SIZE` guarding the legacy
// `list_media`. Both are 200. Make that one `pub(crate)` and import it here
// rather than declaring a second constant that can drift.
use crate::library::MAX_PAGE_SIZE;

#[derive(Debug, Error)]
pub enum LibraryQueryError {
    #[error("sort {sort} does not support filter {filter}")]
    UnsupportedCombination { sort: &'static str, filter: &'static str },
    #[error("cursor does not belong to this sort and filter combination")]
    CursorMismatch,
    #[error("invalid {field}: {reason}")]
    InvalidInput { field: &'static str, reason: String },
}

impl LibraryQueryError {
    /// Every planner rejection is one stable client-facing code.
    pub fn code(&self) -> &'static str {
        "invalid_query"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LibrarySort {
    CreatedDesc,
    FilenameAsc,
    SizeDesc,
}

impl LibrarySort {
    fn as_str(self) -> &'static str {
        match self {
            Self::CreatedDesc => "createdDesc",
            Self::FilenameAsc => "filenameAsc",
            Self::SizeDesc => "sizeDesc",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryFilters {
    pub verification_state: Option<GlobalMediaVerificationState>,
    pub storage_root_id: Option<StorageRootId>,
    pub camera_folded: Option<String>,
    pub filename_prefix: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LibraryQuery {
    pub limit: u32,
    pub sort: LibrarySort,
    pub filters: LibraryFilters,
    pub after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "sort", rename_all = "camelCase")]
pub enum CursorKey {
    Created { created_at: String, media_object_id: String },
    Filename { filename_folded: String, media_object_id: String },
    Size { size_bytes: Option<i64>, media_object_id: String },
}

pub struct LibraryQueryPlan {
    pub sql: String,
    pub bindings: Vec<rusqlite::types::Value>,
}

const SELECT_COLUMNS: &str = "media_object_id, primary_storage_object_id, primary_storage_root_id, \
     filename, camera_make, camera_model, verification_state, size_bytes, created_at, \
     storage_location_count, project_membership_count";

pub fn plan_library_query(query: &LibraryQuery) -> Result<LibraryQueryPlan, LibraryQueryError> {
    if query.limit == 0 || query.limit > MAX_PAGE_SIZE {
        return Err(LibraryQueryError::InvalidInput {
            field: "limit",
            reason: format!("must be between 1 and {MAX_PAGE_SIZE}"),
        });
    }

    let filters = &query.filters;
    let named = [
        ("verificationState", filters.verification_state.is_some()),
        ("storageRootId", filters.storage_root_id.is_some()),
        ("cameraFolded", filters.camera_folded.is_some()),
        ("filenamePrefix", filters.filename_prefix.is_some()),
    ];
    let active: Vec<&'static str> = named
        .iter()
        .filter(|(_, present)| *present)
        .map(|(name, _)| *name)
        .collect();
    if active.len() > 1 {
        return Err(LibraryQueryError::UnsupportedCombination {
            sort: query.sort.as_str(),
            filter: active[1],
        });
    }

    let allowed: &[&str] = match query.sort {
        LibrarySort::CreatedDesc => &["verificationState", "storageRootId", "cameraFolded"],
        LibrarySort::FilenameAsc => &["filenamePrefix"],
        LibrarySort::SizeDesc => &[],
    };
    if let Some(filter) = active.first() {
        if !allowed.contains(filter) {
            return Err(LibraryQueryError::UnsupportedCombination {
                sort: query.sort.as_str(),
                filter,
            });
        }
    }

    let mut wheres: Vec<String> = Vec::new();
    let mut bindings: Vec<rusqlite::types::Value> = Vec::new();

    if let Some(state) = filters.verification_state {
        wheres.push(format!("verification_state = ?{}", bindings.len() + 1));
        bindings.push(state.as_str().into());
    }
    if let Some(root) = &filters.storage_root_id {
        wheres.push(format!("primary_storage_root_id = ?{}", bindings.len() + 1));
        bindings.push(root.as_str().into());
    }
    if let Some(camera) = &filters.camera_folded {
        wheres.push(format!("camera_folded = ?{}", bindings.len() + 1));
        bindings.push(camera.to_lowercase().into());
    }
    if let Some(prefix) = &filters.filename_prefix {
        // Prefix only. A wildcard would force LIKE, which cannot use the index.
        if prefix.is_empty() || prefix.contains(['*', '%', '_']) {
            return Err(LibraryQueryError::InvalidInput {
                field: "filenamePrefix",
                reason: "must be a non-empty prefix without wildcards".to_string(),
            });
        }
        let lower = prefix.to_lowercase();
        let upper = prefix_upper_bound(&lower);
        wheres.push(format!(
            "filename_folded >= ?{} AND filename_folded < ?{}",
            bindings.len() + 1,
            bindings.len() + 2
        ));
        bindings.push(lower.into());
        bindings.push(upper.into());
    }

    if let Some(raw) = &query.after {
        let key = decode_cursor(query.sort, filters, raw)?;
        match key {
            CursorKey::Created { created_at, media_object_id } => {
                wheres.push(format!(
                    "(created_at < ?{a} OR (created_at = ?{a} AND media_object_id > ?{b}))",
                    a = bindings.len() + 1,
                    b = bindings.len() + 2
                ));
                bindings.push(created_at.into());
                bindings.push(media_object_id.into());
            }
            CursorKey::Filename { filename_folded, media_object_id } => {
                wheres.push(format!(
                    "(filename_folded > ?{a} OR (filename_folded = ?{a} AND media_object_id > ?{b}))",
                    a = bindings.len() + 1,
                    b = bindings.len() + 2
                ));
                bindings.push(filename_folded.into());
                bindings.push(media_object_id.into());
            }
            CursorKey::Size { size_bytes, media_object_id } => {
                wheres.push(format!(
                    "(size_bytes < ?{a} OR (size_bytes = ?{a} AND media_object_id > ?{b}))",
                    a = bindings.len() + 1,
                    b = bindings.len() + 2
                ));
                bindings.push(size_bytes.map(rusqlite::types::Value::from).unwrap_or(rusqlite::types::Value::Null));
                bindings.push(media_object_id.into());
            }
        }
    }

    let order = match query.sort {
        LibrarySort::CreatedDesc => "created_at DESC, media_object_id ASC",
        LibrarySort::FilenameAsc => "filename_folded ASC, media_object_id ASC",
        LibrarySort::SizeDesc => "size_bytes DESC, media_object_id ASC",
    };
    let clause = if wheres.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", wheres.join(" AND "))
    };
    let limit_index = bindings.len() + 1;
    bindings.push(i64::from(query.limit + 1).into());

    Ok(LibraryQueryPlan {
        sql: format!(
            "SELECT {SELECT_COLUMNS} FROM media_search_index {clause} ORDER BY {order} LIMIT ?{limit_index}"
        ),
        bindings,
    })
}

/// Smallest string strictly greater than every string starting with `prefix`.
fn prefix_upper_bound(prefix: &str) -> String {
    let mut characters: Vec<char> = prefix.chars().collect();
    for index in (0..characters.len()).rev() {
        if let Some(next) = char::from_u32(characters[index] as u32 + 1) {
            characters[index] = next;
            characters.truncate(index + 1);
            return characters.into_iter().collect();
        }
    }
    // Every character was at its maximum; fall back to an open upper bound.
    format!("{prefix}\u{10FFFF}")
}

#[derive(Serialize, Deserialize)]
struct CursorEnvelope {
    v: u8,
    d: String,
    k: CursorKey,
}

fn combination_digest(sort: LibrarySort, filters: &LibraryFilters) -> String {
    let canonical = serde_json::json!({ "sort": sort.as_str(), "filters": filters });
    blake3::hash(canonical.to_string().as_bytes()).to_hex().to_string()
}

pub fn encode_cursor(sort: LibrarySort, filters: &LibraryFilters, key: &CursorKey) -> String {
    let envelope = CursorEnvelope {
        v: 1,
        d: combination_digest(sort, filters),
        k: key.clone(),
    };
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(&envelope).expect("cursor serializes"))
}

pub fn decode_cursor(
    sort: LibrarySort,
    filters: &LibraryFilters,
    raw: &str,
) -> Result<CursorKey, LibraryQueryError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| LibraryQueryError::CursorMismatch)?;
    let envelope: CursorEnvelope =
        serde_json::from_slice(&bytes).map_err(|_| LibraryQueryError::CursorMismatch)?;
    if envelope.v != 1 || envelope.d != combination_digest(sort, filters) {
        return Err(LibraryQueryError::CursorMismatch);
    }
    Ok(envelope.k)
}
```

`blake3`, `base64`, and `tempfile` are already dependencies of `rcdvlog-server-core` (`core/rcdvlog-server/Cargo.toml:10,12,33`). No `Cargo.toml` change is needed.

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p rcdvlog-server-core library_query::`
Expected: PASS.

- [ ] **Step 5: Prove the plans use their indexes**

Add one test that asserts the query planner's SQL is actually index-backed rather than scanning. This is the test that protects the whole design:

```rust
#[test]
fn every_supported_combination_uses_an_index() {
    let directory = tempfile::tempdir().unwrap();
    let mut connection = rusqlite::Connection::open(directory.path().join("control.sqlite3")).unwrap();
    run_sqlite_migrations(&mut connection, CONTROL_CATALOG_MIGRATIONS).unwrap();

    for query in supported_query_matrix() {
        let plan = plan_library_query(&query).unwrap();
        let explanation: String = connection
            .query_row(&format!("EXPLAIN QUERY PLAN {}", plan.sql), [], |row| row.get(3))
            .unwrap();
        assert!(
            explanation.contains("USING INDEX") || explanation.contains("USING COVERING INDEX"),
            "combination produced a scan: {explanation}"
        );
    }
}
```

Write `supported_query_matrix()` as a test helper returning one `LibraryQuery` per row of the matrix table above.

- [ ] **Step 6: Run it**

Run: `cargo test -p rcdvlog-server-core library_query::every_supported_combination_uses_an_index`
Expected: PASS. If any combination scans, add the missing index to migration 8 — it has not shipped yet, so amending it is still legitimate at this point in the branch.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add core/rcdvlog-server/src/library_query.rs core/rcdvlog-server/src/lib.rs
git commit -m "feat(server): add indexed library query planner and validated cursors"
```

---

### Task 4: Library page execution with availability post-filter

**Files:**
- Modify: `core/rcdvlog-server/src/library.rs` (add `search_media`, keep `list_media` unchanged)

**Interfaces:**
- Consumes: `plan_library_query`, `encode_cursor`, `decode_cursor` from Task 3; `media_availability` at `library.rs:1330`.
- Produces:

```rust
pub struct LibrarySearchItem {
    pub media: MediaObject,
    pub filename: String,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub primary_storage_root_id: Option<StorageRootId>,
    pub storage_location_count: u64,
    pub project_membership_count: u64,
    pub availability: NodeAvailabilityState,
}
pub struct LibrarySearchPage {
    pub items: Vec<LibrarySearchItem>,
    pub next_cursor: Option<String>,
    pub availability_filter_applied: bool,
}
pub fn search_media(&self, query: LibraryQuery, availability: Option<NodeAvailabilityState>)
    -> Result<LibrarySearchPage, StorageLibraryError>;
```

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn search_pages_deterministically_across_a_full_archive() {
    let fixture = fixture();
    let library = &fixture.library;
    let root = create_root(&library, "root_a");
    for index in 0..25 {
        let media = create_media(&library, &format!("media_{index:03}"));
        let object = create_object(&library, &format!("object_{index:03}"), &root, &format!("Trips/C{index:04}.MP4"));
        link(&library, &media, &object);
    }

    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = library
            .search_media(
                LibraryQuery { limit: 10, sort: LibrarySort::FilenameAsc, filters: Default::default(), after: cursor.clone() },
                None,
            )
            .unwrap();
        seen.extend(page.items.iter().map(|item| item.filename.clone()));
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(seen.len(), 25);
    let mut sorted = seen.clone();
    sorted.sort();
    assert_eq!(seen, sorted, "pages must not repeat or reorder rows");
}

#[test]
fn availability_filter_marks_itself_as_applied_and_never_pages_the_whole_archive() {
    let fixture = fixture();
    let library = &fixture.library;
    let root = create_root(&library, "root_a");
    let media = create_media(&library, "media_a");
    let object = create_object(&library, "object_a", &root, "Trips/A.MP4");
    link(&library, &media, &object);

    let page = library
        .search_media(
            LibraryQuery { limit: 10, sort: LibrarySort::CreatedDesc, filters: Default::default(), after: None },
            Some(NodeAvailabilityState::AccessibleHere),
        )
        .unwrap();
    assert!(page.availability_filter_applied);
    assert!(page.items.iter().all(|item| item.availability == NodeAvailabilityState::AccessibleHere));
}
```

- [ ] **Step 2: Run and confirm failure**

Run: `cargo test -p rcdvlog-server-core search_media`
Expected: FAIL — `search_media` not found.

- [ ] **Step 3: Implement `search_media`**

Behavior:

1. Validate the limit, decode `after` through `decode_cursor` (returning `invalid_query` on mismatch).
2. Build the plan, fetch `limit + 1` rows from `media_search_index`, and derive `next_cursor` from the last returned row exactly as `list_media` does at `library.rs:1113`.
3. Compute availability for the returned rows only, by calling `media_availability` per row against `self.local_node_id`. Never join availability into the SQL.
4. When `availability` is `Some`, drop non-matching rows from the page **after** computing `next_cursor`, and set `availability_filter_applied = true`. The page may come back shorter than `limit` or even empty while `next_cursor` is still present; that is the documented cost of the post-filter and the client keeps paging.

Document that last point as a `///` comment on `search_media` — it is the contract the web client depends on and the reason `availability_filter_applied` exists.

- [ ] **Step 4: Run and confirm passing**

Run: `cargo test -p rcdvlog-server-core search_media`
Expected: PASS.

- [ ] **Step 5: Add the scale check**

```rust
#[test]
fn search_does_not_load_the_archive_to_serve_one_page() {
    let fixture = fixture();
    let library = &fixture.library;
    let root = create_root(&library, "root_a");
    for index in 0..2000 {
        let media = create_media(&library, &format!("media_{index:05}"));
        let object = create_object(&library, &format!("object_{index:05}"), &root, &format!("Trips/C{index:05}.MP4"));
        link(&library, &media, &object);
    }
    let page = library
        .search_media(
            LibraryQuery { limit: 50, sort: LibrarySort::CreatedDesc, filters: Default::default(), after: None },
            None,
        )
        .unwrap();
    assert_eq!(page.items.len(), 50);
    assert!(page.next_cursor.is_some());
}
```

- [ ] **Step 6: Run the full crate suite**

Run: `cargo test -p rcdvlog-server-core`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add core/rcdvlog-server/src/library.rs
git commit -m "feat(server): execute library search pages with availability post-filter"
```

---

### Task 5: Project list and project asset browse

**Files:**
- Modify: `core/rcdvlog-server/src/replicas.rs` (add `list_projects`, `list_project_assets`)

**Interfaces:**
- Consumes: `ProjectReplicaManager::open_catalog` at `replicas.rs:215`.
- Produces:

```rust
pub struct ProjectSummary {
    pub project_id: String,
    pub state: String,
    pub asset_count: u64,
    pub registered_at: String,
    pub updated_at: String,
}
pub struct ProjectAssetItem {
    pub project_asset_id: String,
    pub filename: String,
    pub media_object_id: Option<String>,
    pub verification_state: Option<String>,
    pub removed: bool,
    pub rotation_degrees: i32,
    pub labels: Vec<String>,
    pub active_note: Option<String>,
}
pub struct ProjectAssetPage { pub items: Vec<ProjectAssetItem>, pub next_cursor: Option<String> }
pub fn list_projects(&self) -> Result<Vec<ProjectSummary>, ProjectReplicaError>;
pub fn list_project_assets(&self, project_id: &str, limit: u32, after: Option<&str>, search: Option<&str>)
    -> Result<ProjectAssetPage, ProjectReplicaError>;
```

Annotation search is served here, from the project replica, and never merged with the library query — that separation is design requirement 4 of the plan's parent task.

**Critical:** the project catalog's `assets` table (`core/rcdvlog-core/src/migrations.rs:400`) holds `path` and `proxy_path`, which are **absolute node-local paths**. Neither may appear in any DTO this task produces. Select columns explicitly; never `SELECT *`.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `core/rcdvlog-server/src/replicas.rs`, reusing whatever replica fixture the existing tests there already build through `import_baseline`:

```rust
#[test]
fn project_assets_page_deterministically_and_exclude_absolute_paths() {
    let fixture = replica_fixture_with_assets(25);
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = fixture
            .manager
            .list_project_assets(&fixture.project_id, 10, cursor.as_deref(), None)
            .unwrap();
        for item in &page.items {
            let encoded = serde_json::to_string(item).unwrap();
            assert!(!encoded.contains('/'), "asset DTO leaked a path: {encoded}");
            seen.push(item.project_asset_id.clone());
        }
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(seen.len(), 25);
    let mut sorted = seen.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), 25, "pages repeated an asset");
}

#[test]
fn project_asset_search_is_scoped_to_one_project() {
    let fixture = replica_fixture_with_assets(3);
    let page = fixture
        .manager
        .list_project_assets(&fixture.project_id, 10, None, Some("asset_00"))
        .unwrap();
    assert!(!page.items.is_empty());
    assert!(page.items.iter().all(|item| item.filename.contains("00")));
}

#[test]
fn listing_projects_reports_asset_counts() {
    let fixture = replica_fixture_with_assets(4);
    let projects = fixture.manager.list_projects().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].project_id, fixture.project_id);
    assert_eq!(projects[0].asset_count, 4);
}

#[test]
fn listing_assets_for_an_unregistered_project_is_a_structured_error() {
    let fixture = replica_fixture_with_assets(1);
    let error = fixture
        .manager
        .list_project_assets("project_missing", 10, None, None)
        .unwrap_err();
    assert!(matches!(error, ProjectReplicaError::NotFound { .. }));
}
```

Write `replica_fixture_with_assets(count)` as a test helper that builds a desktop `Catalog`, adds `count` assets named `asset_000.mov`… through `add_portable_project_asset` with the same `ProjectAssetAddedPayload` shape used at `apps/server/src/lib.rs:1869`, exports a baseline, and imports it through `ProjectReplicaManager::import_baseline`. If `ProjectReplicaError` has no `NotFound` variant, use whichever variant `get` already returns for an unknown project and assert on that instead.

- [ ] **Step 2: Run and confirm failure**

Run: `cargo test -p rcdvlog-server-core replicas::`
Expected: FAIL — methods not found.

- [ ] **Step 3: Implement both methods**

**Crate direction matters here.** `rcdvlog-server-core` depends on `rcdvlog-core`, never the reverse (`core/rcdvlog-server/Cargo.toml:19`; `rcdvlog-core/Cargo.toml` has no server dependency). The SQL below runs against the *project* catalog, which `rcdvlog-core` owns, so `ProjectAssetItem` and `ProjectAssetPage` must be defined in `rcdvlog-core` and re-exported from the server crate. Defining them in `replicas.rs` and returning them from `Catalog` would not compile.

Add to `core/rcdvlog-core/src/operations.rs` (beside the other project projections):

```rust
/// Path-free projection of one project asset. `assets.path` and
/// `assets.proxy_path` are node-local absolutes and are deliberately absent.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectAssetItem {
    pub project_asset_id: String,
    pub filename: String,
    pub camera: String,
    pub media_object_id: Option<String>,
    pub media_verification_state: String,
    pub removed: bool,
    pub rotation_degrees: i32,
    pub labels: Vec<String>,
    pub active_note: Option<String>,
    pub duration_seconds: f64,
    pub best_time: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectAssetPage {
    pub items: Vec<ProjectAssetItem>,
    pub next_cursor: Option<String>,
}
```

`PartialEq` without `Eq` is deliberate: `duration_seconds` is `f64`.

Export both from `core/rcdvlog-core/src/lib.rs`, then add to `impl ProjectReplicaManager` in `core/rcdvlog-server/src/replicas.rs`, importing them with `use rcdvlog_core::{ProjectAssetItem, ProjectAssetPage};`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub project_id: String,
    pub state: String,
    pub asset_count: u64,
    pub registered_at: String,
    pub updated_at: String,
}

impl ProjectReplicaManager {
    pub fn list_projects(&self) -> Result<Vec<ProjectSummary>, ProjectReplicaError> {
        let connection = open_read_only_connection(self.control.database_path())
            .map_err(ProjectReplicaError::Infrastructure)?;
        let rows: Vec<(String, String, String, String)> = {
            let mut statement = connection.prepare(
                "SELECT project_id, state, registered_at, updated_at
                 FROM project_registry ORDER BY project_id ASC",
            )?;
            statement
                .query_map([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };

        let mut summaries = Vec::with_capacity(rows.len());
        for (project_id, state, registered_at, updated_at) in rows {
            let catalog = self.open_catalog(&project_id)?;
            let asset_count = catalog
                .count_project_assets(&project_id)
                .map_err(ProjectReplicaError::Infrastructure)?;
            summaries.push(ProjectSummary {
                project_id,
                state,
                asset_count,
                registered_at,
                updated_at,
            });
        }
        Ok(summaries)
    }

    /// Pages one project's assets by the stable `project_asset_id` order.
    /// Search is a case-folded substring match over this project's own
    /// filename, camera, active note, and labels. A replica is per-project and
    /// bounded, so a substring match is acceptable here in a way it is not for
    /// the global library.
    pub fn list_project_assets(
        &self,
        project_id: &str,
        limit: u32,
        after: Option<&str>,
        search: Option<&str>,
    ) -> Result<ProjectAssetPage, ProjectReplicaError> {
        if limit == 0 || limit > 200 {
            return Err(ProjectReplicaError::InvalidBaseline(
                "limit must be between 1 and 200".to_string(),
            ));
        }
        self.get(project_id)?;
        let catalog = self.open_catalog(project_id)?;
        catalog
            .list_project_asset_page(project_id, limit, after, search)
            .map_err(ProjectReplicaError::Infrastructure)
    }
}
```

Then add the two catalog reads to `core/rcdvlog-core/src/operations.rs` (or wherever `Catalog`'s other project reads live), so the SQL stays with the schema that owns it:

```rust
impl Catalog {
    pub fn count_project_assets(&self, project_id: &str) -> anyhow::Result<u64> {
        let connection = self.connection()?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM assets WHERE project_id = ?1",
            [project_id],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    pub fn list_project_asset_page(
        &self,
        project_id: &str,
        limit: u32,
        after: Option<&str>,
        search: Option<&str>,
    ) -> anyhow::Result<ProjectAssetPage> {
        let connection = self.connection()?;
        let folded = search.map(|value| format!("%{}%", value.to_lowercase()));
        let mut statement = connection.prepare(
            r#"
            SELECT id, filename, camera, media_object_id, media_verification_state,
                   removed, rotation_degrees, labels_json, notes, duration_seconds, best_time
            FROM assets
            WHERE project_id = ?1
              AND (?2 IS NULL OR id > ?2)
              AND (?3 IS NULL
                   OR LOWER(filename) LIKE ?3
                   OR LOWER(camera) LIKE ?3
                   OR LOWER(notes) LIKE ?3
                   OR LOWER(labels_json) LIKE ?3)
            ORDER BY id ASC
            LIMIT ?4
            "#,
        )?;
        let mut items = statement
            .query_map(
                rusqlite::params![project_id, after, folded, i64::from(limit + 1)],
                |row| {
                    let labels_json: String = row.get(7)?;
                    let notes: String = row.get(8)?;
                    Ok(ProjectAssetItem {
                        project_asset_id: row.get(0)?,
                        filename: row.get(1)?,
                        camera: row.get(2)?,
                        media_object_id: row.get(3)?,
                        media_verification_state: row.get(4)?,
                        removed: row.get::<_, i64>(5)? != 0,
                        rotation_degrees: row.get(6)?,
                        labels: serde_json::from_str(&labels_json).unwrap_or_default(),
                        active_note: (!notes.is_empty()).then_some(notes),
                        duration_seconds: row.get(9)?,
                        best_time: row.get(10)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let has_more = items.len() > limit as usize;
        items.truncate(limit as usize);
        let next_cursor = has_more.then(|| {
            items
                .last()
                .expect("a page with more rows has a last item")
                .project_asset_id
                .clone()
        });
        Ok(ProjectAssetPage { items, next_cursor })
    }
}
```

The cursor here is the raw `project_asset_id`, not an encoded envelope, because this list has exactly one sort order and one optional filter — there is no combination to bind a cursor to. If plan 3 adds a second project sort order, migrate this to the `library_query` cursor envelope at that point.

- [ ] **Step 4: Run and confirm passing**

Run: `cargo test -p rcdvlog-server-core replicas::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add core/rcdvlog-server/src/replicas.rs
git commit -m "feat(server): add project list and project asset browse queries"
```

---

### Task 6: Authenticated HTTP endpoints

**Files:**
- Modify: `apps/server/src/lib.rs` (handlers, `internal` router at line 505, `openapi_document` at line 561)

**Interfaces:**
- Consumes: `search_media`, `list_projects`, `list_project_assets`, `media_availability`.
- Produces these routes, all on the authenticated `internal` router:

```text
GET /api/v1/library/media            → LibrarySearchPage
GET /api/v1/library/media/{mediaObjectId} → LibraryMediaDetail
GET /api/v1/projects                 → ProjectSummary[]
GET /api/v1/projects/{projectId}/assets → ProjectAssetPage
```

- [ ] **Step 1: Write the failing HTTP tests**

Add to the existing `tests` module in `apps/server/src/lib.rs`, using the established `Fixture` / `device_auth_token` / `oneshot` / `json` pattern (see `baseline_preview_import_and_reconcile_routes_preserve_protocol_errors` at line 1893):

```rust
#[tokio::test]
async fn library_media_requires_authentication() {
    let fixture = Fixture::new(false);
    let response = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/library/media?limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn library_media_rejects_unsupported_query_combinations() {
    let fixture = Fixture::new(false);
    let token = device_auth_token(&fixture);

    for uri in [
        "/api/v1/library/media?limit=500",
        "/api/v1/library/media?limit=10&sort=filenameAsc&storageRootId=root_a",
        "/api/v1/library/media?limit=10&nonsense=1",
    ] {
        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "uri: {uri}");
        let body = json(response).await;
        assert_eq!(body["code"], "invalid_query", "uri: {uri}");
    }
}

#[tokio::test]
async fn a_cursor_cannot_be_replayed_against_another_sort() {
    let fixture = Fixture::new(false);
    let token = device_auth_token(&fixture);
    let cursor = rcdvlog_server_core::library_query::encode_cursor(
        rcdvlog_server_core::library_query::LibrarySort::CreatedDesc,
        &Default::default(),
        &rcdvlog_server_core::library_query::CursorKey::Created {
            created_at: "2026-07-20T00:00:00Z".to_string(),
            media_object_id: "media_a".to_string(),
        },
    );
    let response = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/library/media?limit=10&sort=filenameAsc&after={cursor}"))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json(response).await["code"], "invalid_query");
}

#[tokio::test]
async fn project_asset_responses_contain_no_absolute_paths() {
    let fixture = Fixture::new(false);
    let token = device_auth_token(&fixture);
    let source = TempDir::new().unwrap();
    let envelope = baseline_envelope(&source);
    let project_id = envelope.payload.project.project_id.clone();
    let body = serde_json::to_vec(&envelope).unwrap();
    let imported = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/internal/projects/baseline/import")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(imported.status(), StatusCode::OK);

    let response = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/projects/{project_id}/assets?limit=10"))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let text = json(response).await.to_string();
    assert!(!text.contains(source.path().to_str().unwrap()));
    assert!(!text.contains("\"path\""));
    assert!(!text.contains("proxyPath"));
}
```

- [ ] **Step 2: Run and confirm failure**

Run: `cargo test -p rcdvlog-server library`
Expected: FAIL — routes return 404, so the status assertions fail.

- [ ] **Step 3: Implement the handlers**

Add to `apps/server/src/lib.rs`. `deny_unknown_fields` is what makes a typo'd parameter an error instead of a silently unfiltered page:

```rust
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LibraryMediaParams {
    #[serde(default = "default_page_limit")]
    limit: u32,
    #[serde(default)]
    sort: Option<LibrarySort>,
    #[serde(default)]
    verification_state: Option<GlobalMediaVerificationState>,
    #[serde(default)]
    storage_root_id: Option<String>,
    #[serde(default)]
    camera: Option<String>,
    #[serde(default)]
    filename_prefix: Option<String>,
    #[serde(default)]
    availability: Option<NodeAvailabilityState>,
    #[serde(default)]
    after: Option<String>,
}

fn default_page_limit() -> u32 {
    50
}

fn query_error_response(request_id: &RequestId, code: &'static str, status: StatusCode) -> Response {
    (
        status,
        Json(ErrorResponse {
            request_id: request_id.0.clone(),
            code,
        }),
    )
        .into_response()
}

async fn list_library_media(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    params: Result<Query<LibraryMediaParams>, QueryRejection>,
) -> Response {
    let Query(params) = match params {
        Ok(value) => value,
        Err(_) => return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST),
    };

    let storage_root_id = match params.storage_root_id.as_deref().map(StorageRootId::parse) {
        Some(Ok(value)) => Some(value),
        Some(Err(_)) => {
            return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
        }
        None => None,
    };

    let query = LibraryQuery {
        limit: params.limit,
        sort: params.sort.unwrap_or(LibrarySort::CreatedDesc),
        filters: LibraryFilters {
            verification_state: params.verification_state,
            storage_root_id,
            camera_folded: params.camera,
            filename_prefix: params.filename_prefix,
        },
        after: params.after,
    };

    match state
        .foundation
        .storage_library()
        .search_media(query, params.availability)
    {
        Ok(page) => (StatusCode::OK, Json(page)).into_response(),
        Err(StorageLibraryError::Query(error)) => {
            query_error_response(&request_id, error.code(), StatusCode::BAD_REQUEST)
        }
        Err(StorageLibraryError::InvalidInput { .. }) => {
            query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST)
        }
        Err(StorageLibraryError::NotFound { .. }) => {
            query_error_response(&request_id, "not_found", StatusCode::NOT_FOUND)
        }
        Err(_) => query_error_response(&request_id, "internal_error", StatusCode::INTERNAL_SERVER_ERROR),
    }
}
```

`StorageLibraryError` needs a `Query(#[from] LibraryQueryError)` variant added in `core/rcdvlog-server/src/library.rs:22` so `search_media` can propagate planner rejections without stringly-typed matching.

Write `get_library_media_detail`, `list_projects`, and `list_project_assets` handlers on the same shape: typed `deny_unknown_fields` params, the same error mapping, `Json` on success.

- [ ] **Step 4: Register the routes**

In `router()` at `apps/server/src/lib.rs:505`, add to the `internal` router — which already applies `auth_middleware` — before its `.layer(...)` calls:

```rust
        .route("/api/v1/library/media", get(list_library_media))
        .route(
            "/api/v1/library/media/{media_object_id}",
            get(get_library_media_detail),
        )
        .route("/api/v1/projects", get(list_projects))
        .route(
            "/api/v1/projects/{project_id}/assets",
            get(list_project_assets),
        )
```

- [ ] **Step 5: Run and confirm passing**

Run: `cargo test -p rcdvlog-server library`
Expected: PASS.

- [ ] **Step 6: Extend the OpenAPI document**

Add the four paths and their response schemas to `openapi_document()` at `apps/server/src/lib.rs:561`, following the existing entries exactly.

Then add the new paths to the hardcoded `API_PATHS` block in `scripts/sync-openapi.mjs:21-27`:

```js
export const API_PATHS = {
  version: "/api/v1/version",
  ready: "/api/v1/ready",
  sessions: "/api/v1/auth/sessions",
  bootstrapExchange: "/api/v1/auth/bootstrap/exchange",
  libraryMedia: "/api/v1/library/media",
  projects: "/api/v1/projects",
} as const;
```

- [ ] **Step 7: Regenerate and verify the client artifacts**

```bash
pnpm generate:openapi
pnpm check:openapi
```

Expected: `generate` rewrites `packages/project-client/openapi.json` and `src/generated.ts`; `check` then exits zero.

- [ ] **Step 8: Run the full gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm check:openapi
```

Expected: all pass.

- [ ] **Step 9: Commit**

```bash
git add apps/server/src/lib.rs scripts/sync-openapi.mjs packages/project-client/openapi.json packages/project-client/src/generated.ts
git commit -m "feat(server): expose authenticated library and project browse endpoints"
```

---

## Plan completion check

Before handing off to plan 2, confirm against the parent task's acceptance criteria:

- [ ] Deterministic pages over thousands of synthetic records, no full-data fetch (Task 4 Steps 1 and 5)
- [ ] Search/filter/sort combinations return correct stable results (Tasks 3 and 4)
- [ ] Unsupported combination and replayed cursor return `invalid_query` (Task 3 Steps 1 and 3, Task 6 Step 1)
- [ ] Annotation search is project-scoped, library search is global, neither claims the other (Task 5)
- [ ] Verification and node-relative availability accurately represented (Task 4 Step 3)
- [ ] Committed OpenAPI and generated client match the Rust contract (Task 6 Step 6)
- [ ] No absolute path in any DTO (Task 6 Step 1)

The remaining parent acceptance criteria — add-to-project, idempotency, projection repair after an injected crash, offline-Mac behavior, and every UI criterion — belong to plans 2 and 3 and must stay open.
