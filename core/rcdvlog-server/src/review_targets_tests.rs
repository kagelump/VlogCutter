use super::*;
use crate::{ConfigDiagnostics, ControlCatalog};
use chrono::Duration;
use serde_json::{Value, json};
use tempfile::TempDir;

const FIXTURE: &str = include_str!("../../rcdvlog-core/tests/fixtures/review-target/v1.json");
const PROJECT: &str = "project_server_fixture";

fn catalog(root: &TempDir) -> ControlCatalog {
    let data = root.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let catalog = ControlCatalog::open_or_create(
        &data,
        &ConfigDiagnostics {
            data_dir: data.clone(),
            temp_dir: data.join("tmp"),
            derived_dir: data.join("derived"),
            backup_dir: data.join("backup"),
            allowed_original_mounts: vec![],
            listen_addr: "127.0.0.1:0".into(),
            shutdown_timeout_seconds: 1,
            trusted_proxy_cidr_count: 0,
            lan_hostname: "localhost".into(),
            session_max_age_seconds: 60,
            bootstrap_code_ttl_seconds: 60,
            pairing_code_ttl_seconds: 60,
            backup_interval_seconds: 86400,
            backup_daily_retention: 14,
            backup_weekly_retention: 8,
            minimum_free_bytes: 64 * 1024 * 1024,
            ffmpeg_command: "ffmpeg".into(),
            ffprobe_command: "ffprobe".into(),
            exiftool_command: "exiftool".into(),
            proxy_encoder_policy: rcdvlog_core::ProxyEncoderPolicy::Auto,
        },
    )
    .unwrap();
    let connection = open_writable_connection(catalog.database_path()).unwrap();
    for project in [PROJECT, "project_other"] {
        connection
            .execute(
                "INSERT INTO project_registry(project_id, replica_relative_path, state, registered_at, created_at, updated_at)
                 VALUES(?1, ?2, 'registered', '2026-11-01T00:00:00Z', '2026-11-01T00:00:00Z', '2026-11-01T00:00:00Z')",
                params![project, format!("replicas/{project}.sqlite3")],
            )
            .unwrap();
    }
    catalog
}

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-02T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn actor() -> ReviewTargetActor {
    ReviewTargetActor::new(AuditActorType::BrowserSession, "session_test")
}

/// The synthetic fixture rebound to the server project, digested by Rust and
/// verified one hour before `now()`. `edit` mutates the typed snapshot before
/// the digest is recomputed.
fn handoff_with(edit: impl FnOnce(&mut ReviewTargetSnapshot)) -> ReviewTargetHandoff {
    let mut handoff: ReviewTargetHandoff = serde_json::from_str(FIXTURE).unwrap();
    handoff.target.project_id = PROJECT.into();
    edit(&mut handoff.target);
    handoff.target_digest = ReviewTargetHandoff::snapshot_digest(&handoff.target).unwrap();
    handoff.verification.verified_at = "2026-11-02T11:00:00Z".into();
    handoff.validate().unwrap();
    handoff
}

fn request(handoff: Value) -> ReviewTargetImportRequest {
    ReviewTargetImportRequest {
        handoff,
        declared_issuer: Some("Synthetic laptop".into()),
        availability: None,
        max_verification_age_seconds: None,
    }
}

fn valid() -> Value {
    serde_json::to_value(handoff_with(|_| {})).unwrap()
}

fn rejected_code(error: ReviewTargetServiceError) -> &'static str {
    match error {
        ReviewTargetServiceError::Rejected { code, preflight } => {
            assert!(!preflight.importable);
            code
        }
        other => panic!("expected rejection, got {other:?}"),
    }
}

