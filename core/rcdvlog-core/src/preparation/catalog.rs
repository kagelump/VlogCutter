use super::{
    model::*,
    store::{self, TranscriptBinding, ValidationReceipt},
};
use crate::{Asset, AssetVideoValidationStatus, Catalog, MomentStatus, RepresentationKind};
use anyhow::{Result, anyhow, ensure};
use chrono::{SecondsFormat, Utc};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use std::{ops::ControlFlow, path::Path};

const PROXY_RECIPE: &str = "proxy.raw.v3";
const MAX_SCOPE: usize = 10_000;

fn digest(value: &impl Serialize) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}

fn clipped(value: &str, limit: usize) -> (String, bool) {
    let text: String = value.chars().take(limit).collect();
    let truncated = text.len() < value.len();
    (text, truncated)
}

fn evidence(state: PreparationState, reason: &str) -> PreparationEvidenceState {
    PreparationEvidenceState {
        state,
        reasons: if reason.is_empty() {
            vec![]
        } else {
            vec![clipped(reason, 1024).0]
        },
    }
}

fn duration_ms(asset: &Asset) -> Option<u32> {
    let millis = asset.duration_seconds * 1000.0;
    (millis.is_finite() && millis > 0.0 && millis < u32::MAX as f64).then(|| millis.round() as u32)
}

fn receipt_current(receipt: &ValidationReceipt, asset: &Asset) -> bool {
    receipt.project_id == asset.project_id
        && receipt.source_fingerprint == asset.fingerprint
        && Some(receipt.source_duration_ms) == duration_ms(asset)
        && (!receipt.decoded
            || (receipt.validated_end_ms > 0
                && receipt.validated_end_ms <= receipt.source_duration_ms))
        && receipt.representation_recipe == PROXY_RECIPE
        && receipt.validation_recipe == PREPARATION_VALIDATION_RECIPE
}

fn actual_proxy_recipe(asset: &Asset) -> Option<String> {
    asset.proxy_representation.as_ref().map(|kind| {
        match kind {
            crate::ProxyRepresentation::RawV3 => PROXY_RECIPE,
            crate::ProxyRepresentation::MetadataTaggedRawV2 => "proxy.metadata-tagged-raw.v2",
            crate::ProxyRepresentation::MetadataBakedV1 => "proxy.metadata-baked.v1",
        }
        .into()
    })
}

fn exceptional_asset(asset: &Asset, state: PreparationState, reason: &str) -> PreparationAsset {
    PreparationAsset {
        asset_id: asset.id.clone(),
        asset_revision: asset.revision,
        original_available: asset.original_available,
        proxy_available: asset.proxy_available,
        state,
        review_questions: vec![clipped(reason, 1024).0],
        review_questions_truncated: false,
        representation: PreparationRepresentation {
            kind: "generated_proxy".into(),
            recipe: actual_proxy_recipe(asset),
            required_recipe: PROXY_RECIPE.into(),
            fingerprint: None,
            fingerprint_kind: "full_blake3".into(),
            byte_length: None,
            validation: evidence(state, reason),
            receipt_id: None,
            validated_at: None,
            validator: None,
            usable_for_analysis: false,
        },
        camera_sidecar_status: None,
        camera_sidecar_reason: None,
        transcript: PreparationTranscript {
            readiness: evidence(state, reason),
            status: None,
            outcome_reason: None,
            recipe: None,
            source_fingerprint: None,
            segment_count: 0,
            speech_seconds: None,
            window_count: None,
            provider_segment_count: None,
            mean_token_probability: None,
            repetition_ratio: None,
            binding_origin: None,
        },
        moments: None,
        moment_details_truncated: false,
        orientation: PreparationOrientation {
            metadata_probed: asset.orientation_probed,
            metadata_rotation_degrees: asset.metadata_rotation_degrees,
            default_rotation_degrees: asset.default_rotation_degrees,
            effective_rotation_degrees: asset.rotation_degrees,
            confirmation: if asset.rotation_degrees != asset.default_rotation_degrees {
                "explicit_override"
            } else {
                "confirmation_not_tracked"
            }
            .into(),
            review_questions: vec![],
        },
        technical_range: None,
        range_certification: "no validated range claimed".into(),
    }
}

#[derive(Debug)]
struct TranscriptHeader {
    recipe: String,
    source_fingerprint: String,
    status: String,
    outcome_reason: String,
    reject_reason: String,
    speech_seconds: f64,
    window_count: u32,
    provider_segment_count: u32,
    mean_token_probability: Option<f32>,
    repetition_ratio: Option<f32>,
    segment_count: usize,
    timings_valid: bool,
    max_end_ms: u32,
    fingerprint: String,
}

type TranscriptHeaderRow = (
    String,
    String,
    String,
    String,
    String,
    f64,
    u32,
    String,
    String,
    u32,
    Option<f64>,
    Option<f64>,
);

impl Catalog {
    fn preparation_asset_active(&self, asset_id: &str, project_id: &str) -> Result<bool> {
        let asset = self.get_asset(asset_id)?;
        ensure!(
            asset.project_id == project_id,
            "asset left the authorized project scope"
        );
        Ok(!asset.removed)
    }
    fn preparation_scope(&self, scope: &PreparationScope) -> Result<PreparationScope> {
        ensure!(
            !scope.project_id.trim().is_empty(),
            "preparation project is required"
        );
        ensure!(
            scope.all_assets != !scope.asset_ids.is_empty(),
            "select exactly one of explicit assets or all assets"
        );
        ensure!(
            scope.asset_ids.len() <= MAX_SCOPE,
            "preparation selection exceeds 10000 assets"
        );
        ensure!(
            self.list_projects()?
                .iter()
                .any(|p| p.id == scope.project_id),
            "project not found"
        );
        let mut scope = scope.clone();
        scope.asset_ids.sort();
        scope.asset_ids.dedup();
        for id in &scope.asset_ids {
            ensure!(
                self.get_asset(id)?.project_id == scope.project_id,
                "asset does not belong to selected project"
            );
        }
        Ok(scope)
    }

