//! Bounded host-controlled source inspection, shared by CLI and reviewer tools.
//! SQLite owns completed receipts; files are derived, hashed evidence only.
use super::{inspection_media as media, inspection_model::*};
use crate::{Asset, Catalog, STT_RECIPE_KEY, TranscriptStatus};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

pub const SOURCE_INSPECTION_RECIPE: &str = "source-inspection.actual-pts.oriented-jpeg.pcm48.v1";

#[derive(Clone)]
pub(crate) struct SelectedSource {
    pub(crate) path: PathBuf,
    pub(crate) fingerprint: String,
    pub(crate) provenance: Value,
    pub(crate) source_offset: f64,
    pub(crate) representation_offset: f64,
    pub(crate) scale: u32,
    pub(crate) precision: &'static str,
    pub(crate) reduction: Option<u32>,
}

impl Catalog {
    /// Run the requested representation and, only when explicitly authorized,
    /// one bounded fallback attempt for unavailable media or a deadline.
    /// Invalid, insufficient, stale, and cancelled requests are never retried.
    pub fn inspect_source_window_with_policy(
        &self,
        policy: &InspectSourceWithPolicyRequest,
        cancel: &dyn Fn() -> bool,
    ) -> Result<InspectSourceWithPolicyResult> {
        let requested = policy.request.representation;
        match self.inspect_source_window(&policy.request, cancel) {
            Ok(inspection) => Ok(InspectSourceWithPolicyResult {
                inspection: Some(inspection),
                attempts: vec![InspectionAttempt {
                    representation: requested,
                    outcome: "completed".into(),
                    error_code: None,
                }],
            }),
            Err(error) => {
                let Some((outcome, error_code)) = fallback_classification(&error) else {
                    return Err(error);
                };
                let mut attempts = vec![InspectionAttempt {
                    representation: requested,
                    outcome: outcome.into(),
                    error_code: Some(error_code.into()),
                }];
                let Some(fallback) = policy.fallback_representation else {
                    return Ok(InspectSourceWithPolicyResult {
                        inspection: None,
                        attempts,
                    });
                };
                ensure!(
                    fallback != requested,
                    "invalid input: fallback representation must differ from requested representation"
                );
                ensure!(
                    requested == InspectionRepresentation::Original
                        || fallback != InspectionRepresentation::Original,
                    "invalid input: proxy inspections cannot fall back to original media"
                );
                ensure!(
                    policy.request.exact_representation_pts.is_empty()
                        && policy.request.expected_representation_fingerprint.is_none(),
                    "invalid input: exact PTS or fingerprint-bound requests cannot use representation fallback"
                );
                if cancel() {
                    return Err(anyhow::anyhow!(
                        "source inspection cancelled before fallback"
                    ));
                }
                let mut retry = policy.request.clone();
                retry.representation = fallback;
                match self.inspect_source_window(&retry, cancel) {
                    Ok(inspection) => {
                        attempts.push(InspectionAttempt {
                            representation: fallback,
                            outcome: "completed".into(),
                            error_code: None,
                        });
                        Ok(InspectSourceWithPolicyResult {
                            inspection: Some(inspection),
                            attempts,
                        })
                    }
                    Err(error) if cancel() || error.to_string().contains("cancelled") => Err(error),
                    Err(error) => {
                        let (outcome, code) = fallback_classification(&error)
                            .unwrap_or(("failed", "inspection_failed"));
                        attempts.push(InspectionAttempt {
                            representation: fallback,
                            outcome: outcome.into(),
                            error_code: Some(code.into()),
                        });
                        Ok(InspectSourceWithPolicyResult {
                            inspection: None,
                            attempts,
                        })
                    }
                }
            }
        }
    }

