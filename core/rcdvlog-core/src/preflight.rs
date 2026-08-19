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
//! seconds. ffprobe and ExifTool are not run here at all; they are import's
//! problem, and only for files that turn out to be new.

use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

use crate::cards::{CardCandidate, card_session_slug, inspect_card};
use crate::{
    ArchiveRoot, Catalog, has_current_proxy, is_media, locations, sampled_media_fingerprint,
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

/// The whole answer the import dialog renders.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreflight {
    pub source_path: String,
    /// Set when the source is a recognizable camera card.
    pub card: Option<CardCandidate>,
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

fn sequence_candidate(
    source: &Path,
    path: &Path,
    card: Option<&CardCandidate>,
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
    let inferred = crate::identify_camera(None, &filename).camera;
    let camera_key = if inferred == crate::UNKNOWN_CAMERA {
        card.map(|candidate| candidate.camera_hint.trim())
            .filter(|hint| !hint.is_empty())
            .unwrap_or(inferred.as_str())
            .to_string()
    } else {
        inferred
    };
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
    pub fn preflight_import<F>(
        &self,
        project_id: &str,
        source: impl AsRef<Path>,
        archive_root_id: Option<&str>,
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
        let sequence_candidates = items
            .iter()
            .filter(|item| item.classification == Classification::New)
            .map(|item| sequence_candidate(source, Path::new(&item.path), card.as_ref()))
            .collect();
        let sequence_preview = self.import_sequence_preview(project_id, sequence_candidates)?;

        Ok(ImportPreflight {
            source_path: source.to_string_lossy().into_owned(),
            card,
            scanned: total,
            new_clips: count(&items, Classification::New),
            known_clips: count(&items, Classification::KnownVerified),
            relinkable: count(&items, Classification::KnownRelinkable),
            missing_proxies: count(&items, Classification::KnownMissingProxy)
                + count(&items, Classification::New),
            collisions: count(&items, Classification::PathCollision),
            unreadable: count(&items, Classification::Unreadable),
            sequence_preview,
            destination,
            items,
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
        };
        match self.classify_inner(project_id, path, &relative_path, archive_root_id, card) {
            Ok((classification, asset_id, size_bytes)) => {
                item.classification = classification;
                item.asset_id = asset_id;
                item.size_bytes = size_bytes;
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
    ) -> Result<(Classification, Option<String>, u64)> {
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
                    // Same name, different content. Overwriting an archived
                    // original is never the answer.
                    return Ok((Classification::PathCollision, None, size_bytes));
                }
            }
            return Ok((Classification::New, None, size_bytes));
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
                        return Ok((Classification::New, Some(asset_id), size_bytes));
                    }
                    return Ok((Classification::PathCollision, None, size_bytes));
                }
            }
            return Ok((Classification::New, Some(asset_id), size_bytes));
        }

        // Known content. Can the asset actually reach an original right now?
        let recorded = locations::recorded_location(&connection, &asset_id, &recorded_path)?;
        let reachable = locations::location_is_available(
            &recorded_path,
            recorded.map(|found| found.state),
            recorded.and_then(|found| found.guard),
        );
        if !reachable {
            return Ok((Classification::KnownRelinkable, Some(asset_id), size_bytes));
        }
        drop(connection);
        let asset = self.get_asset(&asset_id)?;
        if has_current_proxy(&asset) {
            Ok((Classification::KnownVerified, Some(asset_id), size_bytes))
        } else {
            Ok((
                Classification::KnownMissingProxy,
                Some(asset_id),
                size_bytes,
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
    pub failures: Vec<crate::IngestFailure>,
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
            }) {
                outcome.cancelled = true;
                break;
            }
            let completed_before_file = completed_bytes;
            match self.import_one(
                project_id,
                item,
                archive_root_id.as_deref(),
                plan.card.as_ref(),
                verify_copies,
                |file_bytes| {
                    on_progress(ImportProgress {
                        stage,
                        completed_items: index,
                        total_items: total,
                        current_file: item.filename.clone(),
                        completed_bytes: completed_before_file.saturating_add(file_bytes),
                        total_bytes,
                    })
                },
            ) {
                Ok(Some((bytes, asset_id))) => {
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
                }
                // Cancelled mid-copy.
                Ok(None) => {
                    outcome.cancelled = true;
                    break;
                }
                Err(error) => outcome.failures.push(crate::IngestFailure {
                    path: item.path.clone(),
                    error: format!("{error:#}"),
                }),
            }
            if !on_progress(ImportProgress {
                stage,
                completed_items: index + 1,
                total_items: total,
                current_file: item.filename.clone(),
                completed_bytes,
                total_bytes,
            }) {
                outcome.cancelled = index + 1 < total;
                break;
            }
        }
        Ok(outcome)
    }

    /// Returns the bytes copied and stable asset id, or `None` when the copy was cancelled.
    fn import_one<F>(
        &self,
        project_id: &str,
        item: &PreflightItem,
        archive_root_id: Option<&str>,
        card: Option<&CardCandidate>,
        verify_copies: bool,
        mut on_copy_bytes: F,
    ) -> Result<Option<(u64, String)>>
    where
        F: FnMut(u64) -> bool,
    {
        let source = PathBuf::from(&item.path);
        if item.classification == Classification::KnownRelinkable {
            // Already-known content: registering where it is found is the whole
            // job, and it costs no copy.
            // Go through ingest rather than the Locate-only relink flow: this
            // smart-import path is allowed to reactivate a membership that was
            // rolled back, after first recording the freshly verified source.
            self.ingest_file(project_id, &source)?;
            let asset_id = self.asset_id_at_path(project_id, &source)?.ok_or_else(|| {
                anyhow::anyhow!("relinked asset was not found at {}", source.display())
            })?;
            return Ok(Some((0, asset_id)));
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
                    self.ingest_file(project_id, &source)?;
                    let asset_id =
                        self.asset_id_at_path(project_id, &source)?.ok_or_else(|| {
                            anyhow::anyhow!(
                                "reactivated asset was not found at {}",
                                source.display()
                            )
                        })?;
                    return Ok(Some((0, asset_id)));
                }
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
            // Referenced in place: the original stays exactly where it is.
            _ => (source, 0),
        };

        self.ingest_file(project_id, &path_to_register)?;
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
        if let (Some(root_id), Some(card)) = (archive_root_id, card)
            && bytes > 0
        {
            let relative = self.archive_relative_path(project_id, card, &item.relative_path)?;
            let relative = crate::RootRelativePath::parse(relative)
                .map_err(|error| anyhow::anyhow!("unusable archive path: {error}"))?;
            self.record_archive_location(&asset_id, root_id, &relative, &path_to_register)?;
        }
        Ok(Some((bytes, asset_id)))
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
mod tests {
    use super::*;
    use crate::tests::insert_asset_at;
    use std::fs;
    use tempfile::tempdir;

