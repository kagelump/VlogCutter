//! Thumbnail generation and orientation review.

use crate::*;
use anyhow::{Context, Result, anyhow, bail, ensure};
use batch_estimator::BatchEstimator;
use rusqlite::{OptionalExtension, params};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use uuid::Uuid;

pub(crate) const THUMBNAIL_TASK_KIND: &str = "thumbnail.q1.orientation-v2";
pub(crate) const THUMBNAIL_CACHE_VERSION: &str = "v2";
pub(crate) const THUMBNAIL_WORKERS: usize = 1;

pub(crate) fn orientation_review_reasons(asset: &Asset) -> Vec<String> {
    let decision = asset.orientation_decision.as_ref();
    if decision.is_some_and(|decision| {
        decision.status == "confirmed" && decision.rotation_degrees == Some(asset.rotation_degrees)
    }) {
        return Vec::new();
    }
    let mut reasons = Vec::new();
    if !asset.orientation_probed {
        reasons.push("metadata_unprobed".into());
    }
    if decision.is_some_and(|decision| decision.status == "unresolved") {
        reasons.push("explicitly_unresolved".into());
    }
    if decision.is_some_and(|decision| {
        decision.status == "confirmed" && decision.rotation_degrees != Some(asset.rotation_degrees)
    }) {
        reasons.push("confirmed_value_mismatch".into());
    }
    if asset.width < asset.height {
        reasons.push("portrait_dimensions_need_review".into());
    }
    if asset.default_rotation_degrees != asset.metadata_rotation_degrees {
        reasons.push("landscape_default_differs_from_metadata".into());
    }
    if asset.rotation_degrees != asset.default_rotation_degrees
        && !decision.is_some_and(|decision| decision.status == "confirmed")
    {
        reasons.push("explicit_override_unconfirmed".into());
    }
    reasons
}

pub(crate) fn render_orientation_still(
    input: &Path,
    output: &Path,
    timestamp: &str,
    rotation: i32,
) -> Result<()> {
    let rotation_filter = match rotation {
        0 => "",
        90 => "transpose=clock,",
        180 => "hflip,vflip,",
        270 => "transpose=cclock,",
        _ => bail!("unsupported orientation evidence rotation {rotation}"),
    };
    let filter = format!(
        "{rotation_filter}scale='min(960,iw)':'min(960,ih)':force_original_aspect_ratio=decrease"
    );
    let mut command = tool_command("ffmpeg");
    command
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-ss",
            timestamp,
            "-noautorotate",
            "-i",
        ])
        .arg(input)
        .args(["-frames:v", "1", "-vf", &filter])
        .arg(output);
    let (status, diagnostic) = run_captured_media_command(&mut command, input, output)
        .context("start orientation evidence frame extraction")?;
    if !status.success() {
        let _ = fs::remove_file(output);
        return Err(media_tool_failure(
            "orientation evidence frame extraction",
            status,
            diagnostic,
        ));
    }
    ensure!(
        output.is_file(),
        "orientation evidence extractor produced no frame"
    );
    Ok(())
}

pub(crate) fn decode_orientation_decision(
    value: Option<String>,
) -> rusqlite::Result<Option<OrientationDecision>> {
    value
        .map(|json| {
            let decision: OrientationDecision = serde_json::from_str(&json).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    35,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            operations::validate_orientation_decision(&decision.status, decision.rotation_degrees)
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        35,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            error.to_string(),
                        )),
                    )
                })?;
            Ok(decision)
        })
        .transpose()
}

