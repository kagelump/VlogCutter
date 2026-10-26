use super::*;
use crate::analysis::engine::SttEngine;
use crate::analysis::provider::{
    AnalysisRequestCostBound, DEFAULT_MODEL, VlmProvider, VlmReceipt, VlmRequest, VlmResponse,
};
use crate::analysis::quality::DecodedSegment;
use crate::analysis::store::{StoredTranscript, TranscriptStatus, save_transcript};
use crate::analysis::windows::{PackedWindow, SpeechRegion};
use crate::{AssetPatch, Catalog, MomentPayload, MomentRunOptions, RepresentationKind};
use anyhow::Result;
use rusqlite::params;
use serde_json::json;
use std::fs;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tempfile::{TempDir, tempdir};

fn fixture() -> (TempDir, Catalog, String) {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog
        .create_project("Preparation synthetic fixture")
        .unwrap();
    (directory, catalog, project.id)
}

fn media_tools_available() -> bool {
    ["ffmpeg", "ffprobe"].into_iter().all(|tool| {
        crate::tool_command(tool)
            .arg("-version")
            .output()
            .is_ok_and(|output| output.status.success())
    })
}

fn make_asset(
    catalog: &Catalog,
    project_id: &str,
    directory: &Path,
    id: &str,
    seconds: u32,
    with_proxy: bool,
) -> PathBuf {
    make_asset_with_audio(
        catalog, project_id, directory, id, seconds, with_proxy, true,
    )
}

fn make_asset_with_audio(
    catalog: &Catalog,
    project_id: &str,
    directory: &Path,
    id: &str,
    seconds: u32,
    with_proxy: bool,
    with_audio: bool,
) -> PathBuf {
    let source = directory.join(format!("{id}.mp4"));
    let color = &blake3::hash(id.as_bytes()).to_hex()[..6];
    let mut command = crate::tool_command("ffmpeg");
    command
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg(format!("color=c=0x{color}:s=320x180:r=24:d={seconds}"));
    if with_audio {
        command
            .args(["-f", "lavfi", "-i"])
            .arg(format!(
                "sine=frequency=440:sample_rate=48000:duration={seconds}"
            ))
            .args(["-map", "0:v:0", "-map", "1:a:0"]);
    } else {
        command.args(["-map", "0:v:0"]);
    }
    let status = command
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
        ])
        .args(if with_audio {
            vec!["-c:a", "aac", "-shortest"]
        } else {
            Vec::new()
        })
        .arg(&source)
        .status()
        .expect("run ffmpeg for synthetic test media");
    assert!(status.success(), "could not create synthetic H.264 fixture");
    crate::tests::insert_asset_at(catalog, project_id, id, &source);
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET duration_seconds=?2,source_out=?2,frame_rate=24,width=320,height=180,orientation_probed=1 WHERE id=?1",
            params![id, f64::from(seconds)],
        )
        .unwrap();
    catalog.validate_asset_video(id).unwrap();
    if with_proxy {
        catalog.generate_proxy(id).unwrap();
    }
    source
}

fn scope(project_id: &str, asset_ids: &[&str]) -> PreparationScope {
    PreparationScope {
        project_id: project_id.into(),
        asset_ids: asset_ids.iter().map(|id| (*id).into()).collect(),
        all_assets: false,
    }
}

fn moment_options(budget_usd: f64, payload: MomentPayload) -> MomentRunOptions {
    MomentRunOptions {
        model: DEFAULT_MODEL.into(),
        provider: "openrouter".into(),
        payload,
        budget_usd,
        coarse_frames: 8,
        refinement_frames: 8,
        max_candidates: 1,
        profile: Default::default(),
        force: false,
        retry_uncertain: false,
        allowed_models: vec![DEFAULT_MODEL.to_string()],
        allow_fallback: false,
        policy_id: None,
    }
}

#[derive(Clone)]
struct EmptySpeechEngine {
    calls: Arc<AtomicUsize>,
}

impl SttEngine for EmptySpeechEngine {
    fn detect_speech(&self, _pcm: &[f32]) -> Result<Vec<SpeechRegion>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    }

    fn decode_window(&self, _pcm: &[f32], _window: &PackedWindow) -> Result<Vec<DecodedSegment>> {
        unreachable!("empty VAD output must never call the decoder")
    }
}

struct ScriptedProvider {
    calls: AtomicUsize,
    models: Mutex<Vec<String>>,
    cost_usd: f64,
    usage_complete: bool,
}

impl ScriptedProvider {
    fn new(cost_usd: f64, usage_complete: bool) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            models: Mutex::new(Vec::new()),
            cost_usd,
            usage_complete,
        }
    }
}

impl VlmProvider for ScriptedProvider {
    fn analysis_cost_bound(
        &self,
        _model: &str,
        _request: &VlmRequest,
    ) -> Result<AnalysisRequestCostBound> {
        Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
            0.02,
        ))
    }

    fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        self.models.lock().unwrap().push(model.into());
        Ok(VlmResponse {
            model: model.into(),
            body: json!({
                "moments": [],
                "uncertain": true,
                "reason": "synthetic scripted provider output requires review"
            })
            .to_string(),
            prompt_tokens: 2,
            completion_tokens: 2,
            cost_usd: self.cost_usd,
            latency_ms: 1,
            receipt: VlmReceipt {
                actual_model: Some(model.into()),
                provider: Some("openrouter".into()),
                generation_id: Some(format!("preparation-test-{call}")),
                usage_complete: self.usage_complete,
            },
        })
    }
}

