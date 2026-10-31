//! Inventory-based picture usage is distinct from review or temporal coverage.
use super::{EditPlanAudio, EditPlanRevision};
use crate::Catalog;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PictureCoverageGroup {
    pub name: String,
    pub asset_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PictureCoverageRequest {
    /// Freeze a declared full source inventory when a catalog also contains non-footage rows.
    /// Omitted means every project asset, including removed/unavailable media.
    #[serde(default)]
    pub inventory_asset_ids: Option<Vec<String>>,
    pub revision_id: String,
    pub base_revision_id: Option<String>,
    pub target_percent: Option<f64>,
    #[serde(default)]
    pub groups: Vec<PictureCoverageGroup>,
}

fn picture(plan: &EditPlanRevision) -> BTreeSet<String> {
    plan.operations.iter().map(|o| o.asset_id.clone()).collect()
}

impl Catalog {
    pub fn picture_coverage(&self, request: &PictureCoverageRequest) -> Result<Value> {
        let plan = self.edit_plan_revision(&request.revision_id)?;
        let assets = self.list_assets(&plan.project_id)?;
        let project_inventory: BTreeSet<_> = assets.iter().map(|a| a.id.clone()).collect();
        let inventory: BTreeSet<_> = request
            .inventory_asset_ids
            .as_ref()
            .map(|ids| ids.iter().cloned().collect())
            .unwrap_or_else(|| project_inventory.clone());
        ensure!(
            inventory.is_subset(&project_inventory)
                && request
                    .inventory_asset_ids
                    .as_ref()
                    .is_none_or(|ids| ids.len() == inventory.len()),
            "invalid input: inventory contains duplicate or unknown source identities"
        );
        let inventory_digest = blake3::hash(&serde_json::to_vec(&inventory)?)
            .to_hex()
            .to_string();
        let used = picture(&plan);
        ensure!(
            used.is_subset(&inventory),
            "invalid input: pictured source is outside inventory"
        );
        let baseline = request
            .base_revision_id
            .as_deref()
            .map(|id| self.edit_plan_revision(id))
            .transpose()?;
        ensure!(
            baseline
                .as_ref()
                .is_none_or(|b| b.project_id == plan.project_id),
            "invalid input: coverage baseline belongs to another project"
        );
        let old = baseline.as_ref().map(picture).unwrap_or_default();
        ensure!(
            old.is_subset(&inventory),
            "invalid input: baseline picture source is outside declared inventory"
        );
        let target = request.target_percent.unwrap_or(50.0);
        ensure!(
            target.is_finite() && (0.0..=100.0).contains(&target) && request.groups.len() <= 64,
            "invalid input: target percent or group count"
        );
        let target_count = (inventory.len() as f64 * target / 100.0).ceil() as usize;
        let audio: BTreeSet<_> = plan
            .operations
            .iter()
            .filter_map(|o| match &o.audio {
                Some(EditPlanAudio::Source(a)) => Some(a.asset_id.clone()),
                _ => None,
            })
            .collect();
        let mut groups = Vec::new();
        let mut names = BTreeSet::new();
        for group in &request.groups {
            let ids: BTreeSet<_> = group.asset_ids.iter().cloned().collect();
            ensure!(
                !group.name.trim().is_empty()
                    && group.name.len() <= 255
                    && names.insert(&group.name)
                    && ids.len() == group.asset_ids.len()
                    && ids.is_subset(&inventory),
                "invalid input: coverage group name or source identities"
            );
            groups.push(json!({"name":group.name,"inventorySources":ids.len(),
                "uniquePictureSources":used.intersection(&ids).count(),
                "addedSources":used.difference(&old).filter(|id| ids.contains(*id)).count(),
                "droppedSources":old.difference(&used).filter(|id| ids.contains(*id)).count()}));
        }
        if request.groups.is_empty()
            && let Some(metadata) =
                super::selection::load_metadata(&self.connection()?, &plan.revision_id)?
        {
            for sequence in &metadata.input.sequences {
                let ids: BTreeSet<_> = sequence
                    .sources
                    .iter()
                    .map(|source| match source {
                        super::selection::SelectionMomentSource::Current { asset_id, .. }
                        | super::selection::SelectionMomentSource::Declared { asset_id, .. } => {
                            asset_id.clone()
                        }
                    })
                    .filter(|id| inventory.contains(id))
                    .collect();
                groups.push(json!({"name":sequence.sequence_id,"scope":"authored selection source inventory",
                    "inventorySources":ids.len(),"uniquePictureSources":used.intersection(&ids).count(),
                    "addedSources":used.difference(&old).filter(|id| ids.contains(*id)).count(),
                    "droppedSources":old.difference(&used).filter(|id| ids.contains(*id)).count()}));
            }
        }
        Ok(
            json!({"version":1,"revisionId":plan.revision_id,"baseRevisionId":request.base_revision_id,
            "metric":if request.inventory_asset_ids.is_some(){
                "unique picture source clips / declared source inventory; excludes audio-only use"
            }else{"unique picture source clips / full project asset inventory; excludes audio-only use"},
            "inventorySources":inventory.len(),"projectAssetRows":project_inventory.len(),
            "inventoryDigest":inventory_digest,"inventoryScope":if request.inventory_asset_ids.is_some(){"explicitSourceInventory"}else{"allProjectAssets"},"uniquePictureSources":used.len(),
            "usagePercent":if inventory.is_empty() {0.0} else {used.len() as f64 * 100.0 / inventory.len() as f64},
            "pictureOperations":plan.operations.len(),"repeatedPictureUses":plan.operations.len()-used.len(),
            "unusedSources":inventory.difference(&used).count(),
            "excludedOrInvalidSources":assets.iter().filter(|a| inventory.contains(&a.id) && (a.removed || a.duration_seconds <= 0.0)).count(),
            "offlineSources":assets.iter().filter(|a| inventory.contains(&a.id) && !a.available).count(),
            "audioOnlySources":audio.difference(&used).collect::<Vec<_>>(),
            "addedSources":used.difference(&old).collect::<Vec<_>>(),
            "retainedSources":used.intersection(&old).collect::<Vec<_>>(),
            "droppedSources":old.difference(&used).collect::<Vec<_>>(),
            "targetPercent":target,"targetUniqueSources":target_count,
            "additionalSourcesNeeded":target_count.saturating_sub(used.len()),
            "plannedDurationSeconds":plan.duration_seconds(),
            "netSourceDelta":used.len() as i64-old.len() as i64,
            "runtimeDeltaSeconds":baseline.map(|b| plan.duration_seconds()-b.duration_seconds()),
            "groups":groups,"limitations":["Groups are authored scopes or selection assignments, not inferred geography.",
                "Eligibility and availability do not change the full-inventory denominator.",
                "Counts and candidate scores do not certify editorial value or review."]}),
        )
    }

    /// Bounded retrieval of existing evidence only; never transcribes, generates or uploads.
    pub fn unused_picture_sources(
        &self,
        revision_id: &str,
        group: Option<&PictureCoverageGroup>,
        offset: usize,
        limit: usize,
        include_excluded: bool,
    ) -> Result<Value> {
        ensure!(
            limit > 0 && limit <= 100 && offset <= 1_000_000,
            "invalid input: candidate page bounds"
        );
        let plan = self.edit_plan_revision(revision_id)?;
        let used = picture(&plan);
        let mut assets = self.list_assets(&plan.project_id)?;
        let inventory: BTreeSet<_> = assets.iter().map(|a| a.id.as_str()).collect();
        if let Some(g) = group {
            ensure!(
                g.asset_ids.len() <= 100_000
                    && g.asset_ids.iter().all(|id| inventory.contains(id.as_str())),
                "invalid input: candidate group contains unknown sources"
            );
        }
        let constraints = super::ranges::load_for_project(&self.connection()?, &plan.project_id)?;
        assets
            .retain(|a| !used.contains(&a.id) && group.is_none_or(|g| g.asset_ids.contains(&a.id)));
        let mut ranked = Vec::new();
        for a in assets {
            let (allowed, conflicts) = super::ranges::feasible_ranges(
                constraints.get(&a.id).map_or(&[], Vec::as_slice),
                &a.fingerprint,
                a.duration_seconds,
                super::TrimMedia::Picture,
            );
            let excluded = !conflicts.is_empty()
                || a.removed
                || a.duration_seconds <= 0.0
                || allowed.is_empty();
            if excluded && !include_excluded {
                continue;
            }
            let text = format!(
                "{} {} {} {}",
                a.title,
                a.description,
                a.notes,
                a.labels.join(" ")
            )
            .to_lowercase();
            let hints: Vec<_> = [
                "travel",
                "walk",
                "movement",
                "preparation",
                "transfer",
                "arrival",
                "boarding",
                "packing",
            ]
            .into_iter()
            .filter(|word| text.contains(word))
            .collect();
            ranked.push((a, allowed, excluded, hints));
        }
        ranked.sort_by(|a, b| {
            a.2.cmp(&b.2)
                .then_with(|| b.3.len().cmp(&a.3.len()))
                .then_with(|| a.0.best_time.cmp(&b.0.best_time))
                .then_with(|| a.0.id.cmp(&b.0.id))
        });
        let total = ranked.len();
        let mut items = Vec::new();
        for (asset, allowed, excluded, hints) in ranked.into_iter().skip(offset).take(limit) {
            let moments = self.moment_evidence_page(&asset.id, 0, 8)?;
            let candidate_moments: Vec<_> = moments
                .as_ref()
                .map(|m| {
                    m.artifact
                        .candidates
                        .iter()
                        .filter(|c| {
                            allowed.iter().any(|r| {
                                r.source_in_seconds <= f64::from(c.core.start_ms) / 1000.0
                                    && r.source_out_seconds >= f64::from(c.core.end_ms) / 1000.0
                            })
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            items.push(json!({"assetId":asset.id,"sourceFingerprint":asset.fingerprint,
                "filename":asset.filename,"title":asset.title,"description":asset.description,
                "labels":asset.labels,"bestTime":asset.best_time,"durationSeconds":asset.duration_seconds,
                "excluded":excluded,"removed":asset.removed,"originalAvailable":asset.original_available,
                "proxyAvailable":asset.proxy_available,"connectiveRetrievalHints":hints,
                "allowedPictureRanges":allowed,"momentsCurrent":moments.as_ref().is_some_and(|m|m.current),
                "momentIdentity":moments.as_ref().map(|m|&m.artifact.identity),"candidateLocators":candidate_moments,
                "totalMomentCandidates":moments.as_ref().map_or(0,|m|m.total_candidates),
                "needsInspection":true,"limitations":["Text hints and moments are retrieval aids, not verified cut boundaries.",
                    "Offline and stale evidence remains explicit; no provider work is implied."]}));
        }
        Ok(
            json!({"revisionId":revision_id,"total":total,"offset":offset,
            "nextOffset":(offset.saturating_add(limit)<total).then_some(offset+limit),"items":items}),
        )
    }
}