    fn preparation_ids(
        &self,
        scope: &PreparationScope,
        offset: usize,
        limit: usize,
    ) -> Result<(usize, Vec<String>)> {
        if !scope.all_assets {
            return Ok((
                scope.asset_ids.len(),
                scope
                    .asset_ids
                    .iter()
                    .skip(offset)
                    .take(limit)
                    .cloned()
                    .collect(),
            ));
        }
        let connection = self.connection()?;
        let membership = if crate::asset_column_exists(&connection, "membership_removed")? {
            " AND COALESCE(membership_removed,0)=0"
        } else {
            ""
        };
        let total: i64 = connection.query_row(
            &format!("SELECT COUNT(*) FROM assets WHERE project_id=?1{membership}"),
            [&scope.project_id],
            |row| row.get(0),
        )?;
        let total = usize::try_from(total)?;
        let mut statement = connection.prepare(&format!(
            "SELECT id FROM assets WHERE project_id=?1{membership} ORDER BY id LIMIT ?2 OFFSET ?3"
        ))?;
        let ids = statement
            .query_map(
                params![scope.project_id, limit as i64, offset as i64],
                |row| row.get(0),
            )?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok((total, ids))
    }

    fn preparation_transcript_header(&self, asset: &Asset) -> Result<Option<TranscriptHeader>> {
        let connection = self.connection()?;
        if !crate::table_exists(&connection, "asset_transcripts")? {
            return Ok(None);
        }
        let row: Option<TranscriptHeaderRow> = connection.query_row(
            "SELECT recipe_key,source_fingerprint,status,outcome_reason,reject_reason,speech_seconds,window_count,primary_language,created_at,provider_segment_count,mean_token_probability,repetition_ratio FROM asset_transcripts WHERE asset_id=?1",
            [&asset.id], |r| Ok((
                r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,
                r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?,r.get(11)?,
            )),
        ).optional()?;
        let Some(row) = row else {
            return Ok(None);
        };
        let mut hasher = blake3::Hasher::new();
        hasher.update(&serde_json::to_vec(&row)?);
        let mut count = 0;
        let mut timings_valid = true;
        let mut max_end_ms = 0;
        let mut statement = connection.prepare(
            "SELECT start_ms,end_ms,language,text,mean_token_probability FROM transcript_segments WHERE asset_id=?1 ORDER BY segment_index"
        )?;
        let mut rows = statement.query([&asset.id])?;
        while let Some(segment) = rows.next()? {
            let value: (u32, u32, String, String, Option<f64>) = (
                segment.get(0)?,
                segment.get(1)?,
                segment.get(2)?,
                segment.get(3)?,
                segment.get(4)?,
            );
            timings_valid &=
                value.0 < value.1 && duration_ms(asset).is_some_and(|end| value.1 <= end);
            max_end_ms = max_end_ms.max(value.1);
            hasher.update(&serde_json::to_vec(&value)?);
            count += 1;
        }
        Ok(Some(TranscriptHeader {
            recipe: row.0,
            source_fingerprint: row.1,
            status: row.2,
            outcome_reason: row.3,
            reject_reason: row.4,
            speech_seconds: row.5,
            window_count: row.6,
            provider_segment_count: row.9,
            mean_token_probability: row.10.map(|value| value as f32),
            repetition_ratio: row.11.map(|value| value as f32),
            segment_count: count,
            timings_valid,
            max_end_ms,
            fingerprint: hasher.finalize().to_hex().to_string(),
        }))
    }

