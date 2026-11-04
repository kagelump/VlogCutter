use super::*;

fn evidence_with_frames(time_bin: u32, offset_bin: i32, frames: Vec<u32>) -> OffsetEvidence {
    OffsetEvidence {
        time_bin,
        offset_bin,
        offset_seconds_from_capture_start: f64::from(offset_bin) * OFFSET_BIN_SECONDS,
        independent_anchors: frames.len() as u32,
        supporting_seconds: 1,
        frequency_diversity: 4,
        rarity_weight: frames.len() as f64,
        saturated_anchors: 0,
        query_anchor_frames: frames,
    }
}

fn evidence(time_bin: u32, offset_bin: i32, anchors: u32) -> OffsetEvidence {
    evidence_with_frames(
        time_bin,
        offset_bin,
        (0..anchors)
            .map(|ordinal| time_bin * 1000 + ordinal)
            .collect(),
    )
}

fn search(query_seconds: u64, offsets: Vec<OffsetEvidence>) -> Search {
    Search {
        schema_version: 1,
        recipe: Recipe::default(),
        search_policy: SearchPolicy::default(),
        movie_digest: "movie".into(),
        index_digest: "index".into(),
        query_pcm_digest: "query".into(),
        query_sample_count: query_seconds * u64::from(RATE),
        query_landmark_count: 0,
        matched_landmark_count: 0,
        candidate_pair_count: 0,
        complete: true,
        offsets,
    }
}

fn window10() -> PooledOffsetWindow {
    PooledOffsetWindow {
        start_seconds: 0,
        duration_seconds: 10,
    }
}

fn request(windows: Vec<PooledOffsetWindow>, mode_limit: Option<u8>) -> PooledRecognitionRequest {
    PooledRecognitionRequest {
        search_policy: None,
        windows: Some(windows),
        mode_limit,
    }
}

fn pool(
    search: &Search,
    windows: Vec<PooledOffsetWindow>,
    mode_limit: Option<u8>,
) -> Result<PooledRecognitionResult> {
    pool_exact_offset_windows(search, &request(windows, mode_limit))
}

/// Evidence that passes every 10-second gate: 50 independent anchors over five
/// one-second cells against a small distant competitor.
fn strong_search() -> Search {
    let mut cells = Vec::new();
    for time_bin in 0..5u32 {
        cells.push(evidence_with_frames(
            time_bin,
            100,
            ((time_bin * 10)..(time_bin * 10 + 10)).collect(),
        ));
    }
    cells.push(evidence_with_frames(0, 200, vec![500, 501]));
    cells.push(evidence_with_frames(1, 200, vec![502]));
    search(10, cells)
}

#[test]
fn exact_offset_bins_remain_separate_and_adjacent_bins_have_no_distinct_competitor() {
    let search = search(
        10,
        vec![
            evidence(0, 100, 3),
            evidence(1, 100, 2),
            evidence(0, 101, 20),
        ],
    );
    let pooled = pool(&search, vec![window10()], Some(8)).unwrap();
    let window = &pooled.windows[0];
    assert_eq!(window.modes[0].offset_bin, 101);
    assert_eq!(window.modes[0].independent_anchors, 20);
    assert_eq!(window.modes[1].offset_bin, 100);
    assert_eq!(window.modes[1].independent_anchors, 5);
    assert_eq!(window.modes[1].shared_query_anchors_with_best, 3);
    assert!(window.best_distinct_competitor.is_none());
    assert!(window.best_anchor_margin.is_none());
    assert!(window.best_unique_anchor_margin.is_none());
    assert_eq!(window.offset_cells_pooled_for_window, 3);
}

