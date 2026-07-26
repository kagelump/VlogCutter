//! Query planner for the global media library's indexed search projection
//! (`media_search_index`, created by migration 8 in `catalog.rs`).
//!
//! The planner only ever emits SQL that one of the six indexes created
//! alongside that table can serve. An unsupported sort/filter combination is
//! rejected outright -- it never falls back to a full-table scan. Cursors are
//! opaque tokens bound to the exact sort and filter combination they were
//! issued for; replaying one against a different sort or filter set is
//! rejected rather than silently reinterpreted.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rcdvlog_core::{GlobalMediaVerificationState, StorageRootId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// `library.rs` has its own `MAX_PAGE_SIZE` guarding the legacy `list_media`.
// Both are 200. Reuse that one instead of declaring a second constant that
// can drift.
use crate::library::MAX_PAGE_SIZE;

#[derive(Debug, Error)]
pub enum LibraryQueryError {
    #[error("sort {sort} does not support filter {filter}")]
    UnsupportedCombination {
        sort: &'static str,
        filter: &'static str,
    },
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
    Created {
        created_at: String,
        media_object_id: String,
    },
    Filename {
        filename_folded: String,
        media_object_id: String,
    },
    Size {
        size_bytes: Option<i64>,
        media_object_id: String,
    },
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
    if let Some(filter) = active.first()
        && !allowed.contains(filter)
    {
        return Err(LibraryQueryError::UnsupportedCombination {
            sort: query.sort.as_str(),
            filter,
        });
    }

    let mut wheres: Vec<String> = Vec::new();
    let mut bindings: Vec<rusqlite::types::Value> = Vec::new();

