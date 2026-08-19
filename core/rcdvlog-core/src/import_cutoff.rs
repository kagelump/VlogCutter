//! "Everything before this clip is old." (#66)
//!
//! A card that has been in a camera for months carries footage that predates
//! the project. Re-presenting it on every reinsertion is noise, and answering
//! the same question on every import is the kind of chore a tool should only
//! ask once. This module stores that answer.
//!
//! ## What the rule is keyed on
//!
//! The **card session** — `cards`' volume-plus-content identity — and nothing
//! else. Three alternatives were considered and rejected:
//!
//! - *A mount path.* Transient by construction; the whole point of the card
//!   session identity is that `/Volumes/Untitled` is not a name.
//! - *Camera evidence* (the `cameras::mapping` key). Shared by every card that
//!   camera ever wrote, so a brand-new card would arrive already filtered and
//!   the user would never see the footage they just shot. A silent filter on
//!   unseen content is worse than the noise it removes.
//! - *A capture timestamp alone.* Preflight never probes media (see
//!   `preflight`'s doc comment), so the only time available at classification
//!   is the filesystem's, which a copy or a card reader can rewrite.
//!
//! A card session survives remounting and reinsertion, changes on reformat —
//! so a reformatted card correctly starts clean rather than inheriting a
//! boundary that no longer refers to anything — and is preserved when more
//! footage is shot onto a partly imported card, which is what makes "adding
//! newer footage still discovers clips after the cutoff" fall out for free.
//!
//! ## What the boundary is
//!
//! The card-relative path of the clip the user pointed at. Preflight sorts its
//! candidates before classifying them, so a card-relative path *is* the order
//! the ledger shows: "everything above this row" is a rule the user can read
//! off the screen rather than infer. It is also stable wherever the volume
//! mounts, because it never contains the mount point.
//!
//! The filename and best-available time are stored beside it purely so the
//! rule can always name the clip that set it. A persistent filter that cannot
//! say what it is filtering is a bug, not a feature.

use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Catalog;