#[test]
fn bin_at_distance_two_is_the_distinct_competitor() {
    // Bin 101 ranks second but sits at distance 1; the first bin at distance
    // >= 2 (bin 102) is the distinct competitor.
    let search = search(
        10,
        vec![
            evidence(0, 100, 10),
            evidence(0, 102, 4),
            evidence(0, 101, 9),
        ],
    );
    let pooled = pool(&search, vec![window10()], Some(8)).unwrap();
    let window = &pooled.windows[0];
    let competitor = window.best_distinct_competitor.as_ref().unwrap();
    assert_eq!(competitor.offset_bin, 102);
    assert_eq!(competitor.independent_anchors, 4);
    assert_eq!(window.best_anchor_margin, Some(2.5));
}

#[test]
fn distinct_competitor_margin_uses_unique_anchor_identity() {
    let best0 = evidence_with_frames(0, 100, vec![1, 2, 3]);
    let best1 = evidence_with_frames(1, 100, vec![4, 5]);
    let competitor0 = evidence_with_frames(0, 110, vec![1, 2, 6]);
    let competitor1 = evidence_with_frames(1, 110, vec![7]);
    let pooled = pool(
        &search(10, vec![best0, best1, competitor0, competitor1]),
        vec![window10()],
        Some(8),
    )
    .unwrap();
    let window = &pooled.windows[0];
    assert_eq!(window.best_anchor_margin, Some(1.25));
    assert_eq!(
        window
            .best_distinct_competitor
            .as_ref()
            .unwrap()
            .shared_query_anchors_with_best,
        2
    );
    assert_eq!(window.best_unique_anchor_margin, Some(1.5));
}

#[test]
fn duplicate_anchor_votes_count_once() {
    let search = search(
        10,
        vec![
            evidence_with_frames(0, 100, vec![1, 2, 3]),
            evidence_with_frames(1, 100, vec![3, 4]),
            evidence_with_frames(0, 200, vec![9]),
        ],
    );
    let pooled = pool(&search, vec![window10()], Some(8)).unwrap();
    let best = &pooled.windows[0].modes[0];
    assert_eq!(best.offset_bin, 100);
    assert_eq!(best.independent_anchors, 4);
    assert_eq!(best.summed_cell_anchor_count, 5);
    assert_eq!(best.duplicate_anchor_votes_removed, 1);
}

#[test]
fn overlapping_windows_are_ranked_separately_without_cross_window_vote_sums() {
    let search = search(
        12,
        vec![evidence(0, 50, 2), evidence(5, 50, 4), evidence(10, 50, 3)],
    );
    let pooled = pool(
        &search,
        vec![
            PooledOffsetWindow {
                start_seconds: 0,
                duration_seconds: 10,
            },
            PooledOffsetWindow {
                start_seconds: 2,
                duration_seconds: 10,
            },
        ],
        Some(8),
    )
    .unwrap();
    assert_eq!(pooled.windows[0].modes[0].independent_anchors, 6);
    assert_eq!(pooled.windows[1].modes[0].independent_anchors, 7);
    assert_eq!(pooled.windows[0].offset_cells_scanned, 3);
    assert_eq!(pooled.windows[1].offset_cells_scanned, 3);
    assert_eq!(pooled.windows[0].offset_cells_dropped, 0);
    assert_eq!(pooled.windows[1].offset_cells_dropped, 0);
}

#[test]
fn ties_resolve_to_the_lower_offset_bin() {
    let search = search(10, vec![evidence(0, 200, 5), evidence(0, 100, 5)]);
    let pooled = pool(&search, vec![window10()], Some(8)).unwrap();
    let window = &pooled.windows[0];
    assert_eq!(window.modes[0].offset_bin, 100);
    assert_eq!(window.modes[1].offset_bin, 200);
}

