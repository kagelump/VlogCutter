//! Independent scorer-parity replay for the core pooled recognition API
//! (recipe `source-audio-pooled-mvp-v1`).
//!
//! The vendored sanitized metrics record the frozen small-set outcomes of the
//! demonstrated M1 matcher. Each case row is replayed through the public
//! `rcdvlog_core::source_audio_pooled` API by reconstructing the per-second
//! exact-offset cells that produce the recorded aggregates (independent-anchor
//! sets with the recorded overlap, cell votes, occupancy, support spans,
//! rarity and frequency diversity), then asserting that the core path
//! reproduces every frozen field, margin, rounding, rejection reason and the
//! complete frozen outcome set: 17/17 top-ranked positives per density within
//! 40 ms, density100 10-second acceptance 5/6, 0/13 negatives, and the
//! moderate composite rejected with `minimum_anchors` plus
//! `minimum_distinct_competitor_ratio`.
//!
//! Expectations come from the vendored metrics file and the documented
//! formulas, not from the implementer's helpers.

use rcdvlog_core::source_audio_pooled::{
    PooledOffsetModeRow, PooledOffsetWindow, PooledRecognitionRequest, PooledRecognitionResult,
    REASON_MINIMUM_ANCHORS, REASON_MINIMUM_DISTINCT_COMPETITOR_RATIO, REASON_NOT_CALIBRATED,
    pool_exact_offset_windows,
};
use rcdvlog_core::tv_soundtrack_landmarks::{OffsetEvidence, Recipe, Search, SearchPolicy};
use serde_json::Value;

const METRICS_JSON: &str =
    include_str!("source_audio_eval/expected/m1-small-set-anchor-aware-results-v2.sanitized.json");
const RECIPE_ID: &str = "source-audio-pooled-mvp-v1";
const POLICY_ID: &str =
    "rc6-v2-density100-exact-bin-complete-evidence-anchor-identity-calibrated-v3";
const RATE: u64 = 11_025;

fn cell(
    time_bin: u32,
    offset_bin: i32,
    frames: Vec<u32>,
    rarity_weight: f64,
    saturated_anchors: u32,
    frequency_diversity: u32,
) -> OffsetEvidence {
    OffsetEvidence {
        time_bin,
        offset_bin,
        offset_seconds_from_capture_start: f64::from(offset_bin) * 0.08,
        independent_anchors: frames.len() as u32,
        supporting_seconds: 1,
        frequency_diversity,
        rarity_weight,
        saturated_anchors,
        query_anchor_frames: frames,
    }
}

/// Distinct one-second bins with `count` members, minimum 0, maximum
/// `span - 1`: exactly the recorded support span with interior holes.
fn span_bins(count: usize, span: u32) -> Vec<u32> {
    if count == 1 {
        assert_eq!(span, 1, "single cell cannot span {span} seconds");
        return vec![0];
    }
    let mut bins = vec![0];
    bins.extend((1..span - 1).take(count - 2));
    bins.push(span - 1);
    assert_eq!(bins.len(), count, "infeasible occupancy/span pair");
    bins
}

/// Median of integer values, matching the pooler's rule.
fn median(mut values: Vec<u32>) -> f64 {
    values.sort_unstable();
    match values.len() {
        0 => 0.0,
        length if length % 2 == 1 => f64::from(values[length / 2]),
        length => f64::from(values[length / 2 - 1] + values[length / 2]) / 2.0,
    }
}

/// Per-cell integer frequency-diversity values whose median is `target`.
fn median_witness(count: usize, target: f64) -> Vec<u32> {
    if target.fract() == 0.0 {
        return vec![target as u32; count];
    }
    assert!(
        count.is_multiple_of(2),
        "fractional median {target} needs an even cell count"
    );
    let low = target.floor() as u32;
    let mut values = vec![low; count / 2];
    values.extend(std::iter::repeat_n(low + 1, count - count / 2));
    assert_eq!(median(values.clone()), target);
    values
}

