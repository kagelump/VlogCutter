//! Classifying a source before touching the catalog.
//!
//! The import dialog has to answer "what is new, what is already here, and where
//! is this going" *before* the user commits to anything. That answer has to be
//! produced without mutating a thing, because a preview that half-imports is not
//! a preview.
//!
//! One classifier serves both entry points. Pointed at a mounted card it drives
//! the import dialog; pointed at a folder the user picked it drives the repair
//! flow. The questions are the same — is this content known, and can this asset
//! reach its original — so the code is too.
//!
//! Cost discipline: identity comes from the sampled fingerprint (size plus the
//! first and last 1 MiB), so a card of a few hundred 4K clips classifies in
//! seconds. Preflight makes one bounded ExifTool read from the first sorted new
//! card item to distinguish physical camera models for sticky camera mappings;
//! it does not probe every file. Full media probing remains import's problem.

use anyhow::Result;
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

use crate::cameras::mapping::CameraEvidence;
use crate::cards::{CardCandidate, card_session_slug, inspect_card};
use crate::{
    ArchiveRoot, Asset, Catalog, UNKNOWN_CAMERA, has_current_proxy, is_media, locations,
    sampled_media_fingerprint,
};

/// Twelve hours is long enough to separate shooting sessions without treating
/// an ordinary meal, battery change, or timezone correction as a new card run.
/// It remains proposal evidence only: the user decides whether the gap is a
/// real capture-sequence boundary.
pub const LARGE_CAPTURE_SEQUENCE_GAP_SECONDS: u64 = 12 * 60 * 60;

/// Observable evidence for one suggested capture-sequence boundary. These are
/// exactly the automatic evidence kinds allowed by the chronology contract;
/// no suggestion is ever applied by preflight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum ImportSequenceBoundaryEvidence {
    VolumeChange { from: String, to: String },
    CounterReset { previous: u64, next: u64 },
    FolderReset { from: String, to: String },
    PrefixChange { from: String, to: String },
    LargeDiscontinuity { gap_seconds: u64 },
}

/// One place where preflight suggests beginning a new card run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSequenceBoundary {
    pub before_relative_path: String,
    pub after_relative_path: String,
    /// Clips in the suggested run beginning at `after_relative_path`.
    pub affected_clip_count: usize,
    pub evidence: Vec<ImportSequenceBoundaryEvidence>,
}

/// Whether one incoming filename run continues a named project sequence or
/// needs a new sequence proposed for later confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportSequenceRunDecision {
    ExtendExisting,
    ProposeNew,
}

/// One incoming monotonic filename run and the core-owned decision behind its
/// preview. A proposed-new run may name the deterministic existing sequence it
/// was compared with, but never claims to have matched it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSequenceRunPreview {
    pub first_relative_path: String,
    pub last_relative_path: String,
    pub clip_count: usize,
    pub camera_key: String,
    pub volume_hint: String,
    pub filename_prefix: String,
    pub decision: ImportSequenceRunDecision,
    pub matched_existing_sequence_id: Option<String>,
    pub matched_existing_sequence_label: Option<String>,
    /// The same-camera sequence used to explain a mismatch, if there was one.
    pub compared_existing_sequence_id: Option<String>,
    pub compared_existing_sequence_label: Option<String>,
    /// True only when this run's own evidence actively disagrees with
    /// `compared_existing_sequence_id` (card/filename evidence differs). A
    /// `proposeNew` run can also carry a `compared_existing_sequence_id`
    /// because another incoming run already claimed that sequence, or
    /// because a boundary split it off deliberately — both are routine,
    /// expected outcomes of the sequencing algorithm, not something the
    /// user needs to look at before import. This is the one field a reader
    /// should use to distinguish "genuinely ambiguous" from "routine new
    /// sequence"; `reason` is prose for humans and must not be pattern
    /// matched to recover this distinction.
    pub needs_review: bool,
    pub reason: String,
    pub evidence: Vec<ImportSequenceBoundaryEvidence>,
}

/// Capture-sequence impact shown alongside the copy/reference delta. It is a
/// suggestion layer only; accepting an import never accepts these boundaries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSequencePreview {
    pub new_clip_count: usize,
    pub existing_sequence_count: usize,
    pub proposed_sequence_count: usize,
    pub matched_existing_sequence_count: usize,
    pub suggested_new_sequence_count: usize,
    pub boundaries: Vec<ImportSequenceBoundary>,
    #[serde(default)]
    pub runs: Vec<ImportSequenceRunPreview>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImportSequenceCandidate {
    relative_path: String,
    camera_key: String,
    volume_hint: String,
    folder: String,
    prefix: String,
    counter: Option<u64>,
    observed_millis: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProposedImportRun {
    camera_key: String,
    volume_hint: String,
    prefix: String,
    first_relative_path: String,
    last_relative_path: String,
    clip_count: usize,
    boundary_evidence: Vec<ImportSequenceBoundaryEvidence>,
}

/// What import would do with one candidate file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Classification {
    /// Content this project has never seen. Import creates an asset.
    New,
    /// Known, and the asset can already reach its original. Nothing to do.
    KnownVerified,
    /// Known, but the asset's original is offline and this is it. The whole
    /// reason a duplicate must not simply be skipped.
    KnownRelinkable,
    /// Known and reachable, but its proxy is missing or stale.
    KnownMissingProxy,
    /// Something already occupies the archive path this file would take, and it
    /// is different content. Reported, never overwritten.
    PathCollision,
    /// Could not be read well enough to classify.
    Unreadable,
    /// Older than this project's stored "start here" boundary for this card
    /// (#66). Not copied, not ingested, not proxied, and deliberately not a
    /// failure: the user said this footage predates the project. Kept in the
    /// item list rather than dropped, so a persistent filter is always
    /// inspectable — see `import_cutoff`.
    ExcludedByCutoff,
}

/// One classified file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightItem {
    pub path: String,
    /// Path relative to the source root, which becomes the archive-relative path
    /// and so preserves the camera's own folder structure.
    pub relative_path: String,
    pub filename: String,
    pub size_bytes: u64,
    pub classification: Classification,
    /// The asset this content already belongs to, when it is known.
    pub asset_id: Option<String>,
    /// Why a file could not be classified. Empty unless `Unreadable`.
    pub error: String,
    /// The sampled content fingerprint already computed to answer "is this
    /// known" (see this module's cost-discipline note above). Reused as the
    /// cache key for import-preview posters so identical bytes at a
    /// different path do not re-decode. Absent when the file could not be
    /// read well enough to classify (`Unreadable`).
    #[serde(default)]
    pub fingerprint: Option<String>,
    /// The file's own filesystem modification time, RFC 3339 UTC, read from
    /// the same `stat` preflight already pays for every candidate. This is a
    /// best-available stand-in for a probed capture time, not a claim about
    /// when the footage was actually shot — ffprobe never runs during
    /// preflight (see this module's doc comment). Import-preview's probed
    /// capture time, and after import the asset's own probed `bestTime`,
    /// both supersede it.
    #[serde(default)]
    pub filesystem_modified_at: Option<String>,
}

/// Where an import would put originals, stated plainly enough to show a user.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportDestination {
    /// True when originals are copied; false when they are referenced in place.
    pub copies: bool,
    /// The archive root originals land in, when copying.
    pub archive_root: Option<ArchiveRoot>,
    /// Where under that root, or the source folder when referencing in place.
    pub resolved_path: String,
    /// A human-readable statement of what happens to the originals, so the
    /// dialog never has to infer it.
    pub summary: String,
    /// Bytes the copy would write. Zero when referencing in place.
    pub bytes_to_copy: u64,
    /// Free bytes at the destination, when it is reachable.
    pub available_bytes: Option<u64>,
    /// True when the destination is reachable and has room for the copy.
    pub ready: bool,
    /// Why the destination is not ready. Empty when it is.
    pub blocked_reason: String,
}