    if let Some(state) = filters.verification_state {
        wheres.push(format!("verification_state = ?{}", bindings.len() + 1));
        bindings.push(state.as_str().to_string().into());
    }
    if let Some(root) = &filters.storage_root_id {
        wheres.push(format!("primary_storage_root_id = ?{}", bindings.len() + 1));
        bindings.push(root.as_str().to_string().into());
    }
    if let Some(camera) = &filters.camera_folded {
        wheres.push(format!("camera_folded = ?{}", bindings.len() + 1));
        bindings.push(camera.to_lowercase().into());
    }
    if let Some(prefix) = &filters.filename_prefix {
        // Prefix only. A wildcard would force a substring scan, which no
        // index can serve -- so it is an unsupported combination, not merely
        // an invalid value.
        if prefix.is_empty() || prefix.contains(['*', '%', '_']) {
            return Err(LibraryQueryError::UnsupportedCombination {
                sort: query.sort.as_str(),
                filter: "filenamePrefixWildcard",
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
            CursorKey::Created {
                created_at,
                media_object_id,
            } => {
                wheres.push(format!(
                    "(created_at < ?{a} OR (created_at = ?{a} AND media_object_id > ?{b}))",
                    a = bindings.len() + 1,
                    b = bindings.len() + 2
                ));
                bindings.push(created_at.into());
                bindings.push(media_object_id.into());
            }
            CursorKey::Filename {
                filename_folded,
                media_object_id,
            } => {
                wheres.push(format!(
                    "(filename_folded > ?{a} OR (filename_folded = ?{a} AND media_object_id > ?{b}))",
                    a = bindings.len() + 1,
                    b = bindings.len() + 2
                ));
                bindings.push(filename_folded.into());
                bindings.push(media_object_id.into());
            }
            CursorKey::Size {
                size_bytes,
                media_object_id,
            } => {
                wheres.push(format!(
                    "(size_bytes < ?{a} OR (size_bytes = ?{a} AND media_object_id > ?{b}))",
                    a = bindings.len() + 1,
                    b = bindings.len() + 2
                ));
                bindings.push(
                    size_bytes
                        .map(rusqlite::types::Value::from)
                        .unwrap_or(rusqlite::types::Value::Null),
                );
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
    blake3::hash(canonical.to_string().as_bytes())
        .to_hex()
        .to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn filters() -> LibraryFilters {
        LibraryFilters {
            verification_state: None,
            storage_root_id: None,
            camera_folded: None,
            filename_prefix: None,
        }
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
        assert!(
            plan.sql
                .contains("ORDER BY created_at DESC, media_object_id ASC")
        );
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
        let key = CursorKey::Created {
            created_at: "2026-07-20T00:00:00Z".into(),
            media_object_id: "media_a".into(),
        };
        let raw = encode_cursor(LibrarySort::CreatedDesc, &filters(), &key);
        assert!(matches!(
            decode_cursor(LibrarySort::FilenameAsc, &filters(), &raw),
            Err(LibraryQueryError::CursorMismatch)
        ));
    }

    #[test]
    fn a_cursor_from_another_filter_set_is_rejected() {
        let key = CursorKey::Created {
            created_at: "2026-07-20T00:00:00Z".into(),
            media_object_id: "media_a".into(),
        };
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
            filters: LibraryFilters {
                filename_prefix: Some("*c010".into()),
                ..filters()
            },
            after: None,
        };
        assert!(matches!(
            plan_library_query(&query),
            Err(LibraryQueryError::UnsupportedCombination { .. })
        ));
    }

    /// One `LibraryQuery` per row of the supported sort/filter matrix in the
    /// task brief, plus a cursor-bearing variant of each row -- a
    /// continuation predicate can change which index SQLite picks, so the
    /// cursor-present shape has to be proven independently of the bare one.
    fn supported_query_matrix() -> Vec<LibraryQuery> {
        let mut queries = Vec::new();

        let created_filter_sets = [
            LibraryFilters::default(),
            LibraryFilters {
                verification_state: Some(GlobalMediaVerificationState::Verified),
                ..LibraryFilters::default()
            },
            LibraryFilters {
                storage_root_id: Some(StorageRootId::parse("root_matrix").unwrap()),
                ..LibraryFilters::default()
            },
            LibraryFilters {
                camera_folded: Some("acme cam-1".into()),
                ..LibraryFilters::default()
            },
        ];
        for filter_set in created_filter_sets {
            queries.push(LibraryQuery {
                limit: 50,
                sort: LibrarySort::CreatedDesc,
                filters: filter_set.clone(),
                after: None,
            });
            let key = CursorKey::Created {
                created_at: "2026-07-20T00:00:00Z".into(),
                media_object_id: "media_a".into(),
            };
            let after = encode_cursor(LibrarySort::CreatedDesc, &filter_set, &key);
            queries.push(LibraryQuery {
                limit: 50,
                sort: LibrarySort::CreatedDesc,
                filters: filter_set,
                after: Some(after),
            });
        }

        let filename_filter_sets = [
            LibraryFilters::default(),
            LibraryFilters {
                filename_prefix: Some("clip".into()),
                ..LibraryFilters::default()
            },
        ];
        for filter_set in filename_filter_sets {
            queries.push(LibraryQuery {
                limit: 50,
                sort: LibrarySort::FilenameAsc,
                filters: filter_set.clone(),
                after: None,
            });
            let key = CursorKey::Filename {
                filename_folded: "clip_001.mov".into(),
                media_object_id: "media_a".into(),
            };
            let after = encode_cursor(LibrarySort::FilenameAsc, &filter_set, &key);
            queries.push(LibraryQuery {
                limit: 50,
                sort: LibrarySort::FilenameAsc,
                filters: filter_set,
                after: Some(after),
            });
        }

        let size_filters = LibraryFilters::default();
        queries.push(LibraryQuery {
            limit: 50,
            sort: LibrarySort::SizeDesc,
            filters: size_filters.clone(),
            after: None,
        });
        let key = CursorKey::Size {
            size_bytes: Some(1_048_576),
            media_object_id: "media_a".into(),
        };
        let after = encode_cursor(LibrarySort::SizeDesc, &size_filters, &key);
        queries.push(LibraryQuery {
            limit: 50,
            sort: LibrarySort::SizeDesc,
            filters: size_filters,
            after: Some(after),
        });

        queries
    }

    #[test]
    fn every_supported_combination_uses_an_index() {
        let directory = tempfile::tempdir().unwrap();
        let mut connection =
            rusqlite::Connection::open(directory.path().join("control.sqlite3")).unwrap();
        rcdvlog_core::run_sqlite_migrations(
            &mut connection,
            crate::catalog::CONTROL_CATALOG_MIGRATIONS,
        )
        .unwrap();

        for query in supported_query_matrix() {
            let plan = plan_library_query(&query).unwrap();
            let mut statement = connection
                .prepare(&format!("EXPLAIN QUERY PLAN {}", plan.sql))
                .unwrap();
            let rows: Vec<String> = statement
                .query_map(rusqlite::params_from_iter(plan.bindings.iter()), |row| {
                    row.get::<_, String>(3)
                })
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            let uses_index = rows
                .iter()
                .any(|row| row.contains("USING INDEX") || row.contains("USING COVERING INDEX"));
            assert!(
                uses_index,
                "combination produced a scan: sort={:?} filters={:?} after={:?} plan={rows:?}",
                query.sort, query.filters, query.after
            );
        }
    }
}