    fn sequence_hint(
        relative_path: &str,
        camera_key: &str,
        volume_hint: &str,
        folder: &str,
        prefix: &str,
        counter: Option<u64>,
        observed_seconds: Option<u64>,
    ) -> ImportSequenceCandidate {
        ImportSequenceCandidate {
            relative_path: relative_path.to_string(),
            camera_key: camera_key.to_string(),
            volume_hint: volume_hint.to_string(),
            folder: folder.to_string(),
            prefix: prefix.to_string(),
            counter,
            observed_millis: observed_seconds.map(|seconds| seconds * 1_000),
        }
    }

    fn proposal_evidence(
        candidates: Vec<ImportSequenceCandidate>,
    ) -> Vec<ImportSequenceBoundaryEvidence> {
        let (_, _, boundaries) = propose_import_runs(candidates);
        assert_eq!(boundaries.len(), 1);
        boundaries.into_iter().next().unwrap().evidence
    }

    #[test]
    fn sequence_detector_covers_every_frozen_boundary_kind() {
        let volume = proposal_evidence(vec![
            sequence_hint("A/C0001.MP4", "Sony", "card-a", "A", "C", Some(1), Some(1)),
            sequence_hint("B/C0002.MP4", "Sony", "card-b", "B", "C", Some(2), Some(2)),
        ]);
        assert!(matches!(
            volume.as_slice(),
            [ImportSequenceBoundaryEvidence::VolumeChange { .. }]
        ));

        let counter = proposal_evidence(vec![
            sequence_hint("A/C0241.MP4", "Sony", "card", "A", "C", Some(241), Some(1)),
            sequence_hint("A/C0001.MP4", "Sony", "card", "A", "C", Some(1), Some(2)),
        ]);
        assert!(counter.iter().any(|found| matches!(
            found,
            ImportSequenceBoundaryEvidence::CounterReset {
                previous: 241,
                next: 1
            }
        )));

        let folder = proposal_evidence(vec![
            sequence_hint("A/C0042.MP4", "Sony", "card", "A", "C", Some(42), Some(1)),
            sequence_hint("B/C0001.MP4", "Sony", "card", "B", "C", Some(1), Some(2)),
        ]);
        assert!(folder.iter().any(|found| matches!(
            found,
            ImportSequenceBoundaryEvidence::FolderReset { from, to }
                if from == "A" && to == "B"
        )));

        let prefix = proposal_evidence(vec![
            sequence_hint("A/C0001.MP4", "Camera", "card", "A", "C", Some(1), Some(1)),
            sequence_hint(
                "A/DJI_0002.MP4",
                "Camera",
                "card",
                "A",
                "DJI",
                Some(2),
                Some(2),
            ),
        ]);
        assert!(matches!(
            prefix.as_slice(),
            [ImportSequenceBoundaryEvidence::PrefixChange { .. }]
        ));

        let discontinuity = proposal_evidence(vec![
            sequence_hint("A/C0001.MP4", "Sony", "card", "A", "C", Some(1), Some(1)),
            sequence_hint(
                "A/C0002.MP4",
                "Sony",
                "card",
                "A",
                "C",
                Some(2),
                Some(1 + LARGE_CAPTURE_SEQUENCE_GAP_SECONDS),
            ),
        ]);
        assert!(matches!(
            discontinuity.as_slice(),
            [ImportSequenceBoundaryEvidence::LargeDiscontinuity { gap_seconds }]
                if *gap_seconds == LARGE_CAPTURE_SEQUENCE_GAP_SECONDS
        ));
    }

