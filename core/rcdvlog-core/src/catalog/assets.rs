//! Ingest, relink, and asset reads/updates.

use crate::analysis::vlm_store::set_title_and_description;
use crate::*;
use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{OptionalExtension, params};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;
use walkdir::WalkDir;

/// What ingest did with one candidate file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IngestOutcome {
    /// New content; an asset was created.
    Imported,
    /// Already known, and already resolvable somewhere. The discovered location
    /// is recorded, but the asset was left alone.
    Duplicate,
    /// Already known, but its original was offline. The asset now points here.
    Relinked,
}

impl Catalog {
    pub fn ingest_folder(&self, project_id: &str, root: impl AsRef<Path>) -> Result<IngestReport> {
        self.ingest_folder_with_progress(project_id, root, |_, _, _| {})
    }

    pub fn ingest_folder_with_progress<F>(
        &self,
        project_id: &str,
        root: impl AsRef<Path>,
        on_progress: F,
    ) -> Result<IngestReport>
    where
        F: FnMut(usize, usize, &Path),
    {
        self.ingest_folder_with_camera_and_progress(project_id, root, None, on_progress)
    }

    /// Imports a folder with an explicit camera identity chosen by the user.
    /// The override is applied consistently to every newly ingested asset.
    pub fn ingest_folder_with_camera<F>(
        &self,
        project_id: &str,
        root: impl AsRef<Path>,
        camera: &str,
        on_progress: F,
    ) -> Result<IngestReport>
    where
        F: FnMut(usize, usize, &Path),
    {
        let camera = camera.trim();
        if camera.is_empty() {
            bail!("camera override must not be empty");
        }
        self.ingest_folder_with_camera_and_progress(project_id, root, Some(camera), on_progress)
    }

    pub(crate) fn ingest_folder_with_camera_and_progress<F>(
        &self,
        project_id: &str,
        root: impl AsRef<Path>,
        camera_override: Option<&str>,
        mut on_progress: F,
    ) -> Result<IngestReport>
    where
        F: FnMut(usize, usize, &Path),
    {
        self.require_project(project_id)?;
        let root = root.as_ref();
        if !root.is_dir() {
            bail!("ingest path is not a directory: {}", root.display());
        }

        let candidates: Vec<PathBuf> = WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.into_path())
            .filter(|path| is_media(path))
            .collect();

        let mut report = IngestReport {
            scanned: candidates.len(),
            imported: 0,
            duplicates: 0,
            relinked: 0,
            failures: Vec::new(),
        };

        let total = candidates.len();
        for (index, path) in candidates.into_iter().enumerate() {
            match match camera_override {
                Some(camera) => self.ingest_file_with_camera(project_id, &path, Some(camera)),
                None => self.ingest_file(project_id, &path),
            } {
                Ok(IngestOutcome::Imported) => report.imported += 1,
                Ok(IngestOutcome::Duplicate) => report.duplicates += 1,
                Ok(IngestOutcome::Relinked) => report.relinked += 1,
                Err(error) => {
                    // The transient report is not enough: a rejected file must
                    // still be discoverable after the run, and after relaunch
                    // (D01-T027). Sidecars remain folder-deduped, but each
                    // actual media candidate retains its own failure details.
                    let folder = path
                        .parent()
                        .map(|parent| parent.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let filename = path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    // Recording a diagnostic must never itself fail an ingest.
                    let reason = format!("{error:#}");
                    let _ = if reason.contains("no video stream") {
                        self.record_no_video_stream(project_id, &folder, &filename, &reason)
                    } else if reason.contains("video stream") {
                        self.record_invalid_video_stream(project_id, &folder, &filename, &reason)
                    } else if filename.starts_with("._") {
                        self.record_unreadable_media(project_id, &folder, &filename, &reason)
                    } else {
                        self.record_unreadable_media_file(project_id, &folder, &filename, &reason)
                    };
                    report.failures.push(IngestFailure {
                        path: path.to_string_lossy().into_owned(),
                        error: format!("{error:#}"),
                    });
                }
            }
            on_progress(index + 1, total, &path);
        }
        Ok(report)
    }