    fn preparation_transcript(
        &self,
        asset: &Asset,
        representation: &PreparationRepresentation,
        receipt: Option<&ValidationReceipt>,
    ) -> Result<PreparationTranscript> {
        let Some(header) = self.preparation_transcript_header(asset)? else {
            return Ok(PreparationTranscript {
                readiness: evidence(
                    if receipt.is_some_and(|r| r.transcript_error.is_some()) {
                        PreparationState::ReviewRequired
                    } else {
                        PreparationState::Missing
                    },
                    receipt
                        .and_then(|r| r.transcript_error.as_deref())
                        .unwrap_or("transcript has not been prepared"),
                ),
                status: None,
                outcome_reason: None,
                recipe: None,
                source_fingerprint: None,
                segment_count: 0,
                speech_seconds: None,
                window_count: None,
                provider_segment_count: None,
                mean_token_probability: None,
                repetition_ratio: None,
                binding_origin: None,
            });
        };
        let ready_source = representation.usable_for_analysis;
        let fingerprint_matches = self
            .proxy_fingerprint(&asset.id)
            .is_ok_and(|f| f == header.source_fingerprint);
        let binding = receipt
            .and_then(|r| r.transcript.as_ref())
            .filter(|b| b.artifact_fingerprint == header.fingerprint);
        let readiness = if header.recipe != crate::STT_RECIPE_KEY || !fingerprint_matches {
            evidence(
                PreparationState::Stale,
                "transcript recipe or input identity has changed",
            )
        } else if !header.timings_valid
            || receipt.is_some_and(|r| r.decoded && header.max_end_ms > r.validated_end_ms)
        {
            evidence(
                PreparationState::ReviewRequired,
                "transcript source-time bounds require review; repeated preparation will not repair them automatically",
            )
        } else if header.status == "no_speech"
            && header.outcome_reason == "legacy_no_speech_unverified"
        {
            // Older rows cannot prove whether the source had audio or whether a
            // provider returned nothing. Do not present that as verified silence;
            // keep the evidence visible and require explicit re-transcription.
            evidence(
                PreparationState::ReviewRequired,
                "legacy no_speech outcome cannot prove literal silence; force explicit transcription to verify",
            )
        } else if header.status == "no_accepted_speech" {
            evidence(
                PreparationState::ReviewRequired,
                &format!(
                    "speech was detected but the provider returned no accepted output ({} detected speech seconds, {} windows); targeted recovery may help",
                    header.speech_seconds, header.window_count
                ),
            )
        } else if !matches!(header.status.as_str(), "speech" | "no_speech" | "no_audio") {
            evidence(
                PreparationState::ReviewRequired,
                &format!(
                    "transcript {} ({}): {}; use explicit transcription recovery",
                    header.status, header.outcome_reason, header.reject_reason
                ),
            )
        } else if header.status == "speech" && header.segment_count == 0 {
            evidence(
                PreparationState::ReviewRequired,
                "speech transcript has no timed segments",
            )
        } else if !ready_source || binding.is_none() {
            evidence(
                PreparationState::Stale,
                "transcript lacks a current full-content preparation binding; explicitly prepare to adopt or refresh it",
            )
        } else {
            evidence(PreparationState::Ready, "")
        };
        Ok(PreparationTranscript {
            readiness,
            status: Some(header.status),
            outcome_reason: Some(header.outcome_reason),
            recipe: Some(header.recipe),
            source_fingerprint: Some(header.source_fingerprint),
            segment_count: header.segment_count,
            speech_seconds: Some(header.speech_seconds),
            window_count: Some(header.window_count),
            provider_segment_count: Some(header.provider_segment_count),
            mean_token_probability: header.mean_token_probability,
            repetition_ratio: header.repetition_ratio,
            binding_origin: binding.map(|b| b.origin.clone()),
        })
    }

    fn preparation_asset(&self, asset: &Asset) -> PreparationAsset {
        if asset.removed {
            return exceptional_asset(
                asset,
                PreparationState::Excluded,
                "asset is explicitly excluded; preparation will not process it",
            );
        }
        self.inspect_preparation_asset(asset)
            .unwrap_or_else(|error| {
                exceptional_asset(
                    asset,
                    PreparationState::ReviewRequired,
                    &format!("evidence could not be read: {error:#}"),
                )
            })
    }

