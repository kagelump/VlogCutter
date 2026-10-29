//! Bounded selected-source sound evidence. Energy is not speech or editorial QA.
use super::*;
#[cfg(test)]
#[path = "audio_integrity_tests.rs"]
mod tests;
use crate::Catalog;
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const RECIPE: &str = "selected-audio.presence.mapped-mix-energy.pcm48.v1";
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AudioIntegritySampleRequest {
    pub movie_in_seconds: f64,
    pub movie_out_seconds: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AudioIntegrityRequest {
    pub revision_id: String,
    pub scope: PreviewExcerptScope,
    pub artifact_id: Option<String>,
    pub samples: Vec<AudioIntegritySampleRequest>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioPresenceFact {
    pub available: bool,
    pub audio_present: Option<bool>,
    pub content_fingerprint: Option<String>,
    pub audio_range_seconds: Option<(f64, f64)>,
    pub reason: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedAudioPair {
    pub asset_id: String,
    pub source_fingerprint: String,
    pub rotation_degrees: u16,
    pub selected_representation: String,
    pub original: AudioPresenceFact,
    pub original_matches_registered_identity: Option<bool>,
    pub selected: AudioPresenceFact,
    pub state: String,
    pub matches_rendered_identity: Option<bool>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioEnergy {
    pub rms: f64,
    pub peak: f64,
    pub samples_per_channel: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioListeningSample {
    pub energy: AudioEnergy,
    pub playable: InspectionFile,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioIntegritySample {
    pub request: AudioIntegritySampleRequest,
    pub full_timeline_in_samples48k: u64,
    pub full_timeline_out_samples48k: u64,
    pub state: String,
    pub master: Option<AudioListeningSample>,
    pub selected: Option<AudioListeningSample>,
    pub rendered: Option<AudioListeningSample>,
    pub exceptions: Vec<String>,
    pub unchecked_reason: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioIntegrityReport {
    pub evidence_id: String,
    pub recipe: String,
    pub request: AudioIntegrityRequest,
    pub state: String,
    pub pairs: Vec<SelectedAudioPair>,
    pub derivatives: Vec<Value>,
    pub samples: Vec<AudioIntegritySample>,
    pub output_fingerprint: Option<String>,
    pub rendered_inputs_match: Option<bool>,
    pub constraints_match: Option<bool>,
    pub exceptions: Vec<String>,
    pub limitations: Vec<String>,
    pub created_at: String,
    pub sampled_energy_seconds: f64,
    pub unchecked_movie_ranges: Vec<AudioIntegritySampleRequest>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioIntegrityResult {
    pub report: AudioIntegrityReport,
    pub reused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioIntegrityProgress {
    pub stage: String,
    pub observed_sources: u32,
    pub total_sources: u32,
    pub attempted_samples: u32,
    pub total_samples: u32,
    pub worker_count: u32,
}

fn unavailable(reason: &str) -> AudioPresenceFact {
    AudioPresenceFact {
        available: false,
        audio_present: None,
        content_fingerprint: None,
        audio_range_seconds: None,
        reason: Some(reason.into()),
    }
}
fn observe(path: &Path, available: bool, cancel: &dyn Fn() -> bool) -> AudioPresenceFact {
    if !available || !path.is_file() {
        return unavailable("registered media unavailable");
    }
    match (|| -> Result<AudioPresenceFact> {
        let before = fs::metadata(path)?;
        let fingerprint = crate::full_file_blake3(path, cancel)?;
        let audio = super::audio::probe_audio_bounds(
            path.to_str().context("media path is not UTF-8")?,
            cancel,
        )?;
        let after = fs::metadata(path)?;
        ensure!(
            before.len() == after.len() && before.modified()? == after.modified()?,
            "source changed during audio probe"
        );
        Ok(AudioPresenceFact {
            available: true,
            audio_present: Some(audio.present),
            content_fingerprint: Some(fingerprint.hex_digest),
            audio_range_seconds: audio.range,
            reason: None,
        })
    })() {
        Ok(fact) => fact,
        Err(_) => unavailable("source identity or audio header unchecked"),
    }
}
fn pair_state(original: &AudioPresenceFact, selected: &AudioPresenceFact) -> &'static str {
    match (original.audio_present, selected.audio_present) {
        (None, _) => "masterUnavailableOrUnchecked",
        (_, None) => "selectedUnavailableOrUnchecked",
        (Some(true), Some(false)) => "unexpectedMissingAudio",
        (Some(false), Some(true)) => "unexpectedAddedAudio",
        (Some(false), Some(false)) => "masterHasNoAudio",
        (Some(true), Some(true)) => "audioStreamsPresent",
    }
}
fn listening(
    path: &Path,
    target: &Path,
    expected_samples: u64,
    cancel: &dyn Fn() -> bool,
) -> Result<AudioListeningSample> {
    ensure!(!cancel(), "audio audit cancelled");
    ensure!(
        path.metadata()?.len() == expected_samples * 8 && expected_samples <= 48000,
        "audio sample is incomplete or exceeds one second"
    );
    let data = fs::read(path)?;
    let mut sum = 0.0;
    let mut peak = 0.0_f64;
    for bytes in data.chunks_exact(4) {
        let value = f64::from(f32::from_le_bytes(bytes.try_into()?));
        ensure!(value.is_finite(), "nonfinite audio sample");
        sum += value * value;
        peak = peak.max(value.abs());
    }
    // IEEE float WAV preserves the measured mix without normalization/clamping.
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(target)?;
    let size = u32::try_from(data.len())?;
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + size).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16_u32.to_le_bytes())?;
    file.write_all(&3_u16.to_le_bytes())?;
    file.write_all(&2_u16.to_le_bytes())?;
    file.write_all(&48000_u32.to_le_bytes())?;
    file.write_all(&384000_u32.to_le_bytes())?;
    file.write_all(&8_u16.to_le_bytes())?;
    file.write_all(&32_u16.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&size.to_le_bytes())?;
    file.write_all(&data)?;
    file.sync_all()?;
    let hash = crate::full_file_blake3(target, cancel)?;
    Ok(AudioListeningSample {
        energy: AudioEnergy {
            rms: (sum / (expected_samples * 2) as f64).sqrt(),
            peak,
            samples_per_channel: expected_samples,
        },
        playable: InspectionFile {
            path: target.to_string_lossy().into_owned(),
            blake3: hash.hex_digest,
            byte_length: hash.byte_count,
        },
    })
}
fn mix_sample(
    root: &Path,
    name: &str,
    events: &[super::audio::AudioEvent],
    start: u64,
    end: u64,
    cancel: &dyn Fn() -> bool,
) -> Result<AudioListeningSample> {
    let directory = root.join(name);
    fs::create_dir(&directory)?;
    let raw = super::audio_render::render_mix_window(
        &directory,
        events,
        super::audio_render::AudioMixWindow {
            in_samples: start,
            out_samples: end,
        },
        cancel,
    )?;
    listening(&raw, &directory.join("listen.wav"), end - start, cancel)
}
fn classify(
    master: Option<&AudioEnergy>,
    selected: &AudioEnergy,
    rendered: &AudioEnergy,
    declared_sound: bool,
    no_audio: bool,
) -> (String, Vec<String>) {
    let mut exceptions = Vec::new();
    if master.is_some_and(|m| m.rms >= 0.001) && selected.rms <= 0.0001 {
        exceptions.push("suspected_master_sound_missing_in_selected_sample".into());
    }
    if selected.rms >= 0.001 && rendered.rms <= 0.0001 {
        exceptions.push("suspected_source_sound_missing_in_rendered_sample".into());
    }
    if no_audio && (selected.rms >= 0.001 || rendered.rms >= 0.001) {
        exceptions.push("unexpected_sound_for_no_audio_source".into());
    }
    if !declared_sound && rendered.rms >= 0.001 {
        exceptions.push("unexpected_sound_in_deliberately_silent_sample".into());
    }
    let state = if !exceptions.is_empty() {
        "exception"
    } else if !declared_sound {
        "deliberateSilence"
    } else if no_audio {
        "noSourceAudio"
    } else if master.is_none() {
        "masterUnchecked"
    } else if selected.rms <= 0.0001 && rendered.rms <= 0.0001 {
        "sampledSourceLowEnergy"
    } else {
        "sampledEnergyPresent"
    };
    (state.into(), exceptions)
}
fn files_current(report: &AudioIntegrityReport, root: &Path, cancel: &dyn Fn() -> bool) -> bool {
    report
        .samples
        .iter()
        .flat_map(|s| [s.master.as_ref(), s.selected.as_ref(), s.rendered.as_ref()])
        .flatten()
        .all(|s| {
            let path = Path::new(&s.playable.path);
            path.starts_with(root)
                && fs::symlink_metadata(path).is_ok_and(|m| {
                    m.is_file() && !m.file_type().is_symlink() && m.len() == s.playable.byte_length
                })
                && fs::canonicalize(path).is_ok_and(|p| p == path)
                && crate::full_file_blake3(path, cancel)
                    .is_ok_and(|h| h.hex_digest == s.playable.blake3)
        })
}

fn prove_inputs(
    paths: &BTreeMap<String, (PathBuf, PathBuf)>,
    pairs: &[SelectedAudioPair],
    output: Option<&str>,
    output_fp: Option<&str>,
    tools: &[(PathBuf, String)],
    cancel: &dyn Fn() -> bool,
) -> Result<()> {
    let mut expected = BTreeMap::new();
    for pair in pairs {
        let (master, selected) = &paths[&pair.asset_id];
        for (path, fact) in [(master, &pair.original), (selected, &pair.selected)] {
            if let Some(fp) = &fact.content_fingerprint {
                expected.insert(path.clone(), fp.clone());
            }
        }
    }
    if let (Some(path), Some(fp)) = (output, output_fp) {
        expected.insert(PathBuf::from(path), fp.into());
    }
    expected.extend(tools.iter().cloned());
    for (path, fp) in expected {
        ensure!(
            crate::full_file_blake3(&path, cancel)?.hex_digest == fp,
            SourceInspectionError::StaleEvidence
        );
    }
    Ok(())
}

fn derivative_contracts(
    catalog: &Catalog,
    project: &str,
    artifact: Option<&PreviewArtifact>,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<Value>> {
    let mut values = Vec::new();
    if let Some(artifact) = artifact {
        for cut in &artifact.cut_outcomes {
            if let Some(key) = &cut.cache_key {
                ensure!(!cancel(), "audio audit cancelled");
                let mut value =
                    super::preview_segments::inspect_audio_contract(catalog, project, key, cancel)
                        .unwrap_or_else(|_| json!({"segmentKey":key,"state":"unchecked"}));
                ensure!(!cancel(), "audio audit cancelled");
                value["operationId"] = json!(cut.operation_id);
                values.push(value);
            }
        }
    }
    Ok(values)
}

impl Catalog {
    pub fn audit_selected_audio(
        &self,
        request: &AudioIntegrityRequest,
        cancel: &dyn Fn() -> bool,
    ) -> Result<AudioIntegrityResult> {
        self.audit_selected_audio_with_progress(request, cancel, &mut |_| {})
    }

    pub fn audit_selected_audio_with_progress(
        &self,
        request: &AudioIntegrityRequest,
        cancel: &dyn Fn() -> bool,
        progress: &mut dyn FnMut(AudioIntegrityProgress),
    ) -> Result<AudioIntegrityResult> {
        ensure!(
            request.samples.len() <= 32
                && (request.artifact_id.is_some() || request.samples.is_empty()),
            "invalid input: audio audit needs a preview for at most 32 samples"
        );
        for sample in &request.samples {
            ensure!(
                sample.movie_in_seconds.is_finite()
                    && sample.movie_out_seconds.is_finite()
                    && sample.movie_in_seconds >= 0.0
                    && (0.02..=1.0).contains(&(sample.movie_out_seconds - sample.movie_in_seconds)),
                "invalid input: audio samples must span 20 ms to one second"
            );
        }
        ensure!(!cancel(), "audio audit cancelled");
        let plan = self.edit_plan_revision(&request.revision_id)?;
        let window = super::preview_excerpt::resolve(&plan, &request.scope)?;
        let ids = super::preview_excerpt::relevant_assets(&plan, &window);
        ensure!(
            ids.len() <= 24,
            "invalid input: bounded audio audit supports at most 24 contributing sources"
        );
        let total_sources = ids.len() as u32;
        let emit = |progress: &mut dyn FnMut(AudioIntegrityProgress),
                    stage: &str,
                    sources: usize,
                    samples: usize| {
            progress(AudioIntegrityProgress {
                stage: stage.into(),
                observed_sources: sources as u32,
                total_sources,
                attempted_samples: samples as u32,
                total_samples: request.samples.len() as u32,
                worker_count: 1,
            })
        };
        emit(progress, "waitingForResource", 0, 0);
        let artifact = request
            .artifact_id
            .as_ref()
            .map(|id| self.preview_artifact_status(id, false).map(|s| s.artifact))
            .transpose()?;
        if let Some(a) = &artifact {
            ensure!(
                a.revision_id == plan.revision_id
                    && a.project_id == plan.project_id
                    && a.excerpt.as_ref() == Some(&request.scope),
                "invalid input: audio audit artifact scope differs from revision"
            );
        }
        let duration = artifact
            .as_ref()
            .and_then(|a| a.validation.as_ref())
            .map_or(window.duration(), |v| v.duration_seconds);
        for s in &request.samples {
            ensure!(
                s.movie_out_seconds <= duration,
                "invalid input: audio sample exceeds movie duration"
            );
        }
        let _heavy = self.acquire_local_heavy_cancellable(cancel)?;
        let assets = self.list_assets(&plan.project_id)?;
        let mut pairs = Vec::new();
        let mut paths = BTreeMap::<String, (PathBuf, PathBuf)>::new();
        let mut measured = BTreeMap::<PathBuf, AudioPresenceFact>::new();
        for id in ids {
            ensure!(!cancel(), "audio audit cancelled");
            let asset = assets
                .iter()
                .find(|a| a.id == id)
                .context("audit source asset missing")?;
            let original_path = PathBuf::from(&asset.path);
            // Refresh the canonical location owner before reading registered master bytes.
            let original_verified = self
                .resolve_original(&asset.id)
                .is_ok_and(|path| path == original_path);
            let original = observe(&original_path, original_verified, cancel);
            let original_matches_registered_identity = fs::metadata(&original_path)
                .ok()
                .filter(|m| m.is_file())
                .map(|meta| {
                    original_verified
                        && original.content_fingerprint.is_some()
                        && crate::sampled_media_fingerprint(&original_path, meta.len())
                            .is_ok_and(|fingerprint| fingerprint == asset.fingerprint)
                });
            measured.insert(original_path.clone(), original.clone());
            let selected_path = PathBuf::from(super::audio::review_source(asset));
            let selected = if let Some(fact) = measured.get(&selected_path) {
                fact.clone()
            } else {
                let fact = observe(&selected_path, true, cancel);
                measured.insert(selected_path.clone(), fact.clone());
                fact
            };
            let frozen = artifact
                .as_ref()
                .and_then(|a| a.identity.as_ref())
                .and_then(|i| i.sources.iter().find(|s| s.asset_id == id));
            let matches = frozen.map(|f| {
                f.source_fingerprint == asset.fingerprint
                    && f.rotation_degrees == asset.rotation_degrees
                    && selected.content_fingerprint.as_deref()
                        == Some(f.representation_fingerprint.as_str())
            });
            paths.insert(id.to_owned(), (original_path, selected_path.clone()));
            pairs.push(SelectedAudioPair {
                asset_id: id.into(),
                source_fingerprint: asset.fingerprint.clone(),
                rotation_degrees: asset.rotation_degrees,
                selected_representation: if selected_path == Path::new(&asset.path) {
                    "original".into()
                } else {
                    format!("generatedProxy:{:?}", asset.proxy_representation)
                },
                state: pair_state(&original, &selected).into(),
                original,
                original_matches_registered_identity,
                selected,
                matches_rendered_identity: matches,
            });
            emit(progress, "checkingSources", pairs.len(), 0);
        }
        let mut tool_fingerprints = Vec::new();
        let mut tools = Vec::new();
        for name in ["ffmpeg", "ffprobe"] {
            let path = crate::resolve_tool_path(name);
            let fingerprint = crate::full_file_blake3(&path, cancel)?.hex_digest;
            tool_fingerprints.push(fingerprint.clone());
            tools.push((path, fingerprint));
        }
        let constraints = super::ranges::constraints_key(&self.connection()?, &plan)?;
        let mut output_fingerprint = None;
        let mut rendered_inputs_match = None;
        let mut constraints_match = None;
        let mut output_path = None;
        if let Some(a) = &artifact
            && let (Some(v), Some(i), Some(result)) = (&a.validation, &a.identity, &a.result)
        {
            let good = super::preview_artifacts::output_is_owned_file(self, a)
                && crate::full_file_blake3(Path::new(&result.output_path), cancel).is_ok_and(|h| {
                    h.hex_digest == v.output_fingerprint && h.byte_count == v.byte_length
                });
            if good && a.state == PreviewArtifactState::Complete && v.fully_decoded {
                output_fingerprint = Some(v.output_fingerprint.clone());
                output_path = Some(result.output_path.clone());
            }
            rendered_inputs_match = Some(
                pairs
                    .iter()
                    .all(|p| p.matches_rendered_identity == Some(true))
                    && i.tool_content_fingerprints == tool_fingerprints
                    && i.recipe == super::preview_media::RECIPE
                    && i.plan_digest
                        == super::preview_media::plan_digest(self, &plan, Some(&request.scope))?,
            );
            constraints_match = Some(i.constraints_key == constraints);
        }
        let derivatives = derivative_contracts(self, &plan.project_id, artifact.as_ref(), cancel)?;
        let identity = json!({"recipe":RECIPE,"request":request,"pairs":pairs,"tools":super::inspection_media::tool_identity(),"toolContents":tool_fingerprints,
            "planDigest":super::preview_media::plan_digest(self,&plan,Some(&request.scope))?,"constraints":constraints,"output":output_fingerprint,"inputsMatch":rendered_inputs_match,"derivatives":derivatives});
        let identity_json = serde_json::to_string(&identity)?;
        let id = format!(
            "audio_audit_{}",
            blake3::hash(identity_json.as_bytes()).to_hex()
        );
        let cache = super::preview_artifacts::managed_child(
            &super::preview_artifacts::project_directory(self)?,
            "cache",
        )?;
        let root = super::preview_artifacts::managed_child(&cache, "audio-integrity")?;
        let root = super::preview_artifacts::managed_child(&root, &id)?;
        let lock_path = root.join(".lock");
        ensure!(
            !fs::symlink_metadata(&lock_path).is_ok_and(|m| m.file_type().is_symlink()),
            "audio audit lock is a symlink"
        );
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| SourceInspectionError::Busy)?;
        let cached:Option<(String,String,String)>=self.connection()?.query_row("SELECT substr(identity_json,1,2097153),substr(report_json,1,2097153),substr(report_fingerprint,1,65) FROM audio_integrity_audits WHERE evidence_id=?1",[&id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((key, body, hash)) = cached
            && key == identity_json
            && body.len() <= 2 * 1024 * 1024
            && blake3::hash(body.as_bytes()).to_hex().as_str() == hash
            && let Ok(report) = serde_json::from_str::<AudioIntegrityReport>(&body)
            && report.evidence_id == id
            && report.request == *request
            && files_current(&report, &root, cancel)
        {
            ensure!(!cancel(), "audio audit cancelled");
            prove_inputs(
                &paths,
                &pairs,
                output_path.as_deref(),
                output_fingerprint.as_deref(),
                &tools,
                cancel,
            )?;
            self.require_audio_audit_metadata(&plan, &paths, &pairs, &constraints)?;
            ensure!(
                derivative_contracts(self, &plan.project_id, artifact.as_ref(), cancel)?
                    == report.derivatives,
                SourceInspectionError::StaleEvidence
            );
            emit(
                progress,
                "complete",
                report.pairs.len(),
                report.samples.len(),
            );
            return Ok(AudioIntegrityResult {
                report,
                reused: true,
            });
        }
        let attempt = root.join(uuid::Uuid::new_v4().simple().to_string());
        fs::create_dir(&attempt)?;
        let mut report=AudioIntegrityReport{evidence_id:id,recipe:RECIPE.into(),request:request.clone(),state:"complete".into(),pairs,derivatives,samples:vec![],output_fingerprint,rendered_inputs_match,constraints_match,exceptions:vec![],
            limitations:vec!["Presence and sampled energy do not certify words, intelligibility, wind, mixing, sync quality or clean joins; listen to the actual source and movie.".into(),
                "Only requested intervals were sampled. All other sound content is unchecked; a stream or low-energy window does not establish whole-source silence.".into(),
                "Master/proxy sample comparison uses the renderer's declared seconds correspondence, not measured word alignment or a recovered original-sample map.".into(),
                "Potential missing sound uses expected RMS at least -60 dBFS and observed RMS at most -80 dBFS; low energy is a sampled cue only.".into()],created_at:crate::now(),sampled_energy_seconds:0.0,unchecked_movie_ranges:vec![]};
        for pair in &report.pairs {
            if pair.original_matches_registered_identity != Some(true) {
                report.state = "partial".into();
                report.exceptions.push(format!(
                    "{}:master_registered_identity_unchecked_or_changed",
                    pair.asset_id
                ));
            }
            if pair.state.starts_with("unexpected") {
                report
                    .exceptions
                    .push(format!("{}:{}", pair.asset_id, pair.state));
            } else if pair.original.audio_present.is_none() || pair.selected.audio_present.is_none()
            {
                report.state = "partial".into();
                report
                    .exceptions
                    .push(format!("{}:{}", pair.asset_id, pair.state));
            }
        }
        for derived in &report.derivatives {
            if derived["state"] == "unchecked" {
                report.state = "partial".into();
                report
                    .exceptions
                    .push("silent_derivative_policy_unchecked".into());
            }
            if derived["state"] == "mismatch" {
                report
                    .exceptions
                    .push("derived_segment_audio_policy_mismatch".into());
            }
        }
        let events = if request.samples.is_empty() {
            Ok(Vec::new())
        } else {
            super::audio::audio_events_cancellable_window(
                &plan,
                &assets,
                cancel,
                Some((window.in_seconds(), window.audio_end_seconds)),
            )
        };
        let original_assets = assets
            .iter()
            .filter(|a| paths.contains_key(&a.id))
            .cloned()
            .map(|mut a| {
                a.proxy_path = None;
                a
            })
            .collect::<Vec<_>>();
        let master_events = if report.pairs.iter().any(|p| {
            p.original.audio_present.is_none()
                || p.original_matches_registered_identity != Some(true)
        }) {
            Err(anyhow::anyhow!("original sound is unchecked"))
        } else if request.samples.is_empty() {
            Ok(Vec::new())
        } else {
            super::audio::audio_events_cancellable_window(
                &plan,
                &original_assets,
                cancel,
                Some((window.in_seconds(), window.audio_end_seconds)),
            )
        };
        for (index, sample) in request.samples.iter().enumerate() {
            ensure!(!cancel(), "audio audit cancelled");
            let origin = (window.in_seconds() * 48000.0).round() as u64;
            let local_in = (sample.movie_in_seconds * 48000.0).ceil() as u64;
            let local_out = (sample.movie_out_seconds * 48000.0).floor() as u64;
            let start = origin + local_in;
            let end = origin + local_out;
            let mut item = AudioIntegritySample {
                request: sample.clone(),
                full_timeline_in_samples48k: start,
                full_timeline_out_samples48k: end,
                state: "unchecked".into(),
                master: None,
                selected: None,
                rendered: None,
                exceptions: vec![],
                unchecked_reason: None,
            };
            let outcome = (|| -> Result<()> {
                ensure!(
                    report.rendered_inputs_match == Some(true),
                    "rendered input identity is stale or unchecked"
                );
                let movie = output_path
                    .as_ref()
                    .context("completed output bytes are unavailable or changed")?;
                let events = events
                    .as_ref()
                    .map_err(|_| anyhow::anyhow!("source audio policy or handles are unchecked"))?;
                let workspace = attempt.join(format!("sample-{index}"));
                fs::create_dir(&workspace)?;
                item.selected = Some(mix_sample(
                    &workspace, "selected", events, start, end, cancel,
                )?);
                if let Ok(masters) = &master_events
                    && masters.iter().any(|event| {
                        event.timeline_start_samples < end
                            && event.timeline_start_samples + event.sample_count() > start
                    })
                {
                    item.master = Some(mix_sample(
                        &workspace, "master", masters, start, end, cancel,
                    )?);
                }
                let rendered = super::audio::AudioEvent {
                    source: movie.clone(),
                    source_in_samples: local_in,
                    source_out_samples: local_out,
                    timeline_start_samples: 0,
                    gain_db: 0.0,
                    fade_in_samples: 0,
                    fade_out_samples: 0,
                };
                item.rendered = Some(mix_sample(
                    &workspace,
                    "rendered",
                    &[rendered],
                    0,
                    end - start,
                    cancel,
                )?);
                let sources = declared_sources(&plan, start, end);
                let declared = !sources.is_empty();
                let no_audio = !sources.is_empty()
                    && sources.iter().all(|id| {
                        report.pairs.iter().any(|p| {
                            p.asset_id == *id
                                && p.original.audio_present == Some(false)
                                && p.original_matches_registered_identity == Some(true)
                        })
                    });
                let (state, exceptions) = classify(
                    item.master.as_ref().map(|s| &s.energy),
                    &item.selected.as_ref().unwrap().energy,
                    &item.rendered.as_ref().unwrap().energy,
                    declared,
                    no_audio,
                );
                item.state = state;
                item.exceptions = exceptions;
                Ok(())
            })();
            if outcome.is_err() {
                item.state = "unchecked".into();
                item.unchecked_reason=Some("Expected source/mapping/output sample could not be verified or decoded completely.".into());
                report.state = "partial".into();
            }
            if item.state == "masterUnchecked" {
                report.state = "partial".into();
            }
            report.exceptions.extend(
                item.exceptions
                    .iter()
                    .map(|e| format!("sample-{index}:{e}")),
            );
            report.samples.push(item);
            emit(
                progress,
                "checkingSamples",
                report.pairs.len(),
                report.samples.len(),
            );
        }
        if !report.exceptions.is_empty() && report.state != "partial" {
            report.state = "completeWithExceptions".into();
        }
        if request.artifact_id.is_some()
            && (report.rendered_inputs_match != Some(true) || output_path.is_none())
        {
            report.state = "partial".into();
            report
                .exceptions
                .push("preview_bytes_or_inputs_unchecked".into());
        }
        ensure!(!cancel(), "audio audit cancelled");
        if report.constraints_match == Some(false) {
            report.state = "partial".into();
            report
                .exceptions
                .push("human_constraints_changed_since_render".into());
        }
        if request.samples.is_empty() && report.state != "partial" {
            report.state = if report.exceptions.is_empty() {
                "presenceOnly"
            } else {
                "presenceOnlyWithExceptions"
            }
            .into();
        }
        let mut intervals = report
            .samples
            .iter()
            .filter(|s| s.state != "unchecked")
            .map(|s| (s.request.movie_in_seconds, s.request.movie_out_seconds))
            .collect::<Vec<_>>();
        intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut cursor = 0.0_f64;
        for (start, end) in intervals {
            if start > cursor {
                report
                    .unchecked_movie_ranges
                    .push(AudioIntegritySampleRequest {
                        movie_in_seconds: cursor,
                        movie_out_seconds: start,
                    });
            }
            if end > cursor {
                report.sampled_energy_seconds += end - start.max(cursor);
                cursor = end;
            }
        }
        if cursor < duration {
            report
                .unchecked_movie_ranges
                .push(AudioIntegritySampleRequest {
                    movie_in_seconds: cursor,
                    movie_out_seconds: duration,
                });
        }
        emit(
            progress,
            "verifying",
            report.pairs.len(),
            report.samples.len(),
        );
        prove_inputs(
            &paths,
            &report.pairs,
            output_path.as_deref(),
            report.output_fingerprint.as_deref(),
            &tools,
            cancel,
        )?;
        self.require_audio_audit_metadata(&plan, &paths, &report.pairs, &constraints)?;
        ensure!(
            derivative_contracts(self, &plan.project_id, artifact.as_ref(), cancel)?
                == report.derivatives,
            SourceInspectionError::StaleEvidence
        );
        let body = serde_json::to_string(&report)?;
        ensure!(
            body.len() <= 2 * 1024 * 1024,
            "audio audit report exceeds bound"
        );
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        ensure!(
            super::ranges::constraints_key(&tx, &plan)? == constraints,
            SourceInspectionError::StaleEvidence
        );
        tx.execute("INSERT INTO audio_integrity_audits(evidence_id,project_id,revision_id,identity_json,report_json,report_fingerprint,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(evidence_id) DO UPDATE SET identity_json=excluded.identity_json,report_json=excluded.report_json,report_fingerprint=excluded.report_fingerprint,created_at=excluded.created_at",params![report.evidence_id,plan.project_id,plan.revision_id,identity_json,body,blake3::hash(body.as_bytes()).to_hex().as_str(),report.created_at])?;
        tx.commit()?;
        emit(
            progress,
            "complete",
            report.pairs.len(),
            report.samples.len(),
        );
        Ok(AudioIntegrityResult {
            report,
            reused: false,
        })
    }

    fn require_audio_audit_metadata(
        &self,
        plan: &EditPlanRevision,
        paths: &BTreeMap<String, (PathBuf, PathBuf)>,
        pairs: &[SelectedAudioPair],
        constraints: &str,
    ) -> Result<()> {
        ensure!(
            super::ranges::constraints_key(&self.connection()?, plan)? == constraints,
            SourceInspectionError::StaleEvidence
        );
        for pair in pairs {
            let asset = self.get_asset(&pair.asset_id)?;
            let (master, selected) = &paths[&pair.asset_id];
            ensure!(
                asset.fingerprint == pair.source_fingerprint
                    && asset.rotation_degrees == pair.rotation_degrees
                    && Path::new(&asset.path) == master
                    && Path::new(super::audio::review_source(&asset)) == selected,
                SourceInspectionError::StaleEvidence
            );
        }
        Ok(())
    }

    /// Historical local evidence only; no fresh source/output verification.
    pub fn audio_integrity_evidence(&self, id: &str) -> Result<AudioIntegrityReport> {
        let (body,hash):(String,String)=self.connection()?.query_row("SELECT substr(report_json,1,2097153),substr(report_fingerprint,1,65) FROM audio_integrity_audits WHERE evidence_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?.context("audio audit evidence not found")?;
        ensure!(
            body.len() <= 2 * 1024 * 1024
                && blake3::hash(body.as_bytes()).to_hex().as_str() == hash,
            "audio audit receipt is corrupt"
        );
        Ok(serde_json::from_str(&body)?)
    }
}

fn declared_sources(plan: &EditPlanRevision, start: u64, end: u64) -> Vec<&str> {
    let mut at = 0.0;
    let mut sources = Vec::new();
    for cut in &plan.operations {
        let duration = cut.source_out_seconds - cut.source_in_seconds;
        let interval = match &cut.audio {
            Some(EditPlanAudio::Silent) => None,
            Some(EditPlanAudio::Source(a)) => Some((
                a.asset_id.as_str(),
                a.timeline_start_seconds,
                a.source_out_seconds - a.source_in_seconds,
            )),
            None => Some((cut.asset_id.as_str(), at, duration)),
        };
        if let Some((id, placement, len)) = interval
            && (placement * 48000.0).round() < (end as f64)
            && ((placement + len) * 48000.0).round() > start as f64
        {
            sources.push(id);
        }
        at += duration;
    }
    sources
}