fn set_removed(catalog: &Catalog, asset_id: &str) {
    let asset = catalog.get_asset(asset_id).unwrap();
    catalog
        .update_asset(
            asset_id,
            &AssetPatch {
                notes: asset.notes,
                labels: asset.labels,
                removed: true,
                rotation_degrees: asset.rotation_degrees,
                expected_revision: Some(asset.revision),
            },
        )
        .unwrap();
}

fn assert_no_scratch_audio(catalog: &Catalog, project_id: &str) {
    let directory = catalog
        .database_path()
        .parent()
        .unwrap()
        .join("cache/audio")
        .join(project_id);
    if directory.exists() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            assert!(
                !entry.file_name().to_string_lossy().ends_with(".16k.wav"),
                "fake STT scratch audio must be removed after transcription"
            );
        }
    }
}

fn default_receipt(
    asset_id: &str,
    project_id: &str,
    source_fingerprint: &str,
    representation_fingerprint: &str,
    decoded: bool,
    transcript: Option<store::TranscriptBinding>,
) -> store::ValidationReceipt {
    store::ValidationReceipt {
        id: format!("receipt-{asset_id}-{representation_fingerprint}"),
        asset_id: asset_id.into(),
        project_id: project_id.into(),
        source_fingerprint: source_fingerprint.into(),
        source_duration_ms: 2_000,
        validated_end_ms: 2_000,
        representation_fingerprint: representation_fingerprint.into(),
        representation_recipe: "proxy.raw.v3".into(),
        validation_recipe: PREPARATION_VALIDATION_RECIPE.into(),
        validator: "synthetic fixture".into(),
        decoded,
        diagnostic: None,
        validated_at: "2026-01-01T00:00:00Z".into(),
        transcript,
        transcript_error: None,
    }
}

#[test]
fn readonly_preflight_is_observational_and_rejects_bad_scopes_and_pages() {
    let (directory, catalog, project_id) = fixture();
    let original = directory.path().join("readonly.mp4");
    fs::write(&original, b"synthetic non-media bytes").unwrap();
    crate::tests::insert_asset_at(&catalog, &project_id, "readonly", &original);
    drop(catalog);

    let catalog_path = directory.path().join("catalog.sqlite3");
    let before = fs::read(&catalog_path).unwrap();
    let readonly = Catalog::open_read_only_existing(&catalog_path).unwrap();
    let manifest = readonly
        .preparation_preflight(
            &PreparationScope {
                project_id: project_id.clone(),
                asset_ids: vec![],
                all_assets: true,
            },
            0,
            1,
        )
        .unwrap();
    assert_eq!(manifest.items.len(), 1);
    assert!(!directory.path().join("cache").exists());

    let contradictory = PreparationScope {
        project_id: project_id.clone(),
        asset_ids: vec!["readonly".into()],
        all_assets: true,
    };
    assert!(
        readonly
            .preparation_preflight(&contradictory, 0, 1)
            .is_err()
    );
    let wrong_project = scope("project_does_not_exist", &["readonly"]);
    assert!(
        readonly
            .preparation_preflight(&wrong_project, 0, 1)
            .is_err()
    );
    let all = PreparationScope {
        project_id,
        asset_ids: vec![],
        all_assets: true,
    };
    assert!(readonly.preparation_preflight(&all, 0, 0).is_err());
    assert!(readonly.preparation_preflight(&all, 0, 51).is_err());
    assert!(readonly.preparation_preflight(&all, 1_000_001, 1).is_err());
    drop(readonly);

    assert_eq!(fs::read(&catalog_path).unwrap(), before);
    assert!(!directory.path().join("cache").exists());
}

