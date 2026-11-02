//! Read-only integrity review of a completed movie. This is deliberately a
//! different contract from a currently renderable v1 review-target handoff.
use super::{PreviewArtifactState, review_target};
use crate::{Catalog, ReviewTargetHandoff, ReviewTargetSnapshot, full_file_blake3};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HistoricalReviewPurpose {
    #[serde(rename = "historicalRenderedArtifactReview")]
    HistoricalRenderedArtifactReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HistoricalReviewUnresolved {
    #[serde(rename = "notEstablishedByThisReview")]
    NotEstablishedByThisReview,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoricalRenderedArtifactReview {
    pub schema_version: u32,
    pub purpose: HistoricalReviewPurpose,
    pub target_digest: String,
    /// Same immutable movie/mapping locators. This does not make it a v1 handoff.
    pub target: ReviewTargetSnapshot,
    pub verified_at: String,
    pub output_matches: bool,
    pub mapping_matches_revision: bool,
    /// Prior decode receipt is bound to the freshly hashed bytes, not rerun here.
    pub stored_fully_decoded: bool,
    pub current_source_verification: HistoricalReviewUnresolved,
    pub current_constraints_verification: HistoricalReviewUnresolved,
    pub current_plan_renderability: HistoricalReviewUnresolved,
}

impl HistoricalRenderedArtifactReview {
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= review_target::REVIEW_TARGET_MAX_BYTES,
            "invalid input: historical review exceeds 16 MiB"
        );
        let review: Self = serde_json::from_slice(bytes)?;
        review.validate()?;
        Ok(review)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1,
            crate::ReviewTargetError::UnsupportedVersion
        );
        ensure!(
            self.output_matches && self.mapping_matches_revision && self.stored_fully_decoded,
            "invalid input: historical movie/mapping integrity is incomplete"
        );
        chrono::DateTime::parse_from_rfc3339(&self.verified_at)?;
        review_target::validate_snapshot(&self.target, &self.target_digest)
    }
}

impl Catalog {
    /// No migrations, source reads, plan mutations, new render, activation or
    /// current-input/renderability assertion. Catalog may be opened read-only.
    pub fn review_historical_rendered_artifact(
        &self,
        artifact_id: &str,
        cancel: &dyn Fn() -> bool,
    ) -> Result<HistoricalRenderedArtifactReview> {
        ensure!(!cancel(), "historical review cancelled");
        // Read-only catalog access still coordinates expensive hashing through
        // the same host scheduling lease; no catalog write/migration is needed.
        let _heavy =
            crate::acquire_local_heavy_at_cancellable(&self.local_heavy_lock_path(), cancel)?;
        let status = self.preview_artifact_status_cancellable(artifact_id, false, cancel)?;
        ensure!(
            status.observed_state == PreviewArtifactState::Complete && status.file_present,
            crate::ReviewTargetError::ArtifactNotVerified
        );
        let artifact = status.artifact;
        ensure!(
            artifact.kind == "fullPreview" && artifact.excerpt.is_none(),
            "invalid input: historical review requires a completed full preview"
        );
        let validation = artifact
            .validation
            .as_ref()
            .context("missing decoded receipt")?;
        ensure!(
            validation.fully_decoded,
            "invalid receipt: movie was not fully decoded"
        );
        let path = std::path::Path::new(
            &artifact
                .result
                .as_ref()
                .context("missing output")?
                .output_path,
        );
        let observed = full_file_blake3(path, cancel)?;
        ensure!(
            super::preview_artifacts::output_is_owned_file(self, &artifact)
                && observed.hex_digest == validation.output_fingerprint
                && observed.byte_count == validation.byte_length,
            crate::ReviewTargetError::ArtifactNotVerified
        );
        // Require the receipt's picture mapping to equal the immutable revision,
        // even if a new validator no longer permits rendering that revision.
        let plan = self.edit_plan_revision(&artifact.revision_id)?;
        ensure!(
            super::preview_media::historical_plan_identity_matches(
                self,
                &plan,
                &artifact
                    .identity
                    .as_ref()
                    .context("missing immutable plan identity")?
                    .plan_digest
            )?,
            "invalid receipt: canonical plan digest differs from completed render"
        );
        let target = review_target::snapshot_from_artifact(artifact, &plan, None, cancel)?;
        let review = HistoricalRenderedArtifactReview {
            schema_version: 1,
            purpose: HistoricalReviewPurpose::HistoricalRenderedArtifactReview,
            target_digest: ReviewTargetHandoff::snapshot_digest(&target)?,
            target,
            verified_at: crate::now(),
            output_matches: true,
            mapping_matches_revision: true,
            stored_fully_decoded: true,
            current_source_verification: HistoricalReviewUnresolved::NotEstablishedByThisReview,
            current_constraints_verification:
                HistoricalReviewUnresolved::NotEstablishedByThisReview,
            current_plan_renderability: HistoricalReviewUnresolved::NotEstablishedByThisReview,
        };
        review.validate()?;
        ensure!(
            serde_json::to_vec(&review)?.len() <= review_target::REVIEW_TARGET_MAX_BYTES,
            "invalid input: historical review exceeds 16 MiB"
        );
        Ok(review)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AssetPatch, EditPlanOperation, PreviewRequest, editplan::inspection::tests::fixture,
    };