    fn inspect_preparation_asset(&self, asset: &Asset) -> Result<PreparationAsset> {
        let mut representation = PreparationRepresentation {
            kind: "generated_proxy".into(),
            recipe: actual_proxy_recipe(asset),
            required_recipe: PROXY_RECIPE.into(),
            fingerprint: None,
            fingerprint_kind: "full_blake3".into(),
            byte_length: None,
            validation: evidence(
                PreparationState::Missing,
                "current analysis proxy is missing",
            ),
            receipt_id: None,
            validated_at: None,
            validator: None,
            usable_for_analysis: false,
        };
        let connection = self.connection()?;
        let mut receipt = None;
        if asset.removed {
            representation.validation =
                evidence(PreparationState::Excluded, "asset is explicitly excluded");
        } else if !crate::has_current_proxy(asset) {
            representation.validation = evidence(
                if asset.proxy_available {
                    PreparationState::Stale
                } else if asset.original_available {
                    PreparationState::Missing
                } else {
                    PreparationState::Unavailable
                },
                "current raw v3 analysis proxy is unavailable; alternate originals require a verified time mapping",
            );
        } else if let Some(path) = &asset.proxy_path {
            match crate::full_file_blake3(Path::new(path), &|| false) {
                Ok(hash) => {
                    representation.fingerprint = Some(hash.hex_digest.clone());
                    representation.byte_length = Some(hash.byte_count);
                    receipt = store::load(&connection, &asset.id, &hash.hex_digest)?;
                    representation.validation = match &receipt {
                        Some(r) if !receipt_current(r, asset) => evidence(
                            PreparationState::Stale,
                            "source identity, duration or validation recipe has changed",
                        ),
                        Some(r) if !r.decoded => evidence(
                            PreparationState::Quarantined,
                            r.diagnostic
                                .as_deref()
                                .unwrap_or("representation validation failed"),
                        ),
                        Some(r) => {
                            representation.receipt_id = Some(r.id.clone());
                            representation.validated_at = Some(r.validated_at.clone());
                            representation.validator = Some(r.validator.clone());
                            representation.usable_for_analysis = true;
                            evidence(PreparationState::Ready, "")
                        }
                        None if store::has_history(&connection, &asset.id)? => evidence(
                            PreparationState::Stale,
                            "representation content changed; full-decode readiness was invalidated",
                        ),
                        None => evidence(
                            PreparationState::Missing,
                            "full-content validation receipt is missing",
                        ),
                    };
                }
                Err(error) => {
                    representation.validation = evidence(
                        PreparationState::Unavailable,
                        &format!("representation identity cannot be verified: {error:#}"),
                    )
                }
            }
        }
        let video_validation = self.asset_video_validation(&asset.id)?;
        if !asset.removed && video_validation.status == AssetVideoValidationStatus::Quarantined {
            representation.usable_for_analysis = false;
            representation.validation = evidence(
                PreparationState::Quarantined,
                video_validation
                    .diagnostic
                    .as_deref()
                    .unwrap_or("catalog video is quarantined"),
            );
        } else if !asset.removed
            && video_validation.status == AssetVideoValidationStatus::Unverified
        {
            representation.usable_for_analysis = false;
            representation.validation = evidence(
                PreparationState::Missing,
                "catalog video eligibility has not been verified",
            );
        }
        let transcript = self.preparation_transcript(asset, &representation, receipt.as_ref())?;
        let mut moments = self.moment_evidence_page(&asset.id, 0, 2)?;
        let mut moment_details_truncated = false;
        if let Some(page) = &mut moments {
            for candidate in &mut page.artifact.candidates {
                moment_details_truncated |= candidate.evidence.len() > 4
                    || candidate.dialogue_boundaries.len() > 8
                    || candidate.uncertainty.len() > 4;
                candidate.evidence.truncate(4);
                candidate.dialogue_boundaries.truncate(8);
                candidate.uncertainty.truncate(4);
                let (reason, truncated) = clipped(&candidate.reason, 512);
                candidate.reason = reason;
                moment_details_truncated |= truncated;
            }
        }
        let mut questions = representation.validation.reasons.clone();
        questions.extend(transcript.readiness.reasons.clone());
        let moment_state = match &moments {
            None => {
                questions.push("ranked moment evidence is missing; paid preparation requires explicit policy authorization".into());
                PreparationState::Missing
            }
            Some(page)
                if !page.current
                    || representation.fingerprint.as_ref()
                        != Some(&page.artifact.provenance.representation_fingerprint) =>
            {
                questions.push("ranked moment evidence is stale".into());
                PreparationState::Stale
            }
            Some(page)
                if matches!(
                    page.artifact.status,
                    MomentStatus::Ready | MomentStatus::NoCandidates
                ) =>
            {
                PreparationState::Ready
            }
            Some(page) => {
                questions.extend(page.artifact.review_questions.clone());
                questions.push("moment coverage or candidate boundaries require review".into());
                PreparationState::ReviewRequired
            }
        };
        let mut orientation = PreparationOrientation {
            metadata_probed: asset.orientation_probed,
            metadata_rotation_degrees: asset.metadata_rotation_degrees,
            default_rotation_degrees: asset.default_rotation_degrees,
            effective_rotation_degrees: asset.rotation_degrees,
            confirmation: if asset.rotation_degrees != asset.default_rotation_degrees {
                "explicit_override"
            } else {
                "confirmation_not_tracked"
            }
            .into(),
            review_questions: vec![],
        };
        if !asset.orientation_probed {
            orientation.review_questions.push(
                "orientation metadata has not been probed; preparation never applies a rotation"
                    .into(),
            );
        }
        if asset.width < asset.height && asset.rotation_degrees == asset.default_rotation_degrees {
            orientation.review_questions.push("portrait dimensions use the inferred default; human orientation confirmation is not recorded".into());
        }
        questions.extend(orientation.review_questions.clone());
        let sidecar = if crate::table_exists(&connection, "camera_proxy_sidecars")? {
            self.camera_proxy_sidecar(&asset.id)?
        } else {
            None
        };
        let state = if asset.removed {
            PreparationState::Excluded
        } else if representation.validation.state != PreparationState::Ready {
            representation.validation.state
        } else if transcript.readiness.state != PreparationState::Ready {
            transcript.readiness.state
        } else if moment_state != PreparationState::Ready {
            moment_state
        } else if !orientation.review_questions.is_empty() {
            PreparationState::ReviewRequired
        } else {
            PreparationState::Ready
        };
        if asset.removed {
            questions =
                vec!["asset is explicitly excluded; preparation will not process it".into()];
        }
        questions.sort();
        questions.dedup();
        let review_questions_truncated = questions.len() > 8;
        questions.truncate(8);
        let technical_range = receipt
            .as_ref()
            .filter(|r| representation.usable_for_analysis && r.decoded)
            .map(|r| crate::moments::MomentSpan {
                start_ms: 0,
                end_ms: r.validated_end_ms,
            });
        Ok(PreparationAsset {
            asset_id: asset.id.clone(), asset_revision: asset.revision,
            original_available: asset.original_available, proxy_available: asset.proxy_available,
            state, review_questions: questions,
            review_questions_truncated, representation,
            camera_sidecar_status: sidecar.as_ref().map(|s| s.status.clone()),
            camera_sidecar_reason: sidecar.map(|s| if s.status == "registered" {
                if s.provenance.is_some() {
                    "verified camera sidecar supports offline review and explicit camera-proxy frame-pack; generated analysis recipe readiness is separate".into()
                } else {
                    "legacy camera sidecar supports review; re-register for verified time-mapped frame packs".into()
                }
            } else { clipped(&s.detail, 512).0 }),
            transcript, moments, moment_details_truncated, orientation, technical_range,
            range_certification: "technical source bounds only; sparse moment samples do not certify arbitrary edit ranges".into(),
        })
    }