/// One project's stored "start here" boundary for one card session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportCutoff {
    pub id: String,
    /// The card session this rule belongs to. See this module's doc comment.
    pub card_session_id: String,
    /// What the volume called itself when the rule was set. Display only; a
    /// label is not an identity.
    pub card_label: String,
    /// The boundary: the card-relative path of the clip the user pointed at.
    /// Everything sorting before it is excluded.
    pub relative_path: String,
    /// That clip's filename, so the rule can name itself without the card.
    pub filename: String,
    /// The best time known for that clip when the rule was set. Preflight's
    /// filesystem modification time unless a probed capture time was already
    /// on screen, so it is a label rather than evidence.
    pub captured_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl Catalog {
    /// Records or moves this project's cutoff for one card session.
    ///
    /// Moving is the same operation as setting: there is exactly one boundary
    /// per card session, and pointing at a different clip replaces it. That is
    /// what makes "move the cutoff" a single click on another row rather than
    /// a clear-then-set the user has to know to perform in order.
    pub fn set_import_cutoff(
        &self,
        project_id: &str,
        card_session_id: &str,
        card_label: &str,
        relative_path: &str,
        filename: &str,
        captured_at: Option<&str>,
    ) -> Result<ImportCutoff> {
        self.require_project(project_id)?;
        let card_session_id = card_session_id.trim();
        let relative_path = relative_path.trim();
        if card_session_id.is_empty() {
            anyhow::bail!("an import cutoff needs the card session it applies to");
        }
        if relative_path.is_empty() {
            anyhow::bail!("an import cutoff needs the clip it starts at");
        }
        let now = crate::now();
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO import_cutoffs (
                 id, project_id, card_session_id, card_label,
                 relative_path, filename, captured_at, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
             ON CONFLICT(project_id, card_session_id) DO UPDATE SET
                 card_label = excluded.card_label,
                 relative_path = excluded.relative_path,
                 filename = excluded.filename,
                 captured_at = excluded.captured_at,
                 updated_at = excluded.updated_at",
            params![
                format!("cutoff_{}", Uuid::new_v4().simple()),
                project_id,
                card_session_id,
                card_label,
                relative_path,
                filename,
                captured_at,
                now,
            ],
        )?;
        self.import_cutoff(project_id, card_session_id)?
            .ok_or_else(|| anyhow::anyhow!("import cutoff was not stored for {card_session_id}"))
    }

    /// This project's cutoff for one card session, if it has one.
    pub fn import_cutoff(
        &self,
        project_id: &str,
        card_session_id: &str,
    ) -> Result<Option<ImportCutoff>> {
        let connection = self.connection()?;
        if !crate::table_exists(&connection, "import_cutoffs")? {
            return Ok(None);
        }
        Ok(connection
            .query_row(
                "SELECT id, card_session_id, card_label, relative_path, filename,
                        captured_at, created_at, updated_at
                 FROM import_cutoffs
                 WHERE project_id = ?1 AND card_session_id = ?2",
                params![project_id, card_session_id],
                read_cutoff,
            )
            .optional()?)
    }

    /// Every cutoff this project holds, so a rule can never become invisible
    /// just because its card is not in the slot. Ordered by card label then
    /// id, for a deterministic listing.
    pub fn list_import_cutoffs(&self, project_id: &str) -> Result<Vec<ImportCutoff>> {
        self.require_project(project_id)?;
        let connection = self.connection()?;
        if !crate::table_exists(&connection, "import_cutoffs")? {
            return Ok(Vec::new());
        }
        let mut statement = connection.prepare(
            "SELECT id, card_session_id, card_label, relative_path, filename,
                    captured_at, created_at, updated_at
             FROM import_cutoffs
             WHERE project_id = ?1
             ORDER BY card_label ASC, id ASC",
        )?;
        let rows = statement.query_map(params![project_id], read_cutoff)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Forgets a cutoff. Idempotent: clearing one that is already gone — a
    /// second click, a card cleared from the settings list while the import
    /// dialog is open — is not an error.
    pub fn clear_import_cutoff(&self, project_id: &str, card_session_id: &str) -> Result<()> {
        self.require_project(project_id)?;
        self.connection()?.execute(
            "DELETE FROM import_cutoffs WHERE project_id = ?1 AND card_session_id = ?2",
            params![project_id, card_session_id],
        )?;
        Ok(())
    }
}

fn read_cutoff(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImportCutoff> {
    Ok(ImportCutoff {
        id: row.get(0)?,
        card_session_id: row.get(1)?,
        card_label: row.get(2)?,
        relative_path: row.get(3)?,
        filename: row.get(4)?,
        captured_at: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn open_project() -> (tempfile::TempDir, Catalog, String) {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        (directory, catalog, project.id)
    }

    #[test]
    fn a_cutoff_is_stored_per_card_session_and_moves_in_place() {
        let (_directory, catalog, project_id) = open_project();

        let first = catalog
            .set_import_cutoff(
                &project_id,
                "card_sony",
                "Untitled",
                "DCIM/100MSDCF/C0042.MP4",
                "C0042.MP4",
                Some("2026-08-01T09:00:00Z"),
            )
            .unwrap();
        assert_eq!(first.relative_path, "DCIM/100MSDCF/C0042.MP4");

        // Pointing at another clip moves the one boundary rather than adding
        // a second one the user would then have to reconcile.
        let moved = catalog
            .set_import_cutoff(
                &project_id,
                "card_sony",
                "Untitled",
                "DCIM/100MSDCF/C0100.MP4",
                "C0100.MP4",
                None,
            )
            .unwrap();
        assert_eq!(moved.id, first.id);
        assert_eq!(moved.relative_path, "DCIM/100MSDCF/C0100.MP4");
        assert_eq!(moved.captured_at, None);
        assert_eq!(catalog.list_import_cutoffs(&project_id).unwrap().len(), 1);
    }

    #[test]
    fn cutoffs_do_not_leak_between_cards_or_projects() {
        let (_directory, catalog, project_id) = open_project();
        let other_project = catalog.create_project("Okinawa 2026").unwrap();

        catalog
            .set_import_cutoff(
                &project_id,
                "card_sony",
                "Untitled",
                "DCIM/100MSDCF/C0042.MP4",
                "C0042.MP4",
                None,
            )
            .unwrap();

        assert!(
            catalog
                .import_cutoff(&project_id, "card_dji")
                .unwrap()
                .is_none()
        );
        assert!(
            catalog
                .import_cutoff(&other_project.id, "card_sony")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn clearing_is_idempotent_and_a_boundary_needs_both_a_card_and_a_clip() {
        let (_directory, catalog, project_id) = open_project();
        catalog
            .set_import_cutoff(&project_id, "card_sony", "", "DCIM/C1.MP4", "C1.MP4", None)
            .unwrap();

        catalog
            .clear_import_cutoff(&project_id, "card_sony")
            .unwrap();
        catalog
            .clear_import_cutoff(&project_id, "card_sony")
            .unwrap();
        assert!(
            catalog
                .import_cutoff(&project_id, "card_sony")
                .unwrap()
                .is_none()
        );

        assert!(
            catalog
                .set_import_cutoff(&project_id, "  ", "", "DCIM/C1.MP4", "C1.MP4", None)
                .is_err()
        );
        assert!(
            catalog
                .set_import_cutoff(&project_id, "card_sony", "", "  ", "", None)
                .is_err()
        );
    }
}