#[test]
fn complete_evidence_survives_top_n_diagnostic_truncation() {
    // Best bin, an adjacent bin, then a non-adjacent competitor outside the
    // returned top-2 rows: totals stay complete and the competitor comes from
    // the full ranking.
    let search = search(
        10,
        vec![
            evidence(0, 100, 10),
            evidence(0, 101, 9),
            evidence(0, 200, 5),
            evidence(0, 300, 2),
        ],
    );
    let pooled = pool(&search, vec![window10()], Some(2)).unwrap();
    let window = &pooled.windows[0];
    assert_eq!(window.returned_offset_bin_count, 2);
    assert_eq!(window.ranked_offset_bin_count, 4);
    assert_eq!(window.internal_offset_cell_count, 4);
    assert_eq!(window.offset_cells_scanned, 4);
    assert_eq!(window.offset_cells_pooled_for_window, 4);
    assert_eq!(window.offset_cells_dropped, 0);
    assert!(pooled.complete && pooled.offset_cell_scan_complete);
    let competitor = window.best_distinct_competitor.as_ref().unwrap();
    assert_eq!(competitor.offset_bin, 200);
    assert!(!window.modes.iter().any(|mode| mode.offset_bin == 200));
    assert_eq!(window.best_anchor_margin, Some(2.0));
}

#[test]
fn missing_competitor_and_zero_denominator_yield_none_margins_and_never_accept() {
    // No non-adjacent bin at all: the margins stay undefined (never invented)
    // and the window is not accepted with the frozen gate reasons.
    let no_competitor = search(
        10,
        vec![
            evidence(0, 100, 10),
            evidence(1, 100, 10),
            evidence(2, 100, 10),
            evidence(3, 100, 10),
            evidence(4, 100, 10),
            evidence(0, 101, 7),
        ],
    );
    let pooled = pool(&no_competitor, vec![window10()], Some(8)).unwrap();
    let window = &pooled.windows[0];
    assert!(window.best_distinct_competitor.is_none());
    assert_eq!(window.best_anchor_margin, None);
    assert_eq!(window.best_unique_anchor_margin, None);
    assert_eq!(window.acceptance.accepted, Some(false));
    assert_eq!(
        window.acceptance.failure_reasons,
        vec![
            REASON_MINIMUM_DISTINCT_COMPETITOR_RATIO,
            REASON_MINIMUM_UNIQUE_ANCHOR_MARGIN
        ]
    );

    // Zero denominator: every competitor anchor is shared with the best mode,
    // so the unique margin is undefined while the raw ratio is fine.
    let mut cells = Vec::new();
    for time_bin in 0..5u32 {
        cells.push(evidence_with_frames(
            time_bin,
            100,
            ((time_bin * 7)..(time_bin * 7 + 7)).collect(),
        ));
    }
    cells.push(evidence_with_frames(0, 200, vec![0, 1]));
    cells.push(evidence_with_frames(1, 200, vec![2, 3]));
    cells.push(evidence_with_frames(2, 200, vec![4]));
    let pooled = pool(&search(10, cells), vec![window10()], Some(8)).unwrap();
    let window = &pooled.windows[0];
    assert_eq!(window.best_anchor_margin, Some(7.0));
    assert_eq!(window.best_unique_anchor_margin, None);
    assert_eq!(window.acceptance.accepted, Some(false));
    assert_eq!(
        window.acceptance.failure_reasons,
        vec![REASON_MINIMUM_UNIQUE_ANCHOR_MARGIN]
    );
}

#[test]
fn margins_round_to_four_decimals() {
    let search = search(
        10,
        vec![
            evidence_with_frames(0, 100, (0..10).collect()),
            evidence_with_frames(0, 200, vec![100, 101, 102]),
        ],
    );
    let pooled = pool(&search, vec![window10()], Some(8)).unwrap();
    let window = &pooled.windows[0];
    // 10 / 3 rounds to 3.3333 on both margins (no shared anchors).
    assert_eq!(window.best_anchor_margin, Some(3.3333));
    assert_eq!(window.best_unique_anchor_margin, Some(3.3333));
}