/// Where a plan's camera identity came from — the answer the import dialog
/// needs to decide how much confidence to show, and whether to make
/// uncertainty visible rather than presenting a guess as settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportCameraSource {
    /// The user chose this camera for this import (an existing project
    /// camera, or a deliberately typed new one).
    UserSelected,
    /// This project's durable evidence-to-camera store (`cameras::mapping`)
    /// recognized this card's folder layout and filename prefix.
    Remembered,
    /// Bounded preflight inference: representative EXIF first, then the
    /// filename-pattern table, then the card's folder-layout hint. This is not
    /// durable unless the user explicitly selects it.
    /// A `Detected` value is therefore never durable — never written to the
    /// mapping store, never forced onto ingest via `run_import`'s
    /// `camera_override` — precisely because it can be wrong in a way
    /// a heuristic should never become a sticky correction merely because no
    /// one changed it. See `ResolvedImportCamera::uniform_camera`.
    Detected,
    /// Nothing identified the camera at all.
    Unknown,
}

/// The one camera identity a plan resolves to, so it can flow unchanged into
/// both the sequence preview below and every new asset `run_import` creates —
/// see this module's and `cameras::mapping`'s doc comments for why the two
/// must never be free to disagree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedImportCamera {
    pub camera: String,
    pub source: ImportCameraSource,
    /// Whether this import's own card evidence (folder layout plus filename
    /// prefix) was strong enough to safely consult or update the durable
    /// mapping store. When false, `source` is never `Remembered`: the
    /// dialog should present `camera` as a guess to confirm, not a fact.
    pub evidence_strong: bool,
}

impl ResolvedImportCamera {
    /// The camera every candidate/asset in this plan must be forced to, or
    /// `None` to leave each file's own independent guess alone. Only an
    /// active decision — a live user selection, or a durable mapping this
    /// project already committed to — earns that override; plain detection
    /// never does (see `ImportCameraSource::Detected`'s doc comment). This
    /// is the single place that policy is expressed; `preflight_import`'s
    /// sequence-preview grouping and `run_import`'s ingest override both
    /// call this rather than each re-stating the `matches!` themselves, so a
    /// future source variant cannot be wired into one and forgotten in the
    /// other.
    pub fn uniform_camera(&self) -> Option<&str> {
        matches!(
            self.source,
            ImportCameraSource::UserSelected | ImportCameraSource::Remembered
        )
        .then_some(self.camera.as_str())
    }
}

impl Default for ResolvedImportCamera {
    fn default() -> Self {
        Self {
            camera: UNKNOWN_CAMERA.to_string(),
            source: ImportCameraSource::Unknown,
            evidence_strong: false,
        }
    }
}

/// The whole answer the import dialog renders.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreflight {
    pub source_path: String,
    /// Set when the source is a recognizable camera card.
    pub card: Option<CardCandidate>,
    /// The camera identity this plan resolved to. See `ResolvedImportCamera`.
    #[serde(default)]
    pub camera_identity: ResolvedImportCamera,
    /// The exact evidence key consulted for `camera_identity`, carried into
    /// `run_import` so a successful explicit choice is recorded under the same
    /// key rather than recomputed from potentially changed source state.
    #[serde(default)]
    pub camera_evidence: CameraEvidence,
    pub destination: ImportDestination,
    pub scanned: usize,
    pub new_clips: usize,
    /// Known content that needs nothing.
    pub known_clips: usize,
    /// Known content whose original is offline and is present here.
    pub relinkable: usize,
    /// Clips this import would leave needing a proxy.
    pub missing_proxies: usize,
    pub collisions: usize,
    pub unreadable: usize,
    /// The project's stored "start here" boundary for this card, when it has
    /// one and this source is that card. See `import_cutoff`.
    #[serde(default)]
    pub cutoff: Option<crate::ImportCutoff>,
    /// Candidates the cutoff excluded. Reported separately from
    /// `known_clips` on purpose: "you already have these" and "you told me to
    /// ignore these" are different facts and lead to different actions.
    #[serde(default)]
    pub excluded_by_cutoff: usize,
    /// Clips before the cutoff whose original this project has lost and which
    /// this source could repair. The cutoff never suppresses these — see
    /// `apply_import_cutoff` — and the count exists so the dialog can say so
    /// rather than leaving a surprise inside a filtered list.
    #[serde(default)]
    pub relinkable_before_cutoff: usize,
    /// Suggested card runs and their evidence. Preflight never applies them.
    #[serde(default)]
    pub sequence_preview: ImportSequencePreview,
    /// Every file, so the result stays inspectable after the dialog closes.
    pub items: Vec<PreflightItem>,
}

fn filename_prefix(filename: &str) -> String {
    let stem = filename
        .rsplit_once('.')
        .map_or(filename, |(stem, _extension)| stem);
    let prefix = stem
        .split_once(|character: char| character.is_ascii_digit())
        .map_or(stem, |(prefix, _)| prefix)
        .trim_matches(|character: char| matches!(character, '_' | '-' | ' ' | '.'));
    if prefix.is_empty() {
        stem.to_uppercase()
    } else {
        prefix.to_uppercase()
    }
}

/// The most common value, ties broken lexicographically so the result is
/// deterministic regardless of scan order. `None` for an empty input.
fn mode(values: impl IntoIterator<Item = String>) -> Option<String> {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for value in values {
        *counts.entry(value).or_insert(0) += 1;
    }
    // Iteration ascends by key; keep the current leader unless a later
    // (lexicographically larger) key strictly beats its count, so the first
    // (smallest) key among an exact tie wins.
    counts
        .into_iter()
        .fold(None::<(String, usize)>, |best, (value, count)| match best {
            Some((best_value, best_count)) if best_count >= count => Some((best_value, best_count)),
            _ => Some((value, count)),
        })
        .map(|(value, _)| value)
}

/// The card evidence one set of new clips presents, in the exact shape
/// `cameras::mapping` keys a durable mapping on. Used both to *resolve* a
/// plan's camera identity in `preflight_import` and, after a successful
/// import, to *record* it in `run_import` — sharing this function is what
/// keeps those two computations from silently drifting apart into different
/// keys for what should be the same evidence.
fn camera_evidence_for<'a>(
    card: Option<&CardCandidate>,
    new_filenames: impl Iterator<Item = &'a str>,
    exif_hint: String,
) -> CameraEvidence {
    CameraEvidence {
        folder_hint: card
            .map(|candidate| candidate.camera_hint.trim().to_string())
            .unwrap_or_default(),
        filename_prefix: mode(new_filenames.map(filename_prefix)).unwrap_or_default(),
        exif_hint,
    }
}

/// Today's ordinary per-file camera guess: the filename-pattern table first,
/// then the card's folder-layout hint, then `UNKNOWN_CAMERA`. This is exactly
/// what asset ingest's own EXIF-blind fallback would produce (`probe_media`
/// tries EXIF first; preflight never has it — see this module's doc
/// comment), which is why it stays the default whenever nothing more
/// specific — a user selection or a remembered mapping — overrides it.
fn detected_camera_key(filename: &str, card: Option<&CardCandidate>) -> String {
    let inferred = crate::identify_camera(None, filename).camera;
    if inferred == UNKNOWN_CAMERA {
        card.map(|candidate| candidate.camera_hint.trim())
            .filter(|hint| !hint.is_empty())
            .unwrap_or(inferred.as_str())
            .to_string()
    } else {
        inferred
    }
}

/// One incoming file's contribution to the sequence preview. `camera_override`
/// forces every candidate in a plan to the one identity that resolved for it
/// (a user selection or a remembered mapping — see `ResolvedImportCamera`),
/// which is what makes the preview and asset ingest structurally unable to
/// disagree. `None` preserves today's independent per-file detection, so a
/// heterogeneous "Add media folder" import (no single card, no override in
/// play) still groups by each file's own camera as before.
fn sequence_candidate(
    source: &Path,
    path: &Path,
    card: Option<&CardCandidate>,
    camera_override: Option<&str>,
) -> ImportSequenceCandidate {
    let relative_path = path
        .strip_prefix(source)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let filename = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let camera_key = camera_override
        .map(str::to_string)
        .unwrap_or_else(|| detected_camera_key(&filename, card));
    let folder = Path::new(&relative_path)
        .parent()
        .map(|parent| parent.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let observed_millis = std::fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| u64::try_from(duration.as_millis()).ok());
    ImportSequenceCandidate {
        relative_path,
        camera_key,
        volume_hint: card
            .map(|candidate| candidate.session_id.clone())
            .unwrap_or_else(|| source.to_string_lossy().into_owned()),
        folder,
        prefix: filename_prefix(&filename),
        counter: crate::chronology::parse_filename_counter(&filename),
        observed_millis,
    }
}