    /// Observational inventory. No migration, media generation, downloads or paid calls.
    pub fn preparation_preflight(
        &self,
        scope: &PreparationScope,
        offset: usize,
        limit: usize,
    ) -> Result<PreparationManifest> {
        ensure!(
            offset <= 1_000_000 && (1..=50).contains(&limit),
            "preparation page requires offset <= 1000000 and limit 1..=50"
        );
        let scope = self.preparation_scope(scope)?;
        let (total_assets, ids) = self.preparation_ids(&scope, offset, limit)?;
        let items = ids
            .iter()
            .map(|id| Ok(self.preparation_asset(&self.get_asset(id)?)))
            .collect::<Result<Vec<_>>>()?;
        let page_outcome = if items.is_empty() {
            "empty_scope"
        } else if items.iter().all(|i| i.state == PreparationState::Excluded) {
            "excluded_only"
        } else if items.iter().all(|i| {
            matches!(
                i.state,
                PreparationState::Ready | PreparationState::Excluded
            )
        }) {
            "analysis_ready"
        } else {
            "review_required"
        };
        Ok(PreparationManifest {
            schema_version: PREPARATION_SCHEMA_VERSION.into(), schema_ready: store::schema_ready(&self.connection()?)?,
            project_id: scope.project_id.clone(), scope_fingerprint: digest(&scope)?,
            scope: PreparationScopeSummary { all_assets: scope.all_assets, selected_asset_count: total_assets },
            total_assets, offset, limit,
            next_offset: (offset + limit < total_assets).then_some(offset + limit),
            page_outcome: page_outcome.into(), items,
            policy_references: vec![
                crate::STT_RECIPE_KEY.into(), crate::moments::RECIPE_VERSION.into(),
                PREPARATION_VALIDATION_RECIPE.into(), "paid work requires explicit scope/provider/model/payload/budget; no implicit fallback or uncertain-request retry".into(),
            ],
        })
    }

    fn adopt_preparation_transcript(
        &self,
        asset: &Asset,
        receipt: &mut ValidationReceipt,
        produced: bool,
    ) -> Result<bool> {
        let Some(header) = self.preparation_transcript_header(asset)? else {
            return Ok(false);
        };
        if header.recipe != crate::STT_RECIPE_KEY
            || !header.timings_valid
            || header.max_end_ms > receipt.validated_end_ms
            || !matches!(header.status.as_str(), "speech" | "no_speech" | "no_audio")
            || (header.status == "no_speech"
                && header.outcome_reason == "legacy_no_speech_unverified")
            || (header.status == "speech" && header.segment_count == 0)
            || !self
                .proxy_fingerprint(&asset.id)
                .is_ok_and(|f| f == header.source_fingerprint)
        {
            return Ok(false);
        }
        if receipt
            .transcript
            .as_ref()
            .is_some_and(|b| b.artifact_fingerprint == header.fingerprint)
        {
            return Ok(false);
        }
        // A legacy sampled fingerprint cannot authorize carrying a known old
        // transcript across a full-content replacement with matching samples.
        if !produced
            && store::bound_to_other_representation(
                &self.connection()?,
                &asset.id,
                &receipt.representation_fingerprint,
                &header.fingerprint,
            )?
        {
            return Ok(false);
        }
        receipt.transcript = Some(TranscriptBinding {
            artifact_fingerprint: header.fingerprint,
            origin: if produced {
                "prepared_current_input"
            } else {
                "adopted_existing_sampled_recipe"
            }
            .into(),
        });
        receipt.transcript_error = None;
        store::save(&self.connection()?, receipt)?;
        Ok(true)
    }

    fn validate_preparation_proxy(
        &self,
        asset_id: &str,
        project_id: &str,
        revalidate: bool,
    ) -> Result<(ValidationReceipt, bool)> {
        let asset = self.get_asset(asset_id)?;
        ensure!(
            asset.project_id == project_id,
            "asset left the authorized project scope"
        );
        ensure!(!asset.removed, "asset was excluded before validation");
        ensure!(
            crate::has_current_proxy(&asset),
            "a current raw v3 proxy is required"
        );
        let path = Self::require_proxy(&asset)?;
        let fingerprint = crate::full_file_blake3(&path, &|| false)?.hex_digest;
        if !revalidate
            && let Some(mut receipt) = store::load(&self.connection()?, asset_id, &fingerprint)?
            && receipt_current(&receipt, &asset)
        {
            let changed = if receipt.decoded {
                self.adopt_preparation_transcript(&asset, &mut receipt, false)?
            } else {
                false
            };
            return Ok((receipt, changed));
        }
        let source = self.analysis_proxy_source(&asset);
        let validated_end_ms = source.as_ref().map_or(0, |source| source.duration_ms);
        let (decoded, diagnostic) = match source {
            Ok(source) => {
                ensure!(
                    source.fingerprint == fingerprint,
                    "representation changed during validation"
                );
                match crate::verify_camera_sidecar_decode(&path) {
                    Ok(()) => (true, None),
                    Err(error) if format!("{error:#}").contains("truncated_or_unreadable") => {
                        (false, Some(clipped(&format!("{error:#}"), 1024).0))
                    }
                    Err(error) => {
                        return Err(error.context("full-decode validation did not complete"));
                    }
                }
            }
            Err(error) => (
                false,
                Some(
                    clipped(
                        &format!("representation recipe validation failed: {error:#}"),
                        1024,
                    )
                    .0,
                ),
            ),
        };
        ensure!(
            crate::full_file_blake3(&path, &|| false)?.hex_digest == fingerprint,
            "representation changed during full decode"
        );
        let refreshed = self.get_asset(asset_id)?;
        ensure!(
            !refreshed.removed
                && refreshed.project_id == asset.project_id
                && refreshed.fingerprint == asset.fingerprint
                && duration_ms(&refreshed) == duration_ms(&asset)
                && refreshed.proxy_path == asset.proxy_path,
            "asset scope or representation changed during validation"
        );
        let validator = crate::prerequisites::run_bounded_command(
            &crate::resolve_tool_path("ffmpeg"),
            &[std::ffi::OsString::from("-version")],
            std::time::Duration::from_secs(3),
            16 * 1024,
        )
        .ok()
        .filter(|out| out.status.success())
        .map(|out| {
            clipped(
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .next()
                    .unwrap_or(""),
                256,
            )
            .0
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "version unavailable".into());
        let mut receipt = ValidationReceipt {
            id: digest(&(
                asset_id,
                &asset.fingerprint,
                duration_ms(&asset),
                &fingerprint,
                PREPARATION_VALIDATION_RECIPE,
            ))?,
            asset_id: asset_id.into(),
            project_id: project_id.into(),
            source_fingerprint: asset.fingerprint.clone(),
            source_duration_ms: duration_ms(&asset)
                .ok_or_else(|| anyhow!("invalid source duration"))?,
            validated_end_ms,
            representation_fingerprint: fingerprint,
            representation_recipe: PROXY_RECIPE.into(),
            validation_recipe: PREPARATION_VALIDATION_RECIPE.into(),
            validator,
            decoded,
            diagnostic,
            validated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true),
            transcript: None,
            transcript_error: None,
        };
        store::save(&self.connection()?, &receipt)?;
        if decoded {
            self.adopt_preparation_transcript(&asset, &mut receipt, false)?;
        }
        Ok((receipt, true))
    }