#[test]
fn incomplete_search_is_reported_and_never_accepted() {
    let mut search = search(10, vec![evidence(0, 50, 2)]);
    search.complete = false;
    let pooled = pool(&search, vec![window10()], Some(8)).unwrap();
    assert!(!pooled.complete);
    assert_eq!(pooled.status, "incomplete");
    assert!(pooled.offset_cell_scan_complete);
    assert_eq!(pooled.windows[0].offset_cells_dropped, 0);
    assert_eq!(pooled.windows[0].acceptance.accepted, Some(false));
    assert_eq!(
        pooled.windows[0].acceptance.failure_reasons,
        vec![
            REASON_INCOMPLETE_EVIDENCE,
            REASON_MINIMUM_ANCHORS,
            REASON_MINIMUM_OCCUPIED_TIME_BINS,
            REASON_MINIMUM_DISTINCT_COMPETITOR_RATIO,
            REASON_MINIMUM_UNIQUE_ANCHOR_MARGIN
        ]
    );

    // Even complete-looking evidence can never accept an incomplete search.
    let mut strong = strong_search();
    strong.complete = false;
    let pooled = pool(&strong, vec![window10()], Some(8)).unwrap();
    assert_eq!(pooled.windows[0].acceptance.accepted, Some(false));
    assert_eq!(
        pooled.windows[0].acceptance.failure_reasons[0],
        REASON_INCOMPLETE_EVIDENCE
    );
}

#[test]
fn exact_floor_boundary_equality_passes_the_gates() {
    // Gates are >= floors: 34 anchors / 17 = exactly 2.0 and the unique margin
    // (no shared anchors) is also exactly 2.0.
    let best_frames: Vec<u32> = (0..34).collect();
    let mut cells = Vec::new();
    for (ordinal, chunk) in best_frames.chunks(7).enumerate() {
        cells.push(evidence_with_frames(ordinal as u32, 100, chunk.to_vec()));
    }
    cells.push(evidence_with_frames(0, 200, (500..510).collect()));
    cells.push(evidence_with_frames(1, 200, (510..517).collect()));
    let pooled = pool(&search(10, cells), vec![window10()], Some(8)).unwrap();
    let window = &pooled.windows[0];
    assert_eq!(window.modes[0].independent_anchors, 34);
    assert_eq!(window.modes[0].occupied_time_bins, 5);
    assert_eq!(window.best_anchor_margin, Some(2.0));
    assert_eq!(window.best_unique_anchor_margin, Some(2.0));
    assert_eq!(window.acceptance.accepted, Some(true));
    assert!(window.acceptance.failure_reasons.is_empty());
}

#[test]
fn scores_and_acceptance_do_not_depend_on_the_diagnostic_mode_limit() {
    let search = search(
        10,
        vec![
            evidence(0, 100, 40),
            evidence(1, 100, 12),
            evidence(2, 100, 10),
            evidence(3, 100, 8),
            evidence(4, 100, 8),
            evidence(5, 100, 6),
            evidence(0, 101, 30),
            evidence(0, 200, 9),
            evidence(0, 300, 4),
            evidence(0, 400, 1),
        ],
    );
    let one = pool(&search, vec![window10()], Some(1)).unwrap();
    let all = pool(&search, vec![window10()], Some(64)).unwrap();
    assert_eq!(one.windows[0].returned_offset_bin_count, 1);
    assert!(all.windows[0].returned_offset_bin_count > 1);
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
    assert_eq!(one.windows[0].modes[0], all.windows[0].modes[0]);
    assert_eq!(one.windows[0].acceptance, all.windows[0].acceptance);
}