    #[test]
    fn synthetic_historical_contract_is_distinct_and_bounded() {
        let bytes = include_bytes!("../../tests/fixtures/review-target/historical-v1.json");
        let review = HistoricalRenderedArtifactReview::from_json(bytes).unwrap();
        assert!(ReviewTargetHandoff::from_json(bytes).is_err());
        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../docs/contracts/historical-review-v1.schema.json"
        ))
        .unwrap();
        jsonschema::validator_for(&schema)
            .unwrap()
            .validate(&serde_json::from_slice(bytes).unwrap())
            .unwrap();
        let mut bad = review;
        bad.mapping_matches_revision = false;
        assert!(bad.validate().is_err());
        assert!(
            HistoricalRenderedArtifactReview::from_json(&vec![
                b' ';
                review_target::REVIEW_TARGET_MAX_BYTES
                    + 1
            ])
            .is_err()
        );
    }

    #[test]
    fn historical_integrity_is_read_only_separate_and_rejects_mapping_or_movie_damage() {
        let (_dir, catalog, asset_id) = fixture();
        let asset = catalog.get_asset(&asset_id).unwrap();
        let plan = catalog
            .create_edit_plan_from_base(
                &asset.project_id,
                &[EditPlanOperation {
                    operation_id: "cut_historical".into(),
                    position: 0,
                    asset_id: asset_id.clone(),
                    source_in_seconds: 0.0,
                    source_out_seconds: 1.0,
                    rationale: "synthetic".into(),
                    audio: None,
                }],
                None,
                None,
                None,
            )
            .unwrap();
        let rendered = catalog
            .render_preview(
                &PreviewRequest {
                    revision_id: plan.revision_id.clone(),
                    max_height: 144,
                },
                &mut |_| {},
                &|| false,
            )
            .unwrap();
        let id = rendered.artifact_id.unwrap();
        catalog
            .update_asset(
                &asset_id,
                &AssetPatch {
                    notes: asset.notes,
                    labels: asset.labels,
                    removed: asset.removed,
                    rotation_degrees: 180,
                    expected_revision: Some(asset.revision),
                },
            )
            .unwrap();
        assert!(catalog.export_review_target(&id, None, &|| false).is_err());
        let stored = catalog
            .preview_artifact_status(&id, false)
            .unwrap()
            .artifact
            .identity
            .unwrap()
            .plan_digest;
        let legacy = serde_json::json!({"revisionId":plan.revision_id,"projectId":plan.project_id,
            "parentRevisionId":plan.parent_revision_id,"scriptRevisionId":plan.script_revision_id,"operations":plan.operations});
        let legacy_digest = blake3::hash(&serde_json::to_vec(&legacy).unwrap())
            .to_hex()
            .to_string();
        assert!(
            super::super::preview_media::historical_plan_identity_matches(
                &catalog,
                &plan,
                &legacy_digest
            )
            .unwrap()
        );
        let mut altered = plan.clone();
        altered.operations[0].rationale = "changed immutable decision".into();
        assert!(
            !super::super::preview_media::historical_plan_identity_matches(
                &catalog, &altered, &stored
            )
            .unwrap()
        );
        let ro = Catalog::open_read_only_existing(&catalog.database_path).unwrap();
        let review = ro
            .review_historical_rendered_artifact(&id, &|| false)
            .unwrap();
        assert_eq!(
            review.current_plan_renderability,
            HistoricalReviewUnresolved::NotEstablishedByThisReview
        );
        let json = serde_json::to_vec(&review).unwrap();
        assert_eq!(
            HistoricalRenderedArtifactReview::from_json(&json).unwrap(),
            review
        );
        assert!(ReviewTargetHandoff::from_json(&json).is_err());
        assert_eq!(ro.edit_plan_revision(&plan.revision_id).unwrap(), plan);
        assert_eq!(ro.active_edit_plan(&asset.project_id).unwrap(), None);
        assert!(
            ro.review_historical_rendered_artifact(&id, &|| true)
                .is_err()
        );
        // Damage only the generated test receipt: metadata cannot assert a
        // different immutable operation or an incomplete movie clock.
        let raw: String = catalog
            .connection()
            .unwrap()
            .query_row(
                "SELECT validation_json FROM preview_artifacts WHERE artifact_id=?1",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        let mut corrupt: serde_json::Value = serde_json::from_str(&raw).unwrap();
        corrupt["cuts"][0]["sourceInSeconds"] = serde_json::json!(0.1);
        catalog
            .connection()
            .unwrap()
            .execute(
                "UPDATE preview_artifacts SET validation_json=?2 WHERE artifact_id=?1",
                rusqlite::params![id, corrupt.to_string()],
            )
            .unwrap();
        assert!(
            ro.review_historical_rendered_artifact(&id, &|| false)
                .is_err()
        );
        catalog
            .connection()
            .unwrap()
            .execute(
                "UPDATE preview_artifacts SET validation_json=?2 WHERE artifact_id=?1",
                rusqlite::params![id, raw],
            )
            .unwrap();
        std::fs::write(&rendered.output_path, b"changed generated movie").unwrap();
        assert!(
            ro.review_historical_rendered_artifact(&id, &|| false)
                .is_err()
        );
        std::fs::remove_file(&rendered.output_path).unwrap();
        assert!(
            ro.review_historical_rendered_artifact(&id, &|| false)
                .is_err()
        );
    }
}