    fn prepare_local_asset(
        &self,
        asset_id: &str,
        project_id: &str,
        options: &PreparationRunOptions,
        engine: &mut Option<Box<dyn crate::analysis::engine::SttEngine>>,
        notify: &mut dyn FnMut(&str) -> ControlFlow<()>,
    ) -> Result<ControlFlow<(), bool>> {
        let mut changed = false;
        let mut generated = false;
        let mut asset = self.get_asset(asset_id)?;
        ensure!(
            asset.project_id == project_id,
            "asset left the authorized project scope"
        );
        if asset.removed {
            return Ok(ControlFlow::Continue(false));
        }
        let video = self.asset_video_validation(asset_id)?;
        if video.status == AssetVideoValidationStatus::Quarantined && !options.revalidate {
            return Ok(ControlFlow::Continue(false));
        }
        if video.status != AssetVideoValidationStatus::Usable {
            if notify("video_validation").is_break() {
                return Ok(ControlFlow::Break(()));
            }
            if !self.preparation_asset_active(asset_id, project_id)? {
                return Ok(ControlFlow::Continue(changed));
            }
            if self.validate_asset_video(asset_id)?.status != AssetVideoValidationStatus::Usable {
                return Ok(ControlFlow::Continue(true));
            }
            changed = true;
        }
        if !crate::has_current_proxy(&asset) && options.generate_proxies {
            if notify("proxy").is_break() {
                return Ok(ControlFlow::Break(()));
            }
            if !self.preparation_asset_active(asset_id, project_id)? {
                return Ok(ControlFlow::Continue(changed));
            }
            self.generate_proxy_with_orientation_policy(asset_id, false, |_| {})?;
            asset = self.get_asset(asset_id)?;
            generated = true;
            changed = true;
        }
        if !crate::has_current_proxy(&asset) {
            return Ok(ControlFlow::Continue(changed));
        }
        if notify("full_decode_validation").is_break() {
            return Ok(ControlFlow::Break(()));
        }
        if !self.preparation_asset_active(asset_id, project_id)? {
            return Ok(ControlFlow::Continue(changed));
        }
        let proxy_claim = self.claim_representation(asset_id, RepresentationKind::Proxy)?;
        let (mut receipt, validated) =
            self.validate_preparation_proxy(asset_id, project_id, options.revalidate)?;
        changed |= validated;
        if !receipt.decoded && options.generate_proxies && !generated && asset.original_available {
            drop(proxy_claim);
            if notify("proxy_repair").is_break() {
                return Ok(ControlFlow::Break(()));
            }
            if !self.preparation_asset_active(asset_id, project_id)? {
                return Ok(ControlFlow::Continue(changed));
            }
            self.generate_proxy_with_orientation_policy(asset_id, false, |_| {})?;
            if notify("full_decode_validation").is_break() {
                return Ok(ControlFlow::Break(()));
            }
            if !self.preparation_asset_active(asset_id, project_id)? {
                return Ok(ControlFlow::Continue(changed));
            }
            let _claim = self.claim_representation(asset_id, RepresentationKind::Proxy)?;
            (receipt, _) = self.validate_preparation_proxy(asset_id, project_id, false)?;
            // Subsequent transcription must keep ownership of the rebuilt proxy.
            return self.prepare_transcript(asset_id, options, engine, receipt, true, notify);
        }
        if !receipt.decoded {
            return Ok(ControlFlow::Continue(changed));
        }
        self.prepare_transcript(asset_id, options, engine, receipt, changed, notify)
    }