fn boundary_evidence(
    before: &ImportSequenceCandidate,
    after: &ImportSequenceCandidate,
) -> Vec<ImportSequenceBoundaryEvidence> {
    let mut evidence = Vec::new();
    if before.volume_hint != after.volume_hint {
        evidence.push(ImportSequenceBoundaryEvidence::VolumeChange {
            from: before.volume_hint.clone(),
            to: after.volume_hint.clone(),
        });
    }
    if before.prefix != after.prefix {
        evidence.push(ImportSequenceBoundaryEvidence::PrefixChange {
            from: before.prefix.clone(),
            to: after.prefix.clone(),
        });
    }
    if let (Some(previous), Some(next)) = (before.counter, after.counter) {
        if next < previous {
            evidence.push(ImportSequenceBoundaryEvidence::CounterReset { previous, next });
        }
        if before.folder != after.folder && next <= previous {
            evidence.push(ImportSequenceBoundaryEvidence::FolderReset {
                from: before.folder.clone(),
                to: after.folder.clone(),
            });
        }
    } else if before.folder != after.folder {
        evidence.push(ImportSequenceBoundaryEvidence::FolderReset {
            from: before.folder.clone(),
            to: after.folder.clone(),
        });
    }
    if let (Some(before_millis), Some(after_millis)) =
        (before.observed_millis, after.observed_millis)
    {
        let gap_seconds = after_millis.saturating_sub(before_millis) / 1_000;
        if gap_seconds >= LARGE_CAPTURE_SEQUENCE_GAP_SECONDS {
            evidence.push(ImportSequenceBoundaryEvidence::LargeDiscontinuity { gap_seconds });
        }
    }
    evidence
}