    /// Finds originals this project has lost track of, somewhere the user points
    /// us at.
    ///
    /// Deliberately not `ingest_folder`: locating moved media must never quietly
    /// enlarge the project. Content this project does not already know is
    /// counted and left alone, so pointing at the wrong folder is a no-op the
    /// user can read rather than an import they have to undo.
    ///
    /// Matching is by content. Filenames and folder structure are not evidence —
    /// two cards legitimately both contain `C0001.MP4`.
    pub fn relink_folder<F>(
        &self,
        project_id: &str,
        root: impl AsRef<Path>,
        mut on_progress: F,
    ) -> Result<RelinkReport>
    where
        F: FnMut(usize, usize, &Path),
    {
        self.require_project(project_id)?;
        let root = root.as_ref();
        if !root.is_dir() {
            bail!("search path is not a directory: {}", root.display());
        }

        let candidates: Vec<PathBuf> = WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.into_path())
            .filter(|path| is_media(path))
            .collect();

        let missing_before = self.offline_asset_count(project_id)?;
        let total = candidates.len();
        let mut report = RelinkReport {
            scanned: total,
            repaired: 0,
            already_online: 0,
            unrecognized: 0,
            still_missing: missing_before,
            failures: Vec::new(),
        };

        for (index, path) in candidates.into_iter().enumerate() {
            match self.relink_file(project_id, &path) {
                Ok(IngestOutcome::Relinked) => report.repaired += 1,
                Ok(IngestOutcome::Duplicate) => report.already_online += 1,
                Ok(IngestOutcome::Imported) => report.unrecognized += 1,
                Err(error) => report.failures.push(IngestFailure {
                    path: path.to_string_lossy().into_owned(),
                    error: format!("{error:#}"),
                }),
            }
            on_progress(index + 1, total, &path);
        }