#[test]
fn uncalibrated_durations_and_recipes_abstain_with_the_frozen_reason() {
    // Strong evidence on a 20-second window: the 20-second floors are
    // diagnostic-only, so the verdict abstains while the gates still report.
    let mut cells = vec![evidence(0, 100, 40)];
    for time_bin in 1..15 {
        cells.push(evidence(time_bin, 100, 10));
    }
    cells.push(evidence(0, 200, 5));
    let pooled = pool(
        &search(20, cells),
        vec![PooledOffsetWindow {
            start_seconds: 0,
            duration_seconds: 20,
        }],
        Some(8),
    )
    .unwrap();
    let window = &pooled.windows[0];
    assert!(!window.acceptance.floors_calibrated);
    assert!(window.acceptance.recipe_calibrated);
    assert_eq!(window.acceptance.accepted, None);
    assert_eq!(
        window.acceptance.failure_reasons,
        vec![REASON_NOT_CALIBRATED]
    );
    assert!(window.acceptance.gate_failure_reasons.is_empty());

    // The uncalibrated 20-second floors still evaluate diagnostically.
    let mut weak = Vec::new();
    for time_bin in 0..5u32 {
        weak.push(evidence(time_bin, 100, 10));
    }
    weak.push(evidence(0, 200, 1));
    let pooled = pool(
        &search(20, weak),
        vec![PooledOffsetWindow {
            start_seconds: 0,
            duration_seconds: 20,
        }],
        Some(8),
    )
    .unwrap();
    let window = &pooled.windows[0];
    assert_eq!(window.acceptance.accepted, None);
    assert_eq!(
        window.acceptance.failure_reasons,
        vec![REASON_NOT_CALIBRATED]
    );
    assert_eq!(
        window.acceptance.gate_failure_reasons,
        vec![REASON_MINIMUM_ANCHORS, REASON_MINIMUM_OCCUPIED_TIME_BINS]
    );

    // A non-frozen extractor recipe (density50) abstains on 10-second windows.
    let mut search = strong_search();
    search.recipe.hashes_per_second = 50;
    let pooled = pool(&search, vec![window10()], Some(8)).unwrap();
    let window = &pooled.windows[0];
    assert!(window.acceptance.floors_calibrated);
    assert!(!window.acceptance.recipe_calibrated);
    assert_eq!(window.acceptance.accepted, None);
    assert_eq!(
        window.acceptance.failure_reasons,
        vec![REASON_NOT_CALIBRATED]
    );
    assert!(window.acceptance.gate_failure_reasons.is_empty());
}

#[test]
fn vendored_policy_semantics_and_recorded_identities_match() {
    let policy = PooledRecognitionPolicy::frozen();
    assert_eq!(
        policy.policy_id,
        "rc6-v2-density100-exact-bin-complete-evidence-anchor-identity-calibrated-v3"
    );
    assert!(policy.completeness_required);
    assert!(policy.anchor_identity_available);
    let ten = &policy.acceptance.ten;
    assert_eq!(ten.minimum_anchors, 33);
    assert_eq!(ten.minimum_occupied_time_bins, 5);
    assert_eq!(ten.minimum_distinct_competitor_ratio, 2.0);
    assert_eq!(ten.minimum_unique_anchor_margin, Some(2.0));
    assert!(ten.calibrated);
    let twenty = &policy.acceptance.twenty;
    assert_eq!(twenty.minimum_anchors, 90);
    assert_eq!(twenty.minimum_occupied_time_bins, 6);
    assert_eq!(twenty.minimum_distinct_competitor_ratio, 3.0);
    assert!(twenty.minimum_unique_anchor_margin.is_none());
    assert!(!twenty.calibrated);
    let thirty = &policy.acceptance.thirty;
    assert_eq!(thirty.minimum_anchors, 150);
    assert_eq!(thirty.minimum_occupied_time_bins, 10);
    assert_eq!(thirty.minimum_distinct_competitor_ratio, 3.0);
    assert!(!thirty.calibrated);

    let pooled = pool(
        &search(10, vec![evidence(0, 50, 2)]),
        vec![window10()],
        None,
    )
    .unwrap();
    assert_eq!(pooled.recipe_identity, RECIPE_ID);
    assert_eq!(pooled.recipe_identity, "source-audio-pooled-mvp-v1");
    assert_eq!(pooled.acceptance_policy_id, policy.policy_id);
    assert_eq!(pooled.mode_limit, DEFAULT_MODE_LIMIT);
}

