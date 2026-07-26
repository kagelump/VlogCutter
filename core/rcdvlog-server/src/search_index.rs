use rusqlite::{OptionalExtension, Transaction, params};

/// Recomputes one media object's search projection from its authoritative
/// source tables. Safe to call repeatedly and safe to call for a media object
/// that has no locations yet.
///
/// The primary storage object for a media object is always the one with the
/// lexicographically lowest `storage_object_id` among its locations -- the
/// same rule the `library_search_index` migration backfill uses, so a refresh
/// never disagrees with a backfill.
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
        .optional()?;

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
            .optional()?,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::CONTROL_CATALOG_MIGRATIONS;
    use crate::{ControlCatalog, ServerConfig};
    use rcdvlog_core::run_sqlite_migrations;
    use std::collections::BTreeMap;
    use std::fs;

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

    fn control_catalog(data_dir: &std::path::Path) -> ControlCatalog {
        let derived = data_dir.join("derived");
        let backups = data_dir.join("backups");
        fs::create_dir_all(data_dir).unwrap();
        fs::create_dir_all(&derived).unwrap();
        fs::create_dir_all(&backups).unwrap();
        let config = ServerConfig::from_values(BTreeMap::from([
            (
                "RCDVLOG_DATA_DIR".to_string(),
                data_dir.display().to_string(),
            ),
            (
                "RCDVLOG_DERIVED_DIR".to_string(),
                derived.display().to_string(),
            ),
            (
                "RCDVLOG_BACKUP_DIR".to_string(),
                backups.display().to_string(),
            ),
        ]))
        .unwrap();
        ControlCatalog::open_or_create(data_dir, &config.diagnostics()).unwrap()
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
            .query_row("SELECT COUNT(*) FROM media_search_index", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn rebuild_restores_a_truncated_index() {
        let directory = tempfile::tempdir().unwrap();
        let data_dir = directory.path().join("state");
        let path = data_dir.join("control.sqlite3");
        let catalog = control_catalog(&data_dir);
        let library = catalog.storage_library();

        let mut connection = rusqlite::Connection::open(&path).unwrap();
        seed(&connection);
        let tx = connection.transaction().unwrap();
        refresh_media_search_row(&tx, "media_a").unwrap();
        tx.commit().unwrap();
        connection
            .execute("DELETE FROM media_search_index", [])
            .unwrap();
        drop(connection);

        let repaired = library.rebuild_media_search_index().unwrap();
        assert_eq!(repaired, 1);
    }
}
