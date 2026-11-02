//! V01-T001 independent compatibility oracle (ARCHITECTURE section 2).
//!
//! Fixtures are verbatim copies of the preserved RC6 run. They are pretty
//! printed with sorted keys (except spec.json, which is in field order), so the
//! byte check serialises the typed value to a `serde_json::Value` (sorted map)
//! and compares the pretty text against the file. The digest check relies on
//! `SoundtrackSessionPlan::from_json`, which recomputes `planDigest` from the
//! typed spec and rejects any difference.
use rcdvlog_core::{
    ReviewTargetHandoff,
    tv_review_sync::{SyncRequest, SyncState},
    tv_soundtrack::{
        DistinctivePolicy, SOUNDTRACK_RECIPE, SOUNDTRACK_RECIPE_V2, SoundtrackIndex,
        SoundtrackMatchReport, SoundtrackPeak, SoundtrackPolicy, SoundtrackReferenceSpec,
    },
    tv_soundtrack_consensus::{ConsensusSegment, SoundtrackConsensus},
    tv_soundtrack_session::{
        SessionEvidencePage, SessionMatchQuery, SessionQueryEvidence, SoundtrackSessionPlan,
        SoundtrackSessionSpec,
    },
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

const PLAN: &str = include_str!("fixtures/v01/plan.json");
const SPEC: &str = include_str!("fixtures/v01/spec.json");
const TARGET: &str = include_str!("fixtures/v01/target.json");
const EVIDENCE: &str = include_str!("fixtures/v01/evidence.data.json");

/// Every key introduced by ARCHITECTURE sections 3-9.
const NEW_KEYS: &[&str] = &[
    "bestPeaks",
    "distinctive",
    "distinctiveness",
    "maskDigest",
    "speechMask",
    "origin",
    "frameEnergy",
    "consensusSegments",
];

fn value(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn pretty<T: Serialize>(t: &T) -> String {
    let mut s = serde_json::to_string_pretty(&serde_json::to_value(t).unwrap()).unwrap();
    s.push('\n');
    s
}

/// Parse through the typed DTO, then check value equality; return the typed value.
fn typed_round_trip<T: Serialize + DeserializeOwned>(text: &str) -> T {
    let typed: T = serde_json::from_str(text).unwrap();
    assert_eq!(serde_json::to_value(&typed).unwrap(), value(text));
    typed
}

fn assert_no_new_keys(compact: &str) {
    for key in NEW_KEYS {
        assert!(
            !compact.contains(&format!("\"{key}\":")),
            "unset v2 field {key} leaked into v1 output"
        );
    }
}

#[test]
fn rc6_plan_round_trips_byte_identically_with_unchanged_digest() {
    let fixture = value(PLAN);
    let recorded = fixture["planDigest"].as_str().unwrap().to_string();
    // from_json recomputes the digest from the typed spec and rejects any drift.
    let plan = SoundtrackSessionPlan::from_json(PLAN.as_bytes()).unwrap();
    assert_eq!(plan.plan_digest, recorded);
    assert_eq!(pretty(&plan), PLAN);
    assert_eq!(serde_json::to_value(&plan).unwrap(), fixture);
    assert_no_new_keys(&serde_json::to_string(&plan).unwrap());
}

#[test]
fn rc6_spec_round_trips_and_rebuilds_the_recorded_plan_digest() {
    let spec: SoundtrackSessionSpec = typed_round_trip(SPEC);
    assert_no_new_keys(&serde_json::to_string(&spec).unwrap());
    let rebuilt = SoundtrackSessionPlan::new(spec.clone()).unwrap();
    assert_eq!(
        rebuilt.plan_digest,
        value(PLAN)["planDigest"].as_str().unwrap()
    );
    assert_eq!(serde_json::to_value(&spec).unwrap(), value(PLAN)["spec"]);
    assert!(spec.recipe.is_none() && spec.speech_mask.is_empty());
    assert!(spec.match_policy.distinctive.is_none());
    spec.match_policy.validate().unwrap();
}

#[test]
fn rc6_target_handoff_round_trips_byte_identically() {
    let target: ReviewTargetHandoff = typed_round_trip(TARGET);
    assert_eq!(pretty(&target), TARGET);
}

#[test]
fn rc6_evidence_page_round_trips_byte_identically() {
    let page: SessionEvidencePage = typed_round_trip(EVIDENCE);
    assert_eq!(pretty(&page), EVIDENCE);
    assert_eq!(
        page.plan_digest,
        value(PLAN)["planDigest"].as_str().unwrap()
    );
    let compact = serde_json::to_string(&page).unwrap();
    assert_no_new_keys(&compact);
    // The embedded sync request is a v1 request too.
    let sync: SyncRequest = serde_json::from_value(value(EVIDENCE)["sync"].clone()).unwrap();
    assert!(sync.consensus_segments.is_empty());
}

#[test]
fn recipe_constants_are_exact_and_distinct() {
    assert_eq!(
        SOUNDTRACK_RECIPE,
        "soundtrack.log-band-delta.fft512-hop128.smooth3-lag6.pcm8k.v1"
    );
    assert_eq!(
        SOUNDTRACK_RECIPE_V2,
        "soundtrack.log-band-delta.fft512-hop128.smooth3-lag6.pcm8k.wtd-robust.v2"
    );
}

#[test]
fn sync_state_consensus_interpolated_wire_name() {
    assert_eq!(
        serde_json::to_string(&SyncState::ConsensusInterpolated).unwrap(),
        "\"consensusInterpolated\""
    );
    let back: SyncState = serde_json::from_str("\"consensusInterpolated\"").unwrap();
    assert_eq!(back, SyncState::ConsensusInterpolated);
    // v1 names are untouched.
    assert_eq!(
        serde_json::to_string(&SyncState::Anchored).unwrap(),
        "\"anchored\""
    );
}

fn policy_json(distinctive: Option<Value>) -> Value {
    let mut p = json!({
        "minimumSimilarity": 0.8, "minimumPeakMargin": 0.08,
        "minimumSubwindowSimilarity": 0.3, "uncertaintySeconds": 0.08,
        "maximumCandidates": 4
    });
    if let Some(d) = distinctive {
        p["distinctive"] = d;
    }
    p
}

fn distinctive(sim: f64, margin: f64, z: f64) -> Value {
    json!({"minimumSimilarity": sim, "minimumMargin": margin, "minimumZScore": z})
}

#[test]
fn distinctive_policy_parses_serialises_and_validates_ranges() {
    let p: SoundtrackPolicy =
        serde_json::from_value(policy_json(Some(distinctive(0.5, 0.25, 6.0)))).unwrap();
    p.validate().unwrap();
    let d = p.distinctive.clone().unwrap();
    assert_eq!(
        d,
        DistinctivePolicy {
            minimum_similarity: 0.5,
            minimum_margin: 0.25,
            minimum_z_score: 6.0
        }
    );
    assert_eq!(
        serde_json::to_value(&p).unwrap()["distinctive"],
        distinctive(0.5, 0.25, 6.0)
    );
    // Inclusive bounds accepted.
    for (s, m, z) in [(0.3, 0.1, 3.0), (1.0, 0.8, 20.0)] {
        let p: SoundtrackPolicy =
            serde_json::from_value(policy_json(Some(distinctive(s, m, z)))).unwrap();
        p.validate().unwrap();
    }
    // Each value just outside its range is rejected.
    for (s, m, z) in [
        (0.29, 0.25, 6.0),
        (1.01, 0.25, 6.0),
        (0.5, 0.09, 6.0),
        (0.5, 0.81, 6.0),
        (0.5, 0.25, 2.9),
        (0.5, 0.25, 20.1),
    ] {
        let p: SoundtrackPolicy =
            serde_json::from_value(policy_json(Some(distinctive(s, m, z)))).unwrap();
        assert!(p.validate().is_err(), "({s},{m},{z}) must be rejected");
    }
    // Non-finite values cannot be smuggled in through the Rust API either.
    let mut bad = p;
    bad.distinctive.as_mut().unwrap().minimum_z_score = f64::NAN;
    assert!(bad.validate().is_err());
}

#[test]
fn unset_distinctive_is_omitted_and_default_is_off() {
    let p: SoundtrackPolicy = serde_json::from_value(policy_json(None)).unwrap();
    assert!(p.distinctive.is_none());
    assert!(
        serde_json::to_value(&p)
            .unwrap()
            .get("distinctive")
            .is_none()
    );
    assert!(SoundtrackPolicy::default().distinctive.is_none());
}

fn movie_json() -> Value {
    value(SPEC)["sync"]["movie"].clone()
}

#[test]
fn reference_spec_recipe_and_index_frame_energy_appear_when_set() {
    let base = json!({"schemaVersion": 1, "movie": movie_json(),
        "startSeconds": 0.0, "durationSeconds": 120.0});
    let v1: SoundtrackReferenceSpec = serde_json::from_value(base.clone()).unwrap();
    assert!(v1.recipe.is_none());
    assert_eq!(serde_json::to_value(&v1).unwrap(), base);
    let mut v2 = base.clone();
    v2["recipe"] = json!(SOUNDTRACK_RECIPE_V2);
    let parsed: SoundtrackReferenceSpec = serde_json::from_value(v2.clone()).unwrap();
    assert_eq!(parsed.recipe.as_deref(), Some(SOUNDTRACK_RECIPE_V2));
    assert_eq!(serde_json::to_value(&parsed).unwrap(), v2);

    let digest = "a".repeat(64);
    let idx = json!({"schemaVersion": 1, "recipe": SOUNDTRACK_RECIPE, "reference": base,
        "pcmDigest": digest, "indexDigest": digest, "sampleCount": 0, "features": []});
    let v1: SoundtrackIndex = serde_json::from_value(idx.clone()).unwrap();
    assert!(v1.frame_energy.is_empty());
    assert_eq!(serde_json::to_value(&v1).unwrap(), idx);
    let mut idx2 = idx;
    idx2["frameEnergy"] = json!([0.5, 1.5]);
    let v2: SoundtrackIndex = serde_json::from_value(idx2.clone()).unwrap();
    assert_eq!(v2.frame_energy, vec![0.5f32, 1.5]);
    assert_eq!(serde_json::to_value(&v2).unwrap(), idx2);
}

fn report_json() -> Value {
    let d = "b".repeat(64);
    json!({
        "schemaVersion": 1, "recipe": SOUNDTRACK_RECIPE,
        "referenceMovieDigest": d, "referencePcmDigest": d, "referenceIndexDigest": d,
        "queryPcmDigest": d,
        "capture": {"evidenceId": "q_1", "clockId": "mic", "sampleRate": 48000,
                    "firstSample": 0, "monotonicOriginNs": 1},
        "policy": policy_json(None),
        "querySampleCount": 48000, "state": "unmatched", "reason": "weak_or_non_distinctive",
        "candidates": [], "searchedOffsets": 10, "comparisonProducts": 100,
        "gridSeconds": 0.016, "bestSimilarity": 0.6, "runnerUpSimilarity": 0.2
    })
}

fn peak_json() -> Value {
    json!({"similarity": 0.615, "referenceOffsetFrames": 300, "movieSeconds": 5.04,
           "subwindowSimilarities": [0.5, 0.6, 0.7]})
}

#[test]
fn report_gains_peaks_distinctiveness_and_mask_digest_only_when_set() {
    let v1 = report_json();
    let parsed: SoundtrackMatchReport = serde_json::from_value(v1.clone()).unwrap();
    assert!(parsed.best_peaks.is_empty());
    assert!(parsed.distinctiveness.is_none() && parsed.mask_digest.is_none());
    assert_eq!(serde_json::to_value(&parsed).unwrap(), v1);

    let mut v2 = v1;
    v2["bestPeaks"] = json!([peak_json()]);
    v2["distinctiveness"] = json!(7.25);
    v2["maskDigest"] = json!("c".repeat(64));
    let parsed: SoundtrackMatchReport = serde_json::from_value(v2.clone()).unwrap();
    assert_eq!(parsed.best_peaks.len(), 1);
    assert_eq!(parsed.best_peaks[0].subwindow_similarities, [0.5, 0.6, 0.7]);
    assert_eq!(parsed.distinctiveness, Some(7.25));
    assert_eq!(serde_json::to_value(&parsed).unwrap(), v2);
}

#[test]
fn peak_has_exactly_the_architecture_fields() {
    let p: SoundtrackPeak = serde_json::from_value(peak_json()).unwrap();
    assert_eq!(p.reference_offset_frames, 300);
    assert_eq!(serde_json::to_value(&p).unwrap(), peak_json());
}

#[test]
fn session_additions_appear_when_set() {
    // Spec: speechMask and recipe.
    let mut spec = value(SPEC);
    spec["speechMask"] = json!([{"clockId": "mic_epoch_0", "startSample": 1, "endSample": 9}]);
    spec["recipe"] = json!(SOUNDTRACK_RECIPE_V2);
    let parsed: SoundtrackSessionSpec = serde_json::from_value(spec.clone()).unwrap();
    assert_eq!(parsed.speech_mask.len(), 1);
    assert_eq!(parsed.recipe.as_deref(), Some(SOUNDTRACK_RECIPE_V2));
    let out = serde_json::to_value(&parsed).unwrap();
    assert_eq!(out["speechMask"], spec["speechMask"]);
    assert_eq!(out["recipe"], spec["recipe"]);

    // Query origin.
    let q0 = value(PLAN)["queries"][0].clone();
    let mut q = q0.clone();
    q["origin"] = json!("speech_gap");
    let parsed: SessionMatchQuery = serde_json::from_value(q.clone()).unwrap();
    assert_eq!(parsed.origin.as_deref(), Some("speech_gap"));
    assert_eq!(serde_json::to_value(&parsed).unwrap(), q);
    let plain: SessionMatchQuery = serde_json::from_value(q0.clone()).unwrap();
    assert!(plain.origin.is_none());
    assert_eq!(serde_json::to_value(&plain).unwrap(), q0);

    // Evidence bestPeaks.
    let e0 = value(EVIDENCE)["queries"][0].clone();
    let mut e = e0.clone();
    e["bestPeaks"] = json!([peak_json()]);
    let parsed: SessionQueryEvidence = serde_json::from_value(e.clone()).unwrap();
    assert_eq!(parsed.best_peaks.len(), 1);
    assert_eq!(serde_json::to_value(&parsed).unwrap(), e);
    let plain: SessionQueryEvidence = serde_json::from_value(e0.clone()).unwrap();
    assert_eq!(serde_json::to_value(&plain).unwrap(), e0);
}

fn segment_json() -> Value {
    json!({"segmentId": "seg_0", "clockId": "mic_epoch_0", "offsetSeconds": -0.96,
           "micStartSample": 288000, "micEndSample": 8000000,
           "supportingQueries": [1, 2, 3], "residualRmsSeconds": 0.01, "driftPpm": 12.5})
}

#[test]
fn consensus_segments_on_sync_request_and_consensus_output_round_trip() {
    let mut sync = value(SPEC)["sync"].clone();
    let v1: SyncRequest = serde_json::from_value(sync.clone()).unwrap();
    assert!(v1.consensus_segments.is_empty());
    assert_eq!(serde_json::to_value(&v1).unwrap(), sync);

    sync["consensusSegments"] = json!([segment_json()]);
    let v2: SyncRequest = serde_json::from_value(sync.clone()).unwrap();
    assert_eq!(v2.consensus_segments.len(), 1);
    assert_eq!(v2.consensus_segments[0].offset_seconds, -0.96);
    assert_eq!(serde_json::to_value(&v2).unwrap(), sync);

    let seg: ConsensusSegment = serde_json::from_value(segment_json()).unwrap();
    assert_eq!(serde_json::to_value(&seg).unwrap(), segment_json());

    let out = json!({"schemaVersion": 1, "planDigest": "d".repeat(64),
        "segments": [segment_json()],
        "boundaries": [{"evidenceId": "b1", "clockId": "mic_epoch_0",
                        "sampleIndex": 4320000, "kind": "seek"}],
        "unsupportedQueries": [0, 4]});
    let parsed: SoundtrackConsensus = serde_json::from_value(out.clone()).unwrap();
    assert_eq!(parsed.unsupported_queries, vec![0, 4]);
    assert_eq!(serde_json::to_value(&parsed).unwrap(), out);
}

fn assert_rejects_unknown<T: DeserializeOwned>(mut base: Value, field: &str) {
    base[field] = json!(1);
    assert!(
        serde_json::from_value::<T>(base).is_err(),
        "unknown field {field} must be rejected"
    );
}

#[test]
fn misspelled_new_fields_are_still_rejected() {
    assert_rejects_unknown::<SoundtrackPolicy>(policy_json(None), "distinctve");
    assert_rejects_unknown::<DistinctivePolicy>(distinctive(0.5, 0.25, 6.0), "minimumZScor");
    assert_rejects_unknown::<SoundtrackMatchReport>(report_json(), "bestPeak");
    assert_rejects_unknown::<SoundtrackMatchReport>(report_json(), "maskDigests");
    assert_rejects_unknown::<SoundtrackPeak>(peak_json(), "movieSecond");
    assert_rejects_unknown::<ConsensusSegment>(segment_json(), "driftPPM");
    assert_rejects_unknown::<SoundtrackSessionSpec>(value(SPEC), "speechMasks");
    assert_rejects_unknown::<SoundtrackSessionSpec>(value(SPEC), "recipes");
    assert_rejects_unknown::<SyncRequest>(value(SPEC)["sync"].clone(), "consensusSegment");
    assert_rejects_unknown::<SessionMatchQuery>(value(PLAN)["queries"][0].clone(), "origins");
    assert_rejects_unknown::<SessionQueryEvidence>(
        value(EVIDENCE)["queries"][0].clone(),
        "bestPeak",
    );
    // A nested typo inside an otherwise valid distinctive block is rejected too.
    let mut p = policy_json(Some(distinctive(0.5, 0.25, 6.0)));
    p["distinctive"]["minimumMarginn"] = json!(0.2);
    assert!(serde_json::from_value::<SoundtrackPolicy>(p).is_err());
}
