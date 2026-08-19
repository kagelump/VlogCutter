//! Durable per-project camera-identity mapping (#71).
//!
//! Smart ingest can misidentify a camera — a weak filename pattern such as
//! `C####.MP4` names a manufacturer family ("Sony (C-series)"), not the
//! specific body the user actually owns. Once the user corrects that, this
//! module is what lets a reformatted or swapped card from the *same* camera
//! default to the corrected identity next time, without the correction living
//! only in one import session.
//!
//! ## Why not the mount path or the card session id
//!
//! `cards::CardCandidate::session_id` is deliberately unstable across a
//! reformat (see `cards.rs`'s module doc) — that instability is exactly right
//! for archive-directory placement, where a reformatted card must *not*
//! collide with the old card's files. It is exactly wrong for camera
//! identity, which issue #71 asks to survive a reformat. The mount path is
//! worse still: it is often a generic OS-assigned label ("UNTITLED",
//! "NO NAME") that a completely different camera can reuse.
//!
//! ## The evidence signature
//!
//! [`CameraEvidence`] is the cheap, preflight-time signal this module keys on
//! instead: the camera family recognized from the card's folder layout
//! (`cards::card_camera_hint` — "Sony", "DJI", "GoPro", ...) plus the
//! dominant filename prefix among the incoming clips ("C", "DJI_", "VID_",
//! ...). Both come from work preflight already does; neither requires EXIF or
//! probing a single byte of media.
//!
//! [`CameraEvidence::is_strong_enough`] is the line between "safe to apply or
//! record automatically" and "ambiguous — the dialog must ask instead of
//! guessing". Evidence is strong only when *both* signals are present. A card
//! preflight could only recognize as a generic `DCIM` folder (no vendor
//! layout — `folder_hint` empty) is common to nearly every camera ever made,
//! so a filename-prefix match alone would be a coincidence, not evidence.
//! Nothing is ever looked up *or* written under weak evidence, so a mapping
//! recorded for one camera can never be silently offered to an unrelated one
//! just because both used a plain `DCIM` card or a shared mount label.
//!
//! This is a heuristic, not a certificate of physical identity: two distinct
//! bodies of the same camera model, using the same card layout and the same
//! filename convention, are indistinguishable from this evidence alone and
//! will share one stored mapping. That is the accepted trade the issue
//! describes ("normally inherit... when the evidence is strong enough") —
//! the user can always override, and can inspect, change, or clear any
//! stored mapping (`list_camera_mappings`, `rename_camera_mapping`,
//! `clear_camera_mapping`).
//!
//! ## Relationship to #74
//!
//! #74 observes that no production path ever gives a `capture_sequences` row
//! a real `volume_hint`/`filename_prefix`, so the chronology mismatch
//! detectors it feeds are unreachable. This module is a *different* store
//! (`camera_identity_mappings`, project-scoped evidence-to-camera-name) built
//! for a different question — "what should this new import call itself" —
//! not a fix for #74's "what does an existing capture sequence remember
//! about the card that created it". The two could plausibly share a future
//! evidence representation, but this task does not merge them; see the task
//! report for the explicit call-out.

use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Catalog;

/// The cheap, preflight-time evidence used to recognize "the same camera"
/// across remounts, reformats, and card swaps. Deliberately excludes the
/// mount path and the volume label — see this module's doc comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraEvidence {
    /// The camera family recognized from the card's folder layout
    /// (`cards::card_camera_hint`), e.g. "Sony", "DJI", "GoPro". Empty when
    /// the card had only a generic `DCIM` folder, or there was no card at
    /// all (an ordinary "Add media folder" import).
    pub folder_hint: String,
    /// The dominant filename prefix among this import's new clips, e.g. "C",
    /// "DJI_", "VID_". Empty when there is nothing new to derive it from.
    pub filename_prefix: String,
}

impl CameraEvidence {
    /// Evidence is strong enough to safely look up or record a durable
    /// mapping only when both signals are present. See this module's doc
    /// comment for why a generic `DCIM`-only card, or a run with no ordinary
    /// filename prefix, must not participate: either alone is shared by too
    /// many unrelated cameras to mean anything.
    pub fn is_strong_enough(&self) -> bool {
        !self.folder_hint.is_empty() && !self.filename_prefix.is_empty()
    }
}

