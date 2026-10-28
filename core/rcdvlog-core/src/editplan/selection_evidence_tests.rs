//! Canonical moment evidence selection stays local and tracks cache/currentness.
use super::*;
use crate::{
    Catalog,
    moments::{
        self, CoverageLevel, CoverageSegment, EditProfile, MomentArtifact, MomentCandidate,
        MomentPayload, MomentProvenance, MomentSpan, MomentStatus,
    },
};

const ID: &str = "canonical-moment-test-id";

fn artifact(catalog: &Catalog, asset_id: &str, identity: &str) -> MomentArtifact {
    let asset = catalog.get_asset(asset_id).unwrap();
    let source = catalog
        .validated_review_proxy_source(&asset, &|| false)
        .unwrap();
    MomentArtifact {
        asset_id: asset_id.into(),
        identity: identity.into(),
        status: MomentStatus::Ready,
        candidates: vec![MomentCandidate {
            id: "candidate-1".into(),
            rank: 1,
            score: 0.95,
            core: MomentSpan {
                start_ms: 500,
                end_ms: 1500,
            },
            payoff: MomentSpan {
                start_ms: 1000,
                end_ms: 1500,
            },
            setup: None,
            context: None,
            allowed_range: MomentSpan {
                start_ms: 0,
                end_ms: source.duration_ms,
            },
            reason: "A visible action reaches its payoff.".into(),
            event: "action payoff".into(),
            role: "action".into(),
            evidence: vec![],
            uncertainty: vec![],
            dialogue_boundaries: vec![],
            refined: false,
            long_hold: false,
        }],
        coverage: vec![CoverageSegment {
            span: MomentSpan {
                start_ms: 0,
                end_ms: source.duration_ms,
            },
            level: CoverageLevel::Sampled,
            sample_times_ms: vec![0, source.duration_ms / 2],
            max_gap_ms: source.duration_ms / 2,
        }],
        provenance: MomentProvenance {
            requested_provider: "fixture-only-no-call".into(),
            payload: MomentPayload::Frames,
            provider: "fixture-provider".into(),
            requested_model: "requested-fixture-model".into(),
            actual_model: "actual-fixture-model".into(),
            prompt_version: moments::MOMENT_PROMPT_VERSION.into(),
            schema_version: moments::MOMENT_SCHEMA_VERSION.into(),
            recipe_version: moments::MOMENT_RECIPE_VERSION.into(),
            representation: "proxy-v3".into(),
            representation_fingerprint: source.fingerprint,
            time_mapping: "proxy-v3.source-ms.identity.v1".into(),
            source_duration_ms: source.duration_ms,
            transcript_fingerprint: None,
            observation_fingerprint: None,
            profile: EditProfile::default(),
            configuration_hash: "fixture-policy-fingerprint".into(),
            cost_usd: 0.0,
            usage_complete: false,
            receipts: vec![],
            estimated_cost_usd: None,
            cost_basis: None,
            native_source: None,
        },
        review_questions: vec![],
    }
}
fn save(catalog: &Catalog, asset: &str, identity: &str, item: &MomentArtifact) {
    moments::store::save_artifact(&catalog.connection().unwrap(), asset, identity, item).unwrap();
}
fn request(catalog: &Catalog, asset: &str, identity: &str) -> SequenceSelectionRequest {
    let project_id = catalog.get_asset(asset).unwrap().project_id;
    SequenceSelectionRequest {
        project_id,
        expected_base_revision_id: None,
        expected_active_revision_id: None,
        input: SequenceSelectionInput {
            version: 1,
            decision_owner: crate::RangeDecisionOwner::Human,
            goal: "Show the action and its payoff".into(),
            sequences: vec![SequenceIntent {
                sequence_id: "sequence-1".into(),
                goal: "Action payoff".into(),
                pacing: SequencePacingProfile::default(),
                sources: vec![SelectionMomentSource::Current {
                    asset_id: asset.into(),
                    expected_moment_identity: identity.into(),
                    candidate_ids: vec![],
                }],
                role_claims: vec![],
                retained_choices: vec![],
                hero_candidates: vec![],
                budget_seconds: None,
                hard_budget: false,
                major_place_change: false,
                place_context: None,
            }],
            include: vec![],
            exclude: vec![],
            counts: vec![],
            relationships: vec![],
            budget_seconds: None,
            hard_budget: false,
            minimum_score: 0.0,
            repetition_penalty: 0.0,
            coverage_bonus: 0.0,
        },
    }
}