        report.still_missing = self.offline_asset_count(project_id)?;
        // Whatever is reachable again should stop being reported as offline, and
        // whatever is still missing should be restated with its new count.
        self.resolve_offline_volume_diagnostics(project_id)?;
        Ok(report)
    }

    /// Matches one file against this project's assets without ever creating one.
    /// `Imported` here means "not recognized", which is the caller's cue to
    /// count it rather than act on it.
    pub(crate) fn relink_file(&self, project_id: &str, path: &Path) -> Result<IngestOutcome> {
        let metadata =
            fs::metadata(path).with_context(|| format!("read metadata for {}", path.display()))?;
        let fingerprint = sampled_fingerprint(path, metadata.len())?;
        let known: Option<(String, String, bool)> = self
            .connection()?
            .query_row(
                "SELECT id, path, COALESCE(membership_removed, 0) FROM assets WHERE project_id = ?1 AND fingerprint = ?2",
                params![project_id, fingerprint],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        match known {
            Some((asset_id, recorded_path, membership_removed)) => {
                if membership_removed {
                    return Ok(IngestOutcome::Imported);
                }
                self.relink_known_asset(&asset_id, &recorded_path, path, &fingerprint)
            }
            None => Ok(IngestOutcome::Imported),
        }
    }

    pub(crate) fn offline_asset_count(&self, project_id: &str) -> Result<usize> {
        Ok(self
            .list_assets(project_id)?
            .iter()
            .filter(|asset| !asset.original_available)
            .count())
    }

    /// Re-states which volumes are offline after a repair pass.
    ///
    /// Diagnostics are acknowledged rather than deleted, per this module's
    /// "acknowledge, don't delete" rule: a volume that comes back and later goes
    /// away again resurfaces with its history intact.
    pub(crate) fn resolve_offline_volume_diagnostics(&self, project_id: &str) -> Result<()> {
        let still_offline = self.detect_offline_volumes(project_id)?;
        let unresolved: Vec<&str> = still_offline
            .iter()
            .map(|diagnostic| diagnostic.dedupe_key.as_str())
            .collect();
        let existing = self.diagnostics(project_id, &DiagnosticFilter::default())?;
        for diagnostic in existing {
            if diagnostic.kind == "media.volume_offline"
                && !unresolved.contains(&diagnostic.dedupe_key.as_str())
            {
                self.acknowledge_diagnostic(project_id, &diagnostic.id)?;
            }
        }
        Ok(())
    }

    pub(crate) fn ingest_file(&self, project_id: &str, path: &Path) -> Result<IngestOutcome> {
        self.ingest_file_with_camera(project_id, path, None)
    }

    /// Same as `ingest_file`, except a genuinely new asset's camera is
    /// `camera_override` when given, instead of `probe_media`'s own
    /// EXIF-then-filename-pattern guess. This is the other half of #71's
    /// "the selected identity is used by both asset ingest and
    /// capture-sequence preflight" requirement: `preflight_import` resolves
    /// one identity into `ImportPreflight.camera_identity`, and `run_import`
    /// passes that same string here so the asset this creates and the
    /// capture sequence `materialize_asset_add` files it under (keyed on
    /// `payload.camera`, see `operations.rs`) can never disagree with what
    /// the dialog showed. Has no effect on already-known content: a relink
    /// never re-probes or re-assigns a camera.
    pub(crate) fn ingest_file_with_camera(
        &self,
        project_id: &str,
        path: &Path,
        camera_override: Option<&str>,
    ) -> Result<IngestOutcome> {
        // Both of these touch the volume the file lives on, and a removable
        // volume that has gone away parks a read rather than failing it. Bound
        // them: the import's disconnect handling only runs on an `Err`, so a
        // call that never returns disables every recovery path we have.
        let stat_target = path.to_path_buf();
        let metadata = crate::stall::with_stall_timeout(
            &format!("reading metadata for {}", path.display()),
            crate::stall::io_stall_timeout(),
            move || fs::metadata(stat_target),
        )?
        .with_context(|| format!("read metadata for {}", path.display()))?;
        let fingerprint_target = path.to_path_buf();
        let size = metadata.len();
        let fingerprint = crate::stall::with_stall_timeout(
            &format!("fingerprinting {}", path.display()),
            crate::stall::io_stall_timeout(),
            move || sampled_fingerprint(&fingerprint_target, size),
        )??;
        let connection = self.connection()?;
        let known: Option<(String, String, bool)> = connection
            .query_row(
                "SELECT id, path, COALESCE(membership_removed, 0) FROM assets WHERE project_id = ?1 AND fingerprint = ?2",
                params![project_id, fingerprint],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((asset_id, recorded_path, membership_removed)) = known {
            if membership_removed {
                drop(connection);
                // Undo only brings the asset back into the materialized project
                // view. The file we just fingerprinted may be a remounted card
                // or a freshly committed archive copy, so record it first.
                self.relink_known_asset(&asset_id, &recorded_path, path, &fingerprint)?;
                self.restore_project_asset_membership(project_id, &asset_id)?;
                return Ok(IngestOutcome::Imported);
            }
            // Seeing content we already know is evidence about *where that
            // content lives*, and throwing it away is how an asset ends up
            // stranded on a path that no longer exists. Register the discovered
            // location, and adopt it if the asset has nowhere else to resolve.
            return self.relink_known_asset(&asset_id, &recorded_path, path, &fingerprint);
        }

        let mut probe = probe_media(path)?;
        if let Some(camera) = camera_override
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            probe.camera = camera.to_string();
        }
        self.record_camera_identity_diagnostics(project_id, path, &probe.camera);
        let id = format!("asset_{}", Uuid::new_v4().simple());
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("media filename is not valid UTF-8"))?;
        drop(connection);
        self.add_project_asset(
            project_id,
            &path.to_string_lossy(),
            operations::ProjectAssetAddedPayload {
                id: id.clone(),
                fingerprint: fingerprint.clone(),
                filename: filename.to_string(),
                size_bytes: metadata.len(),
                duration_seconds: probe.duration_seconds,
                frame_rate: probe.frame_rate,
                width: probe.width,
                height: probe.height,
                codec: probe.codec,
                camera: probe.camera,
                captured_at_raw: probe.captured_at_raw,
                timestamp_candidates: Some(probe.timestamp_candidates),
                best_time: probe.best_time,
                metadata_rotation_degrees: probe.metadata_rotation_degrees,
                default_rotation_degrees: probe.default_rotation_degrees,
                created_at: now(),
            },
        )?;
        self.store_video_validation(&id, &fingerprint, "usable", None)?;
        Ok(IngestOutcome::Imported)
    }

    /// Records a rediscovered location for content this project already holds,
    /// repairing the asset when its recorded path has gone offline.
    ///
    /// The fingerprint has already been computed by the caller and matched an
    /// existing asset, so the discovered path is verified by construction — no
    /// second hash.
    pub(crate) fn relink_known_asset(
        &self,
        asset_id: &str,
        recorded_path: &str,
        discovered: &Path,
        fingerprint: &str,
    ) -> Result<IngestOutcome> {
        let discovered_text = discovered.to_string_lossy().into_owned();
        let node_id = self.node_id()?;
        let connection = self.connection()?;
        locations::record_verification(
            &connection,
            asset_id,
            &node_id,
            &discovered_text,
            locations::Verification {
                state: LocationVerificationState::Verified,
                fingerprint: Some(fingerprint),
                guard: locations::LocationGuard::read(discovered),
                at: &now(),
            },
        )?;

        if discovered_text == recorded_path {
            return Ok(IngestOutcome::Duplicate);
        }
        // The asset already resolves somewhere; a second copy is worth knowing
        // about but is no reason to move the asset off a working location.
        let recorded_state = locations::recorded_location(&connection, asset_id, recorded_path)?;
        if locations::location_is_available(
            recorded_path,
            recorded_state.map(|recorded| recorded.state),
            recorded_state.and_then(|recorded| recorded.guard),
        ) || locations::any_available_location(&connection, asset_id, &discovered_text)?
            .is_some()
        {
            return Ok(IngestOutcome::Duplicate);
        }

        locations::set_preferred_path(&connection, asset_id, &discovered_text)?;
        Ok(IngestOutcome::Relinked)
    }

    pub fn list_assets(&self, project_id: &str) -> Result<Vec<Asset>> {
        self.require_project(project_id)?;
        let connection = self.connection()?;
        let metadata_rotation = asset_column_expression(
            &connection,
            "metadata_rotation_degrees",
            "a.metadata_rotation_degrees",
            "0",
        )?;
        let default_rotation = asset_column_expression(
            &connection,
            "default_rotation_degrees",
            "a.default_rotation_degrees",
            "0",
        )?;
        let rotation =
            asset_column_expression(&connection, "rotation_degrees", "a.rotation_degrees", "0")?;
        let orientation_probed = asset_column_expression(
            &connection,
            "orientation_probed",
            "a.orientation_probed",
            "0",
        )?;
        let orientation_decision = asset_column_expression(
            &connection,
            "orientation_decision_json",
            "a.orientation_decision_json",
            "NULL",
        )?;
        let labels = asset_column_expression(&connection, "labels_json", "a.labels_json", "'[]'")?;
        let removed = asset_column_expression(&connection, "removed", "a.removed", "0")?;
        let media_object_id =
            asset_column_expression(&connection, "media_object_id", "a.media_object_id", "NULL")?;
        let media_verification_state = asset_column_expression(
            &connection,
            "media_verification_state",
            "a.media_verification_state",
            "'unlinked'",
        )?;
        let title = asset_column_expression(&connection, "title", "a.title", "''")?;
        let description =
            asset_column_expression(&connection, "description", "a.description", "''")?;
        let (thumbnail_path, thumbnail_quality, thumbnail_join) = if table_exists(
            &connection,
            "asset_thumbnails",
        )? {
            (
                "t.path",
                "COALESCE(t.quality, 0)",
                "LEFT JOIN asset_thumbnails t ON t.asset_id = a.id AND t.quality = (SELECT MAX(t2.quality) FROM asset_thumbnails t2 WHERE t2.asset_id = a.id)",
            )
        } else {
            ("NULL", "0", "")
        };
        let (location_state, location_size, location_mtime, location_join) =
            location_columns(&connection)?;
        let (revision, revision_join) = revision_columns(&connection)?;
        let camera_proxy_columns = camera_proxy_asset_columns(&connection)?;
        let sql = format!(
            r#"
            SELECT a.id, a.project_id, a.path, a.filename, a.fingerprint, a.size_bytes,
                   a.duration_seconds, a.frame_rate, a.width, a.height, a.codec, a.camera,
                   a.captured_at_raw, a.best_time, a.offset_seconds, a.proxy_path,
                   {metadata_rotation}, {default_rotation}, {rotation},
                   {orientation_probed}, {thumbnail_path}, {thumbnail_quality},
                   a.notes, {labels}, {removed}, {media_object_id},
                   {media_verification_state}, {title}, {description},
                   {location_state}, {location_size}, {location_mtime},
                   {revision}, {camera_proxy_columns}, {orientation_decision}
            FROM assets a
            {thumbnail_join}
            {location_join}
            {revision_join}
            WHERE a.project_id = ?1 AND COALESCE(a.membership_removed, 0) = 0
            ORDER BY a.best_time IS NULL, a.best_time, a.filename, a.id
            "#
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map([project_id], row_to_asset)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Lists assets after explicitly probing legacy rows whose orientation is unknown.
    pub fn list_assets_with_refreshed_orientation(&self, project_id: &str) -> Result<Vec<Asset>> {
        self.list_assets(project_id)?
            .into_iter()
            .map(|asset| {
                if !asset.orientation_probed && asset.original_available {
                    self.refresh_asset_orientation(&asset.id)
                } else {
                    Ok(asset)
                }
            })
            .collect()
    }

    pub fn get_asset(&self, asset_id: &str) -> Result<Asset> {
        let connection = self.connection()?;
        let metadata_rotation = asset_column_expression(
            &connection,
            "metadata_rotation_degrees",
            "a.metadata_rotation_degrees",
            "0",
        )?;
        let default_rotation = asset_column_expression(
            &connection,
            "default_rotation_degrees",
            "a.default_rotation_degrees",
            "0",
        )?;
        let rotation =
            asset_column_expression(&connection, "rotation_degrees", "a.rotation_degrees", "0")?;
        let orientation_probed = asset_column_expression(
            &connection,
            "orientation_probed",
            "a.orientation_probed",
            "0",
        )?;
        let orientation_decision = asset_column_expression(
            &connection,
            "orientation_decision_json",
            "a.orientation_decision_json",
            "NULL",
        )?;
        let labels = asset_column_expression(&connection, "labels_json", "a.labels_json", "'[]'")?;
        let removed = asset_column_expression(&connection, "removed", "a.removed", "0")?;
        let media_object_id =
            asset_column_expression(&connection, "media_object_id", "a.media_object_id", "NULL")?;
        let media_verification_state = asset_column_expression(
            &connection,
            "media_verification_state",
            "a.media_verification_state",
            "'unlinked'",
        )?;
        let title = asset_column_expression(&connection, "title", "a.title", "''")?;
        let description =
            asset_column_expression(&connection, "description", "a.description", "''")?;
        let (thumbnail_path, thumbnail_quality, thumbnail_join) = if table_exists(
            &connection,
            "asset_thumbnails",
        )? {
            (
                "t.path",
                "COALESCE(t.quality, 0)",
                "LEFT JOIN asset_thumbnails t ON t.asset_id = a.id AND t.quality = (SELECT MAX(t2.quality) FROM asset_thumbnails t2 WHERE t2.asset_id = a.id)",
            )
        } else {
            ("NULL", "0", "")
        };
        let (location_state, location_size, location_mtime, location_join) =
            location_columns(&connection)?;
        let (revision, revision_join) = revision_columns(&connection)?;
        let camera_proxy_columns = camera_proxy_asset_columns(&connection)?;
        let sql = format!(
            r#"
            SELECT a.id, a.project_id, a.path, a.filename, a.fingerprint, a.size_bytes,
                   a.duration_seconds, a.frame_rate, a.width, a.height, a.codec, a.camera,
                   a.captured_at_raw, a.best_time, a.offset_seconds, a.proxy_path,
                   {metadata_rotation}, {default_rotation}, {rotation},
                   {orientation_probed}, {thumbnail_path}, {thumbnail_quality},
                   a.notes, {labels}, {removed}, {media_object_id},
                   {media_verification_state}, {title}, {description},
                   {location_state}, {location_size}, {location_mtime},
                   {revision}, {camera_proxy_columns}, {orientation_decision}
            FROM assets a
            {thumbnail_join}
            {location_join}
            {revision_join}
            WHERE a.id = ?1
            "#
        );
        connection
            .query_row(&sql, [asset_id], row_to_asset)
            .optional()?
            .ok_or_else(|| anyhow!("asset not found: {asset_id}"))
    }

    /// Returns the path to this asset's original only after proving the bytes
    /// there are still this asset's bytes.
    ///
    /// Every operation that reads original media goes through here. Path
    /// existence is not enough: a reformatted card can put a different clip at
    /// the same `.../C0012.MP4`, and transcoding or exporting that would produce
    /// silently wrong output. See `4c5eb2f:docs/tasks/import/ARCHITECTURE.md` §3.2.
    ///
    /// The fingerprint is computed at most once per file version. A location
    /// already verified, whose size and modification time are unchanged, is
    /// returned without touching its contents, so repeated proxy or export runs
    /// over a large project pay the hash exactly once.
    pub fn resolve_original(&self, asset_id: &str) -> Result<PathBuf> {
        let asset = self.get_asset(asset_id)?;
        locations::require_path(asset_id, &asset.path)?;
        let path = PathBuf::from(&asset.path);
        let Some(observed) = locations::LocationGuard::read(&path) else {
            bail!("original is offline: {}", asset.path);
        };

        let connection = self.connection()?;
        let recorded = locations::recorded_location(&connection, asset_id, &asset.path)?;
        if let Some(recorded) = recorded {
            match recorded.state {
                LocationVerificationState::Mismatch => {
                    return Err(locations::mismatch_error(&asset.path));
                }
                LocationVerificationState::Verified if observed.matches(recorded.guard) => {
                    return Ok(path);
                }
                _ => {}
            }
        }

        let fingerprint = sampled_fingerprint(&path, observed.size_bytes as u64)?;
        let verified = fingerprint == asset.fingerprint;
        let node_id = self.node_id()?;
        locations::record_verification(
            &connection,
            asset_id,
            &node_id,
            &asset.path,
            locations::Verification {
                state: if verified {
                    LocationVerificationState::Verified
                } else {
                    LocationVerificationState::Mismatch
                },
                fingerprint: Some(&fingerprint),
                guard: Some(observed),
                at: &now(),
            },
        )?;
        if verified {
            Ok(path)
        } else {
            Err(locations::mismatch_error(&asset.path))
        }
    }

    pub fn update_asset(&self, asset_id: &str, patch: &AssetPatch) -> Result<Asset> {
        self.update_asset_with_operations(asset_id, patch)
    }

    pub fn apply_clock_event(
        &self,
        project_id: &str,
        camera: &str,
        start_filename: &str,
        offset_seconds: i64,
        label: &str,
    ) -> Result<ClockEvent> {
        self.create_clock_event_operation(project_id, camera, start_filename, offset_seconds, label)
    }

    /// Replaces a clip's title and description with a human edit.
    ///
    /// These are AI-written but human-editable, which is why they live on the
    /// asset beside `notes` rather than inside the analysis row: an edit has to
    /// survive the observation being deleted or re-run. Deliberately a separate
    /// operation rather than part of `AssetPatch`, whose four fields are a
    /// full-replacement contract shared with the CLI and the browser.
    ///
    /// Written directly rather than as a typed operation, matching how analysis
    /// writes the same columns; neither is synchronized between nodes today.
    pub fn set_clip_summary(
        &self,
        asset_id: &str,
        title: &str,
        description: &str,
    ) -> Result<Asset> {
        let connection = self.connection()?;
        set_title_and_description(&connection, asset_id, title.trim(), description.trim())?;
        drop(connection);
        self.get_asset(asset_id)
    }

    /// Renames a camera identity across every clip and capture sequence in a project.
    ///
    /// The camera on an asset is a guess: `identify_camera` reads a filename
    /// pattern and whatever EXIF survived the card, and `UNKNOWN_CAMERA` is a
    /// routine outcome. The ribbon's lane gutter is where that guess becomes
    /// visible, so it is also where it gets corrected.
    ///
    /// Written as the same typed identity operation as an explicit merge so
    /// replay and synchronization keep clips, card lanes, and future ingest
    /// aliases coherent. Sequence and epoch identities remain intact. Returns
    /// the number of clips moved to the new lane.
    pub fn rename_camera(&self, project_id: &str, from: &str, to: &str) -> Result<usize> {
        Ok(self
            .rename_camera_identity(project_id, from, to)?
            .moved_clip_count)
    }
}
