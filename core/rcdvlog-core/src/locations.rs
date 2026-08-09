//! Node-local physical locations for project assets, and the verification that
//! makes them trustworthy.
//!
//! `assets.path` is a denormalized cache of the preferred location on this node;
//! `project_asset_local_locations` is the model of record. Project operations are
//! deliberately path-free, so nothing here is ever replicated — a location is a
//! fact about this machine and no other.
//!
//! The rule this module exists to enforce: **a pathname is not an identity.**
//! Before `docs/tasks/import/ARCHITECTURE.md` §3, "the original is available"
//! meant "some file exists at that pathname," which let a reformatted card that
//! reused `.../C0012.MP4` silently bind an old asset to entirely new bytes.
//! Availability now requires that the bytes were checked.
//!
//! Checking costs a hash, so it is cached behind a `(size, mtime)` guard: a
//! location verified once stays verified for free until the file changes
//! underneath it, at which point it is re-verified rather than trusted.

use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::time::UNIX_EPOCH;

pub use crate::storage::LocationVerificationState;

/// Cheap filesystem evidence that a verified file has not been replaced.
///
/// Not a security boundary and not a corruption check — it is the staleness
/// signal that decides whether the fingerprint must be recomputed. A file whose
/// size and modification time both match what was recorded at verification time
/// is taken to be the file that was verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocationGuard {
    pub size_bytes: i64,
    /// Milliseconds since the Unix epoch, or `None` when the platform declines
    /// to report a modification time.
    pub mtime_millis: Option<i64>,
}

impl LocationGuard {
    /// Reads the guard for a path that is expected to be a regular file.
    pub fn read(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        if !metadata.is_file() {
            return None;
        }
        Some(Self {
            size_bytes: metadata.len() as i64,
            mtime_millis: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .and_then(|since| i64::try_from(since.as_millis()).ok()),
        })
    }

    /// True when this guard is evidence that `recorded` still describes the same
    /// file.
    ///
    /// A missing recorded size means the location was never verified and cannot
    /// be vouched for. A missing *modification time* on either side degrades to
    /// a size-only comparison rather than failing closed, because a platform
    /// that will not report mtime would otherwise re-hash on every single read.
    pub(crate) fn matches(self, recorded: Option<Self>) -> bool {
        let Some(recorded) = recorded else {
            return false;
        };
        if self.size_bytes != recorded.size_bytes {
            return false;
        }
        match (self.mtime_millis, recorded.mtime_millis) {
            (Some(observed), Some(recorded)) => observed == recorded,
            _ => true,
        }
    }
}

/// What a location row says about itself, as read back from the catalog.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RecordedLocation {
    pub state: LocationVerificationState,
    pub guard: Option<LocationGuard>,
}

/// Decides whether a stored location may be treated as online without hashing.
///
/// This is the advisory answer used to render lists and counts. It is
/// deliberately optimistic for a location that predates verification, so that
/// upgrading a catalog does not report every clip offline; the strict answer
/// lives in [`crate::Catalog::resolve_original`], which every operation that
/// actually reads original bytes goes through.
pub(crate) fn location_is_available(
    path: &str,
    state: Option<LocationVerificationState>,
    recorded: Option<LocationGuard>,
) -> bool {
    if path.is_empty() {
        return false;
    }
    let Some(observed) = LocationGuard::read(Path::new(path)) else {
        return false;
    };
    match state {
        // Proven wrong. Never usable, and never silently rebound by a later scan
        // — only an explicit re-verification can move it out of this state.
        Some(LocationVerificationState::Mismatch) => false,
        // Proven right, and still the same file by cheap evidence.
        Some(LocationVerificationState::Verified) => observed.matches(recorded),
        // Never checked, or checked but not yet proven. The file is there, so
        // report it and let the strict gate decide when bytes are read.
        _ => true,
    }
}

