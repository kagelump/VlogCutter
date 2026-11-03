use anyhow::Result;
use rcdvlog_server_core::{
    AuditActorType, BaselineEnvelope, Catalog, ChunkUpload, ControlCatalog,
    FinalizeReviewSessionRequest, ProjectAssetAddedPayload, RecordingEndReason,
    ReviewSpeechRuntime, ReviewTargetActor, ReviewTargetHandoff, ReviewTargetImportRequest,
    ReviewTranscriptionRequest, ServerConfig, review_speech,
};

fn main() -> Result<()> {
    let config = ServerConfig::from_environment()?;
    let control = ControlCatalog::open_or_create(&config.data_dir, &config.diagnostics())?;
    let mac_catalog_path = config.data_dir.join("web-e2e-mac.sqlite3");
    let mac = Catalog::open(&mac_catalog_path)?;
    mac.set_local_node_id("node_web_e2e_mac")?;
    let project = mac.create_project("Chrome concurrency fixture")?;
    mac.add_portable_project_asset(
        &project.id,
        ProjectAssetAddedPayload {
            id: "asset_web_e2e".into(),
            fingerprint: "fingerprint-web-e2e".into(),
            filename: "chrome-concurrency.mov".into(),
            size_bytes: 10,
            duration_seconds: 1.0,
            frame_rate: 30.0,
            width: 1920,
            height: 1080,
            codec: "h264".into(),
            camera: "Fixture camera".into(),
            captured_at_raw: None,
            timestamp_candidates: None,
            best_time: None,
            metadata_rotation_degrees: 0,
            default_rotation_degrees: 0,
            created_at: "2026-07-28T00:00:00Z".into(),
        },
    )?;
    let baseline = BaselineEnvelope::export_catalog(&mac, &project.id)?;
    control.project_replicas().import_baseline(&baseline)?;
    // A synthetic, path-free TV review target for the seeded project (#221),
    // digested by the shared Rust digest and freshly "verified" so the Chrome
    // suite can import it through the real server. No movie exists.
    if let Some(output) = std::env::var_os("RCDVLOG_E2E_REVIEW_TARGET_FILE") {
        let mut handoff: ReviewTargetHandoff = serde_json::from_str(include_str!(
            "../../../core/rcdvlog-core/tests/fixtures/review-target/v1.json"
        ))?;
        handoff.target.project_id = project.id.clone();
        handoff.target.youtube_video_id = None;
        handoff.target_digest = ReviewTargetHandoff::snapshot_digest(&handoff.target)?;
        handoff.verification.verified_at =
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        handoff.validate()?;
        std::fs::write(output, serde_json::to_vec_pretty(&handoff)?)?;
    }
    // A synthetic #232 historical rendered-artifact review of a different,
    // earlier render: same shape, weaker receipt, imported only through its
    // own acknowledged route.
    if let Some(output) = std::env::var_os("RCDVLOG_E2E_HISTORICAL_REVIEW_FILE") {
        let mut target = serde_json::from_str::<ReviewTargetHandoff>(include_str!(
            "../../../core/rcdvlog-core/tests/fixtures/review-target/v1.json"
        ))?
        .target;
        target.project_id = project.id.clone();
        target.youtube_video_id = None;
        target.revision_id = "editrev_historical".into();
        target.viewed_artifact_id = "preview_historical".into();
        let review = rcdvlog_server_core::HistoricalRenderedArtifactReview {
            schema_version: 1,
            purpose: rcdvlog_server_core::HistoricalReviewPurpose::HistoricalRenderedArtifactReview,
            target_digest: ReviewTargetHandoff::snapshot_digest(&target)?,
            target,
            verified_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            output_matches: true,
            mapping_matches_revision: true,
            stored_fully_decoded: true,
            current_source_verification:
                rcdvlog_server_core::HistoricalReviewUnresolved::NotEstablishedByThisReview,
            current_constraints_verification:
                rcdvlog_server_core::HistoricalReviewUnresolved::NotEstablishedByThisReview,
            current_plan_renderability:
                rcdvlog_server_core::HistoricalReviewUnresolved::NotEstablishedByThisReview,
        };
        review.validate()?;
        std::fs::write(output, serde_json::to_vec_pretty(&review)?)?;
    }
    if let Some(output) = std::env::var_os("RCDVLOG_E2E_REVIEW_RUN_FILE") {
        let path = seed_transcribed_review(&control, &project.id)?;
        std::fs::write(output, path)?;
    }
    println!("{}", project.id);
    Ok(())
}

