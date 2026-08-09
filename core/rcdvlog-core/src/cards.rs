//! Camera card detection and card-session identity.
//!
//! Two questions, both answered before anything expensive happens.
//!
//! **Is this a camera card?** Answered from folder structure alone —
//! `DCIM/100MSDCF` is a Sony, `DJI_001` is a DJI, `PRIVATE/M4ROOT` is Sony's
//! professional layout. A directory listing costs nothing, so a card can be
//! recognized and named in a banner before a single byte is read. This is a
//! card-level hint for the banner and the archive path; per-asset camera
//! identity still comes from EXIF first and the filename-pattern table second
//! (`docs/tasks/desktop/CAMERA_PATTERNS.md`), and folder structure never
//! overrides either.
//!
//! **Is this the same card as last time?** Answered by a session identity built
//! from the volume label and the card's **earliest** media entry in sorted order
//! — its relative path, size, and modification time. Adapted from
//! `docs/tasks/travel/ARCHITECTURE.md` §5.3, lifted here so the desktop does not
//! wait on the travel epic to use it.
//!
//! Three properties the import workflow depends on, and why the anchor is one
//! entry rather than a sample of many:
//!
//! - Reinserting the same card yields the same identity, so an import resumes.
//! - A **reformatted** card yields a different identity, so it starts clean and
//!   cannot collide with the old card's archive directory. DCIM counters wrap
//!   around and filenames repeat; identity must not. A reformat rewrites the
//!   first clip, changing its size and modification time.
//! - Shooting more footage onto a partly imported card preserves identity.
//!
//! That last property is what forces a single anchor. Digesting the first *N*
//! entries is only append-stable once a card holds more than N clips: below that
//! every new clip joins the digest and the card appears to be a different one.
//! An anchor of exactly one entry is append-stable for any card holding at least
//! one clip, because new footage sorts after it and never displaces it.
//!
//! The trade is that deleting the earliest clip reads as a new card. That errs
//! toward a fresh archive directory rather than toward colliding with an
//! existing one, which is the safe direction to be wrong in.

use anyhow::Result;
use blake3::Hasher;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::is_media;

/// One entry's worth of card identity: its path, size, and modification time.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CardEntry {
    relative_path: String,
    size_bytes: u64,
    mtime_millis: i64,
}

/// Camera layouts recognizable from directory structure alone, most specific
/// first. A match names the camera family, not the model — EXIF does that.
const CARD_LAYOUTS: &[(&str, &str)] = &[
    ("PRIVATE/M4ROOT", "Sony"),
    ("PRIVATE/AVCHD", "Sony"),
    ("DCIM/100MSDCF", "Sony"),
    ("DCIM/100GOPRO", "GoPro"),
    ("DCIM/100MEDIA", "DJI"),
    ("DCIM/DJI_001", "DJI"),
    ("DCIM/100CANON", "Canon"),
    ("DCIM/100_FUJI", "Fujifilm"),
    ("DCIM/100OLYMP", "OM System"),
    ("DCIM/100ND750", "Nikon"),
    ("DCIM/100APPLE", "Apple"),
    // The generic fallback: every camera card has a DCIM, so this matches when
    // no vendor folder does and yields no camera claim.
    ("DCIM", ""),
];

/// A mounted volume that looks like a camera card.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardCandidate {
    /// Stable identity for this card *session*. A reformatted card gets a new
    /// one; the same card reinserted gets the same one.
    pub session_id: String,
    /// What the volume calls itself — the mount directory name.
    pub label: String,
    pub mount_path: String,
    /// The camera family inferred from folder layout, empty when only a generic
    /// `DCIM` was found. A hint for display, never an override of EXIF.
    pub camera_hint: String,
    /// Media files found on the card. Counted from a directory walk, with
    /// nothing read or hashed.
    pub clip_count: usize,
    /// Total bytes of that media, so a copy can state its cost up front.
    pub total_bytes: u64,
}

/// Enumerates volumes that might be camera cards.
///
/// A seam rather than a hardcoded path so a platform can be added without
/// touching detection or classification. macOS is the product's stated target;
/// the Linux paths exist so this is testable and so a dev machine behaves.
pub trait VolumeSource {
    fn mounted_volumes(&self) -> Vec<PathBuf>;
}

/// The real one: whatever the OS has mounted for removable media right now.
#[derive(Debug, Default, Clone, Copy)]
pub struct MountedVolumes;

impl VolumeSource for MountedVolumes {
    fn mounted_volumes(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = Vec::new();
        if cfg!(target_os = "macos") {
            roots.push(PathBuf::from("/Volumes"));
        } else {
            roots.push(PathBuf::from("/media"));
            roots.push(PathBuf::from("/run/media"));
            if let Some(user) = std::env::var_os("USER") {
                roots.push(Path::new("/media").join(&user));
                roots.push(Path::new("/run/media").join(&user));
            }
        }
        let mut volumes: Vec<PathBuf> = roots
            .iter()
            .filter_map(|root| std::fs::read_dir(root).ok())
            .flatten()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        volumes.sort();
        volumes.dedup();
        volumes
    }
}