fn propose_import_runs(
    mut candidates: Vec<ImportSequenceCandidate>,
) -> (
    Vec<ImportSequenceCandidate>,
    Vec<ProposedImportRun>,
    Vec<ImportSequenceBoundary>,
) {
    candidates.sort_by(|left, right| {
        left.camera_key
            .cmp(&right.camera_key)
            .then_with(|| left.observed_millis.cmp(&right.observed_millis))
            .then_with(|| left.folder.cmp(&right.folder))
            .then_with(|| left.counter.cmp(&right.counter))
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    if candidates.is_empty() {
        return (candidates, Vec::new(), Vec::new());
    }

    let mut starts = vec![0usize];
    let mut evidence_at_start: Vec<Vec<ImportSequenceBoundaryEvidence>> = vec![Vec::new()];
    for index in 1..candidates.len() {
        let before = &candidates[index - 1];
        let after = &candidates[index];
        let evidence = boundary_evidence(before, after);
        // Camera identity is itself a sequence partition. Pattern-derived
        // identities also carry a prefix change, so the displayed evidence
        // remains one of the frozen boundary kinds.
        if before.camera_key != after.camera_key || !evidence.is_empty() {
            starts.push(index);
            evidence_at_start.push(evidence);
        }
    }

    let mut runs = Vec::with_capacity(starts.len());
    let mut boundaries = Vec::with_capacity(starts.len().saturating_sub(1));
    for (run_index, start) in starts.iter().copied().enumerate() {
        let end = starts
            .get(run_index + 1)
            .copied()
            .unwrap_or(candidates.len());
        let first = &candidates[start];
        runs.push(ProposedImportRun {
            camera_key: first.camera_key.clone(),
            volume_hint: first.volume_hint.clone(),
            prefix: first.prefix.clone(),
            first_relative_path: first.relative_path.clone(),
            last_relative_path: candidates[end - 1].relative_path.clone(),
            clip_count: end - start,
            boundary_evidence: evidence_at_start[run_index].clone(),
        });
        if run_index > 0 {
            boundaries.push(ImportSequenceBoundary {
                before_relative_path: candidates[start - 1].relative_path.clone(),
                after_relative_path: first.relative_path.clone(),
                affected_clip_count: end - start,
                evidence: evidence_at_start[run_index].clone(),
            });
        }
    }
    (candidates, runs, boundaries)
}

fn sequence_matches_run(sequence: &crate::CaptureSequence, run: &ProposedImportRun) -> bool {
    sequence.camera_key == run.camera_key
        && (sequence.volume_hint.is_empty() || sequence.volume_hint == run.volume_hint)
        && (sequence.filename_prefix.is_empty() || sequence.filename_prefix == run.prefix)
}

fn existing_mismatch_evidence(
    sequence: &crate::CaptureSequence,
    run: &ProposedImportRun,
) -> Vec<ImportSequenceBoundaryEvidence> {
    let mut evidence = Vec::new();
    if !sequence.volume_hint.is_empty() && sequence.volume_hint != run.volume_hint {
        evidence.push(ImportSequenceBoundaryEvidence::VolumeChange {
            from: sequence.volume_hint.clone(),
            to: run.volume_hint.clone(),
        });
    }
    if !sequence.filename_prefix.is_empty() && sequence.filename_prefix != run.prefix {
        evidence.push(ImportSequenceBoundaryEvidence::PrefixChange {
            from: sequence.filename_prefix.clone(),
            to: run.prefix.clone(),
        });
    }
    evidence
}

impl Catalog {
    /// Classifies everything at `source` against `project_id` without mutating
    /// anything.
    ///
    /// `archive_root_id` names where originals would be copied. Passing `None`
    /// means reference-in-place, which is what a non-removable source gets.
    ///
    /// `camera_override` is the camera the dialog's user has explicitly
    /// chosen for this import, if any — an existing project camera or a
    /// deliberately typed new one. Passing `None` lets the plan resolve its
    /// own identity: a durable per-project mapping when this card's evidence
    /// is strong enough (`cameras::mapping`), otherwise today's ordinary
    /// per-file detection. Whatever wins becomes `ImportPreflight.camera_identity`
    /// and is the *only* camera identity threaded into both the sequence
    /// preview below and `run_import`'s asset ingest — see
    /// `ResolvedImportCamera`'s doc comment for why that must be one value,
    /// not two independently-recomputed guesses.
    pub fn preflight_import<F>(
        &self,
        project_id: &str,
        source: impl AsRef<Path>,
        archive_root_id: Option<&str>,
        camera_override: Option<&str>,
        mut on_progress: F,
    ) -> Result<ImportPreflight>
    where
        F: FnMut(usize, usize, &Path) -> bool,
    {
        self.require_project(project_id)?;
        let source = source.as_ref();
        if !source.is_dir() {
            anyhow::bail!("import source is not a directory: {}", source.display());
        }
        let card = inspect_card(source)?;
        if let Some(found) = card.as_ref() {
            // Tie this card session to the hardware it was read from (#78).
            // Best-effort: a volume that cannot identify itself must not stop
            // an import from being planned.
            let _ = self.observe_card_volume(source, &found.session_id);
        }

        let mut candidates: Vec<PathBuf> = WalkDir::new(source)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.into_path())
            .filter(|path| is_media(path))
            .collect();
        candidates.sort();

        let total = candidates.len();
        let mut items = Vec::with_capacity(total);
        for (index, path) in candidates.iter().enumerate() {
            items.push(self.classify(project_id, source, path, archive_root_id, card.as_ref()));
            if !on_progress(index + 1, total, path) {
                anyhow::bail!("import preflight cancelled");
            }
        }

        // The boundary is applied here, between classification and every
        // number derived from it, so an excluded clip cannot reach the byte
        // cost, the camera evidence, or a capture-sequence proposal. Footage
        // the user told the project to ignore must not go on shaping it.
        let cutoff = match card.as_ref() {
            Some(card) => self.import_cutoff(project_id, &card.session_id)?,
            None => None,
        };
        let (excluded_by_cutoff, relinkable_before_cutoff) = match cutoff.as_ref() {
            Some(cutoff) => apply_import_cutoff(&mut items, &cutoff.relative_path),
            None => (0, 0),
        };

        let bytes_to_copy = items
            .iter()
            .filter(|item| item.classification == Classification::New)
            .map(|item| item.size_bytes)
            .sum();
        let destination = self.describe_destination(
            project_id,
            source,
            archive_root_id,
            card.as_ref(),
            bytes_to_copy,
        )?;

        let new_items: Vec<&PreflightItem> = items
            .iter()
            .filter(|item| item.classification == Classification::New)
            .collect();
        // Candidates are sorted before classification, so the first new item
        // is a deterministic representative. One EXIF read is the bounded
        // cost needed to avoid treating every same-brand filename convention
        // as the same physical camera. Ordinary folder imports cannot create
        // sticky card mappings, so they do not pay even this one read.
        let representative_exif = card
            .as_ref()
            .and_then(|_| new_items.first())
            .and_then(|item| crate::exif_camera(Path::new(&item.path)))
            .unwrap_or_default();
        let camera_evidence = camera_evidence_for(
            card.as_ref(),
            new_items.iter().map(|item| item.filename.as_str()),
            representative_exif,
        );
        let mut camera_identity = self.resolve_import_camera(
            project_id,
            card.as_ref(),
            &camera_evidence,
            &new_items,
            camera_override,
        )?;
        camera_identity.camera =
            self.canonical_camera_identity(project_id, &camera_identity.camera)?;
        // Only an active selection or a remembered mapping forces every
        // candidate to one identity. Plain detection leaves each file's own
        // per-file guess in place, unchanged from before this identity was
        // introduced, so a heterogeneous "Add media folder" import still
        // groups by each file's own camera rather than being homogenized by
        // a guess nobody confirmed.
        let uniform_camera = camera_identity.uniform_camera();
        let mut canonical_cameras: BTreeMap<String, String> = BTreeMap::new();
        let sequence_candidates = new_items
            .iter()
            .map(|item| {
                let mut candidate = sequence_candidate(
                    source,
                    Path::new(&item.path),
                    card.as_ref(),
                    uniform_camera,
                );
                candidate.camera_key =
                    if let Some(camera) = canonical_cameras.get(&candidate.camera_key) {
                        camera.clone()
                    } else {
                        let camera =
                            self.canonical_camera_identity(project_id, &candidate.camera_key)?;
                        canonical_cameras.insert(candidate.camera_key.clone(), camera.clone());
                        camera
                    };
                Ok(candidate)
            })
            .collect::<Result<Vec<_>>>()?;
        let sequence_preview = self.import_sequence_preview(project_id, sequence_candidates)?;

        Ok(ImportPreflight {
            source_path: source.to_string_lossy().into_owned(),
            card,
            camera_identity,
            camera_evidence,
            scanned: total,
            new_clips: count(&items, Classification::New),
            known_clips: count(&items, Classification::KnownVerified),
            relinkable: count(&items, Classification::KnownRelinkable),
            missing_proxies: count(&items, Classification::KnownMissingProxy)
                + count(&items, Classification::New),
            collisions: count(&items, Classification::PathCollision),
            unreadable: count(&items, Classification::Unreadable),
            cutoff,
            excluded_by_cutoff,
            relinkable_before_cutoff,
            sequence_preview,
            destination,
            items,
        })
    }

    /// Resolves the one camera identity a plan will use. See
    /// `preflight_import`'s doc comment for the precedence
    /// (override, then remembered mapping, then plain detection) and
    /// `cameras::mapping::CameraEvidence` for the evidence-strength rule
    /// that gates the remembered-mapping lookup.
    fn resolve_import_camera(
        &self,
        project_id: &str,
        card: Option<&CardCandidate>,
        evidence: &CameraEvidence,
        new_items: &[&PreflightItem],
        camera_override: Option<&str>,
    ) -> Result<ResolvedImportCamera> {
        let evidence_strong = evidence.is_strong_enough();

        if let Some(explicit) = camera_override
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Ok(ResolvedImportCamera {
                camera: explicit.to_string(),
                source: ImportCameraSource::UserSelected,
                evidence_strong,
            });
        }

        if evidence_strong
            && let Some(remembered) = self.remembered_camera_for_evidence(project_id, evidence)?
        {
            return Ok(ResolvedImportCamera {
                camera: remembered,
                source: ImportCameraSource::Remembered,
                evidence_strong,
            });
        }

        let detected = if evidence.exif_hint.is_empty() {
            mode(
                new_items
                    .iter()
                    .map(|item| detected_camera_key(&item.filename, card)),
            )
            .unwrap_or_else(|| UNKNOWN_CAMERA.to_string())
        } else {
            evidence.exif_hint.clone()
        };
        Ok(ResolvedImportCamera {
            source: if detected == UNKNOWN_CAMERA {
                ImportCameraSource::Unknown
            } else {
                ImportCameraSource::Detected
            },
            camera: detected,
            evidence_strong,
        })
    }

    fn import_sequence_preview(
        &self,
        project_id: &str,
        candidates: Vec<ImportSequenceCandidate>,
    ) -> Result<ImportSequencePreview> {
        let new_clip_count = candidates.len();
        let (_ordered, proposed_runs, boundaries) = propose_import_runs(candidates);
        let mut existing = self.capture_sequences(project_id)?;
        existing.sort_by(|left, right| left.id.cmp(&right.id));
        let mut used_existing = BTreeSet::new();
        let mut runs = Vec::with_capacity(proposed_runs.len());
        for run in proposed_runs {
            let matching = existing.iter().enumerate().find(|(index, sequence)| {
                !used_existing.contains(index) && sequence_matches_run(sequence, &run)
            });
            if let Some((index, sequence)) = matching {
                used_existing.insert(index);
                runs.push(ImportSequenceRunPreview {
                    first_relative_path: run.first_relative_path,
                    last_relative_path: run.last_relative_path,
                    clip_count: run.clip_count,
                    camera_key: run.camera_key,
                    volume_hint: run.volume_hint,
                    filename_prefix: run.prefix,
                    decision: ImportSequenceRunDecision::ExtendExisting,
                    matched_existing_sequence_id: Some(sequence.id.clone()),
                    matched_existing_sequence_label: Some(sequence.label.clone()),
                    compared_existing_sequence_id: None,
                    compared_existing_sequence_label: None,
                    needs_review: false,
                    reason: format!(
                        "This run matches the camera, card, and filename evidence for \"{}\".",
                        sequence.label
                    ),
                    evidence: run.boundary_evidence,
                });
                continue;
            }

            // Explain a rejection against a stable same-camera candidate. An
            // unused candidate comes first; if every matching-camera sequence
            // is already claimed by another incoming run, compare against the
            // first by id so the reason remains deterministic.
            let compared = existing
                .iter()
                .enumerate()
                .find(|(index, sequence)| {
                    !used_existing.contains(index) && sequence.camera_key == run.camera_key
                })
                .or_else(|| {
                    existing
                        .iter()
                        .enumerate()
                        .find(|(_, sequence)| sequence.camera_key == run.camera_key)
                });
            let mismatch = compared
                .map(|(_, sequence)| existing_mismatch_evidence(sequence, &run))
                .unwrap_or_default();
            let has_mismatch = !mismatch.is_empty();
            let mut evidence = run.boundary_evidence;
            for found in mismatch {
                if !evidence.contains(&found) {
                    evidence.push(found);
                }
            }
            let reason = match compared {
                Some((_, sequence)) if has_mismatch => {
                    format!(
                        "This run differs from \"{}\" on card or filename evidence.",
                        sequence.label
                    )
                }
                Some((index, sequence)) if used_existing.contains(&index) => format!(
                    "Another incoming run already extends \"{}\"; this distinct run stays separate.",
                    sequence.label
                ),
                Some((_, sequence)) => format!(
                    "Boundary evidence keeps this run separate from \"{}\".",
                    sequence.label
                ),
                None => format!(
                    "No existing capture sequence is available for {}.",
                    run.camera_key
                ),
            };
            runs.push(ImportSequenceRunPreview {
                first_relative_path: run.first_relative_path,
                last_relative_path: run.last_relative_path,
                clip_count: run.clip_count,
                camera_key: run.camera_key,
                volume_hint: run.volume_hint,
                filename_prefix: run.prefix,
                decision: ImportSequenceRunDecision::ProposeNew,
                matched_existing_sequence_id: None,
                matched_existing_sequence_label: None,
                compared_existing_sequence_id: compared.map(|(_, sequence)| sequence.id.clone()),
                compared_existing_sequence_label: compared
                    .map(|(_, sequence)| sequence.label.clone()),
                needs_review: has_mismatch,
                reason,
                evidence,
            });
        }
        let matched_existing_sequence_count = runs
            .iter()
            .filter(|run| run.decision == ImportSequenceRunDecision::ExtendExisting)
            .count();
        let suggested_new_sequence_count = runs.len() - matched_existing_sequence_count;
        Ok(ImportSequencePreview {
            new_clip_count,
            existing_sequence_count: existing.len(),
            proposed_sequence_count: runs.len(),
            matched_existing_sequence_count,
            suggested_new_sequence_count,
            boundaries,
            runs,
        })
    }

    fn classify(
        &self,
        project_id: &str,
        source: &Path,
        path: &Path,
        archive_root_id: Option<&str>,
        card: Option<&CardCandidate>,
    ) -> PreflightItem {
        let relative_path = path
            .strip_prefix(source)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let filename = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut item = PreflightItem {
            path: path.to_string_lossy().into_owned(),
            relative_path: relative_path.clone(),
            filename,
            size_bytes: 0,
            classification: Classification::Unreadable,
            asset_id: None,
            error: String::new(),
            fingerprint: None,
            filesystem_modified_at: filesystem_modified_at(path),
        };
        match self.classify_inner(project_id, path, &relative_path, archive_root_id, card) {
            Ok((classification, asset_id, size_bytes, fingerprint)) => {
                item.classification = classification;
                item.asset_id = asset_id;
                item.size_bytes = size_bytes;
                item.fingerprint = Some(fingerprint);
            }
            Err(error) => item.error = format!("{error:#}"),
        }
        item
    }

    fn classify_inner(
        &self,
        project_id: &str,
        path: &Path,
        relative_path: &str,
        archive_root_id: Option<&str>,
        card: Option<&CardCandidate>,
    ) -> Result<(Classification, Option<String>, u64, String)> {
        let size_bytes = std::fs::metadata(path)?.len();
        let fingerprint = sampled_media_fingerprint(path, size_bytes)?;
        let connection = self.connection()?;
        let known: Option<(String, String, bool)> = connection
            .query_row(
                "SELECT id, path, COALESCE(membership_removed, 0) FROM assets WHERE project_id = ?1 AND fingerprint = ?2",
                params![project_id, fingerprint],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;

        let Some((asset_id, recorded_path, membership_removed)) = known else {
            // New content. The only remaining question is whether it can land
            // where it would be written.
            if let (Some(root_id), Some(card)) = (archive_root_id, card) {
                let target = self.archive_target(project_id, root_id, card, relative_path)?;
                if target.is_file() {
                    // Something occupies the pathname, which on its own says
                    // nothing: an interruption between `commit_into_archive`
                    // and asset registration leaves *our own* verified bytes
                    // here, with no asset to find them by fingerprint. Ask
                    // what the content is. Same content is a copy we already
                    // committed and can adopt; only genuinely different
                    // content is a collision, and overwriting an archived
                    // original is never the answer.
                    let target_size = std::fs::metadata(&target)?.len();
                    if sampled_media_fingerprint(&target, target_size)? != fingerprint {
                        return Ok((Classification::PathCollision, None, size_bytes, fingerprint));
                    }
                }
            }
            return Ok((Classification::New, None, size_bytes, fingerprint));
        };

        if membership_removed {
            // A rollback preserves identity and every verified location. On a
            // reference import, treat the rediscovery as a new membership. For
            // an archive import, keep the archive create-only: a retained
            // matching copy is reused at execution time, otherwise copy to an
            // empty target and never overwrite a collision. It remains `New`
            // so this new membership can itself be rolled back.
            if let (Some(root_id), Some(card)) = (archive_root_id, card) {
                let target = self.archive_target(project_id, root_id, card, relative_path)?;
                if target.is_file() {
                    let target_size = std::fs::metadata(&target)?.len();
                    if sampled_media_fingerprint(&target, target_size)? == fingerprint {
                        return Ok((Classification::New, Some(asset_id), size_bytes, fingerprint));
                    }
                    return Ok((Classification::PathCollision, None, size_bytes, fingerprint));
                }
            }
            return Ok((Classification::New, Some(asset_id), size_bytes, fingerprint));
        }

        // Known content. Can the asset actually reach an original right now?
        let recorded = locations::recorded_location(&connection, &asset_id, &recorded_path)?;
        let reachable = locations::location_is_available(
            &recorded_path,
            recorded.map(|found| found.state),
            recorded.and_then(|found| found.guard),
        );
        if !reachable {
            return Ok((
                Classification::KnownRelinkable,
                Some(asset_id),
                size_bytes,
                fingerprint,
            ));
        }
        drop(connection);
        let asset = self.get_asset(&asset_id)?;
        if has_current_proxy(&asset) {
            Ok((
                Classification::KnownVerified,
                Some(asset_id),
                size_bytes,
                fingerprint,
            ))
        } else {
            Ok((
                Classification::KnownMissingProxy,
                Some(asset_id),
                size_bytes,
                fingerprint,
            ))
        }
    }

    /// Where one card-relative file would be written under an archive root.
    pub(crate) fn archive_target(
        &self,
        project_id: &str,
        archive_root_id: &str,
        card: &CardCandidate,
        relative_path: &str,
    ) -> Result<PathBuf> {
        let relative = self.archive_relative_path(project_id, card, relative_path)?;
        self.resolve_in_archive_root(archive_root_id, &relative)
    }

    /// `<projectSlug>/<cardSessionSlug>/<cardRelativePath>`.
    ///
    /// The card-session component is what keeps a reformatted card from writing
    /// into the previous card's directory when both are labelled `UNTITLED` and
    /// both contain `C0001.MP4`.
    pub(crate) fn archive_relative_path(
        &self,
        project_id: &str,
        card: &CardCandidate,
        relative_path: &str,
    ) -> Result<String> {
        let project_name: String = self.connection()?.query_row(
            "SELECT name FROM projects WHERE id = ?1",
            params![project_id],
            |row| row.get(0),
        )?;
        let slug = directory_slug(&project_name);
        Ok(format!(
            "{slug}/{}/{relative_path}",
            card_session_slug(card)
        ))
    }

    fn describe_destination(
        &self,
        project_id: &str,
        source: &Path,
        archive_root_id: Option<&str>,
        card: Option<&CardCandidate>,
        bytes_to_copy: u64,
    ) -> Result<ImportDestination> {
        let Some(archive_root_id) = archive_root_id else {
            // Referenced in place. Say so plainly, and say where — a user who
            // does not know their footage is still on the card cannot make an
            // informed decision about ejecting it.
            return Ok(ImportDestination {
                copies: false,
                archive_root: None,
                resolved_path: source.to_string_lossy().into_owned(),
                summary: format!(
                    "Originals stay where they are, in {}",
                    source.to_string_lossy()
                ),
                bytes_to_copy: 0,
                available_bytes: None,
                ready: true,
                blocked_reason: String::new(),
            });
        };

        let root = self.archive_root(archive_root_id)?;
        let relative = match card {
            Some(card) => self.archive_relative_path(project_id, card, "")?,
            None => String::new(),
        };
        let resolved_path = match root.local_mount_path.as_deref() {
            Some(mount) => format!("{mount}/{}", relative.trim_end_matches('/')),
            None => String::new(),
        };
        let (ready, blocked_reason) = if !root.available {
            (
                false,
                format!("\"{}\" is not connected to this machine", root.display_name),
            )
        } else if root
            .available_bytes
            .is_some_and(|available| available < bytes_to_copy)
        {
            (
                false,
                format!(
                    "\"{}\" has {} free but this import needs {}",
                    root.display_name,
                    human_bytes(root.available_bytes.unwrap_or(0)),
                    human_bytes(bytes_to_copy),
                ),
            )
        } else {
            (true, String::new())
        };

        Ok(ImportDestination {
            summary: format!(
                "Originals are copied to \"{}\", and stay on the card too",
                root.display_name
            ),
            copies: true,
            resolved_path,
            bytes_to_copy,
            available_bytes: root.available_bytes,
            archive_root: Some(root),
            ready,
            blocked_reason,
        })
    }
}

/// What one click of Import actually did.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOutcome {
    /// New assets created.
    pub imported: usize,
    /// Stable ids for the assets created by this run, in completion order.
    pub imported_asset_ids: Vec<String>,
    /// Assets whose offline original was found on this source and reconnected.
    pub relinked: usize,
    /// Stable ids for the assets reconnected by this run, in completion order.
    pub relinked_asset_ids: Vec<String>,
    /// Known content that needed nothing.
    pub skipped: usize,
    /// Originals copied into the archive root.
    pub copied: usize,
    pub bytes_copied: u64,
    pub cancelled: bool,
    /// The run stopped because the source volume went away underneath it.
    ///
    /// Distinct from `cancelled`, which is the user's own decision, and from
    /// `failures`, which are per-file problems the user has to look at. A
    /// disconnected source leaves no failed file: the interrupted item copied
    /// nothing and is simply work still to do. Everything already committed
    /// stays committed, so resuming re-plans and copies only the remainder.
    #[serde(default)]
    pub source_disconnected: bool,
    /// The run stopped because the archive destination went away underneath it.
    ///
    /// The mirror image of `source_disconnected`, and stopping matters more
    /// here: with the destination gone, every remaining item would fail the
    /// same way, and each one first pays a full stall timeout waiting on a
    /// volume that is not coming back inside this run.
    #[serde(default)]
    pub destination_disconnected: bool,
    pub failures: Vec<crate::IngestFailure>,
    /// Non-fatal bookkeeping problems after a clip's membership and bytes are
    /// already durable. These are deliberately separate from `failures`: the
    /// clip must stay visible and resumable, while the user still gets a clear
    /// path to repair the optional archive-location metadata.
    #[serde(default)]
    pub warnings: Vec<crate::IngestFailure>,
    /// A non-fatal failure to persist the user's sticky camera choice after
    /// the imported assets were already committed. The import remains valid;
    /// callers should surface this warning and let the user retry on a later
    /// matching import.
    #[serde(default)]
    pub camera_mapping_warning: Option<String>,
}