#[test]
fn result_json_keeps_the_demonstrated_key_names() {
    let pooled = pool(
        &search(10, vec![evidence(0, 100, 3), evidence(0, 200, 1)]),
        vec![window10()],
        Some(8),
    )
    .unwrap();
    let value = serde_json::to_value(&pooled).unwrap();
    for key in [
        "schemaVersion",
        "status",
        "complete",
        "searchComplete",
        "internalOffsetCellCount",
        "offsetCellScanComplete",
        "diagnosticTruncationAppliedAfterPooling",
        "queryAnchorIdentityAvailable",
        "anchorIdentitySerialized",
        "anchorIdentityScope",
        "aggregationScope",
        "modeLimit",
        "windows",
    ] {
        assert!(value.get(key).is_some(), "missing top-level key {key}");
    }
    assert_eq!(value["recipeIdentity"], "source-audio-pooled-mvp-v1");
    let window = &value["windows"][0];
    for key in [
        "startSeconds",
        "durationSeconds",
        "endSeconds",
        "complete",
        "internalOffsetCellCount",
        "offsetCellsScanned",
        "offsetCellsPooledForWindow",
        "offsetCellsDropped",
        "rankedOffsetBinCount",
        "returnedOffsetBinCount",
        "bestDistinctCompetitor",
        "bestAnchorMargin",
        "bestUniqueAnchorMargin",
        "modes",
    ] {
        assert!(window.get(key).is_some(), "missing window key {key}");
    }
    assert!(window.get("acceptance").is_some());
    for key in [
        "offsetBin",
        "offsetSeconds",
        "independentAnchors",
        "summedCellAnchorCount",
        "duplicateAnchorVotesRemoved",
        "occupiedTimeBins",
        "supportSpanSeconds",
        "rarityWeight",
        "frequencyDiversityMedianPerTimeBin",
        "saturatedAnchors",
        "sharedQueryAnchorsWithBest",
    ] {
        assert!(
            window["modes"][0].get(key).is_some(),
            "missing mode key {key}"
        );
    }
    for key in [
        "offsetBin",
        "offsetSeconds",
        "independentAnchors",
        "occupiedTimeBins",
        "sharedQueryAnchorsWithBest",
        "uniqueQueryAnchorsOutsideBest",
    ] {
        assert!(
            window["bestDistinctCompetitor"].get(key).is_some(),
            "missing competitor key {key}"
        );
    }
}

#[test]
fn invalid_limits_report_the_demonstrated_errors() {
    let sample = search(10, vec![evidence(0, 50, 2)]);
    let err = pool(&sample, vec![], Some(8)).unwrap_err().to_string();
    assert_eq!(
        err,
        "invalid input: pooled offset window count must be 1..256"
    );
    let err = pool(&sample, vec![window10(); 257], Some(8))
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "invalid input: pooled offset window count must be 1..256"
    );
    let err = pool(&sample, vec![window10()], Some(0))
        .unwrap_err()
        .to_string();
    assert_eq!(err, "invalid input: pooled offset mode limit must be 1..64");
    let err = pool(&sample, vec![window10()], Some(65))
        .unwrap_err()
        .to_string();
    assert_eq!(err, "invalid input: pooled offset mode limit must be 1..64");
    let err = pool(
        &sample,
        vec![PooledOffsetWindow {
            start_seconds: 0,
            duration_seconds: 15,
        }],
        Some(8),
    )
    .unwrap_err()
    .to_string();
    assert_eq!(
        err,
        "invalid input: pooled offset durations must be 10, 20 or 30 seconds"
    );
    let err = pool(
        &sample,
        vec![PooledOffsetWindow {
            start_seconds: 5,
            duration_seconds: 10,
        }],
        Some(8),
    )
    .unwrap_err()
    .to_string();
    assert_eq!(
        err,
        "invalid input: pooled offset window exceeds decoded query duration"
    );

    // Cell-scan budget: 250_001 offset cells x 201 windows exceeds the
    // 50_000_000-cell cap before any window is examined.
    let mut cells = Vec::new();
    for ordinal in 0..250_001u32 {
        cells.push(evidence(0, ordinal as i32, 0));
    }
    let search = search(10, cells);
    let err = pool(&search, vec![window10(); 201], Some(8))
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "invalid input: pooled offset cell-scan budget exceeded"
    );
}