/// Names the camera family for a card layout, if the structure is recognizable.
///
/// Returns `Some("")` for a card that has a `DCIM` but no vendor folder we know:
/// still a card, no camera claim.
pub fn card_camera_hint(mount: &Path) -> Option<&'static str> {
    CARD_LAYOUTS
        .iter()
        .find(|(layout, _)| mount.join(layout).is_dir())
        .map(|(_, camera)| *camera)
}

/// Inspects one mounted volume, returning a candidate when it looks like a card.
///
/// Reads directory entries and file sizes. Nothing is opened, hashed, or probed
/// — this runs while the user is looking at a banner.
pub fn inspect_card(mount: &Path) -> Result<Option<CardCandidate>> {
    let Some(camera_hint) = card_camera_hint(mount) else {
        return Ok(None);
    };
    let label = mount
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| mount.to_string_lossy().into_owned());

    // One walk answers all three: how many clips, how big, and what identity.
    // Only the earliest entry is kept for identity, so this stays O(1) memory
    // over a card holding thousands of clips.
    let mut anchor: Option<CardEntry> = None;
    let mut clip_count: usize = 0;
    let mut total_bytes: u64 = 0;
    for entry in WalkDir::new(mount)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| is_media(entry.path()))
    {
        let Ok(relative) = entry.path().strip_prefix(mount) else {
            continue;
        };
        let metadata = entry.metadata().ok();
        let size_bytes = metadata.as_ref().map(|data| data.len()).unwrap_or(0);
        total_bytes = total_bytes.saturating_add(size_bytes);
        clip_count += 1;
        let candidate = CardEntry {
            relative_path: relative.to_string_lossy().replace('\\', "/"),
            size_bytes,
            mtime_millis: metadata
                .and_then(|data| data.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|since| i64::try_from(since.as_millis()).ok())
                .unwrap_or(0),
        };
        // Ordered by path so the anchor does not depend on walk order, which the
        // filesystem is free to change between insertions.
        if anchor
            .as_ref()
            .is_none_or(|current| candidate.relative_path < current.relative_path)
        {
            anchor = Some(candidate);
        }
    }

    let mut hasher = Hasher::new();
    hasher.update(label.as_bytes());
    if let Some(anchor) = &anchor {
        hasher.update(anchor.relative_path.as_bytes());
        hasher.update(&anchor.size_bytes.to_le_bytes());
        hasher.update(&anchor.mtime_millis.to_le_bytes());
    }
    let session_id = format!("card_{}", &hasher.finalize().to_hex()[..16]);

    Ok(Some(CardCandidate {
        session_id,
        label,
        mount_path: mount.to_string_lossy().into_owned(),
        camera_hint: camera_hint.to_string(),
        clip_count,
        total_bytes,
    }))
}

/// Every mounted volume that looks like a camera card.
pub fn detect_cards(source: &impl VolumeSource) -> Vec<CardCandidate> {
    source
        .mounted_volumes()
        .iter()
        .filter_map(|mount| inspect_card(mount).ok().flatten())
        .collect()
}