/// Reads what the catalog records about one node-local location.
pub(crate) fn recorded_location(
    connection: &Connection,
    asset_id: &str,
    path: &str,
) -> Result<Option<RecordedLocation>> {
    let row = connection
        .query_row(
            r#"
            SELECT verification_state, verified_size_bytes, verified_mtime_millis
            FROM project_asset_local_locations
            WHERE asset_id = ?1 AND local_path = ?2
              AND node_id = (SELECT node_id FROM catalog_identity WHERE singleton_id = 1)
            "#,
            params![asset_id, path],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((state, size_bytes, mtime_millis)) = row else {
        return Ok(None);
    };
    Ok(Some(RecordedLocation {
        state: LocationVerificationState::parse_stored(&state)
            .unwrap_or(LocationVerificationState::Unverified),
        guard: size_bytes.map(|size_bytes| LocationGuard {
            size_bytes,
            mtime_millis,
        }),
    }))
}

/// The outcome of checking one location, ready to be written down.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Verification<'a> {
    pub state: LocationVerificationState,
    /// The fingerprint actually observed, kept even for a mismatch so a later
    /// scan can explain what the file turned out to be.
    pub fingerprint: Option<&'a str>,
    pub guard: Option<LocationGuard>,
    pub at: &'a str,
}

/// Records the outcome of verifying `path` for `asset_id` on this node.
///
/// Create-or-update on `(asset_id, node_id, local_path)`, so an asset can hold
/// several known locations at once — a mounted card and the archive copy taken
/// from it are both true at the same time.
pub(crate) fn record_verification(
    connection: &Connection,
    asset_id: &str,
    node_id: &str,
    path: &str,
    verification: Verification<'_>,
) -> Result<()> {
    let Verification {
        state,
        fingerprint,
        guard,
        at: now,
    } = verification;
    connection.execute(
        r#"
        INSERT INTO project_asset_local_locations(
            asset_id, node_id, local_path, created_at,
            verification_state, verified_fingerprint,
            verified_size_bytes, verified_mtime_millis, verified_at
        ) VALUES (?1, ?2, ?3, ?8, ?4, ?5, ?6, ?7, ?8)
        ON CONFLICT(asset_id, node_id, local_path) DO UPDATE SET
            verification_state = excluded.verification_state,
            verified_fingerprint = excluded.verified_fingerprint,
            verified_size_bytes = excluded.verified_size_bytes,
            verified_mtime_millis = excluded.verified_mtime_millis,
            verified_at = excluded.verified_at
        "#,
        params![
            asset_id,
            node_id,
            path,
            state.as_str(),
            fingerprint,
            guard.map(|guard| guard.size_bytes),
            guard.and_then(|guard| guard.mtime_millis),
            now,
        ],
    )?;
    Ok(())
}

/// Returns a known location for this asset, other than `exclude`, that currently
/// looks usable.
///
/// Used when repairing an asset whose recorded path went offline: before
/// adopting a newly discovered path, there is no point disturbing an asset that
/// already resolves somewhere else.
pub(crate) fn any_available_location(
    connection: &Connection,
    asset_id: &str,
    exclude: &str,
) -> Result<Option<String>> {
    let mut statement = connection.prepare(
        r#"
        SELECT local_path, verification_state, verified_size_bytes, verified_mtime_millis
        FROM project_asset_local_locations
        WHERE asset_id = ?1 AND local_path != ?2
          AND node_id = (SELECT node_id FROM catalog_identity WHERE singleton_id = 1)
          AND verification_state != 'mismatch'
        ORDER BY verification_state = 'verified' DESC, verified_at DESC
        "#,
    )?;
    let rows = statement.query_map(params![asset_id, exclude], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<i64>>(3)?,
        ))
    })?;
    for row in rows {
        let (path, state, size_bytes, mtime_millis) = row?;
        let guard = size_bytes.map(|size_bytes| LocationGuard {
            size_bytes,
            mtime_millis,
        });
        let state = LocationVerificationState::parse_stored(&state);
        if location_is_available(&path, state, guard) {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

/// Points `assets.path` at a location, keeping the denormalized cache honest.
pub(crate) fn set_preferred_path(
    connection: &Connection,
    asset_id: &str,
    path: &str,
) -> Result<()> {
    connection.execute(
        "UPDATE assets SET path = ?2 WHERE id = ?1",
        params![asset_id, path],
    )?;
    Ok(())
}

/// Fails with the message the UI shows when an original cannot be trusted.
pub(crate) fn mismatch_error(path: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "the file at {path} is not this clip any more — its content changed since it was imported. \
         Locate the original, or re-import it as new footage."
    )
}

/// Guard against calling verification with an asset that has no recorded path.
pub(crate) fn require_path(asset_id: &str, path: &str) -> Result<()> {
    if path.is_empty() {
        bail!("asset {asset_id} has no recorded media location on this machine");
    }
    Ok(())
}
