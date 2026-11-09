//! SQL row mapping and schema-introspection helpers for asset queries.

use crate::*;
use anyhow::Result;
use rusqlite::Connection;
use std::fs;
use std::path::Path;

pub(crate) fn asset_column_exists(connection: &Connection, column: &str) -> Result<bool> {
    table_column_exists(connection, "assets", column)
}

pub(crate) fn table_column_exists(
    connection: &Connection,
    table: &str,
    column: &str,
) -> Result<bool> {
    Ok(connection
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == column))
}

pub(crate) fn table_exists(connection: &Connection, table: &str) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )?)
}

pub(crate) fn camera_proxy_asset_columns(connection: &Connection) -> Result<String> {
    if table_exists(connection, "camera_proxy_sidecars")? {
        Ok(
            "(SELECT path FROM camera_proxy_sidecars cps WHERE cps.asset_id = a.id), \
            (SELECT byte_length FROM camera_proxy_sidecars cps WHERE cps.asset_id = a.id)"
                .into(),
        )
    } else {
        Ok("NULL, NULL".into())
    }
}

/// Select expressions and the join that carry the verification state of the
/// location `assets.path` currently points at.
///
/// A catalog old enough to predate verification returns literals and no join, so
/// every asset query keeps working against a legacy schema exactly as the
/// `asset_column_expression` guards already do elsewhere in this file.
pub(crate) fn location_columns(
    connection: &Connection,
) -> Result<(&'static str, &'static str, &'static str, &'static str)> {
    if table_column_exists(
        connection,
        "project_asset_local_locations",
        "verification_state",
    )? {
        Ok((
            "l.verification_state",
            "l.verified_size_bytes",
            "l.verified_mtime_millis",
            "LEFT JOIN project_asset_local_locations l \
             ON l.asset_id = a.id AND l.local_path = a.path \
             AND l.node_id = (SELECT node_id FROM catalog_identity WHERE singleton_id = 1)",
        ))
    } else {
        Ok(("NULL", "NULL", "NULL", ""))
    }
}

pub(crate) fn asset_column_expression(
    connection: &Connection,
    column: &str,
    present_expression: &'static str,
    missing_expression: &'static str,
) -> Result<&'static str> {
    Ok(if asset_column_exists(connection, column)? {
        present_expression
    } else {
        missing_expression
    })
}

pub(crate) fn table_column_expression(
    connection: &Connection,
    table: &str,
    column: &str,
    present_expression: &'static str,
    missing_expression: &'static str,
) -> Result<&'static str> {
    Ok(if table_column_exists(connection, table, column)? {
        present_expression
    } else {
        missing_expression
    })
}

pub(crate) fn row_to_asset(row: &rusqlite::Row<'_>) -> rusqlite::Result<Asset> {
    let path: String = row.get(2)?;
    let proxy_path: Option<String> = row.get(15)?;
    // Advisory availability: cheap enough to run for every row in a list, which
    // rules out hashing. A location proven to hold the wrong bytes is excluded
    // outright, and one proven correct is trusted only while its size and mtime
    // still match. Anything that reads original bytes goes through
    // `Catalog::resolve_original` instead, which hashes.
    let location_state = row
        .get::<_, Option<String>>(29)?
        .as_deref()
        .and_then(LocationVerificationState::parse_stored);
    let location_guard =
        row.get::<_, Option<i64>>(30)?
            .map(|size_bytes| locations::LocationGuard {
                size_bytes,
                mtime_millis: row.get::<_, Option<i64>>(31).unwrap_or(None),
            });
    let original_available =
        locations::location_is_available(&path, location_state, location_guard);
    let proxy_available = proxy_path
        .as_deref()
        .is_some_and(|path| Path::new(path).is_file());
    let camera_proxy_path: Option<String> = row.get(33)?;
    let camera_proxy_size: Option<i64> = row.get(34)?;
    let camera_proxy_available = camera_proxy_path
        .as_deref()
        .zip(camera_proxy_size)
        .is_some_and(|(path, size)| {
            fs::metadata(path)
                .map(|metadata| metadata.is_file() && metadata.len() == size as u64)
                .unwrap_or(false)
        });
    let stored_thumbnail_path: Option<String> = row.get(20)?;
    let stored_thumbnail_quality = row.get::<_, i64>(21)? as u8;
    let thumbnail_available = stored_thumbnail_path
        .as_deref()
        .is_some_and(|path| Path::new(path).is_file());
    let available = original_available || proxy_available || camera_proxy_available;
    let media_verification_state = match row.get::<_, String>(26)?.as_str() {
        "unlinked" => MediaVerificationState::Unlinked,
        "provisional" => MediaVerificationState::Provisional,
        "verified" => MediaVerificationState::Verified,
        "mismatch" => MediaVerificationState::Mismatch,
        value => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                26,
                rusqlite::types::Type::Text,
                format!("invalid media verification state: {value}").into(),
            ));
        }
    };
    Ok(Asset {
        id: row.get(0)?,
        project_id: row.get(1)?,
        path,
        filename: row.get(3)?,
        fingerprint: row.get(4)?,
        size_bytes: row.get::<_, i64>(5)? as u64,
        duration_seconds: row.get(6)?,
        frame_rate: row.get(7)?,
        width: row.get::<_, i64>(8)? as u32,
        height: row.get::<_, i64>(9)? as u32,
        codec: row.get(10)?,
        camera: row.get(11)?,
        captured_at_raw: row.get(12)?,
        best_time: row.get(13)?,
        offset_seconds: row.get(14)?,
        proxy_representation: proxy_path.as_deref().map(proxy_representation),
        proxy_path,
        camera_proxy_available,
        metadata_rotation_degrees: row.get::<_, i64>(16)? as u16,
        default_rotation_degrees: row.get::<_, i64>(17)? as u16,
        rotation_degrees: row.get::<_, i64>(18)? as u16,
        orientation_probed: row.get::<_, i64>(19)? != 0,
        orientation_decision: decode_orientation_decision(row.get::<_, Option<String>>(35)?)?,
        original_available,
        proxy_available,
        thumbnail_path: thumbnail_available
            .then_some(stored_thumbnail_path)
            .flatten(),
        thumbnail_quality: if thumbnail_available {
            stored_thumbnail_quality
        } else {
            0
        },
        notes: row.get(22)?,
        title: row.get(27)?,
        description: row.get(28)?,
        labels: serde_json::from_str(&row.get::<_, String>(23)?).unwrap_or_default(),
        removed: row.get::<_, i64>(24)? != 0,
        media_object_id: row.get(25)?,
        media_verification_state,
        available,
        revision: row.get::<_, i64>(32)? as u64,
    })
}

/// The asset's operation revision, and the join that supplies it.
///
/// One `LEFT JOIN` on a primary key rather than a query per asset: `list_assets`
/// runs this for every clip in a project, and the target is thousands.
/// A catalog predating the operations log has no such table and reports 0,
/// which is the same revision `reconcile_asset_baseline` gives an asset that
/// has never been written through an operation.
pub(crate) fn revision_columns(
    connection: &rusqlite::Connection,
) -> Result<(&'static str, &'static str)> {
    Ok(if table_exists(connection, "materialized_revisions")? {
        (
            "COALESCE(r.revision, 0)",
            "LEFT JOIN materialized_revisions r ON r.project_id = a.project_id AND r.aggregate_type = 'projectAsset' AND r.aggregate_id = a.id",
        )
    } else {
        ("0", "")
    })
}
