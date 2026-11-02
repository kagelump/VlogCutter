//! Server retention of imported, path-free TV review targets (#221).
//!
//! The owning desktop/CLI catalog exports a version-1 `ReviewTargetHandoff`
//! after freshly verifying a complete preview. This module is the server's
//! *import and retention* owner for that snapshot: it runs the shared Rust
//! validator, applies an explicit freshness/availability policy, deduplicates
//! by `(project, targetDigest)`, refuses a conflicting reuse of one viewed
//! artifact, and records importer identity and provenance separately from the
//! snapshot itself.
//!
//! It deliberately does **not** materialize the cuts as an edit plan, write
//! canonical feedback, or claim that a pasted YouTube ID proves what a TV is
//! playing. Canonical edit-plan feedback remains owned by the project catalog
//! on the owning host (`edit-plan feedback record`).
use crate::{AuditActorType, open_read_only_connection, open_writable_connection};
use anyhow::Context;
use chrono::{DateTime, SecondsFormat, Utc};
use rcdvlog_core::{
    REVIEW_TARGET_SCHEMA_VERSION, ReviewTargetCut, ReviewTargetError, ReviewTargetHandoff,
    ReviewTargetMovie, ReviewTargetSnapshot, ReviewTargetVerification,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;
use uuid::Uuid;

/// The longest a verification receipt may be relied on for a *new* import or
/// a *new* recording session. Callers may request a stricter lifetime, never
/// a longer one. Receipts are never renewed by the server; re-export on the
/// owning host instead.
pub const REVIEW_TARGET_SERVER_MAX_VERIFICATION_AGE_SECONDS: u32 = 7 * 24 * 60 * 60;
pub const REVIEW_TARGET_LIST_MAX_LIMIT: u32 = 200;
const DECLARED_ISSUER_MAX_CHARS: usize = 64;

/// Fixed, honest statements about what an imported target can and cannot
/// establish. Returned with every detail so the UI never has to invent them.
pub const REVIEW_TARGET_LIMITATIONS: &[&str] = &[
    "The target digest establishes snapshot consistency, not the identity of the issuing catalog.",
    "Verification was performed by the owning host at verifiedAt; the server has not seen the movie bytes.",
    "A YouTube video ID is a declaration only. Nothing verifies that YouTube or the TV plays these exact bytes.",
    "Source in/out values are declared source-clock locators, not decoded original-frame correspondence.",
    "Declared audio describes intent; it does not prove audible presence or measured alignment.",
    "A recording made against this target does not by itself establish human audiovisual coverage.",
];

#[derive(Debug, Error)]
pub enum ReviewTargetServiceError {
    #[error("project is not registered on this server")]
    ProjectNotFound,
    #[error("review target not found")]
    NotFound,
    /// A preflight check failed; the stable code names the first blocking check.
    #[error("review target rejected: {code}")]
    Rejected {
        code: &'static str,
        preflight: Box<ReviewTargetPreflight>,
    },
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error(transparent)]
    Infrastructure(#[from] anyhow::Error),
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
}

impl ReviewTargetServiceError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ProjectNotFound => "project_replica_not_found",
            Self::NotFound => "not_found",
            Self::Rejected { code, .. } => code,
            Self::InvalidInput(_) => "invalid_input",
            Self::Infrastructure(_) | Self::Database(_) => "internal_error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArtifactAvailabilityState {
    Unknown,
    Present,
    Missing,
    Modified,
}

impl ArtifactAvailabilityState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Present => "present",
            Self::Missing => "missing",
            Self::Modified => "modified",
        }
    }
    fn parse(value: &str) -> Self {
        match value {
            "present" => Self::Present,
            "missing" => Self::Missing,
            "modified" => Self::Modified,
            _ => Self::Unknown,
        }
    }
}