/// Where an import has got to, for a dialog that replaces its body with progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportStage {
    Copying,
    Reading,
}

/// Fine-grained import progress. Byte work includes the verification re-read,
/// so a verified copy has twice its source size as total work.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportProgress {
    pub stage: ImportStage,
    pub completed_items: usize,
    pub total_items: usize,
    pub current_file: String,
    pub completed_bytes: u64,
    pub total_bytes: u64,
    /// Present only after the item has been durably committed. A copying,
    /// verifying, or failed item is deliberately never exposed as an asset.
    #[serde(default)]
    pub asset: Option<Asset>,
}

/// Applies a stored "start here" boundary to a freshly classified list (#66).
///
/// Returns `(excluded, relinkable_before_cutoff)`.
///
/// Comparison is by `Path`, not by string, so the boundary means exactly what
/// the ledger shows: preflight sorts its candidate paths before classifying
/// them, and `Path`'s component-wise ordering is the ordering that sort used.
/// A raw string comparison disagrees with it (`a/b` versus `a.txt`), and a
/// filter whose rule does not match the list it is drawn on is a filter nobody
/// can reason about.
///
/// What the cutoff does *not* touch is as important as what it does:
///
/// - `KnownRelinkable` survives untouched. That is a clip this project already
///   owns whose original it has lost, and this source is where the original
///   is. Hiding it behind "that footage is old" would quietly leave the
///   project broken; a relink copies nothing, so there is no cost to weigh
///   against it. The count comes back so the dialog can say this out loud.
/// - `KnownVerified` and `KnownMissingProxy` stay as they are. They describe
///   clips the project already has, which is a fact about the project rather
///   than about this import, and conflating the two is exactly what the
///   separate `excluded_by_cutoff` count exists to avoid.
///
/// What it does absorb is `New`, and also `PathCollision` and `Unreadable`:
/// both of those are only problems *because* the file was going to be
/// imported, and excluded files must not be reported as failures.
fn apply_import_cutoff(items: &mut [PreflightItem], boundary: &str) -> (usize, usize) {
    let boundary = Path::new(boundary);
    let mut excluded = 0;
    let mut relinkable = 0;
    for item in items.iter_mut() {
        if Path::new(&item.relative_path) >= boundary {
            continue;
        }
        match item.classification {
            Classification::New | Classification::PathCollision | Classification::Unreadable => {
                item.classification = Classification::ExcludedByCutoff;
                item.error.clear();
                excluded += 1;
            }
            Classification::KnownRelinkable => relinkable += 1,
            Classification::KnownVerified
            | Classification::KnownMissingProxy
            | Classification::ExcludedByCutoff => {}
        }
    }
    (excluded, relinkable)
}