/// Reconstruct the per-second exact-offset cells behind one frozen case row.
/// Every aggregate of the reconstruction equals the recorded value by
/// construction, so any pooler drift shows up as a failed comparison.
fn reconstruct(row: &Value) -> Search {
    let best = &row["bestMode"];
    let competitor = &row["bestDistinctCompetitor"];
    let duration = row["durationSeconds"].as_u64().unwrap() as u32;
    let pooled_cells = row["offsetCellsPooledForWindow"].as_u64().unwrap() as usize;
    let internal_cells = row["internalOffsetCellCount"].as_u64().unwrap() as usize;
    let best_bin = best["offsetBin"].as_i64().unwrap() as i32;
    assert_eq!(row["windowStartSeconds"].as_i64().unwrap(), 0);
    assert!(
        pooled_cells
            >= best["occupiedTimeBins"].as_u64().unwrap() as usize
                + competitor["occupiedTimeBins"].as_u64().unwrap() as usize
    );

    let cells_per_mode = best["occupiedTimeBins"].as_u64().unwrap() as usize;
    let unique = best["independentAnchors"].as_u64().unwrap();
    let votes = best["summedCellAnchorCount"].as_u64().unwrap();
    let span = best["supportSpanSeconds"].as_u64().unwrap() as u32;
    assert!(unique >= 1 && votes >= unique && cells_per_mode >= 1);

    // Anchor frames 0..unique distributed round-robin; the recorded duplicate
    // votes re-add existing frames only.
    let mut best_frames: Vec<Vec<u32>> = vec![Vec::new(); cells_per_mode];
    for frame in 0..unique {
        best_frames[(frame % cells_per_mode as u64) as usize].push(frame as u32);
    }
    for repeat in 0..votes - unique {
        best_frames[(repeat % cells_per_mode as u64) as usize].push((repeat % unique) as u32);
    }
    let bins = span_bins(cells_per_mode, span);
    let rarity = best["rarityWeight"].as_f64().unwrap();
    let saturated = best["saturatedAnchors"].as_u64().unwrap() as u32;
    let medians = median_witness(
        cells_per_mode,
        best["frequencyDiversityMedianPerTimeBin"].as_f64().unwrap(),
    );

    let mut offsets = Vec::new();
    for (index, frames) in best_frames.into_iter().enumerate() {
        offsets.push(cell(
            bins[index],
            best_bin,
            frames,
            if index == 0 { rarity } else { 0.0 },
            if index == 0 { saturated } else { 0 },
            medians[index],
        ));
    }

    // Competitor: `sharedQueryAnchorsWithBest` frames overlap the best set,
    // the rest are fresh frames.
    let comp_cells = competitor["occupiedTimeBins"].as_u64().unwrap() as usize;
    let comp_unique = competitor["independentAnchors"].as_u64().unwrap();
    let shared = competitor["sharedQueryAnchorsWithBest"].as_u64().unwrap();
    let mut comp_frames: Vec<u32> = (0..shared).map(|frame| frame as u32).collect();
    comp_frames.extend((unique..unique + comp_unique - shared).map(|frame| frame as u32));
    assert_eq!(comp_frames.len() as u64, comp_unique);
    let comp_bin = competitor["offsetBin"].as_i64().unwrap() as i32;
    let mut spread: Vec<Vec<u32>> = vec![Vec::new(); comp_cells];
    for (index, frame) in comp_frames.into_iter().enumerate() {
        spread[index % comp_cells].push(frame);
    }
    for (index, frames) in spread.into_iter().enumerate() {
        offsets.push(cell(index as u32, comp_bin, frames, 0.0, 0, 1));
    }

    // Zero-vote filler cells keep the recorded pooled-cell total without
    // touching any asserted aggregate; their bin stays clear of both modes.
    let filler_bin = best_bin + 1_000_000;
    assert_ne!(filler_bin, comp_bin);
    let filler_cells = pooled_cells - cells_per_mode - comp_cells;
    for ordinal in 0..filler_cells {
        offsets.push(cell(
            ordinal as u32 % duration,
            filler_bin,
            Vec::new(),
            0.0,
            0,
            1,
        ));
    }
    // Out-of-window cells reach the recorded complete internal cell count.
    while offsets.len() < internal_cells {
        offsets.push(cell(duration, filler_bin, Vec::new(), 0.0, 0, 1));
    }

    Search {
        schema_version: 1,
        recipe: if row["density"] == "density100" {
            Recipe::default()
        } else {
            Recipe {
                hashes_per_second: 50,
                ..Recipe::default()
            }
        },
        search_policy: SearchPolicy::default(),
        movie_digest: "movie".into(),
        index_digest: "index".into(),
        query_pcm_digest: "query".into(),
        query_sample_count: u64::from(duration.max(30)) * RATE,
        query_landmark_count: 0,
        matched_landmark_count: 0,
        candidate_pair_count: 0,
        complete: row["completeEvidence"].as_bool().unwrap(),
        offsets,
    }
}