/// An explicit observation of the movie on its owning host. When omitted, the
/// server records `present` at `verifiedAt`, sourced from the export receipt
/// (the exporter refuses missing or modified bytes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactAvailabilityAssertion {
    pub state: ArtifactAvailabilityState,
    pub observed_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewTargetImportRequest {
    /// The `data` member of `edit-plan.review-target-export`, not the CLI envelope.
    pub handoff: serde_json::Value,
    /// Short human label for the owning host. A provenance assertion by the
    /// authenticated uploader; never authenticated as an issuer.
    #[serde(default)]
    pub declared_issuer: Option<String>,
    #[serde(default)]
    pub availability: Option<ArtifactAvailabilityAssertion>,
    /// Optional stricter lifetime, capped at the server policy.
    #[serde(default)]
    pub max_verification_age_seconds: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTargetActor {
    pub actor_type: String,
    pub actor_id: String,
}

impl ReviewTargetActor {
    pub fn new(actor_type: AuditActorType, actor_id: &str) -> Self {
        Self {
            actor_type: actor_type.as_str().to_string(),
            actor_id: actor_id.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewTargetCheckStatus {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTargetCheck {
    pub name: &'static str,
    pub status: ReviewTargetCheckStatus,
    pub code: Option<&'static str>,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PreflightOutcome {
    /// Passes every check and has no stored equivalent.
    New,
    /// Identical digest and verification receipt already stored.
    Duplicate,
    /// Identical digest; this import appends a new verification observation.
    Refresh,
    /// Reuses a viewed artifact with different bytes, plan, revision or mapping.
    Conflict,
    /// Fails a structural, integrity, freshness or availability check.
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTargetPreview {
    pub project_id: String,
    pub revision_id: String,
    pub viewed_artifact_id: String,
    pub target_digest: String,
    pub movie_blake3: String,
    pub duration_seconds: f64,
    pub cut_count: usize,
    pub youtube_video_id: Option<String>,
    pub verified_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTargetPreflight {
    pub outcome: PreflightOutcome,
    pub importable: bool,
    pub checks: Vec<ReviewTargetCheck>,
    pub preview: Option<ReviewTargetPreview>,
    pub existing_review_target_id: Option<String>,
    pub conflicting_review_target_ids: Vec<String>,
    pub max_verification_age_seconds: u32,
}

impl ReviewTargetPreflight {
    fn first_failure(&self) -> Option<&'static str> {
        self.checks
            .iter()
            .find(|c| c.status == ReviewTargetCheckStatus::Fail)
            .and_then(|c| c.code)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTargetCutView {
    #[serde(flatten)]
    pub cut: ReviewTargetCut,
    /// Elapsed movie seconds derived from measured PTS by the shared formula.
    pub movie_in_seconds: f64,
    pub movie_out_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTargetYoutube {
    pub video_id: Option<String>,
    /// `export` when the owning host declared it; `serverRebind` when a server
    /// user declared it afterwards.
    pub declared_by: Option<String>,
    /// Always `declaredUnverified` when present, `absent` otherwise.
    pub identity_status: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTargetMappingStatus {
    /// `measuredComplete`: contiguous, ordered, frame-count complete (validated).
    pub status: &'static str,
    pub picture_precision: &'static str,
    pub source_range_precision: &'static str,
    pub audio_declaration: &'static str,
    pub cut_count: usize,
    pub picture_end_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTargetProvenance {
    pub imported_at: String,
    pub importer: ReviewTargetActor,
    pub declared_issuer: Option<String>,
    /// Always `unverified`: an authenticated upload is a provenance assertion.
    pub issuer_verification: &'static str,
    pub derived_from_review_target_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTargetObservation {
    pub observation_id: String,
    pub verification: ReviewTargetVerification,
    pub availability_state: ArtifactAvailabilityState,
    pub availability_observed_at: String,
    pub availability_source: String,
    /// Verification lifetime accepted with this observation. The strictest
    /// lifetime ever accepted for a target governs its readiness.
    pub max_verification_age_seconds: u32,
    pub recorded_at: String,
    pub recorded_by: ReviewTargetActor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum VerificationFreshness {
    Fresh,
    Expired,
    Future,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTargetReadiness {
    pub ready: bool,
    pub freshness: VerificationFreshness,
    pub verification_age_seconds: i64,
    pub max_verification_age_seconds: u32,
    pub availability_state: ArtifactAvailabilityState,
    pub blockers: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTargetSummary {
    pub review_target_id: String,
    pub project_id: String,
    pub revision_id: String,
    pub viewed_artifact_id: String,
    pub target_digest: String,
    pub duration_seconds: f64,
    pub cut_count: usize,
    pub youtube: ReviewTargetYoutube,
    pub imported_at: String,
    pub latest_verified_at: String,
    pub readiness: ReviewTargetReadiness,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTargetDetail {
    #[serde(flatten)]
    pub summary: ReviewTargetSummary,
    pub schema_version: u32,
    pub plan_digest: String,
    pub constraints_key: String,
    pub movie: ReviewTargetMovie,
    pub mapping: ReviewTargetMappingStatus,
    pub cuts: Vec<ReviewTargetCutView>,
    pub provenance: ReviewTargetProvenance,
    pub observations: Vec<ReviewTargetObservation>,
    pub limitations: &'static [&'static str],
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTargetPage {
    pub items: Vec<ReviewTargetSummary>,
    pub next_before: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportOutcome {
    Created,
    Reused,
    Refreshed,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTargetImportResult {
    pub outcome: ImportOutcome,
    pub review_target: ReviewTargetDetail,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTargetCandidates {
    pub review_target_id: String,
    pub start_seconds: f64,
    pub end_seconds: f64,
    /// Picture locators only. Never a source trim, audio owner, or confirmed target.
    pub operation_ids: Vec<String>,
}

#[derive(Clone)]
pub struct ReviewTargets {
    database_path: PathBuf,
}

/// Parsed and checked import, before it touches storage.
struct Evaluated {
    handoff: ReviewTargetHandoff,
    max_age: u32,
    binding_digest: String,
    availability: ArtifactAvailabilityAssertion,
    availability_source: &'static str,
}

fn timestamp(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn check(
    name: &'static str,
    status: ReviewTargetCheckStatus,
    code: Option<&'static str>,
    detail: impl Into<String>,
) -> ReviewTargetCheck {
    ReviewTargetCheck {
        name,
        status,
        code,
        detail: detail.into(),
    }
}

/// Digest of the snapshot ignoring the YouTube declaration. Two targets with
/// the same binding digest are the same movie/plan/revision/mapping.
fn binding_digest(target: &ReviewTargetSnapshot) -> anyhow::Result<String> {
    let mut unbound = target.clone();
    unbound.youtube_video_id = None;
    ReviewTargetHandoff::snapshot_digest(&unbound)
}

fn movie_seconds(movie: &ReviewTargetMovie, pts: i64) -> f64 {
    (pts as f64 - movie.origin_pts as f64) * movie.time_base_numerator as f64
        / movie.time_base_denominator as f64
}

fn validate_declared_issuer(value: Option<&str>) -> Result<Option<String>, String> {
    let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    if value.chars().count() > DECLARED_ISSUER_MAX_CHARS
        || value
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        return Err(format!(
            "declared issuer must be at most {DECLARED_ISSUER_MAX_CHARS} characters without paths or control characters"
        ));
    }
    Ok(Some(value.to_string()))
}

fn validate_youtube_video_id(value: &str) -> bool {
    value.len() == 11
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn freshness(verified_at: &str, now: DateTime<Utc>, max_age: u32) -> (VerificationFreshness, i64) {
    let Ok(at) = DateTime::parse_from_rfc3339(verified_at) else {
        return (VerificationFreshness::Expired, i64::MAX);
    };
    let age = now.signed_duration_since(at).num_seconds();
    let state = if age < 0 {
        VerificationFreshness::Future
    } else if age > i64::from(max_age) {
        VerificationFreshness::Expired
    } else {
        VerificationFreshness::Fresh
    };
    (state, age)
}

/// Readiness from the whole observation history: the freshest receipt, the
/// most recently observed availability, and the strictest accepted lifetime.
fn readiness(
    observations: &[ReviewTargetObservation],
    now: DateTime<Utc>,
) -> ReviewTargetReadiness {
    let verified_at = observations
        .iter()
        .map(|o| o.verification.verified_at.as_str())
        .max_by_key(|v| DateTime::parse_from_rfc3339(v).ok())
        .unwrap_or_default();
    // Ties go to the later record (observations are in insertion order within a receipt).
    let availability = observations
        .iter()
        .enumerate()
        .max_by_key(|(i, o)| {
            (
                DateTime::parse_from_rfc3339(&o.availability_observed_at).ok(),
                *i,
            )
        })
        .map(|(_, o)| o.availability_state)
        .unwrap_or(ArtifactAvailabilityState::Unknown);
    let max = observations
        .iter()
        .map(|o| o.max_verification_age_seconds)
        .min()
        .unwrap_or(REVIEW_TARGET_SERVER_MAX_VERIFICATION_AGE_SECONDS)
        .min(REVIEW_TARGET_SERVER_MAX_VERIFICATION_AGE_SECONDS);
    let (fresh, age) = freshness(verified_at, now, max);
    let mut blockers = Vec::new();
    if fresh != VerificationFreshness::Fresh {
        blockers.push("review_target_verification_expired");
    }
    if availability != ArtifactAvailabilityState::Present {
        blockers.push("review_target_artifact_unavailable");
    }
    ReviewTargetReadiness {
        ready: blockers.is_empty(),
        freshness: fresh,
        verification_age_seconds: age,
        max_verification_age_seconds: max,
        availability_state: availability,
        blockers,
    }
}

impl ReviewTargets {
    pub(crate) fn new(database_path: PathBuf) -> Self {
        Self { database_path }
    }

    fn project_registered(
        connection: &Connection,
        project_id: &str,
    ) -> Result<(), ReviewTargetServiceError> {
        let registered = connection
            .query_row(
                "SELECT 1 FROM project_registry WHERE project_id=?1 AND state='registered'",
                [project_id],
                |_| Ok(()),
            )
            .optional()?;
        registered.ok_or(ReviewTargetServiceError::ProjectNotFound)
    }

    /// Run every check against stored state without writing. Content problems
    /// are reported as failing checks, not as errors.
    pub fn preflight(
        &self,
        project_id: &str,
        request: &ReviewTargetImportRequest,
        now: DateTime<Utc>,
    ) -> Result<ReviewTargetPreflight, ReviewTargetServiceError> {
        let connection = open_read_only_connection(&self.database_path)?;
        Self::project_registered(&connection, project_id)?;
        Ok(Self::evaluate(&connection, project_id, request, now)?.0)
    }

    fn evaluate(
        connection: &Connection,
        project_id: &str,
        request: &ReviewTargetImportRequest,
        now: DateTime<Utc>,
    ) -> Result<(ReviewTargetPreflight, Option<Evaluated>), ReviewTargetServiceError> {
        let max_age = request
            .max_verification_age_seconds
            .unwrap_or(REVIEW_TARGET_SERVER_MAX_VERIFICATION_AGE_SECONDS)
            .min(REVIEW_TARGET_SERVER_MAX_VERIFICATION_AGE_SECONDS);
        let mut report = ReviewTargetPreflight {
            outcome: PreflightOutcome::Blocked,
            importable: false,
            checks: Vec::new(),
            preview: None,
            existing_review_target_id: None,
            conflicting_review_target_ids: Vec::new(),
            max_verification_age_seconds: max_age,
        };
        if max_age == 0 {
            report.checks.push(check(
                "policy",
                ReviewTargetCheckStatus::Fail,
                Some("invalid_input"),
                "verification lifetime must be positive",
            ));
            return Ok((report, None));
        }
        let declared_issuer = match validate_declared_issuer(request.declared_issuer.as_deref()) {
            Ok(value) => value,
            Err(message) => {
                report.checks.push(check(
                    "declaredIssuer",
                    ReviewTargetCheckStatus::Fail,
                    Some("invalid_input"),
                    message,
                ));
                return Ok((report, None));
            }
        };

        // Version first, so an unknown future version is reported as such
        // rather than as an unknown-field parse failure.
        let version = request
            .handoff
            .get("schemaVersion")
            .and_then(serde_json::Value::as_u64);
        if version != Some(u64::from(REVIEW_TARGET_SCHEMA_VERSION)) {
            report.checks.push(check(
                "schemaVersion",
                ReviewTargetCheckStatus::Fail,
                Some("review_target_unsupported_version"),
                format!(
                    "this server imports review target schema version {REVIEW_TARGET_SCHEMA_VERSION} only"
                ),
            ));
            return Ok((report, None));
        }
        report.checks.push(check(
            "schemaVersion",
            ReviewTargetCheckStatus::Pass,
            None,
            "schema version 1",
        ));

        let bytes = serde_json::to_vec(&request.handoff).context("serialize handoff")?;
        let handoff = match ReviewTargetHandoff::from_json(&bytes) {
            Ok(handoff) => handoff,
            Err(error) => {
                let code = match error.downcast_ref::<ReviewTargetError>() {
                    Some(typed) => typed.code(),
                    None => "review_target_invalid",
                };
                let name = match code {
                    "review_target_digest_mismatch" => "targetDigest",
                    "review_target_artifact_not_verified" => "verificationReceipt",
                    _ => "structure",
                };
                report.checks.push(check(
                    name,
                    ReviewTargetCheckStatus::Fail,
                    Some(code),
                    error.to_string(),
                ));
                return Ok((report, None));
            }
        };
        report.checks.push(check(
            "structure",
            ReviewTargetCheckStatus::Pass,
            None,
            "shape, limits, unique operations and contiguous frame-complete mapping validated by the shared Rust validator",
        ));
        report.checks.push(check(
            "targetDigest",
            ReviewTargetCheckStatus::Pass,
            None,
            "recomputed by the shared Rust digest; establishes consistency only",
        ));
        report.checks.push(check(
            "verificationReceipt",
            ReviewTargetCheckStatus::Pass,
            None,
            "owning host reported output, inputs, constraints, renderability and full decode",
        ));
        let target = &handoff.target;
        report.preview = Some(ReviewTargetPreview {
            project_id: target.project_id.clone(),
            revision_id: target.revision_id.clone(),
            viewed_artifact_id: target.viewed_artifact_id.clone(),
            target_digest: handoff.target_digest.clone(),
            movie_blake3: target.movie.blake3.clone(),
            duration_seconds: target.movie.duration_seconds,
            cut_count: target.cuts.len(),
            youtube_video_id: target.youtube_video_id.clone(),
            verified_at: handoff.verification.verified_at.clone(),
        });

        if target.project_id != project_id {
            report.checks.push(check(
                "project",
                ReviewTargetCheckStatus::Fail,
                Some("review_target_project_mismatch"),
                "the handoff belongs to a different project than the one selected",
            ));
        } else {
            report.checks.push(check(
                "project",
                ReviewTargetCheckStatus::Pass,
                None,
                "handoff project matches the selected server project",
            ));
        }

        match handoff.require_fresh(now, max_age) {
            Ok(()) => report.checks.push(check(
                "freshness",
                ReviewTargetCheckStatus::Pass,
                None,
                format!("verified within the last {max_age} seconds"),
            )),
            Err(error) => {
                let code = error
                    .downcast_ref::<ReviewTargetError>()
                    .map(ReviewTargetError::code)
                    .unwrap_or("review_target_verification_expired");
                report.checks.push(check(
                    "freshness",
                    ReviewTargetCheckStatus::Fail,
                    Some(code),
                    "verification is expired or in the future; re-export on the owning host",
                ));
            }
        }

        let (availability, availability_source) = match &request.availability {
            Some(assertion) => {
                if DateTime::parse_from_rfc3339(&assertion.observed_at).is_err() {
                    report.checks.push(check(
                        "availability",
                        ReviewTargetCheckStatus::Fail,
                        Some("invalid_input"),
                        "availability observedAt must be RFC 3339",
                    ));
                    return Ok((report, None));
                }
                (assertion.clone(), "importerAssertion")
            }
            None => (
                ArtifactAvailabilityAssertion {
                    state: ArtifactAvailabilityState::Present,
                    observed_at: handoff.verification.verified_at.clone(),
                },
                "exportVerification",
            ),
        };
        if availability.state == ArtifactAvailabilityState::Present {
            report.checks.push(check(
                "availability",
                ReviewTargetCheckStatus::Pass,
                None,
                format!(
                    "artifact reported present on the owning host at {}",
                    availability.observed_at
                ),
            ));
        } else {
            report.checks.push(check(
                "availability",
                ReviewTargetCheckStatus::Fail,
                Some("review_target_artifact_unavailable"),
                format!(
                    "artifact availability is {}; re-verify on the owning host",
                    availability.state.as_str()
                ),
            ));
        }
        report.checks.push(check(
            "issuer",
            ReviewTargetCheckStatus::Warn,
            None,
            match &declared_issuer {
                Some(label) => {
                    format!("declared by the uploader as \"{label}\"; not authenticated")
                }
                None => {
                    "no issuer declared; the authenticated uploader is the only provenance".into()
                }
            },
        ));
        report.checks.push(check(
            "youtube",
            ReviewTargetCheckStatus::Warn,
            None,
            match &target.youtube_video_id {
                Some(id) => {
                    format!("YouTube video {id} is declared, not verified against these bytes")
                }
                None => "no YouTube video declared; one can be associated after import".into(),
            },
        ));

        let binding = binding_digest(target)?;
        // Same digest: duplicate or refresh.
        let existing: Option<String> = connection
            .query_row(
                "SELECT review_target_id FROM review_targets WHERE project_id=?1 AND target_digest=?2",
                params![project_id, handoff.target_digest],
                |row| row.get(0),
            )
            .optional()?;
        // Same artifact with different binding: conflict.
        let mut conflicts = Vec::new();
        {
            let mut statement = connection.prepare(
                "SELECT review_target_id FROM review_targets
                 WHERE project_id=?1 AND viewed_artifact_id=?2 AND binding_digest<>?3
                 ORDER BY sequence",
            )?;
            let rows = statement.query_map(
                params![project_id, target.viewed_artifact_id, binding],
                |row| row.get::<_, String>(0),
            )?;
            for row in rows {
                conflicts.push(row?);
            }
        }
        if conflicts.is_empty() {
            report.checks.push(check(
                "identity",
                ReviewTargetCheckStatus::Pass,
                None,
                match &existing {
                    Some(id) => format!("identical target already stored as {id}"),
                    None => "no stored target reuses this viewed artifact differently".into(),
                },
            ));
        } else {
            report.checks.push(check(
                "identity",
                ReviewTargetCheckStatus::Fail,
                Some("review_target_conflict"),
                "this viewed artifact was already imported with different movie bytes, plan, revision or cut mapping; nothing will be overwritten",
            ));
        }
        report.conflicting_review_target_ids = conflicts;

        let failed = report.first_failure().is_some();
        report.importable = !failed;
        report.existing_review_target_id = existing.clone();
        report.outcome = if !report.conflicting_review_target_ids.is_empty() {
            PreflightOutcome::Conflict
        } else if failed {
            PreflightOutcome::Blocked
        } else if let Some(id) = &existing {
            let duplicate = connection
                .query_row(
                    "SELECT 1 FROM review_target_observations
                     WHERE review_target_id=?1 AND verified_at=?2
                       AND availability_state=?3 AND availability_observed_at=?4
                       AND max_verification_age_seconds=?5",
                    params![
                        id,
                        handoff.verification.verified_at,
                        availability.state.as_str(),
                        availability.observed_at,
                        max_age
                    ],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if duplicate {
                PreflightOutcome::Duplicate
            } else {
                PreflightOutcome::Refresh
            }
        } else {
            PreflightOutcome::New
        };
        // An adverse availability report on an already-imported target is
        // still evidence: it is retained (and blocks new sessions) even
        // though the import itself is refused.
        let only_availability_failed = report
            .checks
            .iter()
            .filter(|c| c.status == ReviewTargetCheckStatus::Fail)
            .all(|c| c.code == Some("review_target_artifact_unavailable"));
        let evaluated =
            (!failed || (only_availability_failed && existing.is_some())).then_some(Evaluated {
                handoff,
                max_age,
                binding_digest: binding,
                availability,
                availability_source,
            });
        Ok((report, evaluated))
    }

    /// Import after an identical preflight inside one immediate transaction.
    /// Identical imports reuse the stored target; nothing is ever overwritten.
    pub fn import(
        &self,
        project_id: &str,
        request: &ReviewTargetImportRequest,
        actor: &ReviewTargetActor,
        now: DateTime<Utc>,
    ) -> Result<ReviewTargetImportResult, ReviewTargetServiceError> {
        let mut connection = open_writable_connection(&self.database_path)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::project_registered(&tx, project_id)?;
        let (report, evaluated) = Self::evaluate(&tx, project_id, request, now)?;
        if let Some(code) = report.first_failure() {
            if let (Some(adverse), Some(id)) = (&evaluated, &report.existing_review_target_id) {
                insert_observation(
                    &tx,
                    id,
                    &adverse.handoff.verification,
                    &adverse.availability,
                    adverse.availability_source,
                    adverse.max_age,
                    actor,
                    now,
                )?;
                tx.commit()?;
            }
            return Err(ReviewTargetServiceError::Rejected {
                code,
                preflight: Box::new(report),
            });
        }
        let evaluated = evaluated.context("evaluated import")?;
        let declared_issuer = validate_declared_issuer(request.declared_issuer.as_deref())
            .map_err(ReviewTargetServiceError::InvalidInput)?;
        let (review_target_id, outcome) = match report.existing_review_target_id.clone() {
            Some(id) => {
                let inserted = insert_observation(
                    &tx,
                    &id,
                    &evaluated.handoff.verification,
                    &evaluated.availability,
                    evaluated.availability_source,
                    evaluated.max_age,
                    actor,
                    now,
                )?;
                (
                    id,
                    if inserted {
                        ImportOutcome::Refreshed
                    } else {
                        ImportOutcome::Reused
                    },
                )
            }
            None => {
                let id = insert_target(
                    &tx,
                    &evaluated.handoff,
                    &evaluated.binding_digest,
                    evaluated
                        .handoff
                        .target
                        .youtube_video_id
                        .as_ref()
                        .map(|_| "export"),
                    None,
                    declared_issuer.as_deref(),
                    actor,
                    now,
                )?;
                insert_observation(
                    &tx,
                    &id,
                    &evaluated.handoff.verification,
                    &evaluated.availability,
                    evaluated.availability_source,
                    evaluated.max_age,
                    actor,
                    now,
                )?;
                (id, ImportOutcome::Created)
            }
        };
        let detail = load_detail(&tx, project_id, &review_target_id, now)?;
        tx.commit()?;
        Ok(ReviewTargetImportResult {
            outcome,
            review_target: detail,
        })
    }

    /// Declare (or clear) the YouTube video for an existing target. This
    /// creates — or reuses — a separate immutable target snapshot, digested by
    /// the shared Rust digest; the original target and any session bound to it
    /// are untouched. The latest verification receipt is carried over because
    /// it describes the same movie bytes.
    pub fn rebind_youtube(
        &self,
        project_id: &str,
        review_target_id: &str,
        youtube_video_id: Option<&str>,
        actor: &ReviewTargetActor,
        now: DateTime<Utc>,
    ) -> Result<ReviewTargetImportResult, ReviewTargetServiceError> {
        if let Some(id) = youtube_video_id
            && !validate_youtube_video_id(id)
        {
            return Err(ReviewTargetServiceError::InvalidInput(
                "YouTube video ID must contain 11 URL-safe characters".into(),
            ));
        }
        let mut connection = open_writable_connection(&self.database_path)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::project_registered(&tx, project_id)?;
        let stored = load_stored(&tx, project_id, review_target_id)?;
        let parent_observations = load_observations(&tx, review_target_id)?;
        let parent_policy = parent_observations
            .iter()
            .map(|o| o.max_verification_age_seconds)
            .min()
            .unwrap_or(REVIEW_TARGET_SERVER_MAX_VERIFICATION_AGE_SECONDS);
        let latest = parent_observations
            .last()
            .cloned()
            .context("review target has no verification observation")?;
        let mut target = stored.snapshot.clone();
        target.youtube_video_id = youtube_video_id.map(str::to_string);
        let handoff = ReviewTargetHandoff {
            schema_version: REVIEW_TARGET_SCHEMA_VERSION,
            target_digest: ReviewTargetHandoff::snapshot_digest(&target)?,
            target,
            verification: latest.verification.clone(),
        };
        handoff.validate()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT review_target_id FROM review_targets WHERE project_id=?1 AND target_digest=?2",
                params![project_id, handoff.target_digest],
                |row| row.get(0),
            )
            .optional()?;
        let (id, outcome) = match existing {
            Some(id) => (id, ImportOutcome::Reused),
            None => {
                let id = insert_target(
                    &tx,
                    &handoff,
                    &stored.binding_digest,
                    youtube_video_id.map(|_| "serverRebind"),
                    Some(review_target_id),
                    stored.declared_issuer.as_deref(),
                    actor,
                    now,
                )?;
                insert_observation(
                    &tx,
                    &id,
                    &latest.verification,
                    &ArtifactAvailabilityAssertion {
                        state: latest.availability_state,
                        observed_at: latest.availability_observed_at.clone(),
                    },
                    "carriedFromParentTarget",
                    parent_policy,
                    actor,
                    now,
                )?;
                (id, ImportOutcome::Created)
            }
        };
        let detail = load_detail(&tx, project_id, &id, now)?;
        tx.commit()?;
        Ok(ReviewTargetImportResult {
            outcome,
            review_target: detail,
        })
    }

    pub fn list(
        &self,
        project_id: &str,
        limit: u32,
        before: Option<i64>,
        now: DateTime<Utc>,
    ) -> Result<ReviewTargetPage, ReviewTargetServiceError> {
        if limit == 0 || limit > REVIEW_TARGET_LIST_MAX_LIMIT {
            return Err(ReviewTargetServiceError::InvalidInput(format!(
                "limit must be 1..={REVIEW_TARGET_LIST_MAX_LIMIT}"
            )));
        }
        let connection = open_read_only_connection(&self.database_path)?;
        Self::project_registered(&connection, project_id)?;
        let mut statement = connection.prepare(
            "SELECT sequence, review_target_id FROM review_targets
             WHERE project_id=?1 AND sequence < ?2 ORDER BY sequence DESC LIMIT ?3",
        )?;
        let rows = statement
            .query_map(
                params![project_id, before.unwrap_or(i64::MAX), i64::from(limit) + 1],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let has_more = rows.len() > limit as usize;
        let mut items = Vec::new();
        let mut last = None;
        for (sequence, id) in rows.into_iter().take(limit as usize) {
            items.push(load_detail(&connection, project_id, &id, now)?.summary);
            last = Some(sequence);
        }
        Ok(ReviewTargetPage {
            items,
            next_before: if has_more { last } else { None },
        })
    }

    pub fn get(
        &self,
        project_id: &str,
        review_target_id: &str,
        now: DateTime<Utc>,
    ) -> Result<ReviewTargetDetail, ReviewTargetServiceError> {
        let connection = open_read_only_connection(&self.database_path)?;
        Self::project_registered(&connection, project_id)?;
        load_detail(&connection, project_id, review_target_id, now)
    }

    /// The stored snapshot as a handoff carrying its latest receipt. Used by
    /// later session/sync owners; it is never re-digested or rewritten.
    pub fn handoff(
        &self,
        project_id: &str,
        review_target_id: &str,
    ) -> Result<ReviewTargetHandoff, ReviewTargetServiceError> {
        let connection = open_read_only_connection(&self.database_path)?;
        Self::project_registered(&connection, project_id)?;
        stored_handoff(&connection, project_id, review_target_id)
    }

    /// Candidate cuts for a movie-time point or range, via the shared Rust lookup.
    pub fn candidates(
        &self,
        project_id: &str,
        review_target_id: &str,
        start_seconds: f64,
        end_seconds: f64,
    ) -> Result<ReviewTargetCandidates, ReviewTargetServiceError> {
        let handoff = self.handoff(project_id, review_target_id)?;
        let operation_ids = handoff
            .picture_candidates(start_seconds, end_seconds)
            .map_err(|error| ReviewTargetServiceError::InvalidInput(error.to_string()))?;
        Ok(ReviewTargetCandidates {
            review_target_id: review_target_id.to_string(),
            start_seconds,
            end_seconds,
            operation_ids,
        })
    }
}

struct StoredTarget {
    snapshot: ReviewTargetSnapshot,
    schema_version: u32,
    target_digest: String,
    binding_digest: String,
    youtube_declared_by: Option<String>,
    derived_from: Option<String>,
    declared_issuer: Option<String>,
    imported_at: String,
    importer: ReviewTargetActor,
}

#[allow(clippy::too_many_arguments)]
fn insert_target(
    tx: &Transaction<'_>,
    handoff: &ReviewTargetHandoff,
    binding_digest: &str,
    youtube_declared_by: Option<&str>,
    derived_from: Option<&str>,
    declared_issuer: Option<&str>,
    actor: &ReviewTargetActor,
    now: DateTime<Utc>,
) -> Result<String, ReviewTargetServiceError> {
    let id = format!("rtarget_{}", Uuid::new_v4().simple());
    let t = &handoff.target;
    tx.execute(
        "INSERT INTO review_targets(
            review_target_id, project_id, schema_version, target_digest, binding_digest,
            revision_id, viewed_artifact_id, movie_blake3, movie_byte_length, duration_seconds,
            cut_count, youtube_video_id, youtube_declared_by, derived_from_review_target_id,
            snapshot_json, declared_issuer, imported_at, importer_actor_type, importer_actor_id)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
        params![
            id,
            t.project_id,
            handoff.schema_version,
            handoff.target_digest,
            binding_digest,
            t.revision_id,
            t.viewed_artifact_id,
            t.movie.blake3,
            t.movie.byte_length as i64,
            t.movie.duration_seconds,
            t.cuts.len() as i64,
            t.youtube_video_id,
            youtube_declared_by,
            derived_from,
            serde_json::to_string(t).context("serialize snapshot")?,
            declared_issuer,
            timestamp(now),
            actor.actor_type,
            actor.actor_id,
        ],
    )?;
    Ok(id)
}

/// Returns false when the identical observation is already stored.
#[allow(clippy::too_many_arguments)]
fn insert_observation(
    tx: &Transaction<'_>,
    review_target_id: &str,
    verification: &ReviewTargetVerification,
    availability: &ArtifactAvailabilityAssertion,
    availability_source: &str,
    max_age: u32,
    actor: &ReviewTargetActor,
    now: DateTime<Utc>,
) -> Result<bool, ReviewTargetServiceError> {
    let changed = tx.execute(
        "INSERT INTO review_target_observations(
            observation_id, review_target_id, verified_at, verification_json,
            availability_state, availability_observed_at, availability_source,
            recorded_at, actor_type, actor_id, max_verification_age_seconds)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
         ON CONFLICT(review_target_id, verified_at, availability_state, availability_observed_at, max_verification_age_seconds)
         DO NOTHING",
        params![
            format!("rtobs_{}", Uuid::new_v4().simple()),
            review_target_id,
            verification.verified_at,
            serde_json::to_string(verification).context("serialize verification")?,
            availability.state.as_str(),
            availability.observed_at,
            availability_source,
            timestamp(now),
            actor.actor_type,
            actor.actor_id,
            max_age,
        ],
    )?;
    Ok(changed == 1)
}

fn load_stored(
    connection: &Connection,
    project_id: &str,
    review_target_id: &str,
) -> Result<StoredTarget, ReviewTargetServiceError> {
    let row = connection
        .query_row(
            "SELECT schema_version, target_digest, binding_digest, snapshot_json,
                    youtube_declared_by, derived_from_review_target_id, declared_issuer,
                    imported_at, importer_actor_type, importer_actor_id
             FROM review_targets WHERE project_id=?1 AND review_target_id=?2",
            params![project_id, review_target_id],
            |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                ))
            },
        )
        .optional()?
        .ok_or(ReviewTargetServiceError::NotFound)?;
    Ok(StoredTarget {
        schema_version: row.0,
        target_digest: row.1,
        binding_digest: row.2,
        snapshot: serde_json::from_str(&row.3).context("decode stored review target")?,
        youtube_declared_by: row.4,
        derived_from: row.5,
        declared_issuer: row.6,
        imported_at: row.7,
        importer: ReviewTargetActor {
            actor_type: row.8,
            actor_id: row.9,
        },
    })
}

fn load_observations(
    connection: &Connection,
    review_target_id: &str,
) -> Result<Vec<ReviewTargetObservation>, ReviewTargetServiceError> {
    let mut statement = connection.prepare(
        "SELECT observation_id, verification_json, availability_state, availability_observed_at,
                availability_source, recorded_at, actor_type, actor_id, max_verification_age_seconds
         FROM review_target_observations WHERE review_target_id=?1
         ORDER BY verified_at, sequence",
    )?;
    let rows = statement
        .query_map([review_target_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, u32>(8)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|row| {
            Ok(ReviewTargetObservation {
                max_verification_age_seconds: row.8,
                observation_id: row.0,
                verification: serde_json::from_str(&row.1).context("decode stored verification")?,
                availability_state: ArtifactAvailabilityState::parse(&row.2),
                availability_observed_at: row.3,
                availability_source: row.4,
                recorded_at: row.5,
                recorded_by: ReviewTargetActor {
                    actor_type: row.6,
                    actor_id: row.7,
                },
            })
        })
        .collect()
}

fn stored_handoff(
    connection: &Connection,
    project_id: &str,
    review_target_id: &str,
) -> Result<ReviewTargetHandoff, ReviewTargetServiceError> {
    let stored = load_stored(connection, project_id, review_target_id)?;
    let latest = load_observations(connection, review_target_id)?
        .pop()
        .context("review target has no verification observation")?;
    Ok(ReviewTargetHandoff {
        schema_version: stored.schema_version,
        target_digest: stored.target_digest,
        target: stored.snapshot,
        verification: latest.verification,
    })
}

fn load_detail(
    connection: &Connection,
    project_id: &str,
    review_target_id: &str,
    now: DateTime<Utc>,
) -> Result<ReviewTargetDetail, ReviewTargetServiceError> {
    let stored = load_stored(connection, project_id, review_target_id)?;
    let observations = load_observations(connection, review_target_id)?;
    let latest = observations
        .last()
        .context("review target has no verification observation")?;
    let snapshot = &stored.snapshot;
    let movie = snapshot.movie.clone();
    let cuts: Vec<ReviewTargetCutView> = snapshot
        .cuts
        .iter()
        .map(|cut| ReviewTargetCutView {
            movie_in_seconds: movie_seconds(&movie, cut.movie_in_pts),
            movie_out_seconds: movie_seconds(&movie, cut.movie_out_pts),
            cut: cut.clone(),
        })
        .collect();
    let picture_end_seconds = cuts.last().map(|c| c.movie_out_seconds).unwrap_or(0.0);
    let summary = ReviewTargetSummary {
        review_target_id: review_target_id.to_string(),
        project_id: snapshot.project_id.clone(),
        revision_id: snapshot.revision_id.clone(),
        viewed_artifact_id: snapshot.viewed_artifact_id.clone(),
        target_digest: stored.target_digest.clone(),
        duration_seconds: movie.duration_seconds,
        cut_count: cuts.len(),
        youtube: ReviewTargetYoutube {
            video_id: snapshot.youtube_video_id.clone(),
            declared_by: stored.youtube_declared_by.clone(),
            identity_status: if snapshot.youtube_video_id.is_some() {
                "declaredUnverified"
            } else {
                "absent"
            },
        },
        imported_at: stored.imported_at.clone(),
        latest_verified_at: latest.verification.verified_at.clone(),
        readiness: readiness(&observations, now),
    };
    Ok(ReviewTargetDetail {
        summary,
        schema_version: stored.schema_version,
        plan_digest: snapshot.plan_digest.clone(),
        constraints_key: snapshot.constraints_key.clone(),
        mapping: ReviewTargetMappingStatus {
            status: "measuredComplete",
            picture_precision: "decodedOutputPts",
            source_range_precision: "declaredSourceClock",
            audio_declaration: "declaredNotMeasured",
            cut_count: cuts.len(),
            picture_end_seconds,
        },
        movie,
        cuts,
        provenance: ReviewTargetProvenance {
            imported_at: stored.imported_at,
            importer: stored.importer,
            declared_issuer: stored.declared_issuer,
            issuer_verification: "unverified",
            derived_from_review_target_id: stored.derived_from,
        },
        observations,
        limitations: REVIEW_TARGET_LIMITATIONS,
    })
}

pub(crate) fn migrate(tx: &Transaction<'_>) -> anyhow::Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE review_targets (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            review_target_id TEXT NOT NULL UNIQUE CHECK(length(review_target_id) BETWEEN 1 AND 64),
            project_id TEXT NOT NULL REFERENCES project_registry(project_id),
            schema_version INTEGER NOT NULL CHECK(schema_version = 1),
            target_digest TEXT NOT NULL CHECK(length(target_digest) = 64),
            binding_digest TEXT NOT NULL CHECK(length(binding_digest) = 64),
            revision_id TEXT NOT NULL,
            viewed_artifact_id TEXT NOT NULL,
            movie_blake3 TEXT NOT NULL CHECK(length(movie_blake3) = 64),
            movie_byte_length INTEGER NOT NULL CHECK(movie_byte_length > 0),
            duration_seconds REAL NOT NULL CHECK(duration_seconds > 0),
            cut_count INTEGER NOT NULL CHECK(cut_count > 0),
            youtube_video_id TEXT CHECK(youtube_video_id IS NULL OR length(youtube_video_id) = 11),
            youtube_declared_by TEXT CHECK(youtube_declared_by IS NULL OR youtube_declared_by IN ('export','serverRebind')),
            derived_from_review_target_id TEXT REFERENCES review_targets(review_target_id),
            snapshot_json TEXT NOT NULL CHECK(length(snapshot_json) <= 16777216),
            declared_issuer TEXT CHECK(declared_issuer IS NULL OR length(declared_issuer) <= 256),
            imported_at TEXT NOT NULL,
            importer_actor_type TEXT NOT NULL,
            importer_actor_id TEXT NOT NULL,
            UNIQUE(project_id, target_digest)
        );
        CREATE INDEX review_targets_artifact ON review_targets(project_id, viewed_artifact_id);
        CREATE TABLE review_target_observations (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            observation_id TEXT NOT NULL UNIQUE,
            review_target_id TEXT NOT NULL REFERENCES review_targets(review_target_id),
            verified_at TEXT NOT NULL,
            verification_json TEXT NOT NULL CHECK(length(verification_json) <= 4096),
            availability_state TEXT NOT NULL CHECK(availability_state IN ('unknown','present','missing','modified')),
            availability_observed_at TEXT NOT NULL,
            availability_source TEXT NOT NULL CHECK(availability_source IN ('exportVerification','importerAssertion','carriedFromParentTarget')),
            max_verification_age_seconds INTEGER NOT NULL CHECK(max_verification_age_seconds > 0),
            recorded_at TEXT NOT NULL,
            actor_type TEXT NOT NULL,
            actor_id TEXT NOT NULL,
            UNIQUE(review_target_id, verified_at, availability_state, availability_observed_at, max_verification_age_seconds)
        );
        "#,
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "review_targets_tests.rs"]
mod tests;