/// The archive directory a card's footage belongs in: `<cardLabel>-<shortId>`.
///
/// The short id is what keeps a reformatted card from writing into the previous
/// card's directory even though both are labelled `UNTITLED`.
pub fn card_session_slug(card: &CardCandidate) -> String {
    let label: String = card
        .label
        .trim()
        .chars()
        .map(|character| {
            if character.is_control() || matches!(character, '/' | '\\' | ':') {
                '-'
            } else {
                character
            }
        })
        .collect();
    let label = label.trim_matches([' ', '.']);
    let label = if label.is_empty() { "CARD" } else { label };
    let short = card.session_id.trim_start_matches("card_");
    format!("{label}-{}", &short[..short.len().min(8)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::{TempDir, tempdir};

    struct FixedVolumes(Vec<PathBuf>);

    impl VolumeSource for FixedVolumes {
        fn mounted_volumes(&self) -> Vec<PathBuf> {
            self.0.clone()
        }
    }

    fn card_with(layout: &str, clips: &[(&str, &[u8])]) -> (TempDir, PathBuf) {
        let directory = tempdir().unwrap();
        let mount = directory.path().join("DJI_SD");
        fs::create_dir_all(mount.join(layout)).unwrap();
        for (name, bytes) in clips {
            fs::write(mount.join(layout).join(name), bytes).unwrap();
        }
        (directory, mount)
    }

    #[test]
    fn a_camera_layout_is_recognized_without_reading_any_file() {
        let (_directory, mount) = card_with(
            "DCIM/100MSDCF",
            &[("C0001.MP4", b"aaaa"), ("C0002.MP4", b"bbbbbb")],
        );
        let card = inspect_card(&mount).unwrap().unwrap();
        assert_eq!(card.camera_hint, "Sony");
        assert_eq!(card.label, "DJI_SD");
        assert_eq!(card.clip_count, 2);
        assert_eq!(card.total_bytes, 10);
    }

    #[test]
    fn a_card_with_no_vendor_folder_is_still_a_card_with_no_camera_claim() {
        let (_directory, mount) = card_with("DCIM", &[("VID_0001.MP4", b"aa")]);
        let card = inspect_card(&mount).unwrap().unwrap();
        assert_eq!(card.camera_hint, "");
        assert_eq!(card.clip_count, 1);
    }

    #[test]
    fn an_ordinary_folder_is_not_a_card() {
        let directory = tempdir().unwrap();
        let share = directory.path().join("nas-video");
        fs::create_dir_all(&share).unwrap();
        fs::write(share.join("C0001.MP4"), b"aa").unwrap();
        assert!(inspect_card(&share).unwrap().is_none());
        assert!(detect_cards(&FixedVolumes(vec![share])).is_empty());
    }

    /// The three properties the import workflow depends on.
    #[test]
    fn card_identity_survives_new_footage_but_not_a_reformat() {
        let (_directory, mount) = card_with(
            "DCIM/100MSDCF",
            &[("C0001.MP4", b"first"), ("C0002.MP4", b"second")],
        );
        let first = inspect_card(&mount).unwrap().unwrap();

        // Reinserting the same card is the same session.
        assert_eq!(
            inspect_card(&mount).unwrap().unwrap().session_id,
            first.session_id
        );

        // Shooting more footage keeps the session, so an import resumes rather
        // than starting over.
        fs::write(mount.join("DCIM/100MSDCF/C0003.MP4"), b"third").unwrap();
        let extended = inspect_card(&mount).unwrap().unwrap();
        assert_eq!(extended.session_id, first.session_id);
        assert_eq!(extended.clip_count, 3);

        // Reformatting and reusing the card is a different session, even though
        // the label is unchanged and the filenames repeat.
        fs::remove_dir_all(mount.join("DCIM")).unwrap();
        fs::create_dir_all(mount.join("DCIM/100MSDCF")).unwrap();
        fs::write(mount.join("DCIM/100MSDCF/C0001.MP4"), b"reshot entirely").unwrap();
        let reformatted = inspect_card(&mount).unwrap().unwrap();
        assert_ne!(
            reformatted.session_id, first.session_id,
            "a reformatted card must not inherit the old card's identity"
        );
    }

    /// The dangerous reformat: the camera reuses the filename *and* the new clip
    /// happens to be the same length. Size alone would call these one card.
    #[test]
    fn a_reformatted_card_is_distinct_even_when_the_first_clip_is_the_same_size() {
        let (_directory, mount) = card_with("DCIM/100MSDCF", &[("C0001.MP4", b"aaaaaaaa")]);
        let before = inspect_card(&mount).unwrap().unwrap();

        fs::remove_dir_all(mount.join("DCIM")).unwrap();
        fs::create_dir_all(mount.join("DCIM/100MSDCF")).unwrap();
        // Same path, same byte count, different footage.
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(mount.join("DCIM/100MSDCF/C0001.MP4"), b"bbbbbbbb").unwrap();

        let after = inspect_card(&mount).unwrap().unwrap();
        assert_ne!(
            after.session_id, before.session_id,
            "identity must not rest on size alone"
        );
    }

    #[test]
    fn two_cards_labelled_the_same_get_different_sessions() {
        let (_one, first_mount) = card_with("DCIM/100MSDCF", &[("C0001.MP4", b"card one")]);
        let (_two, second_mount) = card_with("DCIM/100MSDCF", &[("C0001.MP4", b"card two here")]);
        let first = inspect_card(&first_mount).unwrap().unwrap();
        let second = inspect_card(&second_mount).unwrap().unwrap();
        assert_eq!(first.label, second.label);
        assert_ne!(first.session_id, second.session_id);
    }

    #[test]
    fn a_session_slug_is_filesystem_safe_and_carries_the_short_id() {
        let (_directory, mount) = card_with("DCIM/100MSDCF", &[("C0001.MP4", b"x")]);
        let card = inspect_card(&mount).unwrap().unwrap();
        let slug = card_session_slug(&card);
        assert!(slug.starts_with("DJI_SD-"));
        assert!(!slug.contains('/'));
        assert_eq!(slug.len(), "DJI_SD-".len() + 8);
    }
}