fn replay(row: &Value) -> PooledRecognitionResult {
    let search = reconstruct(row);
    let request = PooledRecognitionRequest {
        search_policy: None,
        windows: Some(vec![PooledOffsetWindow {
            start_seconds: row["windowStartSeconds"].as_u64().unwrap() as u32,
            duration_seconds: row["durationSeconds"].as_u64().unwrap() as u16,
        }]),
        mode_limit: Some(8),
    };
    pool_exact_offset_windows(&search, &request).unwrap()
}

fn assert_mode_row(mode: &PooledOffsetModeRow, json: &Value) {
    assert_eq!(
        i64::from(mode.offset_bin),
        json["offsetBin"].as_i64().unwrap()
    );
    assert_eq!(mode.offset_seconds, json["offsetSeconds"].as_f64().unwrap());
    assert_eq!(
        mode.independent_anchors,
        json["independentAnchors"].as_u64().unwrap()
    );
    assert_eq!(
        mode.summed_cell_anchor_count,
        json["summedCellAnchorCount"].as_u64().unwrap()
    );
    assert_eq!(
        mode.duplicate_anchor_votes_removed,
        json["duplicateAnchorVotesRemoved"].as_u64().unwrap()
    );
    assert_eq!(
        mode.occupied_time_bins as u64,
        json["occupiedTimeBins"].as_u64().unwrap()
    );
    assert_eq!(
        u64::from(mode.support_span_seconds),
        json["supportSpanSeconds"].as_u64().unwrap()
    );
    assert_eq!(mode.rarity_weight, json["rarityWeight"].as_f64().unwrap());
    assert_eq!(
        mode.frequency_diversity_median_per_time_bin,
        json["frequencyDiversityMedianPerTimeBin"].as_f64().unwrap()
    );
    assert_eq!(
        mode.saturated_anchors,
        json["saturatedAnchors"].as_u64().unwrap()
    );
    assert_eq!(
        mode.shared_query_anchors_with_best as u64,
        json["sharedQueryAnchorsWithBest"].as_u64().unwrap()
    );
}