#[test]
fn all_asset_and_transcript_pages_are_bounded_and_long_text_is_marked() {
    let (directory, catalog, project_id) = fixture();
    for index in 0..65 {
        let id = format!("asset-{index:03}");
        let original = directory.path().join(format!("{id}.mp4"));
        fs::write(&original, format!("synthetic bytes {index}")).unwrap();
        crate::tests::insert_asset_at(&catalog, &project_id, &id, &original);
        catalog
            .connection()
            .unwrap()
            .execute(
                "UPDATE assets SET duration_seconds=10,source_out=10 WHERE id=?1",
                [&id],
            )
            .unwrap();
    }
    let all = PreparationScope {
        project_id: project_id.clone(),
        asset_ids: vec![],
        all_assets: true,
    };
    let page = catalog.preparation_preflight(&all, 2, 2).unwrap();
    assert_eq!(page.total_assets, 65);
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.next_offset, Some(4));
    assert_eq!(page.items[0].asset_id, "asset-002");

    let explicit = scope(
        &project_id,
        &[
            "asset-064",
            "asset-001",
            "asset-003",
            "asset-000",
            "asset-002",
        ],
    );
    let selected = catalog.preparation_preflight(&explicit, 1, 2).unwrap();
    assert_eq!(selected.scope.selected_asset_count, 5);
    assert_eq!(selected.items.len(), 2);
    assert_eq!(selected.items[0].asset_id, "asset-001");
    assert_eq!(selected.next_offset, Some(3));

    let segments = vec![
        DecodedSegment {
            start_ms: 100,
            end_ms: 200,
            language: "ja".into(),
            text: "a".repeat(5_000),
            mean_token_probability: Some(0.9),
        },
        DecodedSegment {
            start_ms: 300,
            end_ms: 400,
            language: "ja".into(),
            text: "second".into(),
            mean_token_probability: Some(0.9),
        },
        DecodedSegment {
            start_ms: 500,
            end_ms: 600,
            language: "ja".into(),
            text: "third".into(),
            mean_token_probability: Some(0.9),
        },
    ];
    save_transcript(
        &catalog.connection().unwrap(),
        &StoredTranscript {
            asset_id: "asset-000".into(),
            recipe_key: crate::STT_RECIPE_KEY.into(),
            source_fingerprint: "fixture-proxy-fingerprint".into(),
            status: TranscriptStatus::Speech,
            outcome_reason: crate::analysis::store::TranscriptOutcomeReason::Accepted,
            reject_reason: String::new(),
            primary_language: "ja".into(),
            speech_seconds: 0.3,
            window_count: 1,
            provider_segment_count: segments.len() as u32,
            mean_token_probability: None,
            repetition_ratio: None,
            segments,
        },
    )
    .unwrap();
    let transcript = catalog
        .preparation_transcript_page("asset-000", 0, 2)
        .unwrap();
    assert_eq!(transcript.segments.len(), 2);
    assert_eq!(transcript.transcript.segment_count, 3);
    assert_eq!(transcript.next_offset, Some(2));
    assert_eq!(transcript.segments[0].text.chars().count(), 4096);
    assert!(transcript.segments[0].text_truncated);
}