    fn prepare_transcript(
        &self,
        asset_id: &str,
        options: &PreparationRunOptions,
        engine: &mut Option<Box<dyn crate::analysis::engine::SttEngine>>,
        mut receipt: ValidationReceipt,
        mut changed: bool,
        notify: &mut dyn FnMut(&str) -> ControlFlow<()>,
    ) -> Result<ControlFlow<(), bool>> {
        if !receipt.decoded || !options.transcribe {
            return Ok(ControlFlow::Continue(changed));
        }
        let asset = self.get_asset(asset_id)?;
        ensure!(!asset.removed, "asset was excluded before transcription");
        let representation = PreparationRepresentation {
            kind: "generated_proxy".into(),
            recipe: actual_proxy_recipe(&asset),
            required_recipe: PROXY_RECIPE.into(),
            fingerprint: Some(receipt.representation_fingerprint.clone()),
            fingerprint_kind: "full_blake3".into(),
            byte_length: None,
            validation: evidence(PreparationState::Ready, ""),
            receipt_id: Some(receipt.id.clone()),
            validated_at: Some(receipt.validated_at.clone()),
            validator: Some(receipt.validator.clone()),
            usable_for_analysis: true,
        };
        let transcript = self.preparation_transcript(&asset, &representation, Some(&receipt))?;
        if matches!(
            transcript.readiness.state,
            PreparationState::Ready | PreparationState::ReviewRequired
        ) {
            return Ok(ControlFlow::Continue(changed));
        }
        if notify("transcription").is_break() {
            return Ok(ControlFlow::Break(()));
        }
        if !self.preparation_asset_active(asset_id, &receipt.project_id)? {
            return Ok(ControlFlow::Continue(changed));
        }
        let proxy_path = Self::require_proxy(&asset)?;
        let source_fingerprint = self.proxy_fingerprint(asset_id)?;
        let audio_present = match self.transcription_audio_present(&proxy_path) {
            Ok(present) => present,
            Err(error) => {
                let message = format!("{error:#}");
                crate::analysis::store::save_transcript(
                    &self.connection()?,
                    &crate::analysis::stt::failed_transcript(
                        asset_id,
                        &source_fingerprint,
                        message.clone(),
                    ),
                )?;
                receipt.transcript_error = Some(clipped(&message, 1024).0);
                store::save(&self.connection()?, &receipt)?;
                return Err(error);
            }
        };
        if audio_present {
            if engine.is_none() {
                let directory = crate::analysis::engine::model_dir(&self.database_path)?;
                let whisper = directory.join(crate::analysis::engine::WHISPER_MODEL.file_name);
                let vad = directory.join(crate::analysis::engine::VAD_MODEL.file_name);
                ensure!(
                    whisper.is_file() && vad.is_file(),
                    "speech models are not installed; preparation never downloads them"
                );
                let selection = self.resolve_resource_selection(
                    crate::SPEECH_REPRESENTATION_KIND,
                    crate::STT_RECIPE_KEY,
                    None,
                    None,
                    &crate::ResourceOverrides::default(),
                )?;
                *engine = Some(Box::new(
                    crate::analysis::whisper_engine::WhisperEngine::with_threads(
                        &whisper,
                        &vad,
                        selection.vad_threads,
                        selection.decode_threads,
                    )?,
                ));
            }
            if let Err(error) = self.transcribe_source_with_engine(
                asset_id,
                &proxy_path,
                &source_fingerprint,
                engine.as_deref().expect("installed engine"),
            ) {
                let message = format!("{error:#}");
                let (speech_seconds, window_count, provider_segment_count) =
                    crate::analysis::stt::failure_evidence(&error);
                crate::analysis::store::save_transcript(
                    &self.connection()?,
                    &crate::analysis::stt::failed_transcript_with_evidence(
                        asset_id,
                        &source_fingerprint,
                        message.clone(),
                        speech_seconds,
                        window_count,
                        provider_segment_count,
                    ),
                )?;
                receipt.transcript_error = Some(clipped(&message, 1024).0);
                store::save(&self.connection()?, &receipt)?;
                return Err(error);
            }
        } else {
            let stored = crate::analysis::stt::no_audio_transcript(asset_id, &source_fingerprint);
            crate::analysis::store::save_transcript(&self.connection()?, &stored)?;
        }
        let current = self.get_asset(asset_id)?;
        ensure!(
            !current.removed
                && receipt_current(&receipt, &current)
                && current.project_id == asset.project_id
                && current.proxy_path == asset.proxy_path,
            "asset changed during transcription"
        );
        let path = Self::require_proxy(&current)?;
        ensure!(
            crate::full_file_blake3(&path, &|| false)?.hex_digest
                == receipt.representation_fingerprint,
            "representation changed during transcription"
        );
        changed |= self.adopt_preparation_transcript(&current, &mut receipt, true)?;
        Ok(ControlFlow::Continue(changed))
    }

    /// Explicit sequential preparation. Cancellation is checked between local
    /// stages and by the existing moment runner before each provider request.
    pub fn prepare_for_edit<F>(
        &self,
        scope: &PreparationScope,
        options: &PreparationRunOptions,
        notify: F,
    ) -> Result<PreparationRunReport>
    where
        F: FnMut(PreparationProgress) -> ControlFlow<()>,
    {
        self.prepare_with_services(scope, options, None, None, notify)
    }