/// Whether a plan's source is still present *and still the same source*.
///
/// Identity, never pathname. A bus-powered enclosure can drop a volume and
/// remount a different card at the same `/Volumes/Untitled` within seconds,
/// and treating that as "the source came back" would copy the wrong footage
/// into the interrupted session's archive directory. A card therefore has to
/// present the same session identity it presented at preflight; only a
/// referenced-in-place folder, which has no card identity to check, is
/// satisfied by its directory still being there.
///
/// Costs one directory walk with nothing read or hashed, so it is affordable
/// on the error path of an import — where it is asked at most once, because
/// the answer `false` stops the run.
pub fn import_source_present(plan: &ImportPreflight) -> bool {
    let source = Path::new(&plan.source_path);
    if !source.is_dir() {
        return false;
    }
    let Some(expected) = plan.card.as_ref() else {
        return true;
    };
    matches!(
        inspect_card(source),
        Ok(Some(found)) if found.session_id == expected.session_id,
    )
}

impl Catalog {
    /// Whether this plan's archive destination is still mounted.
    ///
    /// Asked only on the error path, where the answer decides whether the run
    /// has anything left worth attempting. A plan that references originals in
    /// place has no destination to lose, so it is always present.
    fn archive_destination_present(&self, plan: &ImportPreflight) -> bool {
        let Some(root) = plan.destination.archive_root.as_ref() else {
            return true;
        };
        self.archive_root(&root.id)
            .map(|current| current.available)
            .unwrap_or(false)
    }
}

/// The per-plan context every item in a `run_import` loop shares, bundled so
/// `import_one` takes one reference instead of growing an argument for each.
#[derive(Debug, Clone, Copy)]
struct ImportOneContext<'a> {
    archive_root_id: Option<&'a str>,
    card: Option<&'a CardCandidate>,
    /// The one camera identity `run_import` resolved for this plan, when it
    /// is either a user selection or a remembered mapping. See
    /// `ResolvedImportCamera` and `run_import`'s doc comment.
    camera_override: Option<&'a str>,
}

/// The durable result of one import item. `warnings` are emitted only after
/// the asset has committed; callers must never reinterpret them as a failed
/// item that should disappear from Review.
struct ImportedItem {
    bytes: u64,
    asset_id: String,
    warnings: Vec<crate::IngestFailure>,
}