fn count_targets(catalog: &ControlCatalog) -> i64 {
    open_read_only_connection(catalog.database_path())
        .unwrap()
        .query_row("SELECT COUNT(*) FROM review_targets", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn imports_a_verified_target_with_measured_mapping_and_honest_identity() {
    let root = TempDir::new().unwrap();
    let catalog = catalog(&root);
    let targets = catalog.review_targets();
    let preflight = targets
        .preflight(PROJECT, &request(valid()), now())
        .unwrap();
    assert_eq!(preflight.outcome, PreflightOutcome::New);
    assert!(preflight.importable);
    assert!(
        preflight
            .checks
            .iter()
            .all(|c| c.status != ReviewTargetCheckStatus::Fail)
    );

    let result = targets
        .import(PROJECT, &request(valid()), &actor(), now())
        .unwrap();
    assert_eq!(result.outcome, ImportOutcome::Created);
    let detail = result.review_target;
    assert!(detail.summary.review_target_id.starts_with("rtarget_"));
    assert_eq!(detail.summary.revision_id, "editrev_synthetic");
    assert_eq!(detail.summary.cut_count, 3);
    assert_eq!(detail.mapping.status, "measuredComplete");
    // Nonzero PTS origin: the first cut starts at elapsed 0, not 1 second.
    let seconds: Vec<_> = detail
        .cuts
        .iter()
        .map(|c| (c.movie_in_seconds, c.movie_out_seconds))
        .collect();
    assert_eq!(seconds, vec![(0.0, 2.0), (2.0, 4.0), (4.0, 6.0)]);
    // Container audio tail: duration exceeds the final picture end.
    assert!(detail.movie.duration_seconds > detail.mapping.picture_end_seconds);
    assert_eq!(
        detail.summary.youtube.video_id.as_deref(),
        Some("synthetic01")
    );
    assert_eq!(detail.summary.youtube.identity_status, "declaredUnverified");
    assert_eq!(
        detail.summary.youtube.declared_by.as_deref(),
        Some("export")
    );
    assert_eq!(detail.provenance.issuer_verification, "unverified");
    assert_eq!(
        detail.provenance.declared_issuer.as_deref(),
        Some("Synthetic laptop")
    );
    assert_eq!(detail.provenance.importer, actor());
    assert!(detail.summary.readiness.ready);
    assert_eq!(detail.observations.len(), 1);
    assert_eq!(
        detail.observations[0].availability_source,
        "exportVerification"
    );

    // Serialized detail carries no path-like or filename fields.
    let text = serde_json::to_string(&detail).unwrap();
    assert!(!text.contains("Path\"") && !text.contains("filename"));
}

#[test]
fn identical_imports_reuse_and_refreshed_receipts_append_observations() {
    let root = TempDir::new().unwrap();
    let catalog = catalog(&root);
    let targets = catalog.review_targets();
    let first = targets
        .import(PROJECT, &request(valid()), &actor(), now())
        .unwrap();
    let preflight = targets
        .preflight(PROJECT, &request(valid()), now())
        .unwrap();
    assert_eq!(preflight.outcome, PreflightOutcome::Duplicate);
    assert_eq!(
        preflight.existing_review_target_id.as_deref(),
        Some(first.review_target.summary.review_target_id.as_str())
    );
    let again = targets
        .import(PROJECT, &request(valid()), &actor(), now())
        .unwrap();
    assert_eq!(again.outcome, ImportOutcome::Reused);
    assert_eq!(
        again.review_target.summary.review_target_id,
        first.review_target.summary.review_target_id
    );

    let mut refreshed = handoff_with(|_| {});
    refreshed.verification.verified_at = "2026-11-02T11:30:00Z".into();
    let refreshed = serde_json::to_value(refreshed).unwrap();
    assert_eq!(
        targets
            .preflight(PROJECT, &request(refreshed.clone()), now())
            .unwrap()
            .outcome,
        PreflightOutcome::Refresh
    );
    let result = targets
        .import(PROJECT, &request(refreshed), &actor(), now())
        .unwrap();
    assert_eq!(result.outcome, ImportOutcome::Refreshed);
    assert_eq!(result.review_target.observations.len(), 2);
    assert_eq!(
        result.review_target.summary.latest_verified_at,
        "2026-11-02T11:30:00Z"
    );
    assert_eq!(count_targets(&catalog), 1);
}

#[test]
fn every_failure_has_a_distinct_stable_code_and_writes_nothing() {
    let root = TempDir::new().unwrap();
    let catalog = catalog(&root);
    let targets = catalog.review_targets();
    let reject = |value: Value| {
        rejected_code(
            targets
                .import(PROJECT, &request(value), &actor(), now())
                .unwrap_err(),
        )
    };

    let mut version = valid();
    version["schemaVersion"] = json!(2);
    version["futureField"] = json!(true);
    assert_eq!(reject(version), "review_target_unsupported_version");

    let mut tampered = valid();
    tampered["target"]["cuts"][0]["sourceInSeconds"] = json!(1.5);
    assert_eq!(reject(tampered), "review_target_digest_mismatch");

    let mut unknown = valid();
    unknown["target"]["movie"]["path"] = json!("/Volumes/private/movie.mp4");
    assert_eq!(reject(unknown), "review_target_invalid");

    let mut unverified = valid();
    unverified["verification"]["outputMatches"] = json!(false);
    assert_eq!(reject(unverified), "review_target_artifact_not_verified");

    let mut stale = valid();
    stale["verification"]["verifiedAt"] = json!("2026-10-01T00:00:00Z");
    assert_eq!(reject(stale), "review_target_verification_expired");

    let mut future = valid();
    future["verification"]["verifiedAt"] = json!("2026-11-03T00:00:00Z");
    assert_eq!(reject(future), "review_target_verification_expired");

    let other =
        serde_json::to_value(handoff_with(|t| t.project_id = "project_other".into())).unwrap();
    assert_eq!(reject(other), "review_target_project_mismatch");

    let mut missing = request(valid());
    missing.availability = Some(ArtifactAvailabilityAssertion {
        state: ArtifactAvailabilityState::Missing,
        observed_at: "2026-11-02T11:59:00Z".into(),
    });
    assert_eq!(
        rejected_code(
            targets
                .import(PROJECT, &missing, &actor(), now())
                .unwrap_err()
        ),
        "review_target_artifact_unavailable"
    );

    let mut strict = request(valid());
    strict.max_verification_age_seconds = Some(60);
    assert_eq!(
        rejected_code(
            targets
                .import(PROJECT, &strict, &actor(), now())
                .unwrap_err()
        ),
        "review_target_verification_expired"
    );

    let mut path_issuer = request(valid());
    path_issuer.declared_issuer = Some("/Users/someone/laptop".into());
    assert_eq!(
        rejected_code(
            targets
                .import(PROJECT, &path_issuer, &actor(), now())
                .unwrap_err()
        ),
        "invalid_input"
    );

    assert!(matches!(
        targets.import("project_unknown", &request(valid()), &actor(), now()),
        Err(ReviewTargetServiceError::ProjectNotFound)
    ));
    assert_eq!(count_targets(&catalog), 0);
}

#[test]
fn incomplete_or_overlapping_mappings_are_refused_by_the_shared_validator() {
    let root = TempDir::new().unwrap();
    let catalog = catalog(&root);
    let targets = catalog.review_targets();
    let mut handoff: ReviewTargetHandoff = serde_json::from_str(FIXTURE).unwrap();
    handoff.target.project_id = PROJECT.into();
    handoff.target.cuts[1].movie_in_pts += 1;
    handoff.target_digest = ReviewTargetHandoff::snapshot_digest(&handoff.target).unwrap();
    handoff.verification.verified_at = "2026-11-02T11:00:00Z".into();
    let preflight = targets
        .preflight(
            PROJECT,
            &request(serde_json::to_value(&handoff).unwrap()),
            now(),
        )
        .unwrap();
    assert_eq!(preflight.outcome, PreflightOutcome::Blocked);
    let failure = preflight
        .checks
        .iter()
        .find(|c| c.status == ReviewTargetCheckStatus::Fail)
        .unwrap();
    assert_eq!(failure.code, Some("review_target_invalid"));
    assert!(failure.detail.contains("mapping"));

    handoff.target.cuts[1].movie_in_pts -= 1;
    handoff.target.cuts.pop();
    handoff.target_digest = ReviewTargetHandoff::snapshot_digest(&handoff.target).unwrap();
    let incomplete = targets
        .preflight(
            PROJECT,
            &request(serde_json::to_value(&handoff).unwrap()),
            now(),
        )
        .unwrap();
    assert!(!incomplete.importable);
}

#[test]
fn reusing_a_viewed_artifact_with_different_bytes_is_a_conflict_not_an_overwrite() {
    let root = TempDir::new().unwrap();
    let catalog = catalog(&root);
    let targets = catalog.review_targets();
    let original = targets
        .import(PROJECT, &request(valid()), &actor(), now())
        .unwrap();
    for edit in [
        (|t: &mut ReviewTargetSnapshot| t.movie.blake3 = "d".repeat(64))
            as fn(&mut ReviewTargetSnapshot),
        |t| t.revision_id = "editrev_other".into(),
        |t| t.plan_digest = "e".repeat(64),
        |t| t.cuts[0].source_out_seconds = 2.5,
    ] {
        let changed = serde_json::to_value(handoff_with(edit)).unwrap();
        let preflight = targets
            .preflight(PROJECT, &request(changed.clone()), now())
            .unwrap();
        assert_eq!(preflight.outcome, PreflightOutcome::Conflict);
        assert_eq!(
            preflight.conflicting_review_target_ids,
            vec![original.review_target.summary.review_target_id.clone()]
        );
        assert_eq!(
            rejected_code(
                targets
                    .import(PROJECT, &request(changed), &actor(), now())
                    .unwrap_err()
            ),
            "review_target_conflict"
        );
    }
    assert_eq!(count_targets(&catalog), 1);

    // A different YouTube association is a new snapshot, not a conflict.
    let relinked = serde_json::to_value(handoff_with(|t| {
        t.youtube_video_id = Some("synthetic02".into())
    }))
    .unwrap();
    let result = targets
        .import(PROJECT, &request(relinked), &actor(), now())
        .unwrap();
    assert_eq!(result.outcome, ImportOutcome::Created);
    assert_ne!(
        result.review_target.summary.target_digest,
        original.review_target.summary.target_digest
    );
    assert_eq!(count_targets(&catalog), 2);
}

#[test]
fn youtube_rebind_creates_a_separate_immutable_snapshot_and_keeps_the_original() {
    let root = TempDir::new().unwrap();
    let catalog = catalog(&root);
    let targets = catalog.review_targets();
    let unlinked = serde_json::to_value(handoff_with(|t| t.youtube_video_id = None)).unwrap();
    let original = targets
        .import(PROJECT, &request(unlinked), &actor(), now())
        .unwrap()
        .review_target;
    assert_eq!(original.summary.youtube.identity_status, "absent");
    let original_id = original.summary.review_target_id.clone();

    let rebound = targets
        .rebind_youtube(PROJECT, &original_id, Some("dQw4w9WgXcQ"), &actor(), now())
        .unwrap();
    assert_eq!(rebound.outcome, ImportOutcome::Created);
    let detail = rebound.review_target;
    assert_ne!(detail.summary.review_target_id, original_id);
    assert_eq!(
        detail.provenance.derived_from_review_target_id.as_deref(),
        Some(original_id.as_str())
    );
    assert_eq!(
        detail.summary.youtube.declared_by.as_deref(),
        Some("serverRebind")
    );
    assert_eq!(
        detail.summary.latest_verified_at,
        original.summary.latest_verified_at
    );
    assert_eq!(
        detail.observations[0].availability_source,
        "carriedFromParentTarget"
    );
    // The digest is the shared Rust digest of the rebound snapshot.
    let handoff = targets
        .handoff(PROJECT, &detail.summary.review_target_id)
        .unwrap();
    handoff.validate().unwrap();
    assert_eq!(
        handoff.target.youtube_video_id.as_deref(),
        Some("dQw4w9WgXcQ")
    );

    let unchanged = targets.get(PROJECT, &original_id, now()).unwrap();
    assert_eq!(unchanged.summary.youtube.video_id, None);
    assert_eq!(
        unchanged.summary.target_digest,
        original.summary.target_digest
    );

    let again = targets
        .rebind_youtube(PROJECT, &original_id, Some("dQw4w9WgXcQ"), &actor(), now())
        .unwrap();
    assert_eq!(again.outcome, ImportOutcome::Reused);
    assert!(matches!(
        targets.rebind_youtube(
            PROJECT,
            &original_id,
            Some("https://youtu.be/x"),
            &actor(),
            now()
        ),
        Err(ReviewTargetServiceError::InvalidInput(_))
    ));
    assert!(matches!(
        targets.rebind_youtube("project_other", &original_id, None, &actor(), now()),
        Err(ReviewTargetServiceError::NotFound)
    ));
}

#[test]
fn readiness_expires_without_rewriting_history_and_candidates_use_the_shared_lookup() {
    let root = TempDir::new().unwrap();
    let catalog = catalog(&root);
    let targets = catalog.review_targets();
    let id = targets
        .import(PROJECT, &request(valid()), &actor(), now())
        .unwrap()
        .review_target
        .summary
        .review_target_id;
    let later = targets
        .get(PROJECT, &id, now() + Duration::days(8))
        .unwrap();
    assert!(!later.summary.readiness.ready);
    assert_eq!(
        later.summary.readiness.freshness,
        VerificationFreshness::Expired
    );
    assert_eq!(
        later.summary.readiness.blockers,
        vec!["review_target_verification_expired"]
    );
    assert_eq!(later.observations.len(), 1);

    let point = targets.candidates(PROJECT, &id, 2.0, 2.0).unwrap();
    assert_eq!(point.operation_ids, vec!["editop_split_b"]);
    let range = targets.candidates(PROJECT, &id, 1.5, 2.5).unwrap();
    assert_eq!(
        range.operation_ids,
        vec!["editop_split_a", "editop_split_b"]
    );
    // The audio tail has no picture candidate.
    assert!(
        targets
            .candidates(PROJECT, &id, 6.01, 6.01)
            .unwrap()
            .operation_ids
            .is_empty()
    );
    assert!(matches!(
        targets.candidates(PROJECT, &id, 0.0, 99.0),
        Err(ReviewTargetServiceError::InvalidInput(_))
    ));
}

#[test]
fn listing_is_bounded_newest_first_and_project_scoped() {
    let root = TempDir::new().unwrap();
    let catalog = catalog(&root);
    let targets = catalog.review_targets();
    for id in ["synthetic01", "synthetic02", "synthetic03"] {
        let value =
            serde_json::to_value(handoff_with(|t| t.youtube_video_id = Some(id.into()))).unwrap();
        targets
            .import(PROJECT, &request(value), &actor(), now())
            .unwrap();
    }
    let page = targets.list(PROJECT, 2, None, now()).unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(
        page.items[0].youtube.video_id.as_deref(),
        Some("synthetic03")
    );
    let rest = targets.list(PROJECT, 2, page.next_before, now()).unwrap();
    assert_eq!(rest.items.len(), 1);
    assert_eq!(rest.next_before, None);
    assert!(
        targets
            .list("project_other", 10, None, now())
            .unwrap()
            .items
            .is_empty()
    );
    assert!(targets.list(PROJECT, 0, None, now()).is_err());
    assert!(targets.list(PROJECT, 201, None, now()).is_err());
}

#[test]
fn a_stricter_accepted_lifetime_keeps_governing_readiness() {
    let root = TempDir::new().unwrap();
    let catalog = catalog(&root);
    let targets = catalog.review_targets();
    let mut strict = request(valid());
    strict.max_verification_age_seconds = Some(2 * 3600);
    let id = targets
        .import(PROJECT, &strict, &actor(), now())
        .unwrap()
        .review_target
        .summary
        .review_target_id;
    // verifiedAt is one hour before now(); four hours later it is three hours old.
    let later = targets
        .get(PROJECT, &id, now() + Duration::hours(3))
        .unwrap();
    assert_eq!(later.summary.readiness.max_verification_age_seconds, 7200);
    assert!(!later.summary.readiness.ready);
    assert_eq!(
        later.summary.readiness.blockers,
        vec!["review_target_verification_expired"]
    );
    // A later default-lifetime import never loosens the accepted policy.
    targets
        .import(PROJECT, &request(valid()), &actor(), now())
        .unwrap();
    assert!(
        !targets
            .get(PROJECT, &id, now() + Duration::hours(3))
            .unwrap()
            .summary
            .readiness
            .ready
    );
    // A YouTube rebind inherits it.
    let rebound = targets
        .rebind_youtube(PROJECT, &id, Some("synthetic07"), &actor(), now())
        .unwrap()
        .review_target
        .summary
        .review_target_id;
    assert_eq!(
        targets
            .get(PROJECT, &rebound, now())
            .unwrap()
            .summary
            .readiness
            .max_verification_age_seconds,
        7200
    );
}

#[test]
fn an_adverse_availability_report_is_retained_and_blocks_new_sessions() {
    let root = TempDir::new().unwrap();
    let catalog = catalog(&root);
    let targets = catalog.review_targets();
    let id = targets
        .import(PROJECT, &request(valid()), &actor(), now())
        .unwrap()
        .review_target
        .summary
        .review_target_id;
    assert!(
        targets
            .get(PROJECT, &id, now())
            .unwrap()
            .summary
            .readiness
            .ready
    );
    let mut missing = request(valid());
    missing.availability = Some(ArtifactAvailabilityAssertion {
        state: ArtifactAvailabilityState::Missing,
        observed_at: "2026-11-02T11:30:00Z".into(),
    });
    assert_eq!(
        rejected_code(
            targets
                .import(PROJECT, &missing, &actor(), now())
                .unwrap_err()
        ),
        "review_target_artifact_unavailable"
    );
    let detail = targets.get(PROJECT, &id, now()).unwrap();
    assert!(!detail.summary.readiness.ready);
    assert_eq!(
        detail.summary.readiness.availability_state,
        ArtifactAvailabilityState::Missing
    );
    assert_eq!(
        detail.summary.readiness.blockers,
        vec!["review_target_artifact_unavailable"]
    );
    assert_eq!(detail.observations.len(), 2);
    // A later present observation restores it.
    let mut present = request(valid());
    present.availability = Some(ArtifactAvailabilityAssertion {
        state: ArtifactAvailabilityState::Present,
        observed_at: "2026-11-02T11:45:00Z".into(),
    });
    targets.import(PROJECT, &present, &actor(), now()).unwrap();
    assert!(
        targets
            .get(PROJECT, &id, now())
            .unwrap()
            .summary
            .readiness
            .ready
    );
    // An adverse report for a target never imported creates nothing.
    let mut unknown = request(
        serde_json::to_value(handoff_with(|t| {
            t.youtube_video_id = Some("synthetic05".into())
        }))
        .unwrap(),
    );
    unknown.availability = missing.availability.clone();
    assert!(targets.import(PROJECT, &unknown, &actor(), now()).is_err());
    assert_eq!(count_targets(&catalog), 1);
}