impl Catalog {
    /// Explicitly refreshes legacy orientation metadata from the immutable original.
    pub fn refresh_asset_orientation(&self, asset_id: &str) -> Result<Asset> {
        let asset = self.get_asset(asset_id)?;
        if asset.orientation_probed {
            return Ok(asset);
        }
        // Reading orientation off whatever happens to sit at the path would
        // rotate this clip by another clip's metadata.
        let original = self.resolve_original(asset_id)?;

        let probe = probe_media(&original)?;
        let explicit_rotation = self.connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM lww_registers WHERE project_id=?1 AND aggregate_type='projectAsset' AND aggregate_id=?2 AND field_name='rotationDegrees')",
            params![asset.project_id, asset.id],
            |row| row.get::<_, bool>(0),
        )?;
        let rotation_degrees =
            if explicit_rotation || asset.rotation_degrees != asset.default_rotation_degrees {
                asset.rotation_degrees
            } else if let Some(decision) = asset
                .orientation_decision
                .as_ref()
                .filter(|decision| decision.status == "confirmed")
            {
                decision
                    .rotation_degrees
                    .unwrap_or(probe.default_rotation_degrees)
            } else {
                probe.default_rotation_degrees
            };
        self.connection()?.execute(
            r#"
                UPDATE assets
                SET metadata_rotation_degrees = ?2, default_rotation_degrees = ?3,
                    rotation_degrees = ?4, orientation_probed = 1
                WHERE id = ?1
                "#,
            params![
                &asset.id,
                probe.metadata_rotation_degrees,
                probe.default_rotation_degrees,
                rotation_degrees,
            ],
        )?;
        self.get_asset(&asset.id)
    }

    /// Returns at most 200 suspicious or unresolved orientation rows, newest
    /// evidence first. It never proposes or applies a visual rotation.
    pub fn orientation_review_queue(
        &self,
        project_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<OrientationReviewPage> {
        ensure!(limit <= 200, "orientation review page limit exceeds 200");
        let assets = self.list_assets(project_id)?;
        let mut items = assets
            .into_iter()
            .filter_map(|asset| {
                let reasons = orientation_review_reasons(&asset);
                (!reasons.is_empty()).then_some(OrientationReviewItem {
                    evidence_available: asset.original_available || asset.proxy_available,
                    asset,
                    review_reasons: reasons,
                })
            })
            .collect::<Vec<_>>();
        items.sort_by_key(|item| {
            (
                item.asset
                    .orientation_decision
                    .as_ref()
                    .is_some_and(|decision| decision.status == "confirmed"),
                item.asset.orientation_decision.is_some(),
                item.asset.filename.clone(),
                item.asset.id.clone(),
            )
        });
        let total = items.len();
        Ok(OrientationReviewPage {
            items: items.into_iter().skip(offset).take(limit).collect(),
            total,
            offset,
            limit,
        })
    }

    pub fn generate_thumbnail(&self, asset_id: &str, quality: u8) -> Result<Asset> {
        if quality != 1 {
            bail!("invalid thumbnail quality {quality}; MVP supports quality 1");
        }
        let claim = self.claim_representation(asset_id, RepresentationKind::Thumbnail)?;
        self.generate_thumbnail_with_claim(&claim, quality)
    }

    /// Execute an already-reserved thumbnail without reacquiring its claim.
    pub fn generate_thumbnail_with_claim(
        &self,
        claim: &RepresentationClaim,
        quality: u8,
    ) -> Result<Asset> {
        if quality != 1 {
            bail!("invalid thumbnail quality {quality}; MVP supports quality 1");
        }
        let asset_id = claim.asset_id();
        self.validate_representation_claim(claim, asset_id, RepresentationKind::Thumbnail)?;
        let asset = self.get_asset(asset_id)?;
        let input = if let Some(proxy) = asset
            .proxy_path
            .as_deref()
            .filter(|path| has_current_proxy(&asset) && Path::new(path).is_file())
        {
            PathBuf::from(proxy)
        } else {
            self.resolve_original(asset_id)?
        };
        let parent = self
            .database_path
            .parent()
            .ok_or_else(|| anyhow!("catalog has no parent directory"))?;
        let thumbnail_dir = parent.join("cache").join("thumbnails").join(&asset.id);
        fs::create_dir_all(&thumbnail_dir)?;
        let output = thumbnail_dir.join(format!("q{quality}.{THUMBNAIL_CACHE_VERSION}.jpg"));
        let temporary = thumbnail_dir.join(format!(
            "q{quality}.{THUMBNAIL_CACHE_VERSION}.{}.tmp.jpg",
            Uuid::new_v4().simple()
        ));
        let seek = (asset.duration_seconds.max(0.0) * 0.2).to_string();
        let mut command = tool_command("ffmpeg");
        command
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-ss",
                &seek,
                "-noautorotate",
                "-i",
            ])
            .arg(&input)
            .args([
                "-frames:v",
                "1",
                "-vf",
                "scale='min(480,iw)':-2:force_original_aspect_ratio=decrease",
                "-q:v",
                "3",
            ])
            .arg(&temporary);
        let (status, diagnostic) = run_captured_media_command(&mut command, &input, &temporary)
            .context("start ffmpeg thumbnail process")?;
        if !status.success() {
            let _ = fs::remove_file(&temporary);
            return Err(media_tool_failure(
                "ffmpeg thumbnail generation",
                status,
                diagnostic,
            ));
        }
        fs::rename(&temporary, &output)?;
        self.connection()?.execute(
            r#"
            INSERT INTO asset_thumbnails (asset_id, quality, path, created_at)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(asset_id, quality)
            DO UPDATE SET path = excluded.path, created_at = excluded.created_at
            "#,
            params![asset_id, quality, output.to_string_lossy(), now()],
        )?;
        self.get_asset(asset_id)
    }

    /// Generates bounded raw-metadata and saved-effective stills from an
    /// available proxy or, when present, the immutable original.
    pub fn orientation_evidence(&self, asset_id: &str) -> Result<OrientationEvidence> {
        let asset = self.get_asset(asset_id)?;
        let proxy_candidate = asset
            .proxy_path
            .as_deref()
            .filter(|path| asset.proxy_available && Path::new(path).is_file());
        let mut _proxy_claim = None;
        let (selected, using_proxy) = if let Some(proxy) = proxy_candidate {
            // The proxy claim is always acquired before the evidence claim.
            // Proxy replacement holds this same lock while atomically swapping
            // the registered output.
            match self.try_claim_representation(asset_id, RepresentationKind::Proxy)? {
                Some(claim) => {
                    self.validate_representation_claim(
                        &claim,
                        asset_id,
                        RepresentationKind::Proxy,
                    )?;
                    _proxy_claim = Some(claim);
                    (PathBuf::from(proxy), true)
                }
                None if asset.original_available => (self.resolve_original(asset_id)?, false),
                None => {
                    return Err(RepresentationBusy {
                        asset_id: asset_id.to_string(),
                        kind: RepresentationKind::Proxy,
                    }
                    .into());
                }
            }
        } else if asset.original_available {
            (self.resolve_original(asset_id)?, false)
        } else {
            return Ok(OrientationEvidence {
                asset_id: asset.id,
                asset_revision: asset.revision,
                source_fingerprint: asset.fingerprint,
                representation: "unavailable".into(),
                representation_fingerprint: None,
                original_available: false,
                raw_frame_path: None,
                effective_frame_path: None,
                unavailable_reason: Some("no usable proxy or original is available".into()),
            });
        };
        let representation_fingerprint = if using_proxy {
            Some(full_media_fingerprint(&selected)?)
        } else {
            None
        };
        let proxy_registration = using_proxy.then(|| {
            (
                asset.proxy_path.clone(),
                asset.proxy_representation,
                representation_fingerprint.clone(),
            )
        });
        let registration_check = self.get_asset(asset_id)?;
        ensure!(
            registration_check.revision == asset.revision
                && registration_check.fingerprint == asset.fingerprint
                && registration_check.rotation_degrees == asset.rotation_degrees
                && registration_check.metadata_rotation_degrees == asset.metadata_rotation_degrees
                && registration_check.default_rotation_degrees == asset.default_rotation_degrees
                && registration_check.orientation_probed == asset.orientation_probed
                && registration_check.orientation_decision == asset.orientation_decision,
            "orientation changed before evidence generation started"
        );
        if let Some((path, representation, _)) = &proxy_registration {
            ensure!(
                registration_check.proxy_path == *path
                    && registration_check.proxy_representation == *representation,
                "registered proxy changed before evidence generation started"
            );
        }
        let claim = self.claim_representation(asset_id, RepresentationKind::OrientationReview)?;
        self.validate_representation_claim(
            &claim,
            asset_id,
            RepresentationKind::OrientationReview,
        )?;
        let baked_rotation = if using_proxy
            && asset.proxy_representation == Some(ProxyRepresentation::MetadataBakedV1)
        {
            asset.metadata_rotation_degrees
        } else {
            0
        };
        // RawV3 removes the source display matrix but leaves encoded pixels
        // untouched, so apply the source metadata rotation for the raw view.
        let input_rotation = asset.metadata_rotation_degrees;
        let parent = self
            .database_path
            .parent()
            .ok_or_else(|| anyhow!("catalog has no parent directory"))?;
        let revision_dir = parent
            .join("cache")
            .join("orientation")
            .join(&asset.id)
            .join(format!(
                "r{}-{}-{}-m{}-e{}",
                asset.revision,
                asset.fingerprint,
                representation_fingerprint
                    .as_deref()
                    .unwrap_or("source-original"),
                asset.metadata_rotation_degrees,
                asset.rotation_degrees
            ));
        fs::create_dir_all(&revision_dir)?;
        // First frame is reliable even for short or low-frame-rate footage,
        // where a 20% seek can land past the final decoded PTS.
        let timestamp = "0".to_string();
        let raw_target = input_rotation;
        let effective_target = asset.rotation_degrees;
        let raw_path = revision_dir.join("raw-metadata.png");
        let effective_path = revision_dir.join("effective.png");
        let token = Uuid::new_v4().simple().to_string();
        let raw_temporary = revision_dir.join(format!("raw-metadata.{token}.tmp.png"));
        let effective_temporary = revision_dir.join(format!("effective.{token}.tmp.png"));
        for (rotation, output) in [(raw_target, &raw_path), (effective_target, &effective_path)] {
            let temporary = if output == &raw_path {
                &raw_temporary
            } else {
                &effective_temporary
            };
            if let Err(error) = render_orientation_still(
                &selected,
                temporary,
                &timestamp,
                (rotation as i32 - baked_rotation as i32).rem_euclid(360),
            ) {
                let _ = fs::remove_file(&raw_temporary);
                let _ = fs::remove_file(&effective_temporary);
                return Err(error);
            }
        }
        let current = self.get_asset(asset_id)?;
        let orientation_unchanged = current.revision == asset.revision
            && current.fingerprint == asset.fingerprint
            && current.rotation_degrees == asset.rotation_degrees
            && current.metadata_rotation_degrees == asset.metadata_rotation_degrees
            && current.default_rotation_degrees == asset.default_rotation_degrees
            && current.orientation_probed == asset.orientation_probed
            && current.orientation_decision == asset.orientation_decision;
        let proxy_unchanged = if using_proxy {
            current.proxy_path == asset.proxy_path
                && current.proxy_representation == asset.proxy_representation
                && full_media_fingerprint(&selected)?
                    == representation_fingerprint.as_deref().unwrap()
        } else {
            true
        };
        if !orientation_unchanged || !proxy_unchanged {
            let _ = fs::remove_file(&raw_temporary);
            let _ = fs::remove_file(&effective_temporary);
            bail!("orientation evidence became stale while frames were decoded");
        }
        fs::rename(&raw_temporary, &raw_path)?;
        if let Err(error) = fs::rename(&effective_temporary, &effective_path) {
            let _ = fs::remove_file(&raw_path);
            let _ = fs::remove_file(&effective_temporary);
            return Err(error.into());
        }
        Ok(OrientationEvidence {
            asset_id: asset.id,
            asset_revision: asset.revision,
            source_fingerprint: asset.fingerprint,
            representation: if using_proxy { "proxy" } else { "original" }.into(),
            representation_fingerprint,
            original_available: asset.original_available,
            raw_frame_path: Some(raw_path.to_string_lossy().into_owned()),
            effective_frame_path: Some(effective_path.to_string_lossy().into_owned()),
            unavailable_reason: None,
        })
    }

    pub fn generate_missing_thumbnails(
        &self,
        project_id: &str,
        quality: u8,
    ) -> Result<ThumbnailReport> {
        self.generate_missing_thumbnails_with_progress(project_id, quality, |_| {})
    }

    pub fn generate_missing_thumbnails_with_progress<F>(
        &self,
        project_id: &str,
        quality: u8,
        on_progress: F,
    ) -> Result<ThumbnailReport>
    where
        F: FnMut(AssetBatchProgress),
    {
        let asset_ids = self
            .list_assets(project_id)?
            .into_iter()
            .map(|asset| asset.id)
            .collect::<Vec<_>>();
        self.generate_thumbnails_for_assets_with_progress(&asset_ids, quality, on_progress)
    }

    pub fn generate_thumbnails_for_assets(
        &self,
        asset_ids: &[String],
        quality: u8,
    ) -> Result<ThumbnailReport> {
        self.generate_thumbnails_for_assets_with_progress(asset_ids, quality, |_| {})
    }

    /// Generates a thumbnail batch for exactly the supplied assets. Duplicate
    /// ids are coalesced and an existing current-version Q1 is left untouched.
    pub fn generate_thumbnails_for_assets_with_progress<F>(
        &self,
        asset_ids: &[String],
        quality: u8,
        on_progress: F,
    ) -> Result<ThumbnailReport>
    where
        F: FnMut(AssetBatchProgress),
    {
        self.generate_thumbnail_batch(asset_ids, quality, None, on_progress)
    }

    /// Batch reserved before entering a desktop execution queue. Claims remain
    /// held for the whole batch, including validation and catalog commits.
    pub fn generate_thumbnails_with_claims_with_progress<F>(
        &self,
        claims: &[RepresentationClaim],
        quality: u8,
        on_progress: F,
    ) -> Result<ThumbnailReport>
    where
        F: FnMut(AssetBatchProgress),
    {
        let ids = claims
            .iter()
            .map(|claim| claim.asset_id().to_owned())
            .collect::<Vec<_>>();
        self.generate_thumbnail_batch(&ids, quality, Some(claims), on_progress)
    }

    pub(crate) fn generate_thumbnail_batch<F>(
        &self,
        asset_ids: &[String],
        quality: u8,
        claims: Option<&[RepresentationClaim]>,
        mut on_progress: F,
    ) -> Result<ThumbnailReport>
    where
        F: FnMut(AssetBatchProgress),
    {
        if quality != 1 {
            bail!("invalid thumbnail quality {quality}; MVP supports quality 1");
        }
        if let Some(claims) = claims {
            for claim in claims {
                self.validate_representation_claim(
                    claim,
                    claim.asset_id(),
                    RepresentationKind::Thumbnail,
                )?;
            }
        }
        let mut seen = HashSet::new();
        let mut pending = Vec::new();
        let mut skipped = 0;
        let mut failures = Vec::new();
        for asset_id in asset_ids {
            if !seen.insert(asset_id.clone()) {
                continue;
            }
            match self.get_asset(asset_id) {
                Ok(asset) if self.has_current_thumbnail(&asset.id, quality)? => skipped += 1,
                Ok(asset) => pending.push(asset),
                Err(error) => failures.push(IngestFailure {
                    path: asset_id.clone(),
                    error: format!("{error:#}"),
                }),
            }
        }
        let mut report = ThumbnailReport {
            quality,
            generated: 0,
            skipped,
            failures,
        };
        let task_work: Vec<u64> = pending.iter().map(|asset| asset.size_bytes).collect();
        let mut estimator = BatchEstimator::new(
            &task_work,
            THUMBNAIL_WORKERS,
            self.load_batch_history(THUMBNAIL_TASK_KIND)?,
        );
        on_progress(AssetBatchProgress {
            asset: None,
            estimate: estimator.estimate(&task_work),
        });
        for (index, asset) in pending.into_iter().enumerate() {
            let started = Instant::now();
            let result = if let Some(claims) = claims {
                let claim = claims
                    .iter()
                    .find(|claim| claim.asset_id() == asset.id)
                    .ok_or_else(|| anyhow!("missing thumbnail claim"))?;
                self.generate_thumbnail_with_claim(claim, quality)
            } else {
                self.generate_thumbnail(&asset.id, quality)
            };
            let updated = match result {
                Ok(updated) => {
                    report.generated += 1;
                    estimator.record_completion(asset.size_bytes, started.elapsed());
                    self.save_batch_history(THUMBNAIL_TASK_KIND, estimator.history())?;
                    Some(updated)
                }
                Err(error) if error.is::<RepresentationBusy>() => {
                    estimator.record_failure(asset.size_bytes);
                    report.skipped += 1;
                    None
                }
                Err(error) => {
                    estimator.record_failure(asset.size_bytes);
                    report.failures.push(IngestFailure {
                        path: asset.path,
                        error: format!("{error:#}"),
                    });
                    None
                }
            };
            on_progress(AssetBatchProgress {
                asset: updated,
                estimate: estimator.estimate(&task_work[index + 1..]),
            });
        }
        Ok(report)
    }

    pub(crate) fn has_current_thumbnail(&self, asset_id: &str, quality: u8) -> Result<bool> {
        let path = self
            .connection()?
            .query_row(
                "SELECT path FROM asset_thumbnails WHERE asset_id = ?1 AND quality = ?2",
                params![asset_id, quality],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(path.is_some_and(|path| {
            Path::new(&path).is_file()
                && path.ends_with(&format!("q{quality}.{THUMBNAIL_CACHE_VERSION}.jpg"))
        }))
    }
}