    pub(super) fn prepare_with_services<F>(
        &self,
        scope: &PreparationScope,
        options: &PreparationRunOptions,
        mut engine: Option<Box<dyn crate::analysis::engine::SttEngine>>,
        provider: Option<&dyn crate::VlmProvider>,
        mut notify: F,
    ) -> Result<PreparationRunReport>
    where
        F: FnMut(PreparationProgress) -> ControlFlow<()>,
    {
        ensure!(
            !self.read_only,
            "preparation requires an explicitly writable catalog"
        );
        let scope = self.preparation_scope(scope)?;
        if let Some(policy) = &options.moments {
            policy.validate()?;
            ensure!(
                !policy.force && !policy.retry_uncertain,
                "use explicit moments recovery commands for force or uncertain-request retries"
            );
        }
        let (total, ids) = self.preparation_ids(&scope, 0, MAX_SCOPE + 1)?;
        ensure!(
            total <= MAX_SCOPE,
            "preparation run exceeds 10000 assets; select a bounded asset scope"
        );
        if let Some(policy_id) = options
            .moments
            .as_ref()
            .and_then(|policy| policy.policy_id.as_deref())
        {
            let state = self
                .authorized_policy_for_bulk(policy_id, crate::AnalysisRunKind::MomentDiscovery)?;
            let mut requested = ids.clone();
            let mut authorized = state
                .authorization
                .policy
                .scope
                .iter()
                .map(|asset| asset.asset_id.clone())
                .collect::<Vec<_>>();
            requested.sort();
            authorized.sort();
            ensure!(
                requested == authorized,
                "preparation scope does not exactly match the authorized policy"
            );
        }
        let mut report = PreparationRunReport {
            run_id: uuid::Uuid::new_v4().to_string(),
            total_assets: total,
            processed: 0,
            reused: 0,
            excluded: 0,
            skipped: 0,
            failed: 0,
            cancelled: false,
            failures: vec![],
            failures_truncated: false,
            moments: None,
            manifest: self.preparation_preflight(&scope, 0, 10)?,
        };
        // One shared local-heavy slot covers proxy generation, full-decode
        // validation and speech transcription. It is released before any pure
        // cloud/network moments phase below.
        let local_heavy = match self.wait_for_local_heavy(|| {
            notify(PreparationProgress {
                run_id: report.run_id.clone(),
                asset_id: String::new(),
                stage: "waiting_for_resource".into(),
                processed: 0,
                total_assets: total,
            })
            .is_break()
        })? {
            Some(lease) => lease,
            None => {
                report.cancelled = true;
                return Ok(report);
            }
        };
        let mut eligible = Vec::new();
        for id in ids {
            let asset = self.get_asset(&id)?;
            if asset.removed {
                report.excluded += 1;
                continue;
            }
            let processed = report.processed;
            let mut step = |stage: &str| {
                notify(PreparationProgress {
                    run_id: report.run_id.clone(),
                    asset_id: id.clone(),
                    stage: stage.into(),
                    processed,
                    total_assets: total,
                })
            };
            if step("preflight").is_break() {
                report.cancelled = true;
                break;
            }
            match self.prepare_local_asset(&id, &scope.project_id, options, &mut engine, &mut step)
            {
                Ok(ControlFlow::Break(())) => {
                    report.cancelled = true;
                    break;
                }
                Ok(ControlFlow::Continue(changed)) => {
                    // A progress callback may exclude an asset before a local
                    // stage starts. Report that refreshed scope truthfully.
                    if !self.preparation_asset_active(&id, &scope.project_id)? {
                        report.excluded += 1;
                        continue;
                    }
                    report.processed += 1;
                    if !changed {
                        report.reused += 1;
                    }
                    if let Some(policy) = &options.moments {
                        let inspected = self.preparation_asset(&self.get_asset(&id)?);
                        if inspected.representation.usable_for_analysis
                            && (policy.payload == crate::MomentPayload::Frames
                                || inspected.transcript.readiness.state == PreparationState::Ready)
                        {
                            eligible.push(id);
                        }
                    }
                }
                Err(error) if error.downcast_ref::<crate::RepresentationBusy>().is_some() => {
                    report.skipped += 1
                }
                Err(error) => {
                    report.failed += 1;
                    if report.failures.len() < 50 {
                        report.failures.push(PreparationFailure {
                            asset_id: id,
                            stage: "local_preparation".into(),
                            reason: clipped(&format!("{error:#}"), 1024).0,
                        });
                    } else {
                        report.failures_truncated = true;
                    }
                }
            }
        }
        drop(local_heavy);
        if !report.cancelled
            && !eligible.is_empty()
            && let Some(policy) = &options.moments
        {
            // One call owns one budget across the exact eligible subset. No
            // per-asset budget resets and no second paid-work accounting path.
            let on_progress = |event: crate::MomentProgress| {
                notify(PreparationProgress {
                    run_id: report.run_id.clone(),
                    asset_id: event.asset_id,
                    stage: format!("moments:{}", event.phase),
                    processed: report.processed,
                    total_assets: total,
                })
            };
            let paid = match provider {
                Some(provider) => self.discover_moments_for_assets_with_provider(
                    &scope.project_id,
                    &eligible,
                    policy,
                    provider,
                    on_progress,
                )?,
                None => self.discover_moments_for_assets(
                    &scope.project_id,
                    &eligible,
                    policy,
                    on_progress,
                )?,
            };
            report.cancelled |= paid.cancelled;
            report.moments = Some(paid);
        }
        report.manifest = self.preparation_preflight(&scope, 0, 10)?;
        Ok(report)
    }

    /// A bounded transcript projection, with the same provenance/readiness as the manifest.
    pub fn preparation_transcript_page(
        &self,
        asset_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<PreparationTranscriptPage> {
        ensure!(
            offset <= 1_000_000 && (1..=50).contains(&limit),
            "transcript page requires offset <= 1000000 and limit 1..=50"
        );
        let asset = self.get_asset(asset_id)?;
        let inspected = self.preparation_asset(&asset);
        let connection = self.connection()?;
        let mut segments = Vec::new();
        if crate::table_exists(&connection, "transcript_segments")? {
            let mut statement = connection.prepare(
                "SELECT start_ms,end_ms,language,text FROM transcript_segments WHERE asset_id=?1 ORDER BY segment_index LIMIT ?2 OFFSET ?3"
            )?;
            let rows =
                statement.query_map(params![asset_id, limit as i64, offset as i64], |row| {
                    let text: String = row.get(3)?;
                    let (text, text_truncated) = clipped(&text, 4096);
                    Ok(PreparationSpeechSegment {
                        start_ms: row.get(0)?,
                        end_ms: row.get(1)?,
                        language: clipped(&row.get::<_, String>(2)?, 64).0,
                        text,
                        text_truncated,
                    })
                })?;
            segments = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        }
        Ok(PreparationTranscriptPage {
            schema_version: PREPARATION_SCHEMA_VERSION.into(),
            asset_id: asset_id.into(),
            next_offset: (offset + limit < inspected.transcript.segment_count)
                .then_some(offset + limit),
            transcript: inspected.transcript,
            segments,
            offset,
            limit,
        })
    }
}