impl Catalog {
    /// Executes a preflight plan: copy what is new, register it, reconnect what
    /// was lost, and leave alone what needs nothing.
    ///
    /// Takes the plan rather than re-deriving it so that what the user agreed to
    /// in the dialog is what runs. Files are handled one at a time and each
    /// commits atomically, so cancelling stops cleanly and re-running resumes.
    pub fn run_import<F>(
        &self,
        project_id: &str,
        plan: &ImportPreflight,
        verify_copies: bool,
        mut on_progress: F,
    ) -> Result<ImportOutcome>
    where
        F: FnMut(ImportProgress) -> bool,
    {
        self.require_project(project_id)?;
        let archive_root_id = plan
            .destination
            .archive_root
            .as_ref()
            .map(|root| root.id.clone());
        if plan.destination.copies && !plan.destination.ready {
            anyhow::bail!("{}", plan.destination.blocked_reason);
        }

        // Only when the plan actively resolved to one identity — a user
        // selection or a remembered mapping — does ingest override each
        // file's own probe. Plain detection leaves `ingest_file`'s ordinary
        // EXIF-then-pattern guess in place, unchanged from before this
        // field existed. See `ResolvedImportCamera::uniform_camera`.
        let canonical_override = plan
            .camera_identity
            .uniform_camera()
            .map(|camera| self.canonical_camera_identity(project_id, camera))
            .transpose()?;
        let camera_override = canonical_override.as_deref();

        let actionable: Vec<&PreflightItem> = plan
            .items
            .iter()
            .filter(|item| {
                matches!(
                    item.classification,
                    Classification::New | Classification::KnownRelinkable
                )
            })
            .collect();
        let total = actionable.len();
        let total_bytes = plan
            .destination
            .bytes_to_copy
            .saturating_mul(if verify_copies { 2 } else { 1 });
        let mut completed_bytes = 0_u64;

        let mut outcome = ImportOutcome {
            skipped: count(&plan.items, Classification::KnownVerified)
                + count(&plan.items, Classification::KnownMissingProxy),
            ..Default::default()
        };

        tracing::info!(
            target: "rcdvlog_core::import",
            total,
            total_bytes,
            verify_copies,
            "run_import: begin copy loop"
        );

        for (index, item) in actionable.iter().enumerate() {
            let stage = if item.classification == Classification::New && plan.destination.copies {
                ImportStage::Copying
            } else {
                ImportStage::Reading
            };
            if !on_progress(ImportProgress {
                stage,
                completed_items: index,
                total_items: total,
                current_file: item.filename.clone(),
                completed_bytes,
                total_bytes,
                asset: None,
            }) {
                outcome.cancelled = true;
                break;
            }
            let completed_before_file = completed_bytes;
            let mut committed_asset = None;
            let file_started = std::time::Instant::now();
            tracing::debug!(
                target: "rcdvlog_core::import",
                index,
                total,
                file = %item.filename,
                "import_one: begin"
            );
            match self.import_one(
                project_id,
                item,
                &ImportOneContext {
                    archive_root_id: archive_root_id.as_deref(),
                    card: plan.card.as_ref(),
                    camera_override,
                },
                verify_copies,
                |file_bytes| {
                    on_progress(ImportProgress {
                        stage,
                        completed_items: index,
                        total_items: total,
                        current_file: item.filename.clone(),
                        completed_bytes: completed_before_file.saturating_add(file_bytes),
                        total_bytes,
                        asset: None,
                    })
                },
            ) {
                Ok(Some(item_result)) => {
                    let ImportedItem {
                        bytes,
                        asset_id,
                        warnings,
                    } = item_result;
                    let emitted_asset_id = asset_id.clone();
                    outcome.warnings.extend(warnings);
                    if item.classification == Classification::New {
                        outcome.imported += 1;
                        outcome.imported_asset_ids.push(asset_id);
                        if bytes > 0 {
                            outcome.copied += 1;
                            outcome.bytes_copied += bytes;
                            completed_bytes = completed_bytes.saturating_add(
                                bytes.saturating_mul(if verify_copies { 2 } else { 1 }),
                            );
                        }
                    } else {
                        outcome.relinked += 1;
                        outcome.relinked_asset_ids.push(asset_id);
                    }
                    // Read the materialized row only after all of
                    // `import_one`'s bookkeeping (including an archive
                    // location) has committed. Failure to produce this
                    // optional live update must not turn a safe import into a
                    // failed one; the end-of-run reconciliation remains the
                    // authority.
                    committed_asset = self.get_asset(&emitted_asset_id).ok();
                }
                // Cancelled mid-copy.
                Ok(None) => {
                    outcome.cancelled = true;
                    break;
                }
                Err(error) => {
                    // A removable source disappearing mid-copy is a normal
                    // field failure, not one corrupt file per remaining item.
                    // Stop the run and report the interruption, so the caller
                    // can offer a reconnect instead of burning through the
                    // whole card with the same I/O error and presenting the
                    // result as hundreds of files needing attention.
                    if !import_source_present(plan) {
                        outcome.source_disconnected = true;
                        break;
                    }
                    // The same reasoning for the far end of the copy. It
                    // matters more here: with the destination gone, every
                    // remaining item fails identically, and each one first
                    // spends a full stall timeout finding that out.
                    if !self.archive_destination_present(plan) {
                        outcome.destination_disconnected = true;
                        break;
                    }
                    outcome.failures.push(crate::IngestFailure {
                        path: item.path.clone(),
                        error: format!("{error:#}"),
                    });
                }
            }
            tracing::debug!(
                target: "rcdvlog_core::import",
                index,
                file = %item.filename,
                elapsed_ms = file_started.elapsed().as_millis() as u64,
                "import_one: end"
            );
            if !on_progress(ImportProgress {
                stage,
                completed_items: index + 1,
                total_items: total,
                current_file: item.filename.clone(),
                completed_bytes,
                total_bytes,
                asset: committed_asset,
            }) {
                outcome.cancelled = index + 1 < total;
                break;
            }
        }

        // A completed import driven by a real user decision is the project's
        // best current statement of what this card's evidence means. This is
        // what makes "reinsert a matching new card" work without the user
        // repeating a correction. Deliberately **not** triggered by plain
        // `Detected` agreeing with itself: `Detected` is preflight's own
        // EXIF-blind guess and can differ from what ingest's EXIF-aware
        // probe actually assigned these very assets — recording it would let
        // a later `Remembered` reinsertion force that guess over real EXIF,
        // splitting the camera lane #71 exists to keep whole. See
        // `ImportCameraSource::Detected` and `remember_camera_mapping`'s doc
        // comments. `Remembered` needs no rewrite either: the row it read
        // already exists.
        //
        // The write itself is bookkeeping, not the import: every asset is
        // already copied and committed by this point (this loop's own doc
        // comment — "each commits atomically"), so a failure here (a busy
        // database, say) must not turn a completed import into a reported
        // one. Report it as a warning rather than propagating an error that
        // would falsely imply the already-committed assets were rolled back.
        if outcome.imported > 0
            && plan.camera_identity.source == ImportCameraSource::UserSelected
            && let Err(error) = self.remember_camera_mapping(
                project_id,
                &plan.camera_evidence,
                canonical_override
                    .as_deref()
                    .unwrap_or(&plan.camera_identity.camera),
            )
        {
            outcome.camera_mapping_warning = Some(format!(
                "The clips were imported, but VlogCutter could not remember this camera choice: {error:#}"
            ));
        }

        // Record which card these clips actually came off (#74).
        //
        // Ingest creates one capture sequence per camera with empty hints, and
        // an empty hint matches any run — so without this, every later import
        // from the same camera model extends the same sequence and the
        // reformatted-card detectors can never fire against a real user. This
        // is the only production write of that evidence.
        //
        // It runs after the loop, on facts the import observed: the card that
        // was in the slot, and the filename prefix the clips it just committed
        // actually used. It is first-write-wins and card-only — see
        // `record_capture_sequence_card`. Like the camera mapping above, a
        // failure here is bookkeeping about already-durable clips, so it is
        // reported as a warning rather than raised.
        if let Some(card) = plan.card.as_ref()
            && !outcome.imported_asset_ids.is_empty()
        {
            for (camera, prefix) in self.imported_camera_prefixes(&outcome.imported_asset_ids)? {
                if let Err(error) = self.record_capture_sequence_card(
                    project_id,
                    &camera,
                    &card.session_id,
                    &prefix,
                ) {
                    outcome.warnings.push(crate::IngestFailure {
                        path: plan.source_path.clone(),
                        error: format!(
                            "The clips were imported, but VlogCutter could not record which card “{camera}” came from: {error:#}"
                        ),
                    });
                }
            }
        }
        Ok(outcome)
    }

    /// The filename prefix each camera's freshly imported clips actually used.
    ///
    /// A camera whose clips in this run used more than one prefix yields an
    /// empty prefix rather than an arbitrary winner: recording one of two would
    /// make the other look like a mismatch on the next import, which is a false
    /// alarm invented out of this run's own ambiguity. The volume hint is still
    /// recorded in that case — the card is unambiguous even when the prefix is
    /// not.
    fn imported_camera_prefixes(
        &self,
        imported_asset_ids: &[String],
    ) -> Result<BTreeMap<String, String>> {
        let mut prefixes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for asset_id in imported_asset_ids {
            let Ok(asset) = self.get_asset(asset_id) else {
                continue;
            };
            prefixes
                .entry(asset.camera)
                .or_default()
                .insert(filename_prefix(&asset.filename));
        }
        Ok(prefixes
            .into_iter()
            .map(|(camera, found)| {
                let prefix = if found.len() == 1 {
                    found.into_iter().next().unwrap_or_default()
                } else {
                    String::new()
                };
                (camera, prefix)
            })
            .collect())
    }