#[test]
fn canonical_current_evidence_is_reported_and_same_identity_row_change_invalidates_cache() {
    let (_dir, catalog, asset) = crate::editplan::inspection::tests::fixture();
    catalog.generate_proxy(&asset).unwrap();
    catalog.validate_asset_video(&asset).unwrap();
    let identity = ID;
    let first = artifact(&catalog, &asset, identity);
    save(&catalog, &asset, identity, &first);
    // Explicit review is allowed to select an otherwise labeling-hidden asset.
    let prior = catalog.get_asset(&asset).unwrap();
    catalog
        .update_asset(
            &asset,
            &crate::AssetPatch {
                notes: prior.notes,
                labels: prior.labels,
                removed: true,
                rotation_degrees: prior.rotation_degrees,
                expected_revision: Some(prior.revision),
            },
        )
        .unwrap();
    let req = request(&catalog, &asset, identity);
    let drafted = catalog.create_sequence_selection(&req).unwrap();
    assert_eq!(drafted.report.earned_seconds, 1.0);
    let assignment = &drafted.report; // report has current requested duration, metadata below carries provenance
    assert_eq!(assignment.duration_basis, "requestedSourceRanges");
    let metadata = catalog
        .edit_plan_selection_metadata(&drafted.revision.revision_id)
        .unwrap()
        .unwrap();
    let evidence = &metadata.assignments[0].evidence;
    assert_eq!(evidence.origin, "canonicalMoments");
    assert_eq!(evidence.provider.as_deref(), Some("fixture-provider"));
    assert_eq!(
        evidence.requested_model.as_deref(),
        Some("requested-fixture-model")
    );
    assert_eq!(
        evidence.actual_model.as_deref(),
        Some("actual-fixture-model")
    );
    assert_eq!(
        evidence.prompt_version.as_deref(),
        Some(moments::MOMENT_PROMPT_VERSION)
    );
    assert_eq!(
        evidence.schema_version.as_deref(),
        Some(moments::MOMENT_SCHEMA_VERSION)
    );
    assert_eq!(evidence.recipe_version, moments::MOMENT_RECIPE_VERSION);
    assert_eq!(evidence.policy_fingerprint, "fixture-policy-fingerprint");
    assert!(
        evidence
            .representation_fingerprint
            .as_ref()
            .is_some_and(|v| !v.is_empty())
    );

    let mut changed = first;
    changed.provenance.actual_model = "replacement-fixture-model".into();
    changed.provenance.configuration_hash = "replacement-fixture-policy".into();
    save(&catalog, &asset, identity, &changed);
    let refreshed = catalog.preview_sequence_selection(&req).unwrap();
    assert!(
        !refreshed.report.reused,
        "same canonical row identity with changed evidence must not reuse stale selection cache"
    );
    assert_eq!(
        refreshed.metadata.assignments[0]
            .evidence
            .actual_model
            .as_deref(),
        Some("replacement-fixture-model")
    );
}

#[test]
fn stale_current_evidence_is_rejected_and_current_zero_candidate_artifact_is_empty() {
    let (_dir, catalog, asset) = crate::editplan::inspection::tests::fixture();
    catalog.generate_proxy(&asset).unwrap();
    catalog.validate_asset_video(&asset).unwrap();
    let identity = ID;
    let valid = artifact(&catalog, &asset, identity);
    save(&catalog, &asset, identity, &valid);
    let req = request(&catalog, &asset, identity);
    for (label, mut stale) in [
        ("recipe", valid.clone()),
        ("source", valid.clone()),
        ("representation", valid.clone()),
    ] {
        match label {
            "recipe" => stale.provenance.recipe_version = "obsolete-recipe".into(),
            "source" => stale.provenance.source_duration_ms += 100,
            _ => stale.provenance.representation_fingerprint = "obsolete-representation".into(),
        }
        save(&catalog, &asset, identity, &stale);
        let error = catalog
            .preview_sequence_selection(&req)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("changed") || error.contains("stale"),
            "{label} evidence should fail closed: {error}"
        );
    }
    let mut empty = valid;
    empty.status = MomentStatus::NoCandidates;
    empty.candidates.clear();
    save(&catalog, &asset, identity, &empty);
    let proposal = catalog.preview_sequence_selection(&req).unwrap();
    assert!(proposal.operations.is_empty());
    assert_eq!(proposal.report.earned_seconds, 0.0);
}
