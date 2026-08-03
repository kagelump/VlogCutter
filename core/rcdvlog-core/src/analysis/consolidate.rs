//! Project-wide consolidation of labels authored by clip analysis.
//!
//! This is a remap, not a re-analysis. The model sees only the proposed label
//! strings and counts. Human labels are absent because `analysis_labels` is the
//! sole input and sole ownership boundary.

use anyhow::{Result, bail};
use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::analysis::vlm_store::normalize_label;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelProposal {
    pub label: String,
    pub usage_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LabelMapping {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsolidationReport {
    pub remapped_assets: usize,
    pub vocabulary_size: usize,
    pub mappings: Vec<LabelMapping>,
    pub model: String,
    pub cost_usd: f64,
}

pub fn proposals(connection: &Connection, project_id: &str) -> Result<Vec<LabelProposal>> {
    let mut statement = connection.prepare(
        r#"
        SELECT al.display_label, COUNT(*)
        FROM analysis_labels al JOIN assets a ON a.id=al.asset_id
        WHERE a.project_id=?1
        GROUP BY al.normalized_label, al.display_label
        ORDER BY COUNT(*) DESC, al.normalized_label
        "#,
    )?;
    Ok(statement
        .query_map([project_id], |row| {
            Ok(LabelProposal {
                label: row.get(0)?,
                usage_count: row.get::<_, i64>(1)? as usize,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn consolidation_prompt(proposals: &[LabelProposal]) -> String {
    let labels = proposals
        .iter()
        .map(|proposal| format!("- {} ({})", proposal.label, proposal.usage_count))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Consolidate this AI-proposed video label vocabulary. Return one mapping for every input. \
         Merge only spelling, plurality, wording, or genuinely synonymous variants. Preserve \
         distinct concepts. Targets must be concise lowercase English labels and must not invent \
         a concept absent from the sources.\n\n{labels}"
    )
}

pub fn mapping_schema(max_items: usize) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["mappings"],
        "properties": {
            "mappings": {
                "type": "array",
                "maxItems": max_items,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["from", "to"],
                    "properties": {
                        "from": {"type": "string"},
                        "to": {"type": "string"}
                    }
                }
            }
        }
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MappingEnvelope {
    mappings: Vec<LabelMapping>,
}

pub fn parse_mapping(raw: &str, proposals: &[LabelProposal]) -> Result<Vec<LabelMapping>> {
    let envelope: MappingEnvelope = serde_json::from_str(raw)?;
    if envelope.mappings.len() > proposals.len() {
        bail!("mapping has more entries than proposals");
    }
    let known = proposals
        .iter()
        .map(|proposal| normalize_label(&proposal.label))
        .collect::<Vec<_>>();
    for mapping in &envelope.mappings {
        let source = normalize_label(&mapping.from);
        let target = normalize_label(&mapping.to);
        if !known.contains(&source) {
            bail!("mapping names unknown label: {}", mapping.from);
        }
        if target.is_empty() {
            bail!("mapping target cannot be blank: {}", mapping.from);
        }
    }
    Ok(envelope.mappings)
}

pub fn assets_for_label(
    connection: &Connection,
    project_id: &str,
    label: &str,
) -> Result<Vec<String>> {
    let normalized = normalize_label(label);
    let mut statement = connection.prepare(
        r#"
        SELECT al.asset_id FROM analysis_labels al JOIN assets a ON a.id=al.asset_id
        WHERE a.project_id=?1 AND al.normalized_label=?2 ORDER BY al.asset_id
        "#,
    )?;
    Ok(statement
        .query_map(params![project_id, normalized], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Updates AI authorship and rebuilds vocabulary after the catalog has emitted
/// the corresponding typed label remove/add operations.
pub fn apply_mapping(
    connection: &Connection,
    project_id: &str,
    mappings: &[LabelMapping],
) -> Result<(usize, usize)> {
    let known = proposals(connection, project_id)?;
    // Validate before the first write.
    let validation = serde_json::json!({"mappings": mappings});
    parse_mapping(&validation.to_string(), &known)?;

    let mut remapped = 0;
    for mapping in mappings {
        let from = normalize_label(&mapping.from);
        let to = normalize_label(&mapping.to);
        if from == to {
            continue;
        }
        let rows = assets_for_label(connection, project_id, &from)?;
        for asset_id in rows {
            let proposed: String = connection
                .query_row(
                    "SELECT proposed_label FROM analysis_labels WHERE asset_id=?1 AND normalized_label=?2",
                    params![asset_id, from],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or_else(|| mapping.from.clone());
            connection.execute(
                "DELETE FROM analysis_labels WHERE asset_id=?1 AND normalized_label=?2",
                params![asset_id, from],
            )?;
            connection.execute(
                r#"
                INSERT INTO analysis_labels(asset_id,normalized_label,display_label,proposed_label,created_at)
                VALUES (?1,?2,?3,?4,?5)
                ON CONFLICT(asset_id,normalized_label) DO UPDATE SET
                  display_label=excluded.display_label,proposed_label=excluded.proposed_label,
                  created_at=excluded.created_at
                "#,
                params![
                    asset_id,
                    to,
                    mapping.to.trim(),
                    proposed,
                    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
                ],
            )?;
            remapped += 1;
        }
    }
    connection.execute(
        "DELETE FROM label_vocabulary WHERE project_id=?1",
        [project_id],
    )?;
    connection.execute(
        r#"
        INSERT INTO label_vocabulary(project_id,normalized_label,display_label,usage_count,created_at)
        SELECT ?1,al.normalized_label,MIN(al.display_label),COUNT(*),?2
        FROM analysis_labels al JOIN assets a ON a.id=al.asset_id
        WHERE a.project_id=?1 GROUP BY al.normalized_label
        "#,
        params![
            project_id,
            Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
        ],
    )?;
    let vocabulary_size = connection.query_row(
        "SELECT COUNT(*) FROM label_vocabulary WHERE project_id=?1",
        [project_id],
        |row| row.get::<_, i64>(0),
    )? as usize;
    Ok((remapped, vocabulary_size))
}

pub fn vocabulary(connection: &Connection, project_id: &str) -> Result<Vec<LabelProposal>> {
    let mut statement = connection.prepare(
        "SELECT display_label,usage_count FROM label_vocabulary WHERE project_id=?1 ORDER BY usage_count DESC,normalized_label",
    )?;
    Ok(statement
        .query_map([project_id], |row| {
            Ok(LabelProposal {
                label: row.get(0)?,
                usage_count: row.get::<_, i64>(1)? as usize,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::vlm_store::record_ai_labels;
    use crate::migrations::migrate_project_catalog;

    fn database() -> Connection {
        let mut db = Connection::open_in_memory().unwrap();
        migrate_project_catalog(&mut db).unwrap();
        db.execute(
            "INSERT INTO projects(id,name,created_at) VALUES ('p','p','now')",
            [],
        )
        .unwrap();
        for id in ["a1", "a2", "a3"] {
            db.execute("INSERT INTO assets(id,project_id,fingerprint,path,filename,size_bytes,duration_seconds,created_at) VALUES (?1,'p',?1,?1,?1,1,1,'now')", [id]).unwrap();
        }
        record_ai_labels(&db, "a1", &[("street food".into(), "street food".into())]).unwrap();
        record_ai_labels(&db, "a2", &[("food stall".into(), "food stall".into())]).unwrap();
        record_ai_labels(&db, "a3", &[("food stalls".into(), "food stalls".into())]).unwrap();
        db
    }

    #[test]
    fn consolidation_collapses_near_duplicates_into_one_vocabulary_entry() {
        let db = database();
        let mappings = [
            LabelMapping {
                from: "street food".into(),
                to: "food stall".into(),
            },
            LabelMapping {
                from: "food stalls".into(),
                to: "food stall".into(),
            },
        ];
        let (remapped, size) = apply_mapping(&db, "p", &mappings).unwrap();
        assert_eq!(remapped, 2);
        assert_eq!(size, 1);
        assert_eq!(vocabulary(&db, "p").unwrap()[0].usage_count, 3);
    }

    #[test]
    fn a_mapping_naming_an_unknown_label_is_rejected_before_writes() {
        let db = database();
        let error = apply_mapping(
            &db,
            "p",
            &[LabelMapping {
                from: "invented".into(),
                to: "x".into(),
            }],
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("unknown label"));
        assert_eq!(proposals(&db, "p").unwrap().len(), 3);
    }

    #[test]
    fn the_prompt_contains_labels_and_counts() {
        let prompt = consolidation_prompt(&[LabelProposal {
            label: "market".into(),
            usage_count: 4,
        }]);
        assert!(prompt.contains("market (4)"));
    }
}