    /// Returns the durable item result, or `None` when the copy was cancelled.
    fn import_one<F>(
        &self,
        project_id: &str,
        item: &PreflightItem,
        context: &ImportOneContext<'_>,
        verify_copies: bool,
        mut on_copy_bytes: F,
    ) -> Result<Option<ImportedItem>>
    where
        F: FnMut(u64) -> bool,
    {
        let ImportOneContext {
            archive_root_id,
            card,
            camera_override,
        } = *context;
        let source = PathBuf::from(&item.path);
        if item.classification == Classification::KnownRelinkable {
            // Already-known content: registering where it is found is the whole
            // job, and it costs no copy. A relink never re-probes an
            // already-known asset's camera, so the override is passed
            // through for consistency only — it has no effect here.
            // Go through ingest rather than the Locate-only relink flow: this
            // smart-import path is allowed to reactivate a membership that was
            // rolled back, after first recording the freshly verified source.
            self.ingest_file_with_camera(project_id, &source, camera_override)?;
            let asset_id = self.asset_id_at_path(project_id, &source)?.ok_or_else(|| {
                anyhow::anyhow!("relinked asset was not found at {}", source.display())
            })?;
            return Ok(Some(ImportedItem {
                bytes: 0,
                asset_id,
                warnings: Vec::new(),
            }));
        }

        // New content. Copy it off the card first when there is somewhere to put
        // it, then register the asset against wherever it now lives.
        let (path_to_register, bytes) = match (archive_root_id, card) {
            (Some(root_id), Some(card)) => {
                let relative = self.archive_relative_path(project_id, card, &item.relative_path)?;
                let target = self.resolve_in_archive_root(root_id, &relative)?;
                if item.asset_id.is_some() && target.is_file() {
                    // This is a membership rollback being imported again. The
                    // retained archive copy belongs to the earlier session and
                    // must never be overwritten. Recheck it now because the
                    // preview's sampled fingerprint is only a planning fact.
                    let source_size = std::fs::metadata(&source)?.len();
                    let source_fingerprint = sampled_media_fingerprint(&source, source_size)?;
                    let target_size = std::fs::metadata(&target)?.len();
                    let target_fingerprint = sampled_media_fingerprint(&target, target_size)?;
                    if source_fingerprint != target_fingerprint {
                        anyhow::bail!(
                            "archive target already exists with different content: {}",
                            target.display()
                        );
                    }
                    // Smart import, unlike Locate, intentionally restores
                    // membership. Ingest the freshly verified source so it is
                    // registered before that restoration happens.
                    self.ingest_file_with_camera(project_id, &source, camera_override)?;
                    let asset_id =
                        self.asset_id_at_path(project_id, &source)?.ok_or_else(|| {
                            anyhow::anyhow!(
                                "reactivated asset was not found at {}",
                                source.display()
                            )
                        })?;
                    return Ok(Some(ImportedItem {
                        bytes: 0,
                        asset_id,
                        warnings: Vec::new(),
                    }));
                }
                if target.is_file() {
                    // Reaching here with a target already in place means an
                    // earlier run committed these bytes but was interrupted
                    // before registering them. Adopt the committed copy
                    // instead of copying it a second time. Preflight already
                    // rejects a target whose content differs, but the archive
                    // is create-only and that guarantee is not preflight's to
                    // make, so confirm the content here too.
                    let source_size = std::fs::metadata(&source)?.len();
                    let source_fingerprint = sampled_media_fingerprint(&source, source_size)?;
                    let target_size = std::fs::metadata(&target)?.len();
                    if sampled_media_fingerprint(&target, target_size)? != source_fingerprint {
                        anyhow::bail!(
                            "archive target already exists with different content: {}",
                            target.display()
                        );
                    }
                    (target, 0)
                } else {
                    match crate::copy::commit_into_archive(
                        &source,
                        &target,
                        verify_copies,
                        &mut on_copy_bytes,
                    )? {
                        Some(committed) => (committed.path, committed.size_bytes),
                        None => return Ok(None),
                    }
                }
            }
            // Referenced in place: the original stays exactly where it is.
            _ => (source, 0),
        };

        // The bytes are committed but nothing is registered yet, and the
        // ingest below can run for minutes on a large clip — fingerprinting
        // it, then probing it twice. Poll here so Cancel is answered inside
        // that window rather than only between copy chunks. Stopping now
        // needs no undo: the committed copy is adopted by a later run rather
        // than recopied.
        if !on_copy_bytes(bytes.saturating_mul(if verify_copies { 2 } else { 1 })) {
            return Ok(None);
        }

        self.ingest_file_with_camera(project_id, &path_to_register, camera_override)?;
        let asset_id = self
            .asset_id_at_path(project_id, &path_to_register)?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "imported asset was not found at {}",
                    path_to_register.display()
                )
            })?;

        // Bind the new asset to the archive root, so relocating the archive later
        // is one rebind rather than a per-asset repair.
        let mut warnings = Vec::new();
        if let (Some(root_id), Some(card)) = (archive_root_id, card)
            && bytes > 0
        {
            let relative = self.archive_relative_path(project_id, card, &item.relative_path)?;
            let relative = crate::RootRelativePath::parse(relative)
                .map_err(|error| anyhow::anyhow!("unusable archive path: {error}"))?;
            // Asset membership and its verified local location committed in
            // `ingest_file_with_camera` above. The archive-root binding makes
            // a future root rebind convenient, but cannot retroactively make
            // this completed clip a failure or hide it from progressive Review.
            if let Err(error) =
                self.record_archive_location(&asset_id, root_id, &relative, &path_to_register)
            {
                warnings.push(crate::IngestFailure {
                    path: path_to_register.to_string_lossy().into_owned(),
                    error: format!(
                        "Clip imported, but its archive location was not recorded: {error:#}. Keep this archive mounted and review the import warning before relocating it."
                    ),
                });
            }
        }
        Ok(Some(ImportedItem {
            bytes,
            asset_id,
            warnings,
        }))
    }

    fn asset_id_at_path(&self, project_id: &str, path: &Path) -> Result<Option<String>> {
        Ok(self
            .connection()?
            .query_row(
                r#"
                SELECT a.id
                FROM assets a
                WHERE a.project_id = ?1
                  AND COALESCE(a.membership_removed, 0) = 0
                  AND (
                    a.path = ?2
                    OR EXISTS(
                        SELECT 1
                        FROM project_asset_local_locations l
                        WHERE l.asset_id = a.id
                          AND l.local_path = ?2
                          AND l.verification_state = 'verified'
                          AND l.node_id = (
                              SELECT node_id FROM catalog_identity WHERE singleton_id = 1
                          )
                    )
                  )
                ORDER BY a.path = ?2 DESC
                LIMIT 1
                "#,
                params![project_id, path.to_string_lossy()],
                |row| row.get(0),
            )
            .optional()?)
    }
}

/// The file's own filesystem modification time, RFC 3339 UTC. One `stat`,
/// same cost as the one every candidate already pays for size and the
/// sequence-boundary evidence above — never a probed capture time.
fn filesystem_modified_at(path: &Path) -> Option<String> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(DateTime::<Utc>::from(modified).to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn count(items: &[PreflightItem], classification: Classification) -> usize {
    items
        .iter()
        .filter(|item| item.classification == classification)
        .count()
}

/// A project name reduced to one safe path component.
fn directory_slug(name: &str) -> String {
    let slug: String = name
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
    let slug = slug.trim_matches([' ', '.']);
    if slug.is_empty() {
        "project".to_string()
    } else {
        slug.to_string()
    }
}

/// Bytes as a person reads them. Import decisions are made on these numbers, so
/// they are rendered in the core rather than three times in three adapters.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
#[path = "preflight_tests.rs"]
mod tests;