#[test]
fn default_preparation_does_not_call_provider_but_explicit_scope_uses_one_shared_budget() {
    if !media_tools_available() {
        eprintln!("skipping generated preparation provider fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let (directory, catalog, project_id) = fixture();
    make_asset(&catalog, &project_id, directory.path(), "clip-a", 2, true);
    make_asset(&catalog, &project_id, directory.path(), "clip-b", 2, true);
    make_asset(
        &catalog,
        &project_id,
        directory.path(),
        "unselected",
        2,
        true,
    );
    let selected = scope(&project_id, &["clip-a", "clip-b"]);
    let default_provider = ScriptedProvider::new(0.01, true);

    let default_run = catalog
        .prepare_with_services(
            &selected,
            &PreparationRunOptions::default(),
            None,
            Some(&default_provider),
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert!(default_run.moments.is_none());
    assert_eq!(default_provider.calls.load(Ordering::SeqCst), 0);

    let known_provider = ScriptedProvider::new(0.01, true);
    let options = PreparationRunOptions {
        moments: Some(moment_options(0.02, MomentPayload::Frames)),
        ..Default::default()
    };
    let paid = catalog
        .prepare_with_services(&selected, &options, None, Some(&known_provider), |_| {
            ControlFlow::Continue(())
        })
        .unwrap();
    let moment_report = paid.moments.as_ref().expect("explicit moments policy runs");
    assert_eq!(
        moment_report.total_assets, 2,
        "only the explicit scope is analyzed"
    );
    assert_eq!(
        known_provider.calls.load(Ordering::SeqCst),
        1,
        "the pilot's full bound and confirmed cost leave less than the next conservative bulk bound"
    );
    assert!(moment_report.budget_exhausted);
    assert!(
        moment_report.usage_complete,
        "a proven local admission denial is not unknown provider usage"
    );
    assert!(
        moment_report.cost_usd <= 0.02,
        "pilot and bulk share one ceiling"
    );
    assert!(
        known_provider
            .models
            .lock()
            .unwrap()
            .iter()
            .all(|model| model == DEFAULT_MODEL)
    );

    let uncertain_provider = ScriptedProvider::new(0.01, false);
    let uncertain_scope = scope(&project_id, &["clip-b"]);
    let uncertain_options = PreparationRunOptions {
        moments: Some(moment_options(0.5, MomentPayload::Frames)),
        ..Default::default()
    };
    let uncertain = catalog.prepare_with_services(
        &uncertain_scope,
        &uncertain_options,
        None,
        Some(&uncertain_provider),
        |_| ControlFlow::Continue(()),
    );
    assert!(
        uncertain.is_err(),
        "an unknown-usage pilot must stop before bulk moment work"
    );
    let calls_after_unknown_receipt = uncertain_provider.calls.load(Ordering::SeqCst);
    assert_eq!(
        calls_after_unknown_receipt, 1,
        "only the bounded pilot may have been transmitted"
    );
    let resumed = catalog.prepare_with_services(
        &uncertain_scope,
        &uncertain_options,
        None,
        Some(&uncertain_provider),
        |_| ControlFlow::Continue(()),
    );
    assert!(
        resumed.is_err(),
        "unknown pilot usage is not retried implicitly"
    );
    assert_eq!(
        uncertain_provider.calls.load(Ordering::SeqCst),
        calls_after_unknown_receipt
    );
}

#[test]
fn cached_receipt_transcript_and_moments_reuse_without_engine_or_provider_calls() {
    if !media_tools_available() {
        eprintln!("skipping generated preparation reuse fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let (directory, catalog, project_id) = fixture();
    make_asset(&catalog, &project_id, directory.path(), "reuse", 2, true);
    let selected = scope(&project_id, &["reuse"]);
    let engine_calls = Arc::new(AtomicUsize::new(0));
    let provider = ScriptedProvider::new(0.0, true);
    let options = PreparationRunOptions {
        transcribe: true,
        moments: Some(moment_options(1.0, MomentPayload::Frames)),
        ..Default::default()
    };
    let first = catalog
        .prepare_with_services(
            &selected,
            &options,
            Some(Box::new(EmptySpeechEngine {
                calls: engine_calls.clone(),
            })),
            Some(&provider),
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(first.processed, 1);
    assert_eq!(engine_calls.load(Ordering::SeqCst), 1);
    assert_no_scratch_audio(&catalog, &project_id);
    let initial_provider_calls = provider.calls.load(Ordering::SeqCst);
    let first_item = &first.manifest.items[0];
    let first_moment = first_item.moments.as_ref().unwrap().artifact.clone();
    let first_transcript = serde_json::to_value(&first_item.transcript).unwrap();
    let first_representation = first_item.representation.fingerprint.clone();

    let second = catalog
        .prepare_with_services(
            &selected,
            &options,
            Some(Box::new(EmptySpeechEngine {
                calls: engine_calls.clone(),
            })),
            Some(&provider),
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(second.reused, 1);
    assert_eq!(engine_calls.load(Ordering::SeqCst), 1);
    assert_no_scratch_audio(&catalog, &project_id);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        initial_provider_calls
    );
    let second_item = &second.manifest.items[0];
    let second_moment = second_item.moments.as_ref().unwrap().artifact.clone();
    assert_eq!(second_moment.identity, first_moment.identity);
    assert_eq!(second_moment.status, first_moment.status);
    assert_eq!(second_moment.candidates, first_moment.candidates);
    assert_eq!(second_moment.coverage, first_moment.coverage);
    assert_eq!(
        serde_json::to_value(&second_item.transcript).unwrap(),
        first_transcript
    );
    assert_eq!(second_item.representation.fingerprint, first_representation);
    assert!(
        second_moment
            .provenance
            .receipts
            .iter()
            .all(|receipt| receipt.reused)
    );
}

#[test]
fn cached_transcript_past_validated_proxy_end_requires_review_without_retranscription() {
    if !media_tools_available() {
        eprintln!("skipping generated duration-bound fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let (directory, catalog, project_id) = fixture();
    make_asset(
        &catalog,
        &project_id,
        directory.path(),
        "duration-bound",
        2,
        true,
    );
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET duration_seconds=2.1,source_out=2.1 WHERE id='duration-bound'",
            [],
        )
        .unwrap();
    let asset = catalog.get_asset("duration-bound").unwrap();
    let proxy_fingerprint = catalog.proxy_fingerprint(&asset.id).unwrap();
    save_transcript(
        &catalog.connection().unwrap(),
        &StoredTranscript {
            asset_id: asset.id.clone(),
            recipe_key: crate::STT_RECIPE_KEY.into(),
            source_fingerprint: proxy_fingerprint,
            status: TranscriptStatus::Speech,
            outcome_reason: crate::analysis::store::TranscriptOutcomeReason::Accepted,
            reject_reason: String::new(),
            primary_language: "en".into(),
            speech_seconds: 0.1,
            window_count: 1,
            provider_segment_count: 1,
            mean_token_probability: None,
            repetition_ratio: None,
            segments: vec![DecodedSegment {
                start_ms: 1_900,
                end_ms: 2_050,
                language: "en".into(),
                text: "beyond validated proxy end".into(),
                mean_token_probability: Some(0.9),
            }],
        },
    )
    .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let report = catalog
        .prepare_with_services(
            &scope(&project_id, &["duration-bound"]),
            &PreparationRunOptions {
                transcribe: true,
                ..Default::default()
            },
            Some(Box::new(EmptySpeechEngine {
                calls: calls.clone(),
            })),
            None,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    let validated_end = report.manifest.items[0]
        .technical_range
        .as_ref()
        .unwrap()
        .end_ms;
    // The generated AAC container may include its short encoder tail (here 5 ms);
    // the cached segment still ends beyond the exact validated proxy duration.
    assert!(
        (2_000..=2_010).contains(&validated_end),
        "validated proxy end was {validated_end} ms"
    );
    assert_eq!(
        report.manifest.items[0].transcript.readiness.state,
        PreparationState::ReviewRequired
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "invalid cached timing must not silently retrigger transcription"
    );
}

#[test]
fn cancellation_resumes_only_missing_transcript_work_and_busy_proxy_is_skipped() {
    if !media_tools_available() {
        eprintln!("skipping generated preparation resume fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let (directory, catalog, project_id) = fixture();
    make_asset(&catalog, &project_id, directory.path(), "clip-a", 2, true);
    make_asset(&catalog, &project_id, directory.path(), "clip-b", 2, true);
    let selected = scope(&project_id, &["clip-b", "clip-a"]);
    let options = PreparationRunOptions {
        transcribe: true,
        ..Default::default()
    };
    let first_calls = Arc::new(AtomicUsize::new(0));
    let cancelled = catalog
        .prepare_with_services(
            &selected,
            &options,
            Some(Box::new(EmptySpeechEngine {
                calls: first_calls.clone(),
            })),
            None,
            |progress| {
                if progress.asset_id == "clip-b" && progress.stage == "preflight" {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            },
        )
        .unwrap();
    assert!(cancelled.cancelled);
    assert_eq!(cancelled.processed, 1);
    assert_eq!(first_calls.load(Ordering::SeqCst), 1);
    assert_no_scratch_audio(&catalog, &project_id);

    let resume_calls = Arc::new(AtomicUsize::new(0));
    let resumed = catalog
        .prepare_with_services(
            &selected,
            &options,
            Some(Box::new(EmptySpeechEngine {
                calls: resume_calls.clone(),
            })),
            None,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(resumed.processed, 2);
    assert_eq!(resumed.reused, 1);
    assert_eq!(resume_calls.load(Ordering::SeqCst), 1);
    assert_no_scratch_audio(&catalog, &project_id);

    let claim = catalog
        .try_claim_representation("clip-a", RepresentationKind::Proxy)
        .unwrap()
        .expect("test owns the proxy claim");
    let busy_options = PreparationRunOptions {
        revalidate: true,
        ..Default::default()
    };
    let busy = catalog
        .prepare_with_services(
            &scope(&project_id, &["clip-a"]),
            &busy_options,
            None,
            None,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    drop(claim);
    assert_eq!(busy.skipped, 1);
    assert_eq!(busy.failed, 0);
}

#[test]
fn review_required_transcript_exposes_safe_quality_evidence_without_candidate_text() {
    if !media_tools_available() {
        eprintln!("skipping generated preparation review fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let (directory, catalog, project_id) = fixture();
    make_asset(&catalog, &project_id, directory.path(), "review", 2, true);
    let fingerprint = catalog.proxy_fingerprint("review").unwrap();
    save_transcript(
        &catalog.connection().unwrap(),
        &StoredTranscript {
            asset_id: "review".into(),
            recipe_key: crate::STT_RECIPE_KEY.into(),
            source_fingerprint: fingerprint,
            status: TranscriptStatus::ReviewRequired,
            outcome_reason:
                crate::analysis::store::TranscriptOutcomeReason::QualityEvidenceIncomplete,
            reject_reason: "review_missing_quality_evidence".into(),
            primary_language: "en".into(),
            speech_seconds: 2.0,
            window_count: 1,
            provider_segment_count: 2,
            mean_token_probability: None,
            repetition_ratio: None,
            segments: vec![
                DecodedSegment {
                    start_ms: 0,
                    end_ms: 1_000,
                    language: "en".into(),
                    text: "REVIEW_CANDIDATE_CANARY_ONE".into(),
                    mean_token_probability: None,
                },
                DecodedSegment {
                    start_ms: 1_000,
                    end_ms: 2_000,
                    language: "en".into(),
                    text: "REVIEW_CANDIDATE_CANARY_TWO".into(),
                    mean_token_probability: None,
                },
            ],
        },
    )
    .unwrap();

    let manifest = catalog
        .preparation_preflight(&scope(&project_id, &["review"]), 0, 10)
        .unwrap();
    let transcript = &manifest.items[0].transcript;
    assert_eq!(transcript.status.as_deref(), Some("review_required"));
    assert_eq!(
        transcript.outcome_reason.as_deref(),
        Some("quality_evidence_incomplete")
    );
    assert_eq!(
        transcript.readiness.state,
        PreparationState::ReviewRequired,
        "retained review candidates must never be reported as ready/accepted"
    );
    assert_eq!(transcript.segment_count, 2);
    assert_eq!(transcript.speech_seconds, Some(2.0));
    assert_eq!(transcript.provider_segment_count, Some(2));
    assert_eq!(transcript.window_count, Some(1));
    let reason = transcript.readiness.reasons.join(" ");
    assert!(
        reason.contains("quality evidence is incomplete"),
        "{reason}"
    );
    assert!(reason.contains("2 timed candidate segments"), "{reason}");
    assert!(
        !reason.contains("REVIEW_CANDIDATE_CANARY"),
        "reason leaked text"
    );
}

#[test]
fn legacy_no_speech_is_review_required_rather_than_verified_silence() {
    if !media_tools_available() {
        eprintln!("skipping generated preparation legacy fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let (directory, catalog, project_id) = fixture();
    make_asset(&catalog, &project_id, directory.path(), "legacy", 1, true);
    let fingerprint = catalog.proxy_fingerprint("legacy").unwrap();
    save_transcript(
        &catalog.connection().unwrap(),
        &StoredTranscript {
            asset_id: "legacy".into(),
            recipe_key: crate::STT_RECIPE_KEY.into(),
            source_fingerprint: fingerprint,
            status: TranscriptStatus::NoSpeech,
            outcome_reason:
                crate::analysis::store::TranscriptOutcomeReason::LegacyNoSpeechUnverified,
            reject_reason: String::new(),
            primary_language: String::new(),
            speech_seconds: 0.0,
            window_count: 0,
            provider_segment_count: 0,
            mean_token_probability: None,
            repetition_ratio: None,
            segments: Vec::new(),
        },
    )
    .unwrap();
    let manifest = catalog
        .preparation_preflight(&scope(&project_id, &["legacy"]), 0, 10)
        .unwrap();
    let transcript = &manifest.items[0].transcript;
    assert_eq!(transcript.status.as_deref(), Some("no_speech"));
    assert_eq!(
        transcript.outcome_reason.as_deref(),
        Some("legacy_no_speech_unverified")
    );
    assert_eq!(
        transcript.readiness.state,
        PreparationState::ReviewRequired,
        "legacy no_speech must not be presented as verified silence"
    );
    assert!(
        transcript
            .readiness
            .reasons
            .iter()
            .any(|reason| reason.contains("cannot prove literal silence"))
    );
}

#[test]
fn no_audio_preparation_is_ready_without_engine_or_model_work() {
    if !media_tools_available() {
        eprintln!("skipping generated no-audio preparation fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let (directory, catalog, project_id) = fixture();
    make_asset_with_audio(
        &catalog,
        &project_id,
        directory.path(),
        "no-audio",
        1,
        true,
        false,
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let engine = EmptySpeechEngine {
        calls: calls.clone(),
    };
    let options = PreparationRunOptions {
        generate_proxies: false,
        transcribe: true,
        revalidate: false,
        moments: None,
    };
    let report = catalog
        .prepare_with_services(
            &scope(&project_id, &["no-audio"]),
            &options,
            Some(Box::new(engine)),
            None,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(report.failed, 0);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a no-audio clip must not invoke VAD or ASR"
    );
    let transcript = &report.manifest.items[0].transcript;
    assert_eq!(transcript.status.as_deref(), Some("no_audio"));
    assert_eq!(
        transcript.outcome_reason.as_deref(),
        Some("no_audio_stream")
    );
    assert_eq!(transcript.readiness.state, PreparationState::Ready);
    assert_eq!(transcript.speech_seconds, Some(0.0));
    assert_eq!(transcript.window_count, Some(0));
    assert_eq!(transcript.provider_segment_count, Some(0));
}

#[test]
fn callback_exclusion_prevents_proxy_generation_and_preserves_original_bytes() {
    if !media_tools_available() {
        eprintln!("skipping generated preparation exclusion fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let (directory, catalog, project_id) = fixture();
    let original = make_asset(
        &catalog,
        &project_id,
        directory.path(),
        "remove-before-work",
        2,
        false,
    );
    let bytes_before = fs::read(&original).unwrap();
    let options = PreparationRunOptions {
        generate_proxies: true,
        ..Default::default()
    };
    let report = catalog
        .prepare_with_services(
            &scope(&project_id, &["remove-before-work"]),
            &options,
            None,
            None,
            |progress| {
                if progress.stage == "preflight" {
                    set_removed(&catalog, &progress.asset_id);
                }
                ControlFlow::Continue(())
            },
        )
        .unwrap();
    assert!(report.manifest.items[0].state == PreparationState::Excluded);
    assert_eq!(report.excluded, 1);
    assert_eq!(report.processed, 0);
    assert_eq!(report.reused, 0);
    assert_eq!(report.failed, 0);
    assert!(
        catalog
            .get_asset("remove-before-work")
            .unwrap()
            .proxy_path
            .is_none()
    );
    assert_eq!(fs::read(&original).unwrap(), bytes_before);
}

#[test]
fn current_proxy_remains_preparable_after_original_goes_offline() {
    if !media_tools_available() {
        eprintln!("skipping generated offline-original fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let (directory, catalog, project_id) = fixture();
    let original = make_asset(
        &catalog,
        &project_id,
        directory.path(),
        "offline-original",
        2,
        true,
    );
    fs::remove_file(&original).unwrap();
    let offline = catalog.get_asset("offline-original").unwrap();
    assert!(!offline.original_available);
    assert!(offline.proxy_available);

    let report = catalog
        .prepare_with_services(
            &scope(&project_id, &["offline-original"]),
            &PreparationRunOptions {
                revalidate: true,
                ..Default::default()
            },
            None,
            None,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(report.failed, 0);
    assert_eq!(
        report.manifest.items[0].representation.validation.state,
        PreparationState::Ready
    );
}

#[test]
fn corrupt_and_excluded_assets_remain_truthful_in_a_partial_preparation_run() {
    if !media_tools_available() {
        eprintln!("skipping generated quarantine fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let (directory, catalog, project_id) = fixture();
    make_asset(&catalog, &project_id, directory.path(), "good", 2, true);
    make_asset(&catalog, &project_id, directory.path(), "corrupt", 2, true);
    make_asset(&catalog, &project_id, directory.path(), "excluded", 2, true);
    let corrupt_proxy = catalog
        .get_asset("corrupt")
        .unwrap()
        .proxy_path
        .map(PathBuf::from)
        .unwrap();
    fs::write(&corrupt_proxy, b"truncated synthetic proxy").unwrap();
    set_removed(&catalog, "excluded");

    let report = catalog
        .prepare_with_services(
            &scope(&project_id, &["good", "corrupt", "excluded"]),
            &PreparationRunOptions {
                revalidate: true,
                ..Default::default()
            },
            None,
            None,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(
        report.failed, 0,
        "per-asset media evidence stays reportable"
    );
    assert_eq!(report.excluded, 1);
    let states = report
        .manifest
        .items
        .iter()
        .map(|item| (item.asset_id.as_str(), item.state))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(states["good"], PreparationState::Missing);
    assert_eq!(states["corrupt"], PreparationState::Quarantined);
    assert_eq!(states["excluded"], PreparationState::Excluded);
}

#[test]
fn changing_only_unsampled_proxy_bytes_invalidates_full_decode_and_transcript_binding() {
    if !media_tools_available() {
        eprintln!("skipping generated transcript binding fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    use std::io::{Seek, SeekFrom, Write};

    let (directory, catalog, project_id) = fixture();
    make_asset(
        &catalog,
        &project_id,
        directory.path(),
        "replace-proxy",
        2,
        true,
    );
    let asset = catalog.get_asset("replace-proxy").unwrap();
    let proxy = PathBuf::from(asset.proxy_path.as_ref().unwrap());

    // A valid MP4 `free` atom gives this tiny proxy a large unsampled middle.
    let filler_bytes = 8 * 1024 * 1024u32;
    {
        let mut file = fs::OpenOptions::new().append(true).open(&proxy).unwrap();
        file.write_all(&(filler_bytes + 8).to_be_bytes()).unwrap();
        file.write_all(b"free").unwrap();
        file.write_all(&vec![0; filler_bytes as usize]).unwrap();
    }
    let sampled_before = catalog.proxy_fingerprint(&asset.id).unwrap();
    let transcript = StoredTranscript {
        asset_id: asset.id.clone(),
        recipe_key: crate::STT_RECIPE_KEY.into(),
        source_fingerprint: sampled_before.clone(),
        status: TranscriptStatus::Speech,
        outcome_reason: crate::analysis::store::TranscriptOutcomeReason::Accepted,
        reject_reason: String::new(),
        primary_language: "ja".into(),
        speech_seconds: 0.5,
        window_count: 1,
        provider_segment_count: 1,
        mean_token_probability: None,
        repetition_ratio: None,
        segments: vec![DecodedSegment {
            start_ms: 250,
            end_ms: 750,
            language: "ja".into(),
            text: "known transcript for the padded proxy".into(),
            mean_token_probability: Some(0.9),
        }],
    };
    save_transcript(&catalog.connection().unwrap(), &transcript).unwrap();
    let options = PreparationRunOptions {
        revalidate: true,
        ..Default::default()
    };
    let first = catalog
        .prepare_with_services(
            &scope(&project_id, &["replace-proxy"]),
            &options,
            None,
            None,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(
        first.manifest.items[0].representation.validation.state,
        PreparationState::Ready
    );
    assert_eq!(
        first.manifest.items[0].transcript.readiness.state,
        PreparationState::Ready
    );
    assert_eq!(
        first.manifest.items[0].transcript.binding_origin.as_deref(),
        Some("adopted_existing_sampled_recipe")
    );
    let old_full_fingerprint = first.manifest.items[0]
        .representation
        .fingerprint
        .clone()
        .unwrap();

    // Modify a byte outside both sampled 1 MiB regions while preserving size
    // and timestamps: sampled identity stays equal, but full BLAKE3 changes.
    let metadata = fs::metadata(&proxy).unwrap();
    let modified = metadata.modified().unwrap();
    let accessed = metadata.accessed().unwrap();
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&proxy)
        .unwrap();
    file.seek(SeekFrom::Start(2 * 1024 * 1024 + 17)).unwrap();
    file.write_all(&[0x5a]).unwrap();
    file.set_times(
        fs::FileTimes::new()
            .set_accessed(accessed)
            .set_modified(modified),
    )
    .unwrap();
    drop(file);
    assert_eq!(
        catalog.proxy_fingerprint(&asset.id).unwrap(),
        sampled_before
    );
    let new_full_fingerprint = crate::full_file_blake3(&proxy, &|| false)
        .unwrap()
        .hex_digest;
    assert_ne!(new_full_fingerprint, old_full_fingerprint);

    let stale = catalog
        .preparation_preflight(&scope(&project_id, &["replace-proxy"]), 0, 1)
        .unwrap();
    assert_eq!(
        stale.items[0].representation.validation.state,
        PreparationState::Stale
    );
    let second = catalog
        .prepare_with_services(
            &scope(&project_id, &["replace-proxy"]),
            &options,
            None,
            None,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    let item = &second.manifest.items[0];
    assert_eq!(
        item.representation.validation.state,
        PreparationState::Ready
    );
    assert_eq!(
        item.representation.fingerprint.as_deref(),
        Some(new_full_fingerprint.as_str())
    );
    assert_eq!(item.transcript.readiness.state, PreparationState::Stale);
    assert_eq!(item.transcript.binding_origin, None);
    let current_receipt: String = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT body_json FROM preparation_validation_receipts WHERE asset_id=?1 AND representation_fingerprint=?2",
            params![asset.id, new_full_fingerprint],
            |row| row.get(0),
        )
        .unwrap();
    let receipt: store::ValidationReceipt = serde_json::from_str(&current_receipt).unwrap();
    assert!(receipt.decoded);
}

#[test]
fn proxy_generation_preserves_orientation_annotations_chronology_and_original_bytes() {
    if !media_tools_available() {
        eprintln!(
            "skipping generated orientation-preservation fixture: ffmpeg/ffprobe unavailable"
        );
        return;
    }
    let (directory, catalog, project_id) = fixture();
    let original = make_asset(
        &catalog,
        &project_id,
        directory.path(),
        "orientation",
        2,
        false,
    );
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET notes='human note',title='human title',description='human description',labels_json='[\"human\"]',removed=0,rotation_degrees=180,default_rotation_degrees=90,orientation_probed=0,best_time='2025-12-01T01:02:03Z',offset_seconds=37 WHERE id='orientation'",
            [],
        )
        .unwrap();
    let before_bytes = fs::read(&original).unwrap();
    let before_revision = catalog.get_asset("orientation").unwrap().revision;
    let before: (String, String, String, String, i64, i64, i64, i64, Option<String>, i64) = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT notes,title,description,labels_json,removed,rotation_degrees,default_rotation_degrees,orientation_probed,best_time,offset_seconds FROM assets WHERE id='orientation'",
            [],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?)),
        )
        .unwrap();
    let report = catalog
        .prepare_with_services(
            &scope(&project_id, &["orientation"]),
            &PreparationRunOptions {
                generate_proxies: true,
                ..Default::default()
            },
            None,
            None,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(report.failed, 0);
    assert!(catalog.get_asset("orientation").unwrap().proxy_available);
    let after: (String, String, String, String, i64, i64, i64, i64, Option<String>, i64) = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT notes,title,description,labels_json,removed,rotation_degrees,default_rotation_degrees,orientation_probed,best_time,offset_seconds FROM assets WHERE id='orientation'",
            [],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?)),
        )
        .unwrap();
    assert_eq!(after, before);
    assert_eq!(
        catalog.get_asset("orientation").unwrap().revision,
        before_revision
    );
    assert_eq!(fs::read(&original).unwrap(), before_bytes);
}

#[test]
fn preparation_marks_confirmed_default_and_stale_or_unresolved_decisions_truthfully() {
    if !media_tools_available() {
        return;
    }
    let (directory, catalog, project_id) = fixture();
    let media_dir = directory.path().join("media");
    fs::create_dir_all(&media_dir).unwrap();
    make_asset(
        &catalog,
        &project_id,
        &media_dir,
        "orientation_state",
        1,
        false,
    );
    let asset = catalog.get_asset("orientation_state").unwrap();
    let confirmed = catalog
        .decide_orientation(
            &asset.id,
            asset.revision,
            crate::OrientationDecision {
                status: "confirmed".into(),
                rotation_degrees: Some(asset.rotation_degrees),
            },
        )
        .unwrap();
    assert_eq!(
        super::catalog::orientation_confirmation(&confirmed),
        "visually_confirmed"
    );
    assert!(super::catalog::orientation_review_questions(&confirmed).is_empty());

    let rotated = catalog.set_rotation(&confirmed.id, 90).unwrap();
    assert_eq!(
        super::catalog::orientation_confirmation(&rotated),
        "confirmed_value_mismatch"
    );
    assert!(
        super::catalog::orientation_review_questions(&rotated)
            .iter()
            .any(|question| question.contains("reconfirm"))
    );

    let unresolved = catalog
        .decide_orientation(
            &rotated.id,
            rotated.revision,
            crate::OrientationDecision {
                status: "unresolved".into(),
                rotation_degrees: None,
            },
        )
        .unwrap();
    assert_eq!(unresolved.rotation_degrees, 90);
    assert_eq!(
        super::catalog::orientation_confirmation(&unresolved),
        "explicitly_unresolved"
    );
    assert!(!super::catalog::orientation_review_questions(&unresolved).is_empty());
}

#[test]
fn unversioned_catalog_replay_preserves_preparation_receipt_rows() {
    let (directory, catalog, project_id) = fixture();
    let original = directory.path().join("legacy-replay.mp4");
    fs::write(&original, b"synthetic receipt migration fixture").unwrap();
    crate::tests::insert_asset_at(&catalog, &project_id, "legacy-replay", &original);
    let receipt = default_receipt(
        "legacy-replay",
        &project_id,
        "source-fingerprint",
        "representation-fingerprint",
        true,
        None,
    );
    store::save(&catalog.connection().unwrap(), &receipt).unwrap();
    let saved_body: String = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT body_json FROM preparation_validation_receipts WHERE asset_id='legacy-replay'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    catalog
        .connection()
        .unwrap()
        .execute("DROP TABLE schema_migrations", [])
        .unwrap();
    let path = catalog.database_path().to_path_buf();
    drop(catalog);

    let reopened = Catalog::open(&path).unwrap();
    let (count, body): (i64, String) = reopened
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*),MAX(body_json) FROM preparation_validation_receipts WHERE asset_id='legacy-replay'",
            [],
            |row| Ok((row.get(0)?,row.get(1)?)),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(body, saved_body);
}

#[test]
fn known_probability_hash_material_preserves_existing_artifact_fingerprint_inputs() {
    let old_segment_material = serde_json::to_vec(&(
        100_u32,
        200_u32,
        "ja".to_string(),
        "known".to_string(),
        0.73_f64,
    ))
    .unwrap();
    let new_segment_material = serde_json::to_vec(&(
        100_u32,
        200_u32,
        "ja".to_string(),
        "known".to_string(),
        Some(0.73_f64),
    ))
    .unwrap();
    assert_eq!(
        old_segment_material, new_segment_material,
        "a known numeric measurement must serialize byte-identically so existing bindings stay current"
    );

    let unknown_segment_material = serde_json::to_vec(&(
        100_u32,
        200_u32,
        "ja".to_string(),
        "unknown".to_string(),
        Option::<f64>::None,
    ))
    .unwrap();
    assert!(
        std::str::from_utf8(&unknown_segment_material)
            .unwrap()
            .ends_with("null]"),
        "an absent measurement must hash distinctly as null"
    );
}