fn reasons(list: &[&'static str]) -> Vec<&'static str> {
    list.to_vec()
}

fn frozen_reasons(json: &Value) -> Vec<&str> {
    json.as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_str().unwrap())
        .collect()
}

fn cases() -> Vec<Value> {
    let metrics: Value = serde_json::from_str(METRICS_JSON).unwrap();
    assert_eq!(
        metrics["provenance"]["sourceFileSha256"],
        "41dcc1ee05923f1897b8a1f902606ca5cdaad9edddd0349c59c031f5d23eabb7"
    );
    metrics["sanitized"]["cases"].as_array().unwrap().clone()
}

#[test]
fn frozen_case_rows_replay_exactly_through_core_pooling() {
    for row in cases() {
        let pooled = replay(&row);
        let window = &pooled.windows[0];
        let best = &row["bestMode"];
        let competitor = &row["bestDistinctCompetitor"];

        assert_eq!(pooled.recipe_identity, RECIPE_ID);
        assert_eq!(pooled.acceptance_policy_id, POLICY_ID);
        assert_eq!(pooled.mode_limit, 8);
        assert_eq!(pooled.complete, row["completeEvidence"].as_bool().unwrap());
        assert_eq!(
            window.internal_offset_cell_count as u64,
            row["internalOffsetCellCount"].as_u64().unwrap()
        );
        assert_eq!(
            window.offset_cells_scanned as u64,
            row["offsetCellsScanned"].as_u64().unwrap()
        );
        assert_eq!(
            window.offset_cells_pooled_for_window as u64,
            row["offsetCellsPooledForWindow"].as_u64().unwrap()
        );
        assert_eq!(window.offset_cells_dropped, 0);

        assert_mode_row(&window.modes[0], best);
        let observed = window.best_distinct_competitor.as_ref().unwrap();
        assert_eq!(
            i64::from(observed.offset_bin),
            competitor["offsetBin"].as_i64().unwrap()
        );
        assert_eq!(
            observed.offset_seconds,
            competitor["offsetSeconds"].as_f64().unwrap()
        );
        assert_eq!(
            observed.independent_anchors,
            competitor["independentAnchors"].as_u64().unwrap()
        );
        assert_eq!(
            observed.occupied_time_bins as u64,
            competitor["occupiedTimeBins"].as_u64().unwrap()
        );
        assert_eq!(
            observed.shared_query_anchors_with_best as u64,
            competitor["sharedQueryAnchorsWithBest"].as_u64().unwrap()
        );
        assert_eq!(
            observed.unique_query_anchors_outside_best as u64,
            competitor["uniqueQueryAnchorsOutsideBest"]
                .as_u64()
                .unwrap()
        );
        assert_eq!(window.best_anchor_margin, row["bestAnchorMargin"].as_f64());
        assert_eq!(
            window.best_unique_anchor_margin,
            row["bestUniqueAnchorMargin"].as_f64()
        );

        // Top-mode offsets stay within the frozen evaluator tolerance.
        let truth = row["truthOffsetEvaluatorOnly"].as_f64();
        if let Some(truth) = truth {
            let error = (window.modes[0].offset_seconds - truth).abs();
            assert_eq!(Some(error), row["topModeOffsetErrorSeconds"].as_f64());
            assert_eq!(
                row["topModeCorrectWithin100ms"].as_bool().unwrap(),
                error <= 0.1
            );
        }
    }
}

#[test]
fn frozen_acceptance_columns_and_outcomes_replay_exactly() {
    let mut top_ranked = std::collections::BTreeMap::new();
    let mut max_error = std::collections::BTreeMap::new();
    let mut positives_ten = (0usize, 0usize);
    let mut negatives_ten = (0usize, 0usize);
    let mut moderate: Option<(bool, Vec<String>)> = None;

    for row in cases() {
        let pooled = replay(&row);
        let window = &pooled.windows[0];
        let duration = row["durationSeconds"].as_u64().unwrap();
        let acceptance = &window.acceptance;

        // Scorer parity: the tri-state verdict and reason vocabulary match
        // score_m1_anchor_identity_v2.py column for column.
        match &row["policyV3AnchorAwareAccepted"] {
            Value::Null => assert_eq!(acceptance.accepted, None),
            Value::Bool(expected) => assert_eq!(acceptance.accepted, Some(*expected)),
            other => panic!("unexpected frozen verdict {other}"),
        }
        assert_eq!(
            reasons(&acceptance.failure_reasons),
            frozen_reasons(&row["policyV3FailureReasons"])
        );
        assert_eq!(acceptance.floors_calibrated, duration == 10);
        assert_eq!(acceptance.recipe_calibrated, row["density"] == "density100");
        // The uncalibrated 20/30-second floors evaluate diagnostically and
        // reproduce the frozen baseline gate columns exactly.
        if duration != 10 {
            assert_eq!(
                reasons(&acceptance.gate_failure_reasons),
                frozen_reasons(&row["baselinePolicyV1FailureReasons"])
            );
        }
        assert_eq!(
            acceptance.accepted.is_some(),
            duration == 10 && row["density"] == "density100"
        );
        if let Some(true) = acceptance.accepted {
            assert!(acceptance.failure_reasons.is_empty());
        }

        // Aggregate frozen outcomes recomputed from the replay.
        if row["role"] == "positive_calibration" {
            let density = row["density"].as_str().unwrap().to_string();
            let truth = row["truthOffsetEvaluatorOnly"].as_f64().unwrap();
            let error = (window.modes[0].offset_seconds - truth).abs();
            *top_ranked.entry(density.clone()).or_insert(0usize) += usize::from(error <= 0.1);
            let worst = max_error.entry(density).or_insert(0.0f64);
            *worst = (*worst).max(error);
        }
        if row["density"] == "density100" && duration == 10 {
            if row["role"] == "positive_calibration" {
                positives_ten.1 += 1;
                positives_ten.0 += usize::from(acceptance.accepted == Some(true));
            } else {
                negatives_ten.1 += 1;
                negatives_ten.0 += usize::from(acceptance.accepted == Some(true));
            }
            if row["condition"] == "moderate_composite_provisional" {
                moderate = Some((
                    acceptance.accepted == Some(true),
                    acceptance
                        .failure_reasons
                        .iter()
                        .map(|reason| (*reason).to_string())
                        .collect(),
                ));
            }
        }
    }

    for density in ["density50", "density100"] {
        assert_eq!(top_ranked[density], 17, "{density}: 17/17 top-ranked");
        assert!(
            max_error[density] <= 0.04 + 1e-9,
            "{density}: errors within 40 ms"
        );
    }
    assert_eq!(positives_ten, (5, 6), "density100 10-s acceptance 5/6");
    assert_eq!(negatives_ten, (0, 13), "0/13 negative accepts");
    let (moderate_accepted, moderate_reasons) = moderate.expect("moderate composite row");
    assert!(!moderate_accepted, "moderate composite stays rejected");
    assert_eq!(
        moderate_reasons,
        vec![
            REASON_MINIMUM_ANCHORS.to_string(),
            REASON_MINIMUM_DISTINCT_COMPETITOR_RATIO.to_string()
        ]
    );
}

#[test]
fn frozen_outcomes_are_independent_of_the_diagnostic_mode_limit() {
    for row in cases() {
        let search = reconstruct(&row);
        let window = PooledOffsetWindow {
            start_seconds: row["windowStartSeconds"].as_u64().unwrap() as u32,
            duration_seconds: row["durationSeconds"].as_u64().unwrap() as u16,
        };
        let one = pool_exact_offset_windows(
            &search,
            &PooledRecognitionRequest {
                search_policy: None,
                windows: Some(vec![window]),
                mode_limit: Some(1),
            },
        )
        .unwrap();
        let all = pool_exact_offset_windows(
            &search,
            &PooledRecognitionRequest {
                search_policy: None,
                windows: Some(vec![window]),
                mode_limit: Some(64),
            },
        )
        .unwrap();
        assert_eq!(
            one.windows[0].returned_offset_bin_count, 1,
            "diagnostic rows truncate"
        );
        assert_eq!(
            one.windows[0].best_anchor_margin,
            all.windows[0].best_anchor_margin
        );
        assert_eq!(
            one.windows[0].best_unique_anchor_margin,
            all.windows[0].best_unique_anchor_margin
        );
        assert_eq!(
            one.windows[0].best_distinct_competitor,
            all.windows[0].best_distinct_competitor
        );
        assert_eq!(one.windows[0].acceptance, all.windows[0].acceptance);
    }
}

#[test]
fn abstaining_rows_use_the_frozen_not_calibrated_reason() {
    let mut checked = 0usize;
    for row in cases() {
        let acceptance = &replay(&row).windows[0].acceptance;
        if acceptance.accepted.is_none() {
            assert_eq!(
                reasons(&acceptance.failure_reasons),
                [REASON_NOT_CALIBRATED]
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 41, "41 of 60 frozen rows abstain");
}