/// Scripted speech for the e2e fixture: an energy VAD and a fixed
/// two-reviewer English/Mandarin exchange. No model, no network.
struct ScriptedSpeech;

struct ScriptedEngine(std::sync::Mutex<Vec<(&'static str, &'static str)>>);

fn energy_regions(pcm: &[f32]) -> Vec<review_speech::SpeechRegion> {
    let mut regions: Vec<review_speech::SpeechRegion> = Vec::new();
    for (index, frame) in pcm.chunks(160).enumerate() {
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt();
        if rms > 0.05 {
            let start = index as u32 * 10;
            match regions.last_mut() {
                Some(last) if last.end_ms + 200 >= start => last.end_ms = start + 10,
                _ => regions.push(review_speech::SpeechRegion {
                    start_ms: start,
                    end_ms: start + 10,
                }),
            }
        }
    }
    regions
}

impl review_speech::SttEngine for ScriptedEngine {
    fn detect_speech(&self, pcm: &[f32]) -> Result<Vec<review_speech::SpeechRegion>> {
        Ok(energy_regions(pcm))
    }
    fn decode_window(
        &self,
        _pcm: &[f32],
        window: &review_speech::PackedWindow,
    ) -> Result<Vec<review_speech::DecodedSegment>> {
        let mut lines = self.0.lock().unwrap();
        let mut offset = 0;
        Ok(window
            .regions
            .iter()
            .map(|region| {
                let (language, text) = if lines.is_empty() {
                    ("en", "okay")
                } else {
                    lines.remove(0)
                };
                let segment = review_speech::DecodedSegment {
                    start_ms: offset,
                    end_ms: offset + region.duration_ms,
                    language: language.into(),
                    text: text.into(),
                    mean_token_probability: Some(0.8),
                };
                offset += region.duration_ms;
                segment
            })
            .collect())
    }
}

impl ReviewSpeechRuntime for ScriptedSpeech {
    fn local_models_present(&self) -> bool {
        true
    }
    fn vad_model_present(&self) -> bool {
        true
    }
    fn local_engine(&self) -> Result<Box<dyn review_speech::SttEngine>> {
        Ok(Box::new(ScriptedEngine(std::sync::Mutex::new(vec![
            ("en", "Keep this shot longer."),
            ("zh", "不要，长度不变，换个转场。"),
            ("en", "OK, keep the duration and change the transition."),
        ]))))
    }
    fn detect_speech(&self, pcm: &[f32]) -> Result<Vec<review_speech::SpeechRegion>> {
        Ok(energy_regions(pcm))
    }
    fn cloud_credentials_present(&self) -> bool {
        false
    }
    fn cloud_transport(&self) -> Result<Box<dyn review_speech::NativeSttTransport>> {
        anyhow::bail!("no cloud in the fixture")
    }
}

