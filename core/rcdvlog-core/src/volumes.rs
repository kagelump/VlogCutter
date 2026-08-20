//! Remembering volumes as volumes rather than as pathnames.
//!
//! Every root path the project stores — an archive destination, a card source,
//! the recorded location of an original — used to mean "whatever is mounted
//! here right now". That is fine until it is not: `/Volumes/Untitled` is the
//! commonest mount name there is, enclosures remount at a different path after
//! a sleep, and a reformat destroys a card's content-derived session id while
//! leaving the same hardware in your hand.
//!
//! So a volume gets an identity of its own, and the pathname becomes a hint.
//! A path change is routine. An identity change at a remembered path means a
//! different volume, and that is the one case nothing may adopt silently.
//!
//! Identity is honest about its own limits. A volume that exposes no UUID is
//! still recorded, still recognised at its path, and still usable — but
//! [`Catalog::volume_identity_is_verifiable`] answers `false` for it, because
//! "the same path" is not proof and must not be allowed to act like proof.

use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use std::path::Path;
use uuid::Uuid;

use crate::Catalog;
use crate::cards::VolumeFacts;

/// A volume this catalog has seen before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownVolume {
    /// Stable id assigned by us, so callers hold one reference whether or not
    /// the volume itself offers a UUID.
    pub volume_id: String,
    /// The volume's own identity, absent when it exposes none.
    pub volume_uuid: Option<String>,
    /// What it calls itself, for naming it while it is disconnected.
    pub label: String,
    /// Where it was last seen. A hint for display and for finding an
    /// unidentified volume again — never proof of identity.
    pub last_mount_path: String,
    pub removable: bool,
    /// The card session last observed on this hardware, when it was a card.
    /// This is the seed for tying a physical card to a camera across reformats.
    pub last_card_session_id: Option<String>,
    pub first_seen_at: String,
    pub last_seen_at: String,
}

impl KnownVolume {
    /// Whether this volume can prove it is itself.
    pub fn identity_is_verifiable(&self) -> bool {
        self.volume_uuid.is_some()
    }
}

/// The volume a path most likely sits on, resolved against volumes already
/// observed rather than by asking the operating system.
///
/// Bookkeeping only. This is a longest-mount-point-prefix guess, so it answers
/// "which drive did this land on" well enough to name a disconnected volume in
/// the UI — and it is emphatically **not** proof of identity. Anything that
/// decides whether it is safe to write must go through
/// [`Catalog::volume_identity_is_verifiable`], which compares UUIDs.
///
/// Called once per verified asset, so it stays a single indexed query and
/// never a subprocess.
pub(crate) fn volume_id_for_path(
    connection: &rusqlite::Connection,
    path: &str,
) -> Result<Option<String>> {
    if !crate::table_exists(connection, "volumes")? {
        return Ok(None);
    }
    Ok(connection
        .query_row(
            r#"
            SELECT volume_id FROM volumes
             WHERE last_mount_path <> ''
               -- rtrim so the root volume's "/" builds "/%" rather than "//%",
               -- which would match nothing and quietly exclude the one volume
               -- every other path sits under.
               AND (?1 = last_mount_path OR ?1 LIKE rtrim(last_mount_path, '/') || '/%')
             ORDER BY length(last_mount_path) DESC
             LIMIT 1
            "#,
            params![path],
            |row| row.get(0),
        )
        .optional()?)
}