/// One stored evidence-to-camera correction, inspectable and editable per
/// project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraMapping {
    pub id: String,
    pub folder_hint: String,
    pub filename_prefix: String,
    pub camera: String,
    pub created_at: String,
    pub updated_at: String,
}

impl Catalog {
    /// The camera identities already known to this project — every distinct
    /// camera a capture sequence exists for, sorted. This is the pick list
    /// for "choose an existing project camera" in the import dialog, so
    /// `"Unknown camera"` is excluded: selecting it would not resolve
    /// anything.
    pub fn known_project_cameras(&self, project_id: &str) -> Result<Vec<String>> {
        let mut cameras: Vec<String> = self
            .capture_sequences(project_id)?
            .into_iter()
            .map(|sequence| sequence.camera_key)
            .filter(|camera| camera != crate::UNKNOWN_CAMERA)
            .collect();
        cameras.sort();
        cameras.dedup();
        Ok(cameras)
    }

    /// The remembered camera for this import's evidence, or `None` when the
    /// evidence is too weak to trust or nothing has been recorded for it yet.
    /// Never returns a mapping recorded under different evidence — an exact
    /// match on both signals is required.
    pub(crate) fn remembered_camera_for_evidence(
        &self,
        project_id: &str,
        evidence: &CameraEvidence,
    ) -> Result<Option<String>> {
        if !evidence.is_strong_enough() {
            return Ok(None);
        }
        let connection = self.connection()?;
        if !crate::table_exists(&connection, "camera_identity_mappings")? {
            return Ok(None);
        }
        Ok(connection
            .query_row(
                "SELECT camera FROM camera_identity_mappings
                 WHERE project_id = ?1 AND folder_hint = ?2 AND filename_prefix = ?3",
                params![project_id, evidence.folder_hint, evidence.filename_prefix],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Records (or updates) the camera this project's evidence should default
    /// to. A no-op — not an error — when the evidence is too weak to key a
    /// durable mapping, or the camera is blank/`"Unknown camera"`: this is
    /// called automatically after every import that resolved a camera
    /// identity, and most of those calls have nothing worth remembering.
    pub(crate) fn remember_camera_mapping(
        &self,
        project_id: &str,
        evidence: &CameraEvidence,
        camera: &str,
    ) -> Result<Option<CameraMapping>> {
        self.require_project(project_id)?;
        if !evidence.is_strong_enough() {
            return Ok(None);
        }
        let camera = camera.trim();
        if camera.is_empty() || camera == crate::UNKNOWN_CAMERA {
            return Ok(None);
        }
        let connection = self.connection()?;
        let timestamp = crate::now();
        let id = format!("cammap_{}", Uuid::new_v4().simple());
        connection.execute(
            r#"
            INSERT INTO camera_identity_mappings
                (id, project_id, folder_hint, filename_prefix, camera, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
            ON CONFLICT(project_id, folder_hint, filename_prefix) DO UPDATE SET
                camera = excluded.camera,
                updated_at = excluded.updated_at
            "#,
            params![
                id,
                project_id,
                evidence.folder_hint,
                evidence.filename_prefix,
                camera,
                timestamp
            ],
        )?;
        let mapping = connection.query_row(
            "SELECT id, folder_hint, filename_prefix, camera, created_at, updated_at
             FROM camera_identity_mappings
             WHERE project_id = ?1 AND folder_hint = ?2 AND filename_prefix = ?3",
            params![project_id, evidence.folder_hint, evidence.filename_prefix],
            |row| {
                Ok(CameraMapping {
                    id: row.get(0)?,
                    folder_hint: row.get(1)?,
                    filename_prefix: row.get(2)?,
                    camera: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            },
        )?;
        Ok(Some(mapping))
    }

    /// Every stored mapping for one project, so it can be inspected. Sorted
    /// by camera, then id, for a deterministic listing.
    pub fn list_camera_mappings(&self, project_id: &str) -> Result<Vec<CameraMapping>> {
        self.require_project(project_id)?;
        let connection = self.connection()?;
        if !crate::table_exists(&connection, "camera_identity_mappings")? {
            return Ok(Vec::new());
        }
        let mut statement = connection.prepare(
            "SELECT id, folder_hint, filename_prefix, camera, created_at, updated_at
             FROM camera_identity_mappings
             WHERE project_id = ?1
             ORDER BY camera ASC, id ASC",
        )?;
        let rows = statement.query_map(params![project_id], |row| {
            Ok(CameraMapping {
                id: row.get(0)?,
                folder_hint: row.get(1)?,
                filename_prefix: row.get(2)?,
                camera: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Points an existing mapping at a different camera — the "change" half
    /// of inspect/change/clear. Unlike `remember_camera_mapping`, this is a
    /// direct user action against a row that already exists, so an empty
    /// name is rejected rather than silently ignored.
    pub fn rename_camera_mapping(
        &self,
        project_id: &str,
        mapping_id: &str,
        camera: &str,
    ) -> Result<CameraMapping> {
        self.require_project(project_id)?;
        let camera = camera.trim();
        if camera.is_empty() {
            anyhow::bail!("a camera mapping needs a name");
        }
        let connection = self.connection()?;
        let updated = connection.execute(
            "UPDATE camera_identity_mappings SET camera = ?1, updated_at = ?2
             WHERE project_id = ?3 AND id = ?4",
            params![camera, crate::now(), project_id, mapping_id],
        )?;
        if updated == 0 {
            anyhow::bail!("camera mapping not found: {mapping_id}");
        }
        Ok(connection.query_row(
            "SELECT id, folder_hint, filename_prefix, camera, created_at, updated_at
             FROM camera_identity_mappings WHERE project_id = ?1 AND id = ?2",
            params![project_id, mapping_id],
            |row| {
                Ok(CameraMapping {
                    id: row.get(0)?,
                    folder_hint: row.get(1)?,
                    filename_prefix: row.get(2)?,
                    camera: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            },
        )?)
    }

    /// Deletes a stored mapping. Idempotent: clearing a mapping that is
    /// already gone (a second click, a race with another view) is not an
    /// error.
    pub fn clear_camera_mapping(&self, project_id: &str, mapping_id: &str) -> Result<()> {
        self.require_project(project_id)?;
        self.connection()?.execute(
            "DELETE FROM camera_identity_mappings WHERE project_id = ?1 AND id = ?2",
            params![project_id, mapping_id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Catalog;
    use tempfile::tempdir;

    fn open_project() -> (tempfile::TempDir, Catalog, String) {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Camera mapping").unwrap();
        (directory, catalog, project.id)
    }

    fn strong(folder_hint: &str, filename_prefix: &str) -> CameraEvidence {
        CameraEvidence {
            folder_hint: folder_hint.to_string(),
            filename_prefix: filename_prefix.to_string(),
        }
    }

    #[test]
    fn weak_evidence_is_never_strong_enough() {
        assert!(!strong("", "C").is_strong_enough());
        assert!(!strong("Sony", "").is_strong_enough());
        assert!(!strong("", "").is_strong_enough());
        assert!(strong("Sony", "C").is_strong_enough());
    }

    #[test]
    fn a_remembered_mapping_round_trips_through_strong_evidence() {
        let (_directory, catalog, project_id) = open_project();
        let evidence = strong("Sony", "C");
        assert_eq!(
            catalog
                .remembered_camera_for_evidence(&project_id, &evidence)
                .unwrap(),
            None
        );

        let mapping = catalog
            .remember_camera_mapping(&project_id, &evidence, "Sony A7C II")
            .unwrap()
            .expect("strong evidence records a mapping");
        assert_eq!(mapping.camera, "Sony A7C II");

        assert_eq!(
            catalog
                .remembered_camera_for_evidence(&project_id, &evidence)
                .unwrap(),
            Some("Sony A7C II".to_string())
        );
    }

    #[test]
    fn weak_evidence_is_never_recorded_or_looked_up() {
        let (_directory, catalog, project_id) = open_project();
        let weak = strong("", "C");
        assert_eq!(
            catalog
                .remember_camera_mapping(&project_id, &weak, "Sony A7C II")
                .unwrap(),
            None
        );
        assert!(
            catalog
                .list_camera_mappings(&project_id)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            catalog
                .remembered_camera_for_evidence(&project_id, &weak)
                .unwrap(),
            None
        );
    }

    /// The exact negative case #71 calls out: a different camera that
    /// happens to share a generic volume label must not inherit a mapping
    /// recorded for the first one. This module never sees the volume label
    /// at all — the evidence signature is folder layout plus filename
    /// prefix — so two cards presented under the same label but with
    /// different card evidence simply key different rows.
    #[test]
    fn a_different_camera_sharing_a_generic_label_does_not_inherit_the_mapping() {
        let (_directory, catalog, project_id) = open_project();
        let sony = strong("Sony", "C");
        catalog
            .remember_camera_mapping(&project_id, &sony, "Sony A7C II")
            .unwrap();

        // A DJI card, mounted under the same generic "UNTITLED" label a test
        // harness would reuse, has different card evidence and must resolve
        // to nothing remembered.
        let dji = strong("DJI", "DJI_");
        assert_eq!(
            catalog
                .remembered_camera_for_evidence(&project_id, &dji)
                .unwrap(),
            None
        );
    }

    #[test]
    fn remembering_again_updates_rather_than_duplicates() {
        let (_directory, catalog, project_id) = open_project();
        let evidence = strong("Sony", "C");
        catalog
            .remember_camera_mapping(&project_id, &evidence, "Sony (C-series)")
            .unwrap();
        catalog
            .remember_camera_mapping(&project_id, &evidence, "Sony A7C II")
            .unwrap();

        let mappings = catalog.list_camera_mappings(&project_id).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].camera, "Sony A7C II");
    }

    #[test]
    fn recording_unknown_camera_or_a_blank_name_is_a_no_op() {
        let (_directory, catalog, project_id) = open_project();
        let evidence = strong("Sony", "C");
        assert_eq!(
            catalog
                .remember_camera_mapping(&project_id, &evidence, crate::UNKNOWN_CAMERA)
                .unwrap(),
            None
        );
        assert_eq!(
            catalog
                .remember_camera_mapping(&project_id, &evidence, "   ")
                .unwrap(),
            None
        );
        assert!(
            catalog
                .list_camera_mappings(&project_id)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rename_and_clear_are_the_inspect_change_clear_surface() {
        let (_directory, catalog, project_id) = open_project();
        let evidence = strong("Sony", "C");
        let mapping = catalog
            .remember_camera_mapping(&project_id, &evidence, "Sony (C-series)")
            .unwrap()
            .unwrap();

        let renamed = catalog
            .rename_camera_mapping(&project_id, &mapping.id, "Sony A7C II")
            .unwrap();
        assert_eq!(renamed.camera, "Sony A7C II");
        assert_eq!(
            catalog
                .remembered_camera_for_evidence(&project_id, &evidence)
                .unwrap(),
            Some("Sony A7C II".to_string())
        );

        catalog
            .clear_camera_mapping(&project_id, &mapping.id)
            .unwrap();
        assert!(
            catalog
                .list_camera_mappings(&project_id)
                .unwrap()
                .is_empty()
        );
        // Clearing again is not an error.
        catalog
            .clear_camera_mapping(&project_id, &mapping.id)
            .unwrap();
    }

    #[test]
    fn renaming_to_a_blank_name_is_rejected() {
        let (_directory, catalog, project_id) = open_project();
        let evidence = strong("Sony", "C");
        let mapping = catalog
            .remember_camera_mapping(&project_id, &evidence, "Sony (C-series)")
            .unwrap()
            .unwrap();
        assert!(
            catalog
                .rename_camera_mapping(&project_id, &mapping.id, "   ")
                .is_err()
        );
    }

    #[test]
    fn known_project_cameras_lists_distinct_sequence_cameras_excluding_unknown() {
        let (_directory, catalog, project_id) = open_project();
        assert!(
            catalog
                .known_project_cameras(&project_id)
                .unwrap()
                .is_empty()
        );

        let mut connection = catalog.connection().unwrap();
        let transaction = connection.transaction().unwrap();
        crate::chronology::ensure_capture_sequence_for_camera(
            &transaction,
            &project_id,
            "Sony A7C II",
            &crate::now(),
        )
        .unwrap();
        crate::chronology::ensure_capture_sequence_for_camera(
            &transaction,
            &project_id,
            crate::UNKNOWN_CAMERA,
            &crate::now(),
        )
        .unwrap();
        transaction.commit().unwrap();

        assert_eq!(
            catalog.known_project_cameras(&project_id).unwrap(),
            vec!["Sony A7C II".to_string()]
        );
    }
}