/// A finalized, transcribed voice review for the Chrome suite. Returns the
/// web path of its editorial review workspace.
fn seed_transcribed_review(control: &ControlCatalog, project_id: &str) -> Result<String> {
    use sha2::Digest;
    let actor = ReviewTargetActor::new(AuditActorType::BrowserSession, "seed");
    let mut handoff: ReviewTargetHandoff = serde_json::from_str(include_str!(
        "../../../core/rcdvlog-core/tests/fixtures/review-target/v1.json"
    ))?;
    handoff.target.project_id = project_id.into();
    handoff.target.youtube_video_id = Some("seedreview1".into());
    handoff.target_digest = ReviewTargetHandoff::snapshot_digest(&handoff.target)?;
    handoff.verification.verified_at =
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let target = control
        .review_targets()
        .import(
            project_id,
            &ReviewTargetImportRequest {
                handoff: serde_json::to_value(&handoff)?,
                declared_issuer: None,
                availability: None,
                max_verification_age_seconds: None,
            },
            &actor,
            chrono::Utc::now(),
        )
        .map_err(|e| anyhow::anyhow!(e.to_string()))?
        .review_target
        .summary
        .review_target_id;
    let sessions = control.review_sessions(0);
    let session = sessions
        .create(project_id, &serde_json::from_value(serde_json::json!({
            "reviewTargetId": target, "clientSessionKey": "seed_review",
            "consent": {"allPresentAcknowledged": true, "additionalSpeakersDisclosed": true},
            "audio": {"sampleRate": 48000, "channels": 1, "encoding": "pcm_s16le"},
            "capture": {"echoCancellation": false, "noiseSuppression": false, "autoGainControl": false, "clock": "audioWorkletCurrentFrame", "baseLatencySeconds": null, "userAgentFamily": null},
            "clientStartedPerformanceMs": null
        }))?, &actor, chrono::Utc::now())
        .map_err(|e| anyhow::anyhow!(e.to_string()))?
        .summary
        .session_id;
    let rate = 48_000_usize;
    let speech = [(1.0, 2.5), (3.5, 5.0), (5.6, 6.0)];
    let samples: Vec<i16> = (0..rate * 10)
        .map(|i| {
            let t = i as f64 / rate as f64;
            if speech.iter().any(|(a, b)| t >= *a && t < *b) {
                ((2.0 * std::f64::consts::PI * 800.0 * t).sin() * 16_000.0) as i16
            } else {
                0
            }
        })
        .collect();
    for (sequence, part) in samples.chunks(rate * 5).enumerate() {
        let bytes: Vec<u8> = part.iter().flat_map(|s| s.to_le_bytes()).collect();
        let sha = sha2::Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        sessions
            .put_chunk(
                project_id,
                &session,
                &ChunkUpload {
                    sequence: sequence as u32,
                    start_frame: (sequence * rate * 5) as u64,
                    frame_count: part.len() as u32,
                    sha256: sha,
                    capture_end_performance_ms: None,
                },
                &bytes,
                chrono::Utc::now(),
            )
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    }
    // A synthetic review-capture-calibration.v1 clock sample (sample 0 at
    // monotonic 0 ns, ±3 ms) and two cues: movie 0 at 0.5 s and movie 5 at
    // 5.5 s, so the shared synchronizer can estimate between them.
    let events = [
        serde_json::json!({"eventId": "seed_calibration", "kind": "clockSample", "audioFrame": 48000, "performanceMs": 1000.0,
            "detail": {"schema": "review-capture-calibration.v1", "method": "workletProbeBracket", "clockId": "mic_epoch_0", "clockEpoch": 0,
                "sampleRate": 48000, "firstSample": 0, "sampleIndex": 48000, "monotonicNs": 1_000_000_000u64, "monotonicOriginNs": 0,
                "monotonicToleranceNs": 3_000_000, "performanceEpochMs": 0.0, "uncertaintyMs": 3.0}}),
        serde_json::json!({"eventId": "seed_cue", "kind": "manualSyncCue", "audioFrame": 24000, "performanceMs": 500.0, "declaredMovieSeconds": 0.0, "detail": {"clockEpoch": 0}}),
        serde_json::json!({"eventId": "seed_cue_five", "kind": "manualSyncCue", "audioFrame": 264000, "performanceMs": 5500.0, "declaredMovieSeconds": 5.0, "detail": {"clockEpoch": 0}}),
    ];
    for event in events {
        sessions
            .record_event(
                project_id,
                &session,
                &serde_json::from_value(event)?,
                chrono::Utc::now(),
            )
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    }
    sessions
        .finalize(
            project_id,
            &session,
            &FinalizeReviewSessionRequest {
                final_frame_count: (rate * 10) as u64,
                end_reason: RecordingEndReason::Stopped,
            },
            chrono::Utc::now(),
        )
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let transcripts = control.review_transcripts();
    let request: ReviewTranscriptionRequest =
        serde_json::from_value(serde_json::json!({"backend": "local"}))?;
    let run = transcripts
        .start(
            project_id,
            &session,
            &request,
            &ScriptedSpeech,
            &control.jobs(),
            &actor,
        )
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    transcripts
        .execute(&run.run_id, &ScriptedSpeech, &|| false)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    Ok(format!(
        "/tv-review/{project_id}/{target}/sessions/{session}/review/{}",
        run.run_id
    ))
}