impl Catalog {
    /// Records the volume mounted at `path` and returns its stable id.
    ///
    /// Idempotent per volume: a UUID-bearing volume keeps one row wherever it
    /// mounts, and an unidentified one keeps one row per path rather than
    /// accumulating a row per sighting.
    pub fn observe_volume(&self, path: &Path, facts: &VolumeFacts) -> Result<String> {
        let connection = self.connection()?;
        let now = crate::now();
        let mount = path.to_string_lossy().into_owned();

        // Find the row this sighting belongs to: by UUID when the volume can
        // identify itself, otherwise by the path it was last seen at. The
        // second is continuity, not identification — see the module note.
        let existing: Option<String> = match facts.volume_uuid.as_deref() {
            Some(uuid) => connection
                .query_row(
                    "SELECT volume_id FROM volumes WHERE volume_uuid = ?1",
                    params![uuid],
                    |row| row.get(0),
                )
                .optional()?
                // A volume first seen without a UUID and now able to prove
                // itself is the same volume, so adopt the existing row rather
                // than leaving a duplicate behind.
                .or(connection
                    .query_row(
                        "SELECT volume_id FROM volumes WHERE volume_uuid IS NULL AND last_mount_path = ?1",
                        params![mount],
                        |row| row.get(0),
                    )
                    .optional()?),
            None => connection
                .query_row(
                    "SELECT volume_id FROM volumes WHERE volume_uuid IS NULL AND last_mount_path = ?1",
                    params![mount],
                    |row| row.get(0),
                )
                .optional()?,
        };

        match existing {
            Some(volume_id) => {
                connection.execute(
                    r#"
                    UPDATE volumes
                       SET volume_uuid = COALESCE(?2, volume_uuid),
                           label = ?3,
                           last_mount_path = ?4,
                           removable = ?5,
                           last_seen_at = ?6
                     WHERE volume_id = ?1
                    "#,
                    params![
                        volume_id,
                        facts.volume_uuid,
                        facts.label,
                        mount,
                        facts.removable as i64,
                        now
                    ],
                )?;
                Ok(volume_id)
            }
            None => {
                let volume_id = format!("vol_{}", Uuid::new_v4().simple());
                connection.execute(
                    r#"
                    INSERT INTO volumes (
                        volume_id, volume_uuid, label, last_mount_path,
                        removable, first_seen_at, last_seen_at
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                    "#,
                    params![
                        volume_id,
                        facts.volume_uuid,
                        facts.label,
                        mount,
                        facts.removable as i64,
                        now
                    ],
                )?;
                Ok(volume_id)
            }
        }
    }

    /// Records the volume a card was seen on, tying the card's session to the
    /// hardware underneath it.
    ///
    /// A card's `session_id` is derived from its content, so a reformat mints a
    /// new one even though it is the same card in your hand. Keeping the last
    /// session seen on a volume is what lets "this physical card is the drone's"
    /// outlive a reformat. Returns the volume's id when one could be observed.
    pub fn observe_card_volume(&self, mount: &Path, session_id: &str) -> Result<Option<String>> {
        let Some(facts) = crate::cards::inspect_volume(mount) else {
            return Ok(None);
        };
        let volume_id = self.observe_volume(mount, &facts)?;
        self.connection()?.execute(
            "UPDATE volumes SET last_card_session_id = ?2 WHERE volume_id = ?1",
            params![volume_id, session_id],
        )?;
        Ok(Some(volume_id))
    }

    /// The volume recorded under `volume_id`, if it is still known.
    pub fn known_volume(&self, volume_id: &str) -> Result<Option<KnownVolume>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                r#"
                SELECT volume_id, volume_uuid, label, last_mount_path, removable,
                       last_card_session_id, first_seen_at, last_seen_at
                  FROM volumes WHERE volume_id = ?1
                "#,
                params![volume_id],
                |row| {
                    Ok(KnownVolume {
                        volume_id: row.get(0)?,
                        volume_uuid: row.get(1)?,
                        label: row.get(2)?,
                        last_mount_path: row.get(3)?,
                        removable: row.get::<_, i64>(4)? != 0,
                        last_card_session_id: row.get(5)?,
                        first_seen_at: row.get(6)?,
                        last_seen_at: row.get(7)?,
                    })
                },
            )
            .optional()?)
    }

    /// Whether `facts` describe the volume recorded as `volume_id`.
    ///
    /// The gate for continuing anything automatically against a volume that
    /// went away and came back. Answers `false` when either side has no UUID:
    /// an unidentified volume is not a match, it is an unknown, and the
    /// difference matters when the alternative is writing to the wrong disk.
    pub fn volume_identity_is_verifiable(
        &self,
        volume_id: &str,
        facts: &VolumeFacts,
    ) -> Result<bool> {
        let Some(observed) = facts.volume_uuid.as_deref() else {
            return Ok(false);
        };
        let Some(known) = self.known_volume(volume_id)? else {
            return Ok(false);
        };
        Ok(known.volume_uuid.as_deref() == Some(observed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn catalog() -> (tempfile::TempDir, Catalog) {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        (directory, catalog)
    }

    fn identified(uuid: &str, label: &str) -> VolumeFacts {
        VolumeFacts {
            volume_uuid: Some(uuid.to_string()),
            label: label.to_string(),
            removable: true,
        }
    }

    fn unidentified(label: &str) -> VolumeFacts {
        VolumeFacts {
            volume_uuid: None,
            label: label.to_string(),
            removable: false,
        }
    }

    /// Stamping a location with its volume happens on a hot path — once per
    /// asset verified — so it resolves against volumes already observed rather
    /// than shelling out to `diskutil` thousands of times.
    #[test]
    fn a_path_resolves_to_the_known_volume_it_sits_under() {
        let (_directory, catalog) = catalog();
        let acasis = catalog
            .observe_volume(
                Path::new("/Volumes/Acasis"),
                &identified("AAA-111", "Acasis"),
            )
            .unwrap();
        catalog
            .observe_volume(
                Path::new("/Volumes/SD_Card"),
                &identified("BBB-222", "SD_Card"),
            )
            .unwrap();

        let connection = catalog.connection().unwrap();
        let found =
            super::volume_id_for_path(&connection, "/Volumes/Acasis/2026 Eclipse/C0001.MP4")
                .unwrap();

        assert_eq!(found.as_deref(), Some(acasis.as_str()));
    }

    /// The longest matching mount point wins, so a volume mounted inside
    /// another volume's tree is not mistaken for its host.
    #[test]
    fn a_nested_mount_point_wins_over_the_volume_it_sits_inside() {
        let (_directory, catalog) = catalog();
        catalog
            .observe_volume(Path::new("/"), &identified("ROOT-000", "Macintosh HD"))
            .unwrap();
        let nested = catalog
            .observe_volume(
                Path::new("/Volumes/Acasis"),
                &identified("AAA-111", "Acasis"),
            )
            .unwrap();

        let connection = catalog.connection().unwrap();
        let found = super::volume_id_for_path(&connection, "/Volumes/Acasis/clip.MP4").unwrap();

        assert_eq!(found.as_deref(), Some(nested.as_str()));
    }

    #[test]
    fn a_path_under_no_known_volume_resolves_to_nothing() {
        let (_directory, catalog) = catalog();
        catalog
            .observe_volume(
                Path::new("/Volumes/Acasis"),
                &identified("AAA-111", "Acasis"),
            )
            .unwrap();

        let connection = catalog.connection().unwrap();
        let found = super::volume_id_for_path(&connection, "/Volumes/Elsewhere/clip.MP4").unwrap();

        assert_eq!(found, None);
    }

    #[test]
    fn one_volume_keeps_one_identity_across_a_remount_at_a_new_path() {
        let (_directory, catalog) = catalog();
        let facts = identified("AAA-111", "Untitled");

        let first = catalog
            .observe_volume(Path::new("/Volumes/Untitled"), &facts)
            .unwrap();
        let after_remount = catalog
            .observe_volume(Path::new("/Volumes/Untitled 1"), &facts)
            .unwrap();

        assert_eq!(
            first, after_remount,
            "a path change is routine and must not mint a second volume"
        );
        let known = catalog.known_volume(&first).unwrap().unwrap();
        assert_eq!(known.last_mount_path, "/Volumes/Untitled 1");
        assert!(known.identity_is_verifiable());
    }

    #[test]
    fn two_volumes_sharing_a_mount_name_stay_distinct() {
        let (_directory, catalog) = catalog();
        let path = Path::new("/Volumes/Untitled");

        let first = catalog
            .observe_volume(path, &identified("AAA-111", "Untitled"))
            .unwrap();
        let second = catalog
            .observe_volume(path, &identified("BBB-222", "Untitled"))
            .unwrap();

        assert_ne!(
            first, second,
            "the same pathname is not evidence of the same volume"
        );
    }

    /// The case that would otherwise copy onto the wrong disk: the remembered
    /// path is occupied, but by something else.
    #[test]
    fn a_different_volume_at_a_remembered_path_is_not_a_match() {
        let (_directory, catalog) = catalog();
        let path = Path::new("/Volumes/Acasis");
        let remembered = catalog
            .observe_volume(path, &identified("AAA-111", "Acasis"))
            .unwrap();

        let matches = catalog
            .volume_identity_is_verifiable(&remembered, &identified("BBB-222", "Acasis"))
            .unwrap();

        assert!(!matches);
    }

    #[test]
    fn the_same_volume_returning_is_a_match() {
        let (_directory, catalog) = catalog();
        let facts = identified("AAA-111", "Acasis");
        let remembered = catalog
            .observe_volume(Path::new("/Volumes/Acasis"), &facts)
            .unwrap();

        assert!(
            catalog
                .volume_identity_is_verifiable(&remembered, &facts)
                .unwrap()
        );
    }

    /// A volume with no UUID still works and still gets a row. What it does not
    /// get is the benefit of the doubt.
    #[test]
    fn an_unidentified_volume_is_recorded_but_never_verifies() {
        let (_directory, catalog) = catalog();
        let path = Path::new("/Volumes/share");
        let facts = unidentified("share");

        let first = catalog.observe_volume(path, &facts).unwrap();
        let again = catalog.observe_volume(path, &facts).unwrap();

        assert_eq!(
            first, again,
            "one row per unidentified volume, not per look"
        );
        let known = catalog.known_volume(&first).unwrap().unwrap();
        assert_eq!(known.volume_uuid, None);
        assert!(!known.identity_is_verifiable());
        assert!(
            !catalog
                .volume_identity_is_verifiable(&first, &facts)
                .unwrap(),
            "a matching path is not proof, and must not be allowed to act like it"
        );
    }

    /// A volume seen before it could identify itself adopts a UUID the next
    /// time it appears, without becoming a second volume.
    #[test]
    fn an_unidentified_volume_adopts_a_uuid_when_one_becomes_available() {
        let (_directory, catalog) = catalog();
        let path = Path::new("/Volumes/Acasis");

        let before = catalog
            .observe_volume(path, &unidentified("Acasis"))
            .unwrap();
        let after = catalog
            .observe_volume(path, &identified("AAA-111", "Acasis"))
            .unwrap();

        assert_eq!(before, after, "the same volume, now able to prove it");
        let known = catalog.known_volume(&after).unwrap().unwrap();
        assert_eq!(known.volume_uuid.as_deref(), Some("AAA-111"));
        assert!(known.identity_is_verifiable());
    }
}
