//! SQLite persistence for moment artifacts and reusable analysis steps.

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::MomentArtifact;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentStep {
    pub asset_id: String,
    pub identity: String,
    pub step_key: String,
    pub actual_model: String,
    pub body: serde_json::Value,
    pub cost_usd: f64,
    pub usage_complete: bool,
}

pub(crate) fn save_artifact(
    connection: &Connection,
    asset_id: &str,
    identity: &str,
    artifact: &MomentArtifact,
) -> Result<()> {
    let body = serde_json::to_string(artifact).context("serialize moment artifact")?;
    let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true);
    connection
        .execute(
            r#"INSERT INTO moment_artifacts (asset_id, identity, body_json, created_at)
           VALUES (?1, ?2, ?3, ?4)
           ON CONFLICT(asset_id, identity) DO UPDATE SET
             body_json = excluded.body_json, created_at = excluded.created_at"#,
            params![asset_id, identity, body, created_at],
        )
        .context("save moment artifact")?;
    Ok(())
}

pub(crate) fn load_artifact(
    connection: &Connection,
    asset_id: &str,
    identity: &str,
) -> Result<Option<MomentArtifact>> {
    let body: Option<String> = connection
        .query_row(
            "SELECT body_json FROM moment_artifacts WHERE asset_id = ?1 AND identity = ?2",
            params![asset_id, identity],
            |row| row.get(0),
        )
        .optional()?;
    body.map(|json| serde_json::from_str(&json).context("decode moment artifact"))
        .transpose()
}

pub(crate) fn save_step(connection: &Connection, step: &MomentStep) -> Result<()> {
    let body = serde_json::to_string(step).context("serialize moment step")?;
    let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true);
    connection
        .execute(
            r#"INSERT INTO moment_steps
             (asset_id, identity, step_key, body_json, created_at)
           VALUES (?1, ?2, ?3, ?4, ?5)
           ON CONFLICT(asset_id, identity, step_key) DO UPDATE SET
             body_json = excluded.body_json, created_at = excluded.created_at"#,
            params![
                step.asset_id,
                step.identity,
                step.step_key,
                body,
                created_at
            ],
        )
        .context("save moment step")?;
    Ok(())
}

pub(crate) fn load_step(
    connection: &Connection,
    asset_id: &str,
    identity: &str,
    step_key: &str,
) -> Result<Option<MomentStep>> {
    let body: Option<String> = connection
        .query_row(
            "SELECT body_json FROM moment_steps WHERE asset_id = ?1 AND identity = ?2 AND step_key = ?3",
            params![asset_id, identity, step_key],
            |row| row.get(0),
        )
        .optional()?;
    body.map(|json| serde_json::from_str(&json).context("decode moment step"))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::super::{CoverageLevel, EditProfile, MomentStatus};
    use super::*;
    use crate::migrations::migrate_project_catalog;

    fn db() -> Connection {
        let mut connection = Connection::open_in_memory().unwrap();
        migrate_project_catalog(&mut connection).unwrap();
        connection
    }

    fn artifact(asset_id: &str, identity: &str) -> MomentArtifact {
        MomentArtifact {
            asset_id: asset_id.to_owned(),
            identity: identity.to_owned(),
            status: super::super::MomentStatus::NoCandidates,
            candidates: vec![],
            coverage: vec![super::super::CoverageSegment {
                span: super::super::MomentSpan {
                    start_ms: 0,
                    end_ms: 9000,
                },
                level: super::super::CoverageLevel::Sampled,
                sample_times_ms: vec![0, 4000, 8000],
                max_gap_ms: 4000,
            }],
            provenance: super::super::MomentProvenance {
                requested_provider: "openrouter".into(),
                payload: super::super::MomentPayload::Frames,
                provider: "test".into(),
                requested_model: "requested".into(),
                actual_model: "actual".into(),
                prompt_version: "p1".into(),
                schema_version: "s1".into(),
                recipe_version: "r1".into(),
                representation: "proxy".into(),
                representation_fingerprint: "fp".into(),
                time_mapping: "identity".into(),
                source_duration_ms: 9000,
                transcript_fingerprint: None,
                observation_fingerprint: Some("obs".into()),
                profile: super::super::EditProfile::default(),
                configuration_hash: "cfg".into(),
                cost_usd: 0.01,
                usage_complete: true,
                receipts: vec![],
            },
            review_questions: vec!["Was the action visible?".into()],
        }
    }

    #[test]
    fn moment_dtos_use_expected_serde_names_and_defaults() {
        assert_eq!(
            serde_json::to_value(MomentStatus::NeedsReview).unwrap(),
            "needs_review"
        );
        assert_eq!(
            serde_json::to_value(CoverageLevel::Uninspected).unwrap(),
            "uninspected"
        );
        assert_eq!(
            EditProfile::default(),
            EditProfile {
                select_min_ms: 3000,
                select_max_ms: 8000,
                hold_min_ms: 20000,
                hold_max_ms: 30000,
            }
        );
        let json = serde_json::to_value(artifact("a", "i")).unwrap();
        assert!(json.get("assetId").is_some());
        assert!(json["coverage"][0].get("sampleTimesMs").is_some());
        assert_eq!(json["coverage"][0]["level"], "sampled");
    }

    #[test]
    fn artifact_and_step_round_trip_and_are_isolated_by_identity_and_step() {
        let connection = db();
        connection
            .execute(
                "INSERT INTO projects(id,name,created_at) VALUES ('p','p','now')",
                [],
            )
            .unwrap();
        for id in ["a", "b"] {
            connection.execute(
                "INSERT INTO assets(id,project_id,fingerprint,path,filename,size_bytes,duration_seconds,created_at) VALUES (?1,'p',?1,'/x',?1,1,1,'now')",
                [id],
            ).unwrap();
        }
        let one = artifact("a", "recipe-one");
        save_artifact(&connection, "a", "recipe-one", &one).unwrap();
        save_artifact(&connection, "b", "recipe-one", &artifact("b", "recipe-one")).unwrap();
        save_artifact(&connection, "a", "recipe-two", &artifact("a", "recipe-two")).unwrap();
        assert_eq!(
            load_artifact(&connection, "a", "recipe-one").unwrap(),
            Some(one)
        );
        assert!(
            load_artifact(&connection, "a", "missing")
                .unwrap()
                .is_none()
        );

        let step = MomentStep {
            asset_id: "a".into(),
            identity: "recipe-one".into(),
            step_key: "sample-1".into(),
            actual_model: "actual".into(),
            body: serde_json::json!({"frames": [1, 2]}),
            cost_usd: 0.002,
            usage_complete: false,
        };
        save_step(&connection, &step).unwrap();
        assert_eq!(
            load_step(&connection, "a", "recipe-one", "sample-1").unwrap(),
            Some(step)
        );
        assert!(
            load_step(&connection, "a", "recipe-two", "sample-1")
                .unwrap()
                .is_none()
        );
        assert!(
            load_step(&connection, "b", "recipe-one", "sample-1")
                .unwrap()
                .is_none()
        );
    }
}