    /// Explicitly inspect a bounded window. Never selects a different source,
    /// transcribes, calls a provider, accepts a trim, or changes orientation.
    pub fn inspect_source_window(
        &self,
        request: &SourceInspectionRequest,
        cancel: &dyn Fn() -> bool,
    ) -> Result<SourceInspectionResult> {
        validate_request(request)?;
        ensure!(!cancel(), "source inspection cancelled");
        let _heavy = self.acquire_local_heavy_cancellable(cancel)?;
        let asset =
            self.inspection_get_asset(&request.asset_id, request.io_timeout_seconds, cancel)?;
        let source = self.inspection_source(
            &asset,
            request.representation,
            request.io_timeout_seconds,
            cancel,
        )?;
        if let Some(expected) = &request.expected_representation_fingerprint
            && expected != &source.fingerprint
        {
            return Err(SourceInspectionError::StaleEvidence.into());
        }
        let start = request.source_in_seconds - request.before_seconds;
        let end = request.source_out_seconds + request.after_seconds;
        ensure!(
            start >= 0.0 && end <= asset.duration_seconds + 1e-7,
            "invalid input: requested handles exceed source duration"
        );
        let stream = media::probe(
            &source.path,
            request.include_audio,
            request.probe_timeout_seconds,
            cancel,
        )?;
        let mapping = InspectionTimeMapping {
            time_base_numerator: stream.numerator,
            time_base_denominator: stream.denominator,
            representation_origin_pts: stream.origin,
            source_offset_seconds: source.source_offset,
            representation_offset_seconds: source.representation_offset,
            seconds_scale: source.scale,
            precision: source.precision.into(),
            source_frame_reduction: source.reduction,
        };
        let representation_start =
            source.representation_offset + (start - source.source_offset) / f64::from(source.scale);
        let representation_end =
            source.representation_offset + (end - source.source_offset) / f64::from(source.scale);
        ensure!(
            representation_start >= 0.0 && representation_end <= stream.duration + 1e-7,
            "invalid input: requested handles exceed selected representation"
        );
        let transcript = self.inspection_transcript(&asset, request)?;
        let tools = media::tool_identity();
        let identity = json!({
            "recipe": SOURCE_INSPECTION_RECIPE, "request": request,
            "sourceFingerprint": asset.fingerprint, "representationFingerprint": source.fingerprint,
            "provenance": source.provenance, "mapping": mapping,
            "rotation": asset.rotation_degrees, "orientationDecision": asset.orientation_decision,
            "transcript": transcript, "tools": tools,
        });
        let identity_json = serde_json::to_string(&identity)?;
        let id = format!(
            "inspect_{}",
            blake3::hash(identity_json.as_bytes()).to_hex()
        );
        let root = self
            .database_path()
            .parent()
            .context("catalog has no parent")?
            .join("cache")
            .join("source-inspections")
            .join(&id);
        fs::create_dir_all(&root)?;
        // Persistent lock inode: corrupt evidence rebuilds cannot race another
        // reader/writer into publishing a mixture of attempts.
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join(".lock"))?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                anyhow::Error::from(SourceInspectionError::Busy)
            } else {
                anyhow::Error::from(error)
            }
        })?;
        let cached: Option<(String, String)> = self
            .connection()?
            .query_row(
                "SELECT identity_json,report_json FROM source_inspections WHERE evidence_id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((stored_identity, body)) = cached
            && stored_identity == identity_json
            && let Ok(report) = serde_json::from_str::<SourceInspection>(&body)
            && report.evidence_id == id
            && report.request == *request
            && media::files_current(&report, &root, cancel)?
        {
            self.require_inspection_source_current(
                &asset,
                &source,
                request.representation,
                request.io_timeout_seconds,
                cancel,
            )?;
            return Ok(SourceInspectionResult {
                inspection: report,
                reused: true,
            });
        }
        let workspace = root.join(uuid::Uuid::new_v4().simple().to_string());
        fs::create_dir(&workspace)?;
        write_attempt_checkpoint(&workspace, &id, "probing", 0, 0)?;
        let available = media::frame_pts(
            &source.path,
            &mapping,
            representation_start,
            representation_end,
            request.probe_timeout_seconds,
            cancel,
        )?;
        write_attempt_checkpoint(&workspace, &id, "framesLocated", 0, 0)?;
        let selected = select_pts(request, &mapping, start, end, &available)?;
        let images = media::encode_frames(
            &source.path,
            &workspace,
            asset.rotation_degrees,
            request.max_width,
            representation_start,
            &selected,
            request.decode_timeout_seconds,
            cancel,
        )?;
        write_attempt_checkpoint(&workspace, &id, "framesDecoded", selected.len(), 0)?;
        let frames: Vec<_> = selected
            .iter()
            .zip(images)
            .map(|(&pts, image)| InspectedFrame {
                evidence_id: format!("{id}:frame:{pts}"),
                representation_pts: pts,
                source_seconds: mapping.source_seconds(pts),
                excerpt_seconds: mapping.source_seconds(pts) - start,
                image,
            })
            .collect();
        let audio = if request.include_audio {
            let audio = media::audio_context(
                &source.path,
                &workspace,
                stream.audio_start_seconds,
                representation_start,
                representation_end,
                request.decode_timeout_seconds,
                cancel,
            )?;
            write_attempt_checkpoint(
                &workspace,
                &id,
                "audioDecoded",
                selected.len(),
                audio.waveform.len(),
            )?;
            Some(audio)
        } else {
            None
        };
        let mut limitations = vec![
            "Samples describe only decoded selected frames; intervening frames are uninspected. No defect or clean-range certification is implied.".into(),
            "Requested times and decoded PTS are distinct. Exact representation frames do not restore frames discarded by a proxy.".into(),
            "Inspection purpose records caller intent, not human acceptance. Natural speech and pacing require listening/playback review.".into(),
        ];
        if source.precision != "actualSourcePts" {
            limitations.push("Mapped source seconds are not a measured original-frame PTS map; inspect the original for exact source-frame evidence.".into());
        }
        let utterances = transcript.map_or_else(Vec::new, |t| {
            t.segments
                .into_iter()
                .filter_map(|s| {
                    let a = f64::from(s.start_ms) / 1000.0;
                    let b = f64::from(s.end_ms) / 1000.0;
                    (a.is_finite() && b > a && a < end && b > start).then(|| InspectedUtterance {
                        source_in_seconds: a,
                        source_out_seconds: b,
                        excerpt_in_seconds: a.max(start) - start,
                        excerpt_out_seconds: b.min(end) - start,
                        text: s.text,
                        clipped_by_window: a < start || b > end,
                    })
                })
                .collect()
        });
        if request.include_transcript {
            limitations.push("Only current accepted segment timings from the selected generated proxy are reusable; word timing is unavailable. No transcription was run.".into());
        }
        if request.include_audio {
            limitations.push("Waveform low-energy cues are not silence, speech, or sound-quality judgments. Audio sample bounds round inward on the selected 48 kHz decode clock.".into());
        }
        let mut previous = start;
        let mut max_gap: f64 = 0.0;
        for frame in &frames {
            max_gap = max_gap.max(frame.source_seconds - previous);
            previous = frame.source_seconds;
        }
        max_gap = max_gap.max(end - previous);
        let intervals: Vec<_> = available
            .windows(2)
            .map(|p| mapping.source_seconds(p[1]) - mapping.source_seconds(p[0]))
            .collect();
        if frames.len() < request.sample_count as usize {
            limitations.push("Requested sample density exceeds distinct decoded frames in this window; no frames were duplicated or invented.".into());
        }
        let report = SourceInspection {
            evidence_id: id.clone(),
            recipe: SOURCE_INSPECTION_RECIPE.into(),
            request: request.clone(),
            actual_representation: Some(request.representation),
            source_fingerprint: asset.fingerprint.clone(),
            representation_fingerprint: source.fingerprint.clone(),
            rotation_degrees: asset.rotation_degrees,
            mapping,
            window_in_seconds: start,
            window_out_seconds: end,
            frames,
            audio,
            utterances,
            available_frame_count: available.len() as u32,
            min_available_frame_interval_seconds: intervals.iter().copied().reduce(f64::min),
            max_available_frame_interval_seconds: intervals.iter().copied().reduce(f64::max),
            max_unsampled_gap_seconds: max_gap,
            limitations,
            created_at: crate::now(),
        };
        self.require_inspection_source_current(
            &asset,
            &source,
            request.representation,
            request.io_timeout_seconds,
            cancel,
        )?;
        ensure!(!cancel(), "source inspection cancelled before publication");
        write_attempt_checkpoint(
            &workspace,
            &id,
            "artifactsReady",
            report.frames.len(),
            report.audio.as_ref().map_or(0, |a| a.waveform.len()),
        )?;
        self.connection()?.execute(
            "INSERT INTO source_inspections(evidence_id,asset_id,identity_json,report_json,created_at) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(evidence_id) DO UPDATE SET identity_json=excluded.identity_json,report_json=excluded.report_json,created_at=excluded.created_at",
            params![id, asset.id, identity_json, serde_json::to_string(&report)?, report.created_at],
        )?;
        let _ = write_attempt_checkpoint(
            &workspace,
            &id,
            "completed",
            report.frames.len(),
            report.audio.as_ref().map_or(0, |a| a.waveform.len()),
        );
        Ok(SourceInspectionResult {
            inspection: report,
            reused: false,
        })
    }

    pub(crate) fn inspection_transcript(
        &self,
        asset: &Asset,
        request: &SourceInspectionRequest,
    ) -> Result<Option<crate::ClipTranscript>> {
        if !request.include_audio
            || !request.include_transcript
            || request.representation != InspectionRepresentation::GeneratedProxy
        {
            return Ok(None);
        }
        let current = self.proxy_fingerprint(&asset.id)?;
        Ok(self.asset_transcript(&asset.id)?.filter(|t| {
            t.recipe_key == STT_RECIPE_KEY
                && t.source_fingerprint == current
                && t.status == TranscriptStatus::Speech
        }))
    }

    /// Strict verification for a new human decision. Never rebuild or replace
    /// what the human saw; stale/missing/corrupt evidence needs explicit review.
    pub(crate) fn require_current_source_inspection(
        &self,
        evidence_id: &str,
        asset_id: &str,
    ) -> Result<SourceInspection> {
        self.require_current_source_inspection_cancellable(evidence_id, asset_id, &|| false)
    }

    pub(crate) fn require_current_source_inspection_cancellable(
        &self,
        evidence_id: &str,
        asset_id: &str,
        cancel: &dyn Fn() -> bool,
    ) -> Result<SourceInspection> {
        ensure!(
            evidence_id
                .strip_prefix("inspect_")
                .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())),
            "invalid input: malformed inspection evidence ID"
        );
        let _heavy = self.acquire_local_heavy_cancellable(cancel)?;
        let row: Option<(String,String)> = self.connection()?.query_row(
            "SELECT identity_json,report_json FROM source_inspections WHERE evidence_id=?1 AND asset_id=?2",
            params![evidence_id,asset_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let (identity_json, body) = row.context("source inspection evidence not found")?;
        let identity: Value = serde_json::from_str(&identity_json)?;
        let report: SourceInspection = serde_json::from_str(&body)?;
        let asset =
            self.inspection_get_asset(asset_id, report.request.io_timeout_seconds, cancel)?;
        let source = self.inspection_source(
            &asset,
            report.request.representation,
            report.request.io_timeout_seconds,
            cancel,
        )?;
        let stream = media::probe(
            &source.path,
            report.request.include_audio,
            report.request.probe_timeout_seconds,
            cancel,
        )?;
        let mapping = InspectionTimeMapping {
            time_base_numerator: stream.numerator,
            time_base_denominator: stream.denominator,
            representation_origin_pts: stream.origin,
            source_offset_seconds: source.source_offset,
            representation_offset_seconds: source.representation_offset,
            seconds_scale: source.scale,
            precision: source.precision.into(),
            source_frame_reduction: source.reduction,
        };
        let expected = json!({"recipe":SOURCE_INSPECTION_RECIPE,"request":report.request,"sourceFingerprint":asset.fingerprint,
            "representationFingerprint":source.fingerprint,"provenance":source.provenance,"mapping":mapping,"rotation":asset.rotation_degrees,
            "orientationDecision":asset.orientation_decision,"transcript":self.inspection_transcript(&asset,&report.request)?,"tools":media::tool_identity()});
        let root = self
            .database_path()
            .parent()
            .context("catalog has no parent")?
            .join("cache")
            .join("source-inspections")
            .join(evidence_id);
        if identity != expected
            || format!(
                "inspect_{}",
                blake3::hash(identity_json.as_bytes()).to_hex()
            ) != evidence_id
            || report.evidence_id != evidence_id
            || report.request.asset_id != asset_id
            || report.source_fingerprint != asset.fingerprint
            || report.representation_fingerprint != source.fingerprint
            || report.mapping != mapping
            || report.rotation_degrees != asset.rotation_degrees
            || report.recipe != SOURCE_INSPECTION_RECIPE
            || report.window_in_seconds
                != report.request.source_in_seconds - report.request.before_seconds
            || report.window_out_seconds
                != report.request.source_out_seconds + report.request.after_seconds
            || !root.is_dir()
            || !media::files_current(&report, &root, cancel)?
        {
            return Err(SourceInspectionError::StaleEvidence.into());
        }
        Ok(report)
    }

    pub(crate) fn inspection_source(
        &self,
        asset: &Asset,
        choice: InspectionRepresentation,
        io_timeout_seconds: u32,
        cancel: &dyn Fn() -> bool,
    ) -> Result<SelectedSource> {
        let database_path = self.database_path().to_owned();
        let asset = asset.clone();
        crate::stall::abandonable_io_with_timeout(
            "inspection representation lookup",
            std::time::Duration::from_secs(io_timeout_seconds.into()),
            cancel,
            move |stop| {
                let readonly = Catalog::open_read_only_existing(database_path)?;
                let cancelled = || stop.load(std::sync::atomic::Ordering::Relaxed);
                readonly.inspection_source_inner(&asset, choice, io_timeout_seconds, &cancelled)
            },
        )
    }

    fn inspection_source_inner(
        &self,
        asset: &Asset,
        choice: InspectionRepresentation,
        io_timeout_seconds: u32,
        cancel: &dyn Fn() -> bool,
    ) -> Result<SelectedSource> {
        let mut source = match choice {
            InspectionRepresentation::Original => {
                ensure!(
                    asset.original_available,
                    "media offline: explicitly selected original unavailable"
                );
                SelectedSource {
                    path: PathBuf::from(&asset.path),
                    fingerprint: String::new(),
                    provenance: json!({"original":asset.fingerprint}),
                    source_offset: 0.0,
                    representation_offset: 0.0,
                    scale: 1,
                    precision: "actualSourcePts",
                    reduction: Some(1),
                }
            }
            InspectionRepresentation::GeneratedProxy => {
                let proxy = self.validated_review_proxy_source(asset, cancel)?;
                SelectedSource {
                    path: proxy.proxy,
                    fingerprint: proxy.fingerprint,
                    provenance: serde_json::to_value(
                        self.registered_proxy_generation_provenance(&asset.id)?,
                    )?,
                    source_offset: 0.0,
                    representation_offset: 0.0,
                    scale: 1,
                    precision: "secondsCorrespondence",
                    reduction: None,
                }
            }
            InspectionRepresentation::CameraProxy => {
                let sidecar = self.resolve_camera_proxy_for_review(&asset.id)?.context(
                    "media offline: explicitly selected verified camera proxy unavailable",
                )?;
                let provenance = sidecar
                    .provenance
                    .as_ref()
                    .context("camera proxy needs verified source time mapping")?;
                ensure!(
                    provenance.validator == "camera-sidecar.cfr-zero-origin.rgb-samples-v1"
                        && provenance.time_mapping.seconds_scale == 1,
                    "camera proxy needs current verified source time mapping"
                );
                let t = &provenance.time_mapping;
                ensure!(
                    t.source_offset_seconds == 0.0 && t.sidecar_offset_seconds == 0.0,
                    "unsupported camera clock: verified zero-origin mapping required"
                );
                SelectedSource {
                    path: PathBuf::from(&sidecar.path),
                    fingerprint: String::new(),
                    provenance: serde_json::to_value(provenance)?,
                    source_offset: t.source_offset_seconds,
                    representation_offset: t.sidecar_offset_seconds,
                    scale: t.seconds_scale,
                    precision: "verifiedCameraNominal",
                    reduction: Some(t.frame_reduction),
                }
            }
        };
        if source.fingerprint.is_empty() {
            source.fingerprint = media::fingerprint(&source.path, io_timeout_seconds, cancel)?;
        }
        Ok(source)
    }

    pub(crate) fn require_inspection_source_current(
        &self,
        previous: &Asset,
        source: &SelectedSource,
        choice: InspectionRepresentation,
        io_timeout_seconds: u32,
        cancel: &dyn Fn() -> bool,
    ) -> Result<()> {
        let current = self.inspection_get_asset(&previous.id, io_timeout_seconds, cancel)?;
        if !(current.fingerprint == previous.fingerprint
            && current.rotation_degrees == previous.rotation_degrees
            && current.orientation_decision == previous.orientation_decision)
        {
            return Err(SourceInspectionError::StaleEvidence.into());
        }
        let selected = self.inspection_source(&current, choice, io_timeout_seconds, cancel)?;
        if !(selected.path == source.path
            && selected.fingerprint == source.fingerprint
            && selected.provenance == source.provenance)
        {
            return Err(SourceInspectionError::StaleEvidence.into());
        }
        Ok(())
    }

    fn inspection_get_asset(
        &self,
        asset_id: &str,
        timeout_seconds: u32,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Asset> {
        let database_path = self.database_path().to_owned();
        let asset_id = asset_id.to_owned();
        crate::stall::abandonable_io_with_timeout(
            "inspection source availability lookup",
            std::time::Duration::from_secs(timeout_seconds.into()),
            cancel,
            move |stop| {
                let readonly = Catalog::open_read_only_existing(database_path)?;
                let asset = readonly.get_asset(&asset_id)?;
                anyhow::ensure!(
                    !stop.load(std::sync::atomic::Ordering::Relaxed),
                    "inspection source lookup cancelled"
                );
                Ok(asset)
            },
        )
    }
}

/// Keep a small durable progress receipt beside attempt artifacts. If the
/// process exits or a later stage fails, successful earlier work is still
/// discoverable and cannot be mistaken for a completed inspection.
fn write_attempt_checkpoint(
    workspace: &std::path::Path,
    evidence_id: &str,
    stage: &str,
    decoded_frames: usize,
    waveform_bins: usize,
) -> Result<()> {
    let checkpoint = json!({
        "evidenceId": evidence_id,
        "state": if stage == "completed" { "completed" } else { "partial" },
        "stage": stage,
        "decodedFrames": decoded_frames,
        "waveformBins": waveform_bins,
        "updatedAt": crate::now(),
    });
    let temporary = workspace.join("attempt.json.tmp");
    fs::write(&temporary, serde_json::to_vec(&checkpoint)?)?;
    fs::rename(temporary, workspace.join("attempt.json"))?;
    Ok(())
}

fn validate_request(r: &SourceInspectionRequest) -> Result<()> {
    ensure!(
        (1..=600).contains(&r.io_timeout_seconds)
            && (1..=600).contains(&r.probe_timeout_seconds)
            && (1..=600).contains(&r.decode_timeout_seconds),
        "invalid input: I/O, probe, and decode timeouts must be 1-600 seconds"
    );
    ensure!(
        [
            r.source_in_seconds,
            r.source_out_seconds,
            r.before_seconds,
            r.after_seconds
        ]
        .iter()
        .all(|v| v.is_finite())
            && r.source_in_seconds >= 0.0
            && r.source_out_seconds > r.source_in_seconds
            && r.before_seconds >= 0.0
            && r.after_seconds >= 0.0,
        "invalid input: inspection range and handles must be finite and positive"
    );
    ensure!(
        r.source_out_seconds - r.source_in_seconds + r.before_seconds + r.after_seconds <= 60.0,
        "invalid input: maximum inspection window is 60 seconds including handles"
    );
    ensure!(
        r.sample_count as usize + r.source_times_seconds.len() + r.exact_representation_pts.len()
            <= 64
            && (r.sample_count > 0
                || !r.source_times_seconds.is_empty()
                || !r.exact_representation_pts.is_empty())
            && (160..=1280).contains(&r.max_width),
        "invalid input: 1-64 frames and width 160-1280 required"
    );
    ensure!(
        r.source_times_seconds.iter().all(|v| v.is_finite()),
        "invalid input: nonfinite requested frame time"
    );
    ensure!(
        r.exact_representation_pts.is_empty() || r.expected_representation_fingerprint.is_some(),
        "invalid input: exact PTS requests require expected representation fingerprint"
    );
    ensure!(
        !r.include_transcript || r.include_audio,
        "invalid input: transcript requires explicit audio inclusion"
    );
    Ok(())
}

fn fallback_classification(error: &anyhow::Error) -> Option<(&'static str, &'static str)> {
    let message = error.to_string().to_ascii_lowercase();
    if message.contains("cancelled") || message.contains("canceled") {
        return None;
    }
    if message.contains("stopped answering after") || message.contains("timed out") {
        return Some(("timeout", "inspection_timeout"));
    }
    if message.contains("media offline") || message.contains("unavailable") {
        return Some(("unavailable", "representation_unavailable"));
    }
    None
}

fn select_pts(
    r: &SourceInspectionRequest,
    mapping: &InspectionTimeMapping,
    start: f64,
    end: f64,
    available: &[i64],
) -> Result<Vec<i64>> {
    ensure!(
        !available.is_empty(),
        "no decoded frames in requested window"
    );
    let mut pts = std::collections::BTreeSet::new();
    for index in 0..r.sample_count as usize {
        let at = if r.sample_count == 1 {
            available.len() / 2
        } else {
            index * (available.len() - 1) / (r.sample_count as usize - 1)
        };
        pts.insert(available[at]);
    }
    for &requested in &r.source_times_seconds {
        ensure!(
            requested >= start && requested < end,
            "invalid input: requested frame time outside inspection window"
        );
        let closest = available
            .iter()
            .min_by(|&&a, &&b| {
                (mapping.source_seconds(a) - requested)
                    .abs()
                    .total_cmp(&(mapping.source_seconds(b) - requested).abs())
            })
            .unwrap();
        pts.insert(*closest);
    }
    for &exact in &r.exact_representation_pts {
        ensure!(
            available.binary_search(&exact).is_ok(),
            "invalid input: requested exact PTS unavailable in selected window/representation"
        );
        pts.insert(exact);
    }
    Ok(pts.into_iter().collect())
}

#[cfg(test)]
#[path = "inspection_tests.rs"]
pub(crate) mod tests;