    #[test]
    fn sequence_proposals_are_deterministic_and_count_the_affected_run() {
        let candidates = vec![
            sequence_hint("A/C0001.MP4", "Sony", "card", "A", "C", Some(1), Some(1)),
            sequence_hint("A/C0002.MP4", "Sony", "card", "A", "C", Some(2), Some(2)),
            sequence_hint("B/C0001.MP4", "Sony", "card", "B", "C", Some(1), Some(3)),
            sequence_hint("B/C0002.MP4", "Sony", "card", "B", "C", Some(2), Some(4)),
        ];
        let mut reversed = candidates.clone();
        reversed.reverse();
        let (ordered, runs, boundaries) = propose_import_runs(candidates);
        let (reordered, reruns, reboundaries) = propose_import_runs(reversed);
        assert_eq!(ordered, reordered);
        assert_eq!(runs, reruns);
        assert_eq!(boundaries, reboundaries);
        assert_eq!(runs.len(), 2);
        assert_eq!(boundaries[0].affected_clip_count, 2);
        assert_eq!(boundaries[0].after_relative_path, "B/C0001.MP4");
    }

    #[test]
    fn import_preflight_surfaces_folder_and_counter_boundary_evidence() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let mount = directory.path().join("SONY_SD");
        let first_folder = mount.join("DCIM/100MSDCF");
        let second_folder = mount.join("DCIM/101MSDCF");
        fs::create_dir_all(&first_folder).unwrap();
        fs::create_dir_all(&second_folder).unwrap();
        let first = first_folder.join("C0241.MP4");
        let second = second_folder.join("C0001.MP4");
        fs::write(&first, b"first card run").unwrap();
        fs::write(&second, b"second card run").unwrap();
        let base = UNIX_EPOCH + std::time::Duration::from_secs(100);
        fs::File::open(&first)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(base))
            .unwrap();
        fs::File::open(&second)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new().set_modified(base + std::time::Duration::from_secs(1)),
            )
            .unwrap();

        let plan = catalog
            .preflight_import(&project.id, &mount, None, |_, _, _| true)
            .unwrap();

        assert_eq!(plan.sequence_preview.new_clip_count, 2);
        assert_eq!(plan.sequence_preview.proposed_sequence_count, 2);
        assert_eq!(plan.sequence_preview.suggested_new_sequence_count, 2);
        assert_eq!(plan.sequence_preview.boundaries.len(), 1);
        let boundary = &plan.sequence_preview.boundaries[0];
        assert_eq!(boundary.after_relative_path, "DCIM/101MSDCF/C0001.MP4");
        assert_eq!(boundary.affected_clip_count, 1);
        assert!(boundary.evidence.iter().any(|found| matches!(
            found,
            ImportSequenceBoundaryEvidence::CounterReset {
                previous: 241,
                next: 1
            }
        )));
        assert!(
            boundary
                .evidence
                .iter()
                .any(|found| matches!(found, ImportSequenceBoundaryEvidence::FolderReset { .. }))
        );
        assert_eq!(plan.sequence_preview.runs.len(), 2);
        assert_eq!(
            plan.sequence_preview.runs[1].decision,
            ImportSequenceRunDecision::ProposeNew
        );
        assert_eq!(plan.sequence_preview.runs[1].clip_count, 1);
        assert_eq!(
            plan.sequence_preview.runs[1].first_relative_path,
            "DCIM/101MSDCF/C0001.MP4"
        );
        assert_eq!(
            plan.sequence_preview.runs[1].last_relative_path,
            "DCIM/101MSDCF/C0001.MP4"
        );
        assert!(
            plan.sequence_preview.runs[1]
                .evidence
                .iter()
                .any(|found| matches!(found, ImportSequenceBoundaryEvidence::CounterReset { .. }))
        );
        assert!(catalog.list_assets(&project.id).unwrap().is_empty());
    }

    #[test]
    fn import_preflight_names_the_existing_sequence_a_run_would_extend() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let mount = card_fixture(
            directory.path(),
            &[("C0101.MP4", b"one"), ("C0102.MP4", b"two")],
        );
        let card = inspect_card(&mount).unwrap().unwrap();
        let existing = catalog
            .declare_capture_sequence(
                &project.id,
                "Sony (C-series)",
                "Sony SD-A",
                &card.session_id,
                "C",
            )
            .unwrap();
        let operations_before = catalog.list_operations(&project.id).unwrap().len();

        let plan = catalog
            .preflight_import(&project.id, &mount, None, |_, _, _| true)
            .unwrap();

        assert_eq!(plan.sequence_preview.runs.len(), 1);
        let run = &plan.sequence_preview.runs[0];
        assert_eq!(run.decision, ImportSequenceRunDecision::ExtendExisting);
        assert_eq!(
            run.matched_existing_sequence_id.as_deref(),
            Some(existing.id.as_str())
        );
        assert_eq!(
            run.matched_existing_sequence_label.as_deref(),
            Some("Sony SD-A")
        );
        assert_eq!(run.compared_existing_sequence_id, None);
        assert_eq!(run.first_relative_path, "DCIM/100MSDCF/C0101.MP4");
        assert_eq!(run.last_relative_path, "DCIM/100MSDCF/C0102.MP4");
        assert_eq!(run.clip_count, 2);
        assert_eq!(run.camera_key, "Sony (C-series)");
        assert!(run.reason.contains("Sony SD-A"));
        // A clean extension is routine: nothing here needs a second look.
        assert!(!run.needs_review);
        assert_eq!(
            catalog.list_operations(&project.id).unwrap().len(),
            operations_before
        );
        assert!(catalog.list_assets(&project.id).unwrap().is_empty());
    }

    #[test]
    fn import_preflight_explains_volume_and_prefix_mismatch_against_a_named_sequence() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let mount = card_fixture(directory.path(), &[("C0101.MP4", b"new card")]);
        let card = inspect_card(&mount).unwrap().unwrap();
        let first_existing = catalog
            .declare_capture_sequence(
                &project.id,
                "Sony (C-series)",
                "Sony SD-A",
                "card_previous_session",
                "SONY",
            )
            .unwrap();
        let second_existing = catalog
            .declare_capture_sequence(
                &project.id,
                "Sony (C-series)",
                "Sony SD-B",
                "card_other_session",
                "CX",
            )
            .unwrap();
        let compared = if first_existing.id < second_existing.id {
            &first_existing
        } else {
            &second_existing
        };
        let operations_before = catalog.list_operations(&project.id).unwrap().len();

        let plan = catalog
            .preflight_import(&project.id, &mount, None, |_, _, _| true)
            .unwrap();

        let run = &plan.sequence_preview.runs[0];
        assert_eq!(run.decision, ImportSequenceRunDecision::ProposeNew);
        assert_eq!(run.matched_existing_sequence_id, None);
        assert_eq!(
            run.compared_existing_sequence_id.as_deref(),
            Some(compared.id.as_str())
        );
        assert_eq!(
            run.compared_existing_sequence_label.as_deref(),
            Some(compared.label.as_str())
        );
        assert!(run.evidence.iter().any(|found| matches!(
            found,
            ImportSequenceBoundaryEvidence::VolumeChange { from, to }
                if from == &compared.volume_hint && to == &card.session_id
        )));
        assert!(run.evidence.iter().any(|found| matches!(
            found,
            ImportSequenceBoundaryEvidence::PrefixChange { from, to }
                if from == &compared.filename_prefix && to == "C"
        )));
        assert!(run.reason.contains(&compared.label));
        // Card/filename evidence actually disagrees with the compared
        // sequence — this is the one case that genuinely needs a look.
        assert!(run.needs_review);
        assert_eq!(plan.sequence_preview.matched_existing_sequence_count, 0);
        assert_eq!(plan.sequence_preview.suggested_new_sequence_count, 1);
        assert_eq!(
            catalog.list_operations(&project.id).unwrap().len(),
            operations_before
        );
        assert!(catalog.list_assets(&project.id).unwrap().is_empty());
    }

    #[test]
    fn import_preflight_does_not_flag_a_second_run_that_only_lost_the_slot() {
        // Two runs of the same camera on one card, split by a folder/counter
        // reset — the same fixture shape as
        // `import_preflight_surfaces_folder_and_counter_boundary_evidence`,
        // but with a matching existing sequence declared first. Both runs
        // agree with it (empty volume/prefix hints match anything), so the
        // first one extends it and the second — which agrees just as much,
        // but lost the slot to the first — must not be flagged as a
        // condition needing review. It is a routine multi-run outcome, not a
        // disagreement with the sequence's evidence.
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let existing = catalog
            .declare_capture_sequence(&project.id, "Sony (C-series)", "Sony SD-A", "", "")
            .unwrap();
        let mount = directory.path().join("SONY_SD");
        let first_folder = mount.join("DCIM/100MSDCF");
        let second_folder = mount.join("DCIM/101MSDCF");
        fs::create_dir_all(&first_folder).unwrap();
        fs::create_dir_all(&second_folder).unwrap();
        let first = first_folder.join("C0241.MP4");
        let second = second_folder.join("C0001.MP4");
        fs::write(&first, b"first card run").unwrap();
        fs::write(&second, b"second card run").unwrap();
        let base = UNIX_EPOCH + std::time::Duration::from_secs(100);
        fs::File::open(&first)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(base))
            .unwrap();
        fs::File::open(&second)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new().set_modified(base + std::time::Duration::from_secs(1)),
            )
            .unwrap();

        let plan = catalog
            .preflight_import(&project.id, &mount, None, |_, _, _| true)
            .unwrap();

        assert_eq!(plan.sequence_preview.runs.len(), 2);
        let extended = &plan.sequence_preview.runs[0];
        assert_eq!(extended.decision, ImportSequenceRunDecision::ExtendExisting);
        assert_eq!(
            extended.matched_existing_sequence_id.as_deref(),
            Some(existing.id.as_str())
        );
        assert!(!extended.needs_review);

        let displaced = &plan.sequence_preview.runs[1];
        assert_eq!(displaced.decision, ImportSequenceRunDecision::ProposeNew);
        assert_eq!(
            displaced.compared_existing_sequence_id.as_deref(),
            Some(existing.id.as_str())
        );
        assert!(displaced.reason.contains("already extends"));
        // The core message: agreeing with a sequence that another run in the
        // same batch already claimed is routine, not a disagreement.
        assert!(!displaced.needs_review);
        assert!(catalog.list_assets(&project.id).unwrap().is_empty());
    }

    /// A card holding real, probeable media. Import creates assets, which runs
    /// ffprobe, so byte blobs will not do here.
    fn card_fixture_media(root: &Path, clips: &[(&str, &str)]) -> PathBuf {
        let mount = root.join("DJI_SD");
        let layout = mount.join("DCIM/100MSDCF");
        fs::create_dir_all(&layout).unwrap();
        for (name, color) in clips {
            let status = crate::tool_command("ffmpeg")
                .args([
                    "-y",
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("color=c={color}:s=64x64:r=5:d=1"),
                    "-c:v",
                    "mpeg4",
                ])
                .arg(layout.join(name))
                .status()
                .unwrap();
            assert!(status.success(), "failed to build fixture {name}");
        }
        mount
    }

    fn card_fixture(root: &Path, clips: &[(&str, &[u8])]) -> PathBuf {
        let mount = root.join("DJI_SD");
        let layout = mount.join("DCIM/100MSDCF");
        fs::create_dir_all(&layout).unwrap();
        for (name, bytes) in clips {
            fs::write(layout.join(name), bytes).unwrap();
        }
        mount
    }

    /// The number the dialog leads with, and the states behind it.
    #[test]
    fn preflight_separates_new_from_known_and_from_lost() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let mount = card_fixture(
            directory.path(),
            &[
                ("C0001.MP4", b"already imported and reachable"),
                ("C0002.MP4", b"already imported but gone missing"),
                ("C0003.MP4", b"brand new footage"),
            ],
        );
        let layout = mount.join("DCIM/100MSDCF");

        // One clip is known and still reachable on the card itself.
        insert_asset_at(
            &catalog,
            &project.id,
            "asset_here",
            &layout.join("C0001.MP4"),
        );
        // One is known but its original lived somewhere that is now gone.
        let lost = directory.path().join("old-drive/C0002.MP4");
        fs::create_dir_all(lost.parent().unwrap()).unwrap();
        fs::copy(layout.join("C0002.MP4"), &lost).unwrap();
        insert_asset_at(&catalog, &project.id, "asset_lost", &lost);
        fs::remove_file(&lost).unwrap();

        let preflight = catalog
            .preflight_import(&project.id, &mount, None, |_, _, _| true)
            .unwrap();

        assert_eq!(preflight.scanned, 3);
        assert_eq!(preflight.new_clips, 1);
        assert_eq!(preflight.relinkable, 1);
        // The known reachable clip and the new clip both need a proxy after import.
        assert_eq!(preflight.missing_proxies, 2);
        assert_eq!(preflight.unreadable, 0);
        assert_eq!(preflight.sequence_preview.new_clip_count, 1);
        assert_eq!(preflight.sequence_preview.proposed_sequence_count, 1);
        assert_eq!(preflight.sequence_preview.suggested_new_sequence_count, 1);
        assert_eq!(
            preflight
                .card
                .as_ref()
                .map(|card| card.camera_hint.as_str()),
            Some("Sony")
        );

        let relinkable = preflight
            .items
            .iter()
            .find(|item| item.classification == Classification::KnownRelinkable)
            .unwrap();
        assert_eq!(relinkable.asset_id.as_deref(), Some("asset_lost"));
        assert_eq!(relinkable.relative_path, "DCIM/100MSDCF/C0002.MP4");
    }

    /// A preview that half-imports is not a preview.
    #[test]
    fn preflight_leaves_the_catalog_untouched() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let mount = card_fixture(directory.path(), &[("C0001.MP4", b"new footage")]);

        let before = catalog.list_assets(&project.id).unwrap().len();
        let operations_before = catalog.list_operations(&project.id).unwrap().len();

        catalog
            .preflight_import(&project.id, &mount, None, |_, _, _| true)
            .unwrap();

        assert_eq!(catalog.list_assets(&project.id).unwrap().len(), before);
        assert_eq!(
            catalog.list_operations(&project.id).unwrap().len(),
            operations_before
        );
    }

    /// The dialog must be able to say where footage is going without the user
    /// inferring it.
    #[test]
    fn a_card_destination_names_the_root_and_the_resolved_path() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let mount = card_fixture(directory.path(), &[("C0001.MP4", b"new footage")]);
        let archive = directory.path().join("nas/VideoArchive");
        fs::create_dir_all(&archive).unwrap();
        let root = catalog.create_archive_root("NAS Video", &archive).unwrap();

        let preflight = catalog
            .preflight_import(&project.id, &mount, Some(&root.id), |_, _, _| true)
            .unwrap();

        let destination = &preflight.destination;
        assert!(destination.copies);
        assert!(destination.ready, "{}", destination.blocked_reason);
        assert_eq!(
            destination.archive_root.as_ref().unwrap().display_name,
            "NAS Video"
        );
        assert!(destination.resolved_path.contains("Spain 2026"));
        assert!(destination.resolved_path.contains("DJI_SD-"));
        assert_eq!(destination.bytes_to_copy, "new footage".len() as u64);
        assert!(destination.summary.contains("stay on the card too"));
    }

    #[test]
    fn referencing_in_place_says_so_and_says_where() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let folder = directory.path().join("nas-share");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("C0001.MP4"), b"nas footage").unwrap();

        let preflight = catalog
            .preflight_import(&project.id, &folder, None, |_, _, _| true)
            .unwrap();

        assert!(!preflight.destination.copies);
        assert_eq!(preflight.destination.bytes_to_copy, 0);
        assert!(
            preflight
                .destination
                .summary
                .contains("stay where they are")
        );
        assert!(
            preflight.destination.resolved_path.contains("nas-share"),
            "the dialog must name the folder, not just the policy"
        );
    }

    /// A copy that will not fit is refused before it starts, not halfway through.
    #[test]
    fn an_unreachable_or_full_destination_blocks_the_import() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let mount = card_fixture(directory.path(), &[("C0001.MP4", b"footage")]);
        let archive = directory.path().join("shuttle");
        fs::create_dir_all(&archive).unwrap();
        let root = catalog
            .create_archive_root("Shuttle SSD", &archive)
            .unwrap();

        fs::remove_dir_all(&archive).unwrap();
        let preflight = catalog
            .preflight_import(&project.id, &mount, Some(&root.id), |_, _, _| true)
            .unwrap();
        assert!(!preflight.destination.ready);
        assert!(
            preflight
                .destination
                .blocked_reason
                .contains("not connected"),
            "{}",
            preflight.destination.blocked_reason
        );
    }

    /// An archived original is never overwritten by a card that reuses its name.
    #[test]
    fn an_occupied_archive_path_is_reported_rather_than_overwritten() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let mount = card_fixture(directory.path(), &[("C0001.MP4", b"reshot footage")]);
        let archive = directory.path().join("nas");
        fs::create_dir_all(&archive).unwrap();
        let root = catalog.create_archive_root("NAS Video", &archive).unwrap();

        // Something already sits exactly where this clip would land.
        let card = inspect_card(&mount).unwrap().unwrap();
        let target = catalog
            .archive_target(&project.id, &root.id, &card, "DCIM/100MSDCF/C0001.MP4")
            .unwrap();
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"different bytes entirely").unwrap();

        let preflight = catalog
            .preflight_import(&project.id, &mount, Some(&root.id), |_, _, _| true)
            .unwrap();
        assert_eq!(preflight.collisions, 1);
        assert_eq!(preflight.new_clips, 0);
    }

    /// The headline workflow: insert a card, click once, and get verified copies
    /// in the archive — then insert it again with more footage and copy only the
    /// new clips.
    #[test]
    fn a_card_imports_once_and_then_incrementally() {
        // Creating an asset probes the media, so this needs the real toolchain.
        if crate::tool_command("ffprobe")
            .arg("-version")
            .output()
            .is_err()
        {
            eprintln!("ffprobe is unavailable; skipping media integration test");
            return;
        }

        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let mount = card_fixture_media(
            directory.path(),
            &[("C0001.MP4", "navy"), ("C0002.MP4", "maroon")],
        );
        let archive = directory.path().join("nas/VideoArchive");
        fs::create_dir_all(&archive).unwrap();
        let root = catalog.create_archive_root("NAS Video", &archive).unwrap();

        let plan = catalog
            .preflight_import(&project.id, &mount, Some(&root.id), |_, _, _| true)
            .unwrap();
        assert_eq!(plan.new_clips, 2);

        let outcome = catalog
            .run_import(&project.id, &plan, true, |_| true)
            .unwrap();
        assert_eq!(outcome.imported, 2);
        assert_eq!(outcome.imported_asset_ids.len(), 2);
        assert!(outcome.relinked_asset_ids.is_empty());
        assert_eq!(outcome.copied, 2);
        assert!(outcome.bytes_copied > 0);
        assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);

        // Originals landed under the archive root, keeping the camera's own
        // folder structure, and the card still has them.
        let card = plan.card.as_ref().unwrap();
        let archived = archive
            .join("Spain 2026")
            .join(crate::cards::card_session_slug(card))
            .join("DCIM/100MSDCF/C0001.MP4");
        assert_eq!(
            fs::read(&archived).unwrap(),
            fs::read(mount.join("DCIM/100MSDCF/C0001.MP4")).unwrap(),
            "the archived original must be byte-identical to the card's"
        );
        assert!(mount.join("DCIM/100MSDCF/C0001.MP4").is_file());

        // Assets point at the archive copy, not the card, so ejecting is safe.
        for asset in catalog.list_assets(&project.id).unwrap() {
            assert!(asset.path.starts_with(archive.to_string_lossy().as_ref()));
            assert!(asset.original_available);
        }

        // Shoot more footage and reinsert the same card.
        let extra = card_fixture_media(&directory.path().join("more"), &[("C0003.MP4", "olive")]);
        fs::copy(
            extra.join("DCIM/100MSDCF/C0003.MP4"),
            mount.join("DCIM/100MSDCF/C0003.MP4"),
        )
        .unwrap();
        let second_plan = catalog
            .preflight_import(&project.id, &mount, Some(&root.id), |_, _, _| true)
            .unwrap();
        assert_eq!(second_plan.scanned, 3);
        assert_eq!(second_plan.new_clips, 1, "only the new clip is new");
        assert!(
            second_plan.destination.bytes_to_copy > 0,
            "the second import must state the cost of only the new clip"
        );

        let second = catalog
            .run_import(&project.id, &second_plan, true, |_| true)
            .unwrap();
        assert_eq!(second.imported, 1);
        assert_eq!(second.imported_asset_ids.len(), 1);
        assert_eq!(second.copied, 1, "already-archived clips are not re-copied");
        assert_eq!(catalog.list_assets(&project.id).unwrap().len(), 3);
    }

    /// Importing must not start a copy it cannot finish.
    #[test]
    fn an_import_refuses_to_run_against_a_blocked_destination() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let mount = card_fixture(directory.path(), &[("C0001.MP4", b"footage")]);
        let archive = directory.path().join("shuttle");
        fs::create_dir_all(&archive).unwrap();
        let root = catalog
            .create_archive_root("Shuttle SSD", &archive)
            .unwrap();

        let plan = catalog
            .preflight_import(&project.id, &mount, Some(&root.id), |_, _, _| true)
            .unwrap();
        fs::remove_dir_all(&archive).unwrap();
        let replanned = catalog
            .preflight_import(&project.id, &mount, Some(&root.id), |_, _, _| true)
            .unwrap();

        assert!(
            catalog
                .run_import(&project.id, &replanned, true, |_| true)
                .is_err()
        );
        assert!(catalog.list_assets(&project.id).unwrap().is_empty());
        drop(plan);
    }

    /// Cancelling stops cleanly, and re-running picks up where it left off.
    #[test]
    fn a_cancelled_import_resumes_without_duplicating_work() {
        // Creating an asset probes the media, so this needs the real toolchain.
        if crate::tool_command("ffprobe")
            .arg("-version")
            .output()
            .is_err()
        {
            eprintln!("ffprobe is unavailable; skipping media integration test");
            return;
        }

        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let mount = card_fixture_media(
            directory.path(),
            &[
                ("C0001.MP4", "navy"),
                ("C0002.MP4", "maroon"),
                ("C0003.MP4", "green"),
            ],
        );
        let archive = directory.path().join("nas");
        fs::create_dir_all(&archive).unwrap();
        let root = catalog.create_archive_root("NAS Video", &archive).unwrap();

        let plan = catalog
            .preflight_import(&project.id, &mount, Some(&root.id), |_, _, _| true)
            .unwrap();
        let outcome = catalog
            .run_import(&project.id, &plan, true, |progress| {
                progress.completed_items < 2
            })
            .unwrap();
        assert!(outcome.cancelled);
        assert_eq!(outcome.imported, 2);

        // Re-planning sees only what is left.
        let resumed_plan = catalog
            .preflight_import(&project.id, &mount, Some(&root.id), |_, _, _| true)
            .unwrap();
        assert_eq!(resumed_plan.new_clips, 1);
        let resumed = catalog
            .run_import(&project.id, &resumed_plan, true, |_| true)
            .unwrap();
        assert_eq!(resumed.imported, 1);
        assert_eq!(catalog.list_assets(&project.id).unwrap().len(), 3);
    }

    /// Importing a card that holds a clip the project lost track of reconnects
    /// it instead of importing a second copy.
    #[test]
    fn importing_reconnects_lost_originals_rather_than_duplicating_them() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let mount = card_fixture(directory.path(), &[("C0001.MP4", b"the lost clip")]);

        let gone = directory.path().join("old-drive/C0001.MP4");
        fs::create_dir_all(gone.parent().unwrap()).unwrap();
        fs::copy(mount.join("DCIM/100MSDCF/C0001.MP4"), &gone).unwrap();
        insert_asset_at(&catalog, &project.id, "asset_lost", &gone);
        fs::remove_file(&gone).unwrap();

        let plan = catalog
            .preflight_import(&project.id, &mount, None, |_, _, _| true)
            .unwrap();
        assert_eq!(plan.relinkable, 1);
        assert_eq!(plan.new_clips, 0);

        let outcome = catalog
            .run_import(&project.id, &plan, true, |_| true)
            .unwrap();
        assert_eq!(outcome.relinked, 1);
        assert_eq!(outcome.imported, 0);
        assert_eq!(outcome.relinked_asset_ids, ["asset_lost"]);
        assert!(outcome.imported_asset_ids.is_empty());
        assert_eq!(catalog.list_assets(&project.id).unwrap().len(), 1);
        assert!(catalog.get_asset("asset_lost").unwrap().original_available);
    }

    #[test]
    fn reference_import_reactivates_a_rolled_back_asset_at_its_current_location() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let mount = card_fixture(directory.path(), &[("C0001.MP4", b"returned footage")]);
        let current = mount.join("DCIM/100MSDCF/C0001.MP4");
        let old = directory.path().join("old-drive/C0001.MP4");
        fs::create_dir_all(old.parent().unwrap()).unwrap();
        fs::copy(&current, &old).unwrap();
        insert_asset_at(&catalog, &project.id, "asset_rolled_back", &old);
        catalog
            .remove_project_asset_membership(&project.id, &["asset_rolled_back".to_string()])
            .unwrap();
        fs::remove_file(&old).unwrap();

        let plan = catalog
            .preflight_import(&project.id, &mount, None, |_, _, _| true)
            .unwrap();
        assert_eq!(plan.new_clips, 1);
        assert_eq!(plan.items[0].asset_id.as_deref(), Some("asset_rolled_back"));

        let outcome = catalog
            .run_import(&project.id, &plan, true, |_| true)
            .unwrap();
        assert_eq!(outcome.imported_asset_ids, ["asset_rolled_back"]);
        assert_eq!(catalog.list_assets(&project.id).unwrap().len(), 1);
        let asset = catalog.get_asset("asset_rolled_back").unwrap();
        assert_eq!(asset.path, current.to_string_lossy());
        assert!(asset.original_available);
    }

    #[test]
    fn locate_does_not_reactivate_a_rolled_back_membership() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let mount = card_fixture(directory.path(), &[("C0001.MP4", b"returned footage")]);
        let current = mount.join("DCIM/100MSDCF/C0001.MP4");
        insert_asset_at(&catalog, &project.id, "asset_rolled_back", &current);
        catalog
            .remove_project_asset_membership(&project.id, &["asset_rolled_back".to_string()])
            .unwrap();

        let report = catalog
            .relink_folder(&project.id, &mount, |_, _, _| {})
            .unwrap();
        assert_eq!(report.repaired, 0);
        assert_eq!(report.unrecognized, 1);
        assert!(catalog.list_assets(&project.id).unwrap().is_empty());
    }

    #[test]
    fn archive_import_reactivates_and_continues_a_rolled_back_asset_without_a_duplicate() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let mount = card_fixture(directory.path(), &[("C0001.MP4", b"returned footage")]);
        let current = mount.join("DCIM/100MSDCF/C0001.MP4");
        let old = directory.path().join("old-drive/C0001.MP4");
        fs::create_dir_all(old.parent().unwrap()).unwrap();
        fs::copy(&current, &old).unwrap();
        insert_asset_at(&catalog, &project.id, "asset_rolled_back", &old);
        catalog
            .remove_project_asset_membership(&project.id, &["asset_rolled_back".to_string()])
            .unwrap();
        fs::remove_file(&old).unwrap();

        let archive = directory.path().join("archive");
        fs::create_dir_all(&archive).unwrap();
        let root = catalog.create_archive_root("Archive", &archive).unwrap();
        let plan = catalog
            .preflight_import(&project.id, &mount, Some(&root.id), |_, _, _| true)
            .unwrap();
        assert_eq!(plan.new_clips, 1, "an absent archive copy must be created");

        let first = catalog
            .run_import(&project.id, &plan, true, |_| true)
            .unwrap();
        assert_eq!(first.imported_asset_ids, ["asset_rolled_back"]);
        assert_eq!(first.copied, 1);
        let asset = catalog.get_asset("asset_rolled_back").unwrap();
        assert!(asset.path.starts_with(archive.to_string_lossy().as_ref()));
        assert!(asset.original_available);
        assert_eq!(catalog.list_assets(&project.id).unwrap().len(), 1);
        let archive_binding: Option<String> = catalog
            .connection()
            .unwrap()
            .query_row(
                "SELECT archive_root_id FROM project_asset_local_locations WHERE asset_id = ?1 AND local_path = ?2",
                rusqlite::params!["asset_rolled_back", asset.path],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(archive_binding.as_deref(), Some(root.id.as_str()));

        // The rollback leaves archive bytes in place. A later smart-import run
        // reuses that matching copy without overwriting it, records the
        // currently mounted source, and makes the new membership rollbackable.
        catalog
            .remove_project_asset_membership(&project.id, &["asset_rolled_back".to_string()])
            .unwrap();
        let matching_plan = catalog
            .preflight_import(&project.id, &mount, Some(&root.id), |_, _, _| true)
            .unwrap();
        let archive_path = PathBuf::from(&asset.path);
        let mut target_replaced = false;
        let changed_after_preflight = catalog
            .run_import(&project.id, &matching_plan, true, |_| {
                if !target_replaced {
                    fs::write(&archive_path, b"replaced footage").unwrap();
                    target_replaced = true;
                }
                true
            })
            .unwrap();
        assert_eq!(changed_after_preflight.failures.len(), 1);
        assert!(
            changed_after_preflight.failures[0]
                .error
                .contains("archive target already exists with different content")
        );
        assert!(catalog.list_assets(&project.id).unwrap().is_empty());
        fs::write(&archive_path, b"returned footage").unwrap();

        let original_mtime = fs::metadata(&current).unwrap().modified().unwrap();
        // The card session anchor contains pathname, size, and mtime. Preserve
        // those values while changing the bytes to prove the archive target is
        // protected even when a reused pathname looks like the same card.
        fs::write(&current, b"replaced footage").unwrap();
        fs::File::open(&current)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(original_mtime))
            .unwrap();
        let collision = catalog
            .preflight_import(&project.id, &mount, Some(&root.id), |_, _, _| true)
            .unwrap();
        assert_eq!(collision.collisions, 1);
        assert_eq!(collision.new_clips, 0);
        assert_eq!(collision.relinkable, 0);
        assert!(
            catalog.list_assets(&project.id).unwrap().is_empty(),
            "different bytes at a reused pathname must not reactivate the old asset"
        );

        fs::write(&current, b"returned footage").unwrap();
        fs::File::open(&current)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(original_mtime))
            .unwrap();
        let continued = catalog
            .preflight_import(&project.id, &mount, Some(&root.id), |_, _, _| true)
            .unwrap();
        assert_eq!(continued.new_clips, 1);
        assert_eq!(continued.relinkable, 0);
        assert_eq!(continued.collisions, 0);
        let resumed = catalog
            .run_import(&project.id, &continued, true, |_| true)
            .unwrap();
        assert_eq!(resumed.imported_asset_ids, ["asset_rolled_back"]);
        assert_eq!(resumed.copied, 0);
        assert_eq!(catalog.list_assets(&project.id).unwrap().len(), 1);
        assert!(
            catalog
                .get_asset("asset_rolled_back")
                .unwrap()
                .original_available
        );

        let current_location_count: i64 = catalog
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM project_asset_local_locations WHERE asset_id = ?1 AND local_path = ?2 AND verification_state = 'verified'",
                rusqlite::params!["asset_rolled_back", current.to_string_lossy()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(current_location_count, 1);

        let session = catalog
            .record_import_session(&project.id, &continued, &resumed)
            .unwrap();
        let preview = catalog
            .import_rollback_preview(&project.id, &session.entry.id)
            .unwrap();
        assert_eq!(preview.removable_asset_count, 1);
        catalog
            .rollback_import_membership(&project.id, &session.entry.id)
            .unwrap();
        assert!(
            catalog.list_assets(&project.id).unwrap().is_empty(),
            "the reactivated membership belongs to this new import session"
        );
    }

    #[test]
    fn byte_sizes_render_for_a_person() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(62_400_000_000), "62.4 GB");
        assert_eq!(human_bytes(1_200_000_000_000), "1.2 TB");
    }
}
