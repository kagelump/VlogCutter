use super::*;
use crate::analysis::provider::{DEFAULT_MODEL, FALLBACK_MODEL, VlmReceipt};
use crate::analysis::quality::DecodedSegment;
use crate::analysis::store::{StoredTranscript, save_transcript};
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::tempdir;

struct EmptyMomentProvider {
    calls: AtomicUsize,
}

struct InvalidMomentProvider;

impl VlmProvider for InvalidMomentProvider {
    fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
        Ok(VlmResponse {
            model: model.to_owned(),
            body: r#"{"moments":[],"uncertain":false,"reason":"bad fixture","unexpected":"retain raw"}"#.into(),
            prompt_tokens: 1,
            completion_tokens: 1,
            cost_usd: 0.0,
            latency_ms: 1,
            receipt: VlmReceipt {
                actual_model: Some(model.to_owned()),
                provider: Some("openrouter".into()),
                generation_id: Some("invalid-moment-fixture".into()),
                usage_complete: true,
            },
        })
    }
}

impl VlmProvider for EmptyMomentProvider {
    fn analyze(&self, model: &str, request: &VlmRequest) -> Result<VlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let evidence: serde_json::Value = request
            .prompt
            .split_once("EVIDENCE=")
            .and_then(|(_, json)| serde_json::from_str(json).ok())
            .context("fake provider did not receive structured evidence")?;
        ensure!(
            evidence["frameTimesMs"]
                .as_array()
                .is_some_and(|times| !times.is_empty()),
            "fake provider expected offered frame times"
        );
        ensure!(
            request.frames.len() == evidence["frameTimesMs"].as_array().unwrap().len()
                && request
                    .frames
                    .iter()
                    .all(|frame| frame.starts_with("data:image/jpeg;base64,")),
            "fake provider expected a JPEG payload for each offered time"
        );
        Ok(VlmResponse {
            model: model.to_owned(),
            body: r#"{"moments":[],"uncertain":false,"reason":"synthetic quiet clip"}"#.into(),
            prompt_tokens: 4,
            completion_tokens: 4,
            cost_usd: 0.0,
            latency_ms: 1,
            receipt: VlmReceipt {
                actual_model: Some(model.to_owned()),
                provider: Some("openrouter".into()),
                generation_id: Some(format!("fake-{}", self.calls.load(Ordering::SeqCst))),
                usage_complete: true,
            },
        })
    }
}

struct EventProvider {
    calls: AtomicUsize,
    centers_ms: Vec<u32>,
}

impl VlmProvider for EventProvider {
    fn analyze(&self, model: &str, request: &VlmRequest) -> Result<VlmResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let input: serde_json::Value = request
            .prompt
            .split_once("EVIDENCE=")
            .and_then(|(_, json)| serde_json::from_str(json).ok())
            .context("fake event provider did not receive structured evidence")?;
        let phase = input["phase"].as_str().unwrap();
        let times = input["frameTimesMs"]
            .as_array()
            .context("frame times missing")?;
        ensure!(
            request.frames.len() == times.len()
                && request
                    .frames
                    .iter()
                    .all(|frame| frame.starts_with("data:image/jpeg;base64,")),
            "each supplied sample must have its proxy JPEG payload"
        );
        let start = input["inspectedSpan"]["startMs"].as_u64().unwrap() as u32;
        let end = input["inspectedSpan"]["endMs"].as_u64().unwrap() as u32;
        let proposal_schema = &request.schema["properties"]["moments"]["items"]["properties"];
        for field in ["coreStartMs", "coreEndMs", "payoffStartMs", "payoffEndMs"] {
            ensure!(
                proposal_schema[field]["minimum"] == start
                    && proposal_schema[field]["maximum"] == end,
                "wire schema for {field} must match inspected source window"
            );
        }
        for field in ["setup", "context"] {
            let bounds = &proposal_schema[field]["anyOf"][1]["properties"];
            ensure!(
                bounds["startMs"]["minimum"] == start
                    && bounds["startMs"]["maximum"] == end
                    && bounds["endMs"]["minimum"] == start
                    && bounds["endMs"]["maximum"] == end,
                "wire schema for {field} must match inspected source window"
            );
        }
        let seed = &input["candidateToRefine"];
        if phase == "refine" {
            let fields = seed.as_object().context("refinement seed missing")?;
            ensure!(
                fields
                    .keys()
                    .all(|key| ["event", "core", "payoff", "reason"].contains(&key.as_str())),
                "refinement seed must not include prior evidence citations"
            );
        }
        let centers = if phase == "refine" {
            let seed_event = input["candidateToRefine"]["event"].as_str();
            self.centers_ms
                .iter()
                .copied()
                .filter(|center| {
                    seed_event.is_some_and(|event| event == format!("event at {center}ms"))
                })
                .collect::<Vec<_>>()
        } else {
            self.centers_ms
                .iter()
                .copied()
                .filter(|center| start <= *center && *center < end)
                .collect()
        };
        let mut moments = Vec::new();
        for center in centers {
            let core_start = center.saturating_sub(2_000).max(start);
            let core_end = center.saturating_add(2_000).min(end);
            ensure!(
                core_end.saturating_sub(core_start) >= 3_000,
                "fixture event span too short"
            );
            let payoff_start = center.saturating_sub(500).max(core_start);
            let payoff_end = center.saturating_add(500).min(core_end);
            let nearest = times
                .iter()
                .filter_map(|time| time.as_u64())
                .min_by_key(|at| at.abs_diff(u64::from(center)))
                .context("event request has no frame samples")?;
            moments.push(json!({
                "event": format!("event at {center}ms"), "role":"action", "reason":"synthetic event at a sampled time",
                "score":0.9, "coreStartMs":core_start, "coreEndMs":core_end,
                "payoffStartMs":payoff_start, "payoffEndMs":payoff_end,
                "setup":null, "context":null, "longHold":false,
                "evidenceKeys":[format!("frame:{nearest}")], "uncertainty":[]
            }));
        }
        let body = json!({"moments":moments,"uncertain":false,"reason":"synthetic event fixture"})
            .to_string();
        Ok(VlmResponse {
            model: model.to_owned(),
            body,
            prompt_tokens: 4,
            completion_tokens: 4,
            cost_usd: 0.0,
            latency_ms: 1,
            receipt: VlmReceipt {
                actual_model: Some(model.to_owned()),
                provider: Some("openrouter".into()),
                generation_id: Some(format!("event-{call}")),
                usage_complete: true,
            },
        })
    }
}

struct LongHoldProvider;

impl VlmProvider for LongHoldProvider {
    fn analyze(&self, model: &str, request: &VlmRequest) -> Result<VlmResponse> {
        let input: serde_json::Value = request
            .prompt
            .split_once("EVIDENCE=")
            .and_then(|(_, json)| serde_json::from_str(json).ok())
            .context("fake hold provider did not receive structured evidence")?;
        let times = input["frameTimesMs"]
            .as_array()
            .context("frame times missing")?;
        ensure!(
            request.frames.len() == times.len(),
            "frame payload count mismatch"
        );
        let start = input["inspectedSpan"]["startMs"].as_u64().unwrap() as u32;
        let end = input["inspectedSpan"]["endMs"].as_u64().unwrap() as u32;
        let center = (u64::from(start) + u64::from(end)) / 2;
        let nearest = times
            .iter()
            .filter_map(|time| time.as_u64())
            .min_by_key(|at| at.abs_diff(center))
            .context("hold request has no samples")?;
        let body = json!({"moments":[{
            "event":"continuous scenic hold", "role":"hold", "reason":"the full sustained scene is useful",
            "score":0.8, "coreStartMs":start, "coreEndMs":end,
            "payoffStartMs":center.saturating_sub(500), "payoffEndMs":center.saturating_add(500),
            "setup":null, "context":null, "longHold":true,
            "evidenceKeys":[format!("frame:{nearest}")], "uncertainty":[]
        }],"uncertain":false,"reason":"synthetic hold fixture"}).to_string();
        Ok(VlmResponse {
            model: model.into(),
            body,
            prompt_tokens: 2,
            completion_tokens: 2,
            cost_usd: 0.0,
            latency_ms: 1,
            receipt: VlmReceipt {
                actual_model: Some(model.into()),
                provider: Some("openrouter".into()),
                generation_id: Some(format!("hold-{center}")),
                usage_complete: true,
            },
        })
    }
}

struct UncertainProvider {
    calls: AtomicUsize,
}

struct FailAtProvider {
    calls: AtomicUsize,
    fail_at: usize,
    events: EventProvider,
}

impl VlmProvider for FailAtProvider {
    fn analyze(&self, model: &str, request: &VlmRequest) -> Result<VlmResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call == self.fail_at {
            anyhow::bail!("synthetic transport interruption after request submission");
        }
        self.events.analyze(model, request)
    }
}

struct CostProvider {
    calls: AtomicUsize,
    cost_usd: f64,
}

impl VlmProvider for CostProvider {
    fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(VlmResponse {
            model: model.into(),
            body: r#"{"moments":[],"uncertain":false,"reason":"quiet synthetic sample"}"#.into(),
            prompt_tokens: 10,
            completion_tokens: 5,
            cost_usd: self.cost_usd,
            latency_ms: 10,
            receipt: VlmReceipt {
                actual_model: Some(model.into()),
                provider: Some("openrouter".into()),
                generation_id: Some(format!("cost-{call}")),
                usage_complete: true,
            },
        })
    }
}

struct ReportedIdentityProvider {
    calls: AtomicUsize,
    actual_model: Option<String>,
    provider: Option<String>,
}

impl VlmProvider for ReportedIdentityProvider {
    fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(VlmResponse {
            model: model.into(),
            body: r#"{"moments":[],"uncertain":false,"reason":"quiet synthetic sample"}"#.into(),
            prompt_tokens: 1,
            completion_tokens: 1,
            cost_usd: 0.0,
            latency_ms: 1,
            receipt: VlmReceipt {
                actual_model: self.actual_model.clone(),
                provider: self.provider.clone(),
                generation_id: Some("reported-identity".into()),
                usage_complete: true,
            },
        })
    }
}

struct DialogueProvider;

impl VlmProvider for DialogueProvider {
    fn analyze(&self, model: &str, request: &VlmRequest) -> Result<VlmResponse> {
        let input: serde_json::Value = request
            .prompt
            .split_once("EVIDENCE=")
            .and_then(|(_, json)| serde_json::from_str(json).ok())
            .context("fake dialogue provider did not receive structured evidence")?;
        let times = input["frameTimesMs"]
            .as_array()
            .context("frame times missing")?;
        ensure!(
            request.frames.len() == times.len(),
            "frame payload count mismatch"
        );
        let start = input["inspectedSpan"]["startMs"].as_u64().unwrap() as u32;
        let end = input["inspectedSpan"]["endMs"].as_u64().unwrap() as u32;
        let speech = input["acceptedTimedSpeech"]
            .as_array()
            .context("timed speech missing")?;
        let mut moments = Vec::new();
        for utterance in speech {
            let key = utterance["key"]
                .as_str()
                .context("utterance evidence key missing")?;
            let utterance_start = utterance["span"]["startMs"].as_u64().unwrap() as u32;
            let utterance_end = utterance["span"]["endMs"].as_u64().unwrap() as u32;
            let core_start = utterance_start.saturating_sub(1_200).max(start);
            let core_end = utterance_end.saturating_add(1_200).min(end);
            if core_end.saturating_sub(core_start) < 3_000 {
                continue;
            }
            let midpoint = (u64::from(utterance_start) + u64::from(utterance_end)) / 2;
            let nearest = times
                .iter()
                .filter_map(|time| time.as_u64())
                .min_by_key(|at| at.abs_diff(midpoint))
                .context("dialogue request has no frames")?;
            moments.push(json!({
                "event":format!("timed utterance {key}"), "role":"dialogue", "reason":"accepted speech aligns with the sampled scene",
                "score":0.8, "coreStartMs":core_start, "coreEndMs":core_end,
                "payoffStartMs":utterance_start, "payoffEndMs":utterance_end,
                "setup":null, "context":null, "longHold":false,
                "evidenceKeys":[format!("frame:{nearest}"),key], "uncertainty":[]
            }));
        }
        Ok(VlmResponse {
            model: model.into(),
            body: json!({"moments":moments,"uncertain":false,"reason":"synthetic timed dialogue"})
                .to_string(),
            prompt_tokens: 2,
            completion_tokens: 2,
            cost_usd: 0.0,
            latency_ms: 1,
            receipt: VlmReceipt {
                actual_model: Some(model.into()),
                provider: Some("openrouter".into()),
                generation_id: Some("dialogue-fixture".into()),
                usage_complete: true,
            },
        })
    }
}

impl VlmProvider for UncertainProvider {
    fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(VlmResponse {
            model: model.into(),
            body: r#"{"moments":[],"uncertain":true,"reason":"repetitive synthetic views do not establish an event"}"#.into(),
            prompt_tokens: 2,
            completion_tokens: 2,
            cost_usd: 0.0,
            latency_ms: 1,
            receipt: VlmReceipt {
                actual_model: Some(model.into()),
                provider: Some("openrouter".into()),
                generation_id: Some(format!("uncertain-{call}")),
                usage_complete: true,
            },
        })
    }
}

fn generated_asset(
    catalog: &Catalog,
    project_id: &str,
    root: &std::path::Path,
    id: &str,
    seconds: u32,
) -> PathBuf {
    let source_dir = root.join("source");
    std::fs::create_dir_all(&source_dir).unwrap();
    let source = source_dir.join(format!("{id}.MP4"));
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
        .arg(if id == "repetitive" {
            format!("testsrc2=s=640x360:r=24:d={seconds}")
        } else {
            format!(
                "color=c=0x{}:s=640x360:r=24:d={seconds}",
                &blake3::hash(id.as_bytes()).to_hex()[..6]
            )
        });
    if id == "dialogue" {
        command
            .args(["-f", "lavfi", "-i"])
            .arg(format!(
                "sine=frequency=440:sample_rate=48000:duration={seconds},volume=0:enable='not(between(t,3.4,4.4)+between(t,16.4,17.4)+between(t,21.9,22.9))'"
            ))
            .args(["-map", "0:v:0", "-map", "1:a:0", "-c:a", "aac"]);
    } else {
        command.arg("-an");
    }
    command.args(["-c:v", "libx264", "-pix_fmt", "yuv420p"]);
    if id == "events" {
        command.args([
            "-vf",
            "drawbox=x='100+50*sin(100*t)':y=90:w=120:h=120:color=red:t=fill:enable='between(t,4.8,5.8)',drawbox=x='350+40*sin(80*t)':y=150:w=140:h=90:color=blue:t=fill:enable='between(t,21.6,22.6)'",
        ]);
    } else {
        command.args(["-vf", "format=yuv420p"]);
    }
    let generated = command.arg(&source).status().unwrap();
    assert!(
        generated.success(),
        "synthetic H.264 source creation failed"
    );
    crate::tests::insert_asset_at(catalog, project_id, id, &source);
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET duration_seconds=?2,source_out=?2 WHERE id=?1",
            params![id, seconds],
        )
        .unwrap();
    catalog.validate_asset_video(id).unwrap();
    catalog.generate_proxy(id).unwrap();
    source
}

fn event_options() -> MomentRunOptions {
    MomentRunOptions {
        model: DEFAULT_MODEL.into(),
        provider: "openrouter".into(),
        payload: MomentPayload::Frames,
        budget_usd: 2.0,
        coarse_frames: 96,
        refinement_frames: 384,
        max_candidates: 8,
        profile: EditProfile::default(),
        force: false,
        retry_uncertain: false,
        allowed_models: vec![DEFAULT_MODEL.to_string()],
        allow_fallback: false,
        policy_id: None,
    }
}

fn human_asset_snapshot(
    catalog: &Catalog,
    asset_id: &str,
) -> (
    String,
    String,
    String,
    String,
    i64,
    i64,
    Option<String>,
    i64,
) {
    catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT notes,title,description,labels_json,removed,rotation_degrees,best_time,offset_seconds FROM assets WHERE id=?1",
            [asset_id],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?)),
        )
        .unwrap()
}

#[test]
fn quiet_opening_late_action_and_disjoint_payoffs_are_refined_from_full_span_samples() {
    if !tool_available("ffmpeg") || !tool_available("ffprobe") {
        eprintln!("skipping generated moment event fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog
        .create_project("Synthetic late and separate events")
        .unwrap();
    let source = generated_asset(&catalog, &project.id, directory.path(), "events", 24);
    let original_bytes = std::fs::read(&source).unwrap();
    {
        let connection = catalog.connection().unwrap();
        connection.execute(
            "UPDATE assets SET notes='human note',title='human title',description='human description',labels_json='[\"human label\"]',removed=0,rotation_degrees=270,best_time='2025-12-07T08:09:10Z',offset_seconds=37 WHERE id='events'",
            [],
        ).unwrap();
        connection.execute(
            "INSERT INTO analysis_labels(asset_id,normalized_label,display_label,proposed_label,created_at) VALUES ('events','synthetic','Synthetic','Synthetic','now')",
            [],
        ).unwrap();
        connection.execute(
            "INSERT INTO capture_sequences(id,project_id,camera_key,label,created_at) VALUES ('sequence',?1,'camera','trip','now')",
            params![project.id],
        ).unwrap();
        connection.execute(
            "INSERT INTO asset_capture_sequence(asset_id,project_id,capture_sequence_id,filename_counter) VALUES ('events',?1,'sequence',1)",
            params![project.id],
        ).unwrap();
    }
    let human_fields_before = human_asset_snapshot(&catalog, "events");
    let provider = EventProvider {
        calls: AtomicUsize::new(0),
        centers_ms: vec![5_000, 22_000],
    };
    let options = event_options();
    let report = catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &["events".into()],
            &options,
            &provider,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    let page = catalog
        .moment_evidence_page("events", 0, 50)
        .unwrap()
        .unwrap();
    assert_eq!(
        report.processed,
        1,
        "discovery report: {}",
        serde_json::to_string(&report).unwrap()
    );
    assert_eq!(
        page.artifact.status,
        MomentStatus::Ready,
        "review questions: {:?}",
        page.artifact.review_questions
    );
    assert_eq!(
        page.artifact.candidates.len(),
        2,
        "independent payoffs must survive deduplication"
    );
    assert!(
        page.artifact
            .candidates
            .iter()
            .any(|candidate| candidate.payoff.start_ms > 19_000),
        "late action was omitted"
    );
    assert!(
        page.artifact
            .candidates
            .iter()
            .all(|candidate| candidate.refined),
        "candidates must use truthful dense refinement"
    );
    assert!(
        page.artifact
            .coverage
            .iter()
            .any(|segment| segment.span.start_ms > 15_000)
    );
    let limited = catalog
        .moment_evidence_page("events", 0, 1)
        .unwrap()
        .unwrap();
    assert!(limited.artifact.candidates.len() <= 1);
    assert!(limited.artifact.coverage.len() <= 1);
    assert!(limited.artifact.provenance.receipts.len() <= 1);
    assert!(limited.artifact.review_questions.len() <= 1);
    assert!(limited.total_coverage_segments > limited.artifact.coverage.len());
    assert_eq!(limited.total_candidates, 2);
    assert_eq!(
        limited.total_receipts,
        page.artifact.provenance.receipts.len()
    );
    let second_page = catalog
        .moment_evidence_page("events", 1, 1)
        .unwrap()
        .unwrap();
    assert_eq!(second_page.offset, 1);
    assert_eq!(second_page.artifact.candidates.len(), 1);
    assert_eq!(second_page.artifact.coverage.len(), 1);
    assert_eq!(second_page.artifact.provenance.receipts.len(), 1);
    assert_eq!(second_page.next_offset, Some(2));
    for (offset, limit) in [(0, 0), (0, 51), (1_000_001, 1)] {
        assert!(
            catalog
                .moment_evidence_page("events", offset, limit)
                .is_err()
        );
        assert!(
            catalog
                .moment_inventory_page(&project.id, offset, limit)
                .is_err()
        );
    }
    assert_eq!(limited.next_offset, Some(1));
    let connection = catalog.connection().unwrap();
    let label: String = connection
        .query_row(
            "SELECT display_label FROM analysis_labels WHERE asset_id='events'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let sequence: String = connection
        .query_row(
            "SELECT capture_sequence_id FROM asset_capture_sequence WHERE asset_id='events'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        label, "Synthetic",
        "moment discovery must not mutate human/analysis labels"
    );
    assert_eq!(
        sequence, "sequence",
        "moment discovery must not mutate chronology facts"
    );
    assert_eq!(
        human_asset_snapshot(&catalog, "events"),
        human_fields_before,
        "discovery must preserve human annotations and timing/rotation overrides"
    );
    assert_eq!(
        std::fs::read(&source).unwrap(),
        original_bytes,
        "discovery must not alter immutable source bytes"
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        6,
        "four coarse windows and two event refinements expected"
    );
    let ids = page
        .artifact
        .candidates
        .iter()
        .map(|candidate| candidate.id.clone())
        .collect::<Vec<_>>();
    let samples_before_reuse: i64 = catalog.connection().unwrap().query_row(
        "SELECT COALESCE(SUM(samples),0) FROM batch_performance WHERE task_kind LIKE 'moments.%'", [], |row| row.get(0),
    ).unwrap();
    let cached = catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &["events".into()],
            &options,
            &provider,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    let cached_page = catalog
        .moment_evidence_page("events", 0, 50)
        .unwrap()
        .unwrap();
    assert_eq!(cached.no_candidates, 0);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        6,
        "cached event evidence made another provider call"
    );
    assert_eq!(
        cached_page
            .artifact
            .candidates
            .iter()
            .map(|candidate| candidate.id.clone())
            .collect::<Vec<_>>(),
        ids
    );
    let samples_after_reuse: i64 = catalog.connection().unwrap().query_row(
        "SELECT COALESCE(SUM(samples),0) FROM batch_performance WHERE task_kind LIKE 'moments.%'", [], |row| row.get(0),
    ).unwrap();
    assert_eq!(
        samples_after_reuse, samples_before_reuse,
        "cache-only work must not train throughput"
    );
    assert_eq!(
        human_asset_snapshot(&catalog, "events"),
        human_fields_before,
        "cache reuse must preserve human annotations and overrides"
    );
    assert_eq!(
        std::fs::read(&source).unwrap(),
        original_bytes,
        "cache reuse must leave source bytes unchanged"
    );
}

#[test]
fn justified_long_hold_retains_a_twenty_four_second_core_through_refinement() {
    if !tool_available("ffmpeg") || !tool_available("ffprobe") {
        eprintln!("skipping generated long-hold fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Synthetic long hold").unwrap();
    generated_asset(&catalog, &project.id, directory.path(), "hold", 24);
    let provider = LongHoldProvider;
    let mut options = event_options();
    options.coarse_frames = 8;
    let report = catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &["hold".into()],
            &options,
            &provider,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    let page = catalog
        .moment_evidence_page("hold", 0, 50)
        .unwrap()
        .unwrap();
    assert_eq!(report.processed, 1);
    assert_eq!(
        page.artifact.status,
        MomentStatus::Ready,
        "review questions: {:?}",
        page.artifact.review_questions
    );
    let candidate = &page.artifact.candidates[0];
    assert!(candidate.long_hold);
    assert_eq!(
        candidate.core.duration_ms(),
        24_000,
        "the justified hold must not be shortened to a select"
    );
    assert!(candidate.refined);
    assert!(
        page.artifact
            .coverage
            .iter()
            .any(|segment| segment.level == CoverageLevel::Refined && segment.max_gap_ms <= 500)
    );
}

#[test]
fn short_quiet_clip_is_a_valid_zero_candidate_result() {
    if !tool_available("ffmpeg") || !tool_available("ffprobe") {
        eprintln!("skipping generated short-source fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Synthetic short source").unwrap();
    generated_asset(&catalog, &project.id, directory.path(), "short", 2);
    let provider = EmptyMomentProvider {
        calls: AtomicUsize::new(0),
    };
    let mut options = event_options();
    options.coarse_frames = 8;
    options.refinement_frames = 8;
    let report = catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &["short".into()],
            &options,
            &provider,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    let page = catalog
        .moment_evidence_page("short", 0, 50)
        .unwrap()
        .unwrap();
    assert_eq!(
        report.no_candidates, 1,
        "quiet concise source should be a valid zero result"
    );
    assert_eq!(page.artifact.status, MomentStatus::NoCandidates);
    assert!(page.artifact.candidates.is_empty());
    assert!(page.artifact.provenance.source_duration_ms <= 2_000);
}

#[test]
fn repetitive_uncertain_views_remain_needs_review_with_reason() {
    if !tool_available("ffmpeg") || !tool_available("ffprobe") {
        eprintln!("skipping generated uncertain-view fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog
        .create_project("Synthetic repetitive views")
        .unwrap();
    generated_asset(&catalog, &project.id, directory.path(), "repetitive", 10);
    let provider = UncertainProvider {
        calls: AtomicUsize::new(0),
    };
    let mut options = event_options();
    options.coarse_frames = 8;
    let report = catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &["repetitive".into()],
            &options,
            &provider,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    let page = catalog
        .moment_evidence_page("repetitive", 0, 50)
        .unwrap()
        .unwrap();
    assert_eq!(report.needs_review, 1);
    assert_eq!(page.artifact.status, MomentStatus::NeedsReview);
    assert!(page.artifact.candidates.is_empty());
    assert!(
        page.artifact
            .review_questions
            .iter()
            .any(|question| question.contains("repetitive synthetic views"))
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "uncertainty should trigger one bounded refinement request"
    );
}

#[test]
fn budget_ceiling_stops_before_the_next_provider_request() {
    if !tool_available("ffmpeg") || !tool_available("ffprobe") {
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Synthetic budget ceiling").unwrap();
    generated_asset(&catalog, &project.id, directory.path(), "budget", 24);
    let provider = CostProvider {
        calls: AtomicUsize::new(0),
        cost_usd: 0.02,
    };
    let mut options = event_options();
    options.budget_usd = 0.005;
    let report = catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &["budget".into()],
            &options,
            &provider,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert!(report.budget_exhausted);
    assert_eq!(
        report.cost_usd, 0.02,
        "complete reported spend stays visible even above the ceiling"
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "the next request must not start after the ceiling is reached"
    );

    let samples_before_resume: i64 = catalog.connection().unwrap().query_row(
        "SELECT COALESCE(SUM(samples),0) FROM batch_performance WHERE task_kind LIKE 'moments.%'", [], |row| row.get(0),
    ).unwrap();
    options.budget_usd = 0.1;
    let resumed = catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &["budget".into()],
            &options,
            &provider,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        4,
        "resume should reuse the first completed window and request only three missing windows"
    );
    assert_eq!(resumed.cost_usd, 0.06, "report only newly incurred spend");
    let page = catalog
        .moment_evidence_page("budget", 0, 50)
        .unwrap()
        .unwrap();
    assert_eq!(
        page.artifact.provenance.cost_usd, 0.08,
        "provenance retains the first run's 0.02 plus the resumed 0.06"
    );
    let samples_after_resume: i64 = catalog.connection().unwrap().query_row(
        "SELECT COALESCE(SUM(samples),0) FROM batch_performance WHERE task_kind LIKE 'moments.%'", [], |row| row.get(0),
    ).unwrap();
    assert_eq!(
        samples_after_resume, samples_before_resume,
        "partial-cache reuse must not train throughput"
    );
}

#[test]
fn cancellation_before_first_request_makes_no_provider_call() {
    if !tool_available("ffmpeg") || !tool_available("ffprobe") {
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Synthetic cancellation").unwrap();
    generated_asset(&catalog, &project.id, directory.path(), "cancel", 2);
    let provider = EmptyMomentProvider {
        calls: AtomicUsize::new(0),
    };
    let options = event_options();
    let report = catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &["cancel".into()],
            &options,
            &provider,
            |_| ControlFlow::Break(()),
        )
        .unwrap();
    assert!(report.cancelled);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        0,
        "cancellation must be observed before the paid call"
    );
}

#[test]
fn busy_proxy_claim_skips_and_preflight_removal_is_refreshed_before_paid_work() {
    if !tool_available("ffmpeg") || !tool_available("ffprobe") {
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog
        .create_project("Synthetic busy representation")
        .unwrap();
    generated_asset(&catalog, &project.id, directory.path(), "busy", 2);
    let provider = EmptyMomentProvider {
        calls: AtomicUsize::new(0),
    };
    let options = event_options();
    let proxy_claim = catalog
        .claim_representation("busy", RepresentationKind::Proxy)
        .unwrap();
    let busy = catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &["busy".into()],
            &options,
            &provider,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(busy.skipped, 1);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        0,
        "a busy proxy representation must skip before any paid request"
    );
    drop(proxy_claim);

    let mut changed = false;
    let removed = catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &["busy".into()],
            &options,
            &provider,
            |_| {
                if !changed {
                    catalog
                        .connection()
                        .unwrap()
                        .execute("UPDATE assets SET removed=1 WHERE id='busy'", [])
                        .unwrap();
                    changed = true;
                }
                ControlFlow::Continue(())
            },
        )
        .unwrap();
    assert!(changed);
    assert_eq!(
        removed.needs_review, 1,
        "the asset changed during preflight and must be reread after representation claims"
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        0,
        "a newly removed asset must never reach the provider"
    );
    let page = catalog
        .moment_evidence_page("busy", 0, 50)
        .unwrap()
        .unwrap();
    assert_eq!(page.artifact.status, MomentStatus::Unavailable);
}

#[test]
fn cancellation_after_first_request_resumes_only_missing_windows() {
    if !tool_available("ffmpeg") || !tool_available("ffprobe") {
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog
        .create_project("Synthetic mid-run cancellation")
        .unwrap();
    generated_asset(
        &catalog,
        &project.id,
        directory.path(),
        "cancel_after_first",
        24,
    );
    let provider = EmptyMomentProvider {
        calls: AtomicUsize::new(0),
    };
    let options = event_options();
    let mut phase_callbacks = 0;
    let cancelled = catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &["cancel_after_first".into()],
            &options,
            &provider,
            |_| {
                phase_callbacks += 1;
                if phase_callbacks == 3 {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            },
        )
        .unwrap();
    assert!(cancelled.cancelled);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "cancellation at the second request boundary follows one completed request"
    );
    let resumed = catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &["cancel_after_first".into()],
            &options,
            &provider,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        4,
        "resume reuses the first coarse window and requests only the three missing windows"
    );
    assert_eq!(resumed.no_candidates, 1);
}

#[test]
fn orphaned_pending_receipt_blocks_force_and_explicit_retry() {
    if !tool_available("ffmpeg") || !tool_available("ffprobe") {
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog
        .create_project("Synthetic orphaned receipt")
        .unwrap();
    generated_asset(&catalog, &project.id, directory.path(), "orphaned", 2);
    let provider = EmptyMomentProvider {
        calls: AtomicUsize::new(0),
    };
    let options = event_options();
    catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &["orphaned".into()],
            &options,
            &provider,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    // Resume rows use the internal base identity from moment_steps. The
    // public artifact identity also includes reported provider/model data.
    let identity: String = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT identity FROM moment_steps WHERE asset_id='orphaned' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let pending = MomentReceipt {
        run_id: "crashed-run".into(),
        step_key: "orphaned-pending-step".into(),
        actual_model: None,
        provider: None,
        generation_id: None,
        cost_usd: None,
        usage_complete: false,
        reused: false,
        estimated_cost_usd: None,
        cost_basis: None,
        native_usage: None,
    };
    store::save_step(
        &catalog.connection().unwrap(),
        &MomentStep {
            asset_id: "orphaned".into(),
            identity: identity.clone(),
            step_key: "orphaned-pending-step".into(),
            actual_model: options.model.clone(),
            body: serde_json::to_value(CachedStep {
                prior_receipts: vec![],
                state: "pending".into(),
                reply: None,
                receipt: pending,
                error: String::new(),
                raw_output: None,
            })
            .unwrap(),
            cost_usd: 0.0,
            usage_complete: false,
        },
    )
    .unwrap();
    assert_eq!(
        catalog
            .unresolved_moment_receipts("orphaned", &identity)
            .unwrap()
            .len(),
        1,
        "the injected pending row is readable under the saved artifact identity"
    );

    let mut forced = options.clone();
    forced.force = true;
    for retry_uncertain in [false, true] {
        forced.retry_uncertain = retry_uncertain;
        let report = catalog
            .discover_moments_with_provider_for_domain_test(
                &project.id,
                &["orphaned".into()],
                &forced,
                &provider,
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        assert!(
            !report.usage_complete,
            "retry_uncertain={retry_uncertain}, calls={}, report={report:?}: retry must keep an orphaned charge unknown until reconciled",
            provider.calls.load(Ordering::SeqCst)
        );
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            if retry_uncertain { 2 } else { 1 },
            "force alone is blocked; explicit retry may make one fresh request"
        );
    }
    let page = catalog
        .moment_evidence_page("orphaned", 0, 50)
        .unwrap()
        .unwrap();
    assert_eq!(page.artifact.status, MomentStatus::NeedsReview);
    assert!(
        page.artifact
            .provenance
            .receipts
            .iter()
            .any(|receipt| receipt.reused
                && !receipt.usage_complete
                && receipt.run_id == "crashed-run")
    );
}

#[test]
fn uncertain_transport_blocks_force_and_explicit_retry_keeps_unknown_history() {
    if !tool_available("ffmpeg") || !tool_available("ffprobe") {
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog
        .create_project("Synthetic interrupted request")
        .unwrap();
    generated_asset(&catalog, &project.id, directory.path(), "interrupted", 24);
    let provider = FailAtProvider {
        calls: AtomicUsize::new(0),
        fail_at: 2,
        events: EventProvider {
            calls: AtomicUsize::new(0),
            centers_ms: vec![5_000],
        },
    };
    let mut options = event_options();
    options.coarse_frames = 8;
    let first = catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &["interrupted".into()],
            &options,
            &provider,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert!(!first.usage_complete);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);

    let mut forced = options.clone();
    forced.force = true;
    let blocked = catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &["interrupted".into()],
            &forced,
            &provider,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert!(!blocked.usage_complete);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "force alone must not duplicate an uncertain provider request"
    );

    let mut retry = options;
    retry.retry_uncertain = true;
    let recovered = catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &["interrupted".into()],
            &retry,
            &provider,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert!(
        !recovered.usage_complete,
        "unknown prior spend remains reflected after explicit retry"
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        3,
        "only the uncertain step should be retried; completed coarse evidence is reused"
    );
    let page = catalog
        .moment_evidence_page("interrupted", 0, 50)
        .unwrap()
        .unwrap();
    assert!(
        page.artifact
            .provenance
            .receipts
            .iter()
            .any(|receipt| receipt.reused && !receipt.usage_complete),
        "uncertain attempt history must remain visible"
    );
}

#[test]
fn accepted_timed_dialogue_is_grounded_across_pauses_on_video_audio_source() {
    if !tool_available("ffmpeg") || !tool_available("ffprobe") {
        eprintln!("skipping generated dialogue fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Synthetic timed dialogue").unwrap();
    generated_asset(&catalog, &project.id, directory.path(), "dialogue", 24);
    let source_fingerprint = catalog.proxy_fingerprint("dialogue").unwrap();
    save_transcript(
        &catalog.connection().unwrap(),
        &StoredTranscript {
            asset_id: "dialogue".into(),
            recipe_key: crate::analysis::STT_RECIPE_KEY.into(),
            source_fingerprint,
            status: TranscriptStatus::Speech,
            outcome_reason: crate::analysis::store::TranscriptOutcomeReason::Accepted,
            reject_reason: String::new(),
            primary_language: "en".into(),
            speech_seconds: 2.4,
            window_count: 1,
            provider_segment_count: 2,
            mean_token_probability: None,
            repetition_ratio: None,
            segments: vec![
                DecodedSegment {
                    start_ms: 3_500,
                    end_ms: 4_300,
                    language: "en".into(),
                    text: "first synthetic line".into(),
                    mean_token_probability: Some(0.9),
                },
                DecodedSegment {
                    start_ms: 16_500,
                    end_ms: 17_300,
                    language: "en".into(),
                    text: "second synthetic line".into(),
                    mean_token_probability: Some(0.9),
                },
                DecodedSegment {
                    start_ms: 22_000,
                    end_ms: 22_800,
                    language: "en".into(),
                    text: "late synthetic line".into(),
                    mean_token_probability: Some(0.9),
                },
            ],
        },
    )
    .unwrap();
    let provider = DialogueProvider;
    let mut options = event_options();
    options.payload = MomentPayload::FramesAndTranscript;
    let report = catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &["dialogue".into()],
            &options,
            &provider,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    let page = catalog
        .moment_evidence_page("dialogue", 0, 50)
        .unwrap()
        .unwrap();
    assert_eq!(
        report.processed,
        1,
        "dialogue discovery report: {}",
        serde_json::to_string(&report).unwrap()
    );
    assert_eq!(
        page.artifact.status,
        MomentStatus::Ready,
        "review questions: {:?}",
        page.artifact.review_questions
    );
    assert_eq!(
        page.artifact.provenance.payload,
        MomentPayload::FramesAndTranscript
    );
    assert_eq!(
        page.artifact.candidates.len(),
        3,
        "only timed utterances should be returned across the pauses"
    );
    assert!(
        page.artifact
            .candidates
            .iter()
            .all(|candidate| candidate.role == "dialogue"
                && !candidate.dialogue_boundaries.is_empty())
    );
    assert!(
        page.artifact
            .candidates
            .iter()
            .all(
                |candidate| candidate.evidence.iter().any(|reference| reference.kind
                    == "accepted_transcript"
                    && reference
                        .span
                        .is_some_and(|span| span.overlaps(candidate.payoff)))
            )
    );
    assert!(
        !page
            .artifact
            .provenance
            .representation_fingerprint
            .is_empty()
    );
    assert!(page.artifact.provenance.source_duration_ms >= 24_000);
}

#[test]
fn actual_provider_identity_is_not_inferred_when_receipt_fields_are_missing_or_mismatched() {
    if !tool_available("ffmpeg") || !tool_available("ffprobe") {
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog
        .create_project("Synthetic provider receipts")
        .unwrap();
    generated_asset(&catalog, &project.id, directory.path(), "receipt", 10);
    let mut options = event_options();
    options.coarse_frames = 8;
    let missing = ReportedIdentityProvider {
        calls: AtomicUsize::new(0),
        actual_model: None,
        provider: None,
    };
    let first = catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &["receipt".into()],
            &options,
            &missing,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(first.needs_review, 1);
    let missing_page = catalog
        .moment_evidence_page("receipt", 0, 50)
        .unwrap()
        .unwrap();
    assert_eq!(
        missing_page.artifact.provenance.requested_provider,
        "openrouter"
    );
    assert!(missing_page.artifact.provenance.provider.is_empty());
    assert!(missing_page.artifact.provenance.actual_model.is_empty());
    assert!(missing_page.artifact.provenance.usage_complete);

    options.model = FALLBACK_MODEL.into();
    options.allowed_models = vec![FALLBACK_MODEL.into()];
    let mismatch = ReportedIdentityProvider {
        calls: AtomicUsize::new(0),
        actual_model: Some("upstream/other-model".into()),
        provider: Some("upstream-endpoint".into()),
    };
    let second = catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &["receipt".into()],
            &options,
            &mismatch,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(second.needs_review, 1);
    let mismatch_page = catalog
        .moment_evidence_page("receipt", 0, 50)
        .unwrap()
        .unwrap();
    assert_eq!(
        mismatch_page.artifact.provenance.requested_model,
        FALLBACK_MODEL
    );
    assert_eq!(
        mismatch_page.artifact.provenance.actual_model,
        "upstream/other-model"
    );
    assert_eq!(
        mismatch_page.artifact.provenance.provider,
        "upstream-endpoint"
    );
    assert!(
        mismatch_page
            .artifact
            .review_questions
            .iter()
            .any(|question| question.contains("differs from the selected model"))
    );
}

#[test]
fn valid_proxy_works_without_original_but_removed_and_quarantined_assets_are_unavailable() {
    if !tool_available("ffmpeg") || !tool_available("ffprobe") {
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog
        .create_project("Synthetic proxy availability")
        .unwrap();
    let offline_source = generated_asset(&catalog, &project.id, directory.path(), "offline", 10);
    generated_asset(&catalog, &project.id, directory.path(), "quarantined", 10);
    generated_asset(&catalog, &project.id, directory.path(), "removed", 10);
    fs::remove_file(&offline_source).unwrap();
    catalog.connection().unwrap().execute(
        "UPDATE asset_video_validation SET status='quarantined',diagnostic='fixture quarantine' WHERE asset_id='quarantined'",
        [],
    ).unwrap();
    catalog
        .connection()
        .unwrap()
        .execute("UPDATE assets SET removed=1 WHERE id='removed'", [])
        .unwrap();
    let provider = EmptyMomentProvider {
        calls: AtomicUsize::new(0),
    };
    let options = event_options();
    let offline = catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &["offline".into()],
            &options,
            &provider,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(
        offline.no_candidates, 1,
        "validated current proxy remains usable when original media is offline"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    for id in ["quarantined", "removed"] {
        let report = catalog
            .discover_moments_with_provider_for_domain_test(
                &project.id,
                &[id.into()],
                &options,
                &provider,
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        assert_eq!(
            report.needs_review, 1,
            "{id} should yield an unavailable review artifact"
        );
        let page = catalog.moment_evidence_page(id, 0, 50).unwrap().unwrap();
        assert_eq!(page.artifact.status, MomentStatus::Unavailable);
        assert!(
            page.artifact
                .coverage
                .iter()
                .all(|segment| segment.level == CoverageLevel::Uninspected)
        );
    }
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "removed/quarantined clips must not reach the provider"
    );
}

fn tool_available(name: &str) -> bool {
    match crate::tool_command(name).arg("-version").output() {
        Ok(output) => {
            assert!(output.status.success(), "{name} -version failed");
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => panic!("could not run {name}: {error}"),
    }
}

#[test]
fn rejected_moment_response_retains_raw_provider_body() {
    if !tool_available("ffmpeg") || !tool_available("ffprobe") {
        eprintln!("skipping generated rejected-moment fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Synthetic rejected moment").unwrap();
    generated_asset(&catalog, &project.id, directory.path(), "rejected", 2);
    let options = event_options();
    let _ = catalog.discover_moments_with_provider_for_domain_test(
        &project.id,
        &["rejected".into()],
        &options,
        &InvalidMomentProvider,
        |_| ControlFlow::Continue(()),
    );
    let body: String = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT body_json FROM moment_steps WHERE asset_id='rejected' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let stored: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        stored["body"]["state"], "rejected",
        "invalid fake-provider response should be stored as rejected"
    );
    assert_eq!(
        stored["body"]["raw_output"],
        r#"{"moments":[],"uncertain":false,"reason":"bad fixture","unexpected":"retain raw"}"#
    );
}

#[test]
fn cached_moment_discovery_reuses_provider_steps_without_training_rate_history() {
    if !tool_available("ffmpeg") || !tool_available("ffprobe") {
        eprintln!("skipping generated moment cache fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Synthetic moments cache").unwrap();
    let source = generated_asset(&catalog, &project.id, directory.path(), "moment_asset", 10);

    let provider = EmptyMomentProvider {
        calls: AtomicUsize::new(0),
    };
    let options = MomentRunOptions {
        model: DEFAULT_MODEL.into(),
        provider: "openrouter".into(),
        payload: MomentPayload::Frames,
        budget_usd: 0.5,
        coarse_frames: 8,
        refinement_frames: 8,
        max_candidates: 1,
        profile: EditProfile::default(),
        force: false,
        retry_uncertain: false,
        allowed_models: vec![DEFAULT_MODEL.to_string()],
        allow_fallback: false,
        policy_id: None,
    };
    let assets = vec!["moment_asset".to_owned()];
    let run = || {
        catalog
            .discover_moments_with_provider_for_domain_test(
                &project.id,
                &assets,
                &options,
                &provider,
                |_| ControlFlow::Continue(()),
            )
            .unwrap()
    };

    let first = run();
    assert_eq!(
        first.no_candidates,
        1,
        "unexpected first discovery report: {}; evidence: {}",
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&catalog.moment_evidence_page("moment_asset", 0, 50).unwrap())
            .unwrap()
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let first_page = catalog
        .moment_evidence_page("moment_asset", 0, 50)
        .unwrap()
        .unwrap();
    let original_identity = first_page.artifact.identity.clone();
    let original_candidate_ids = first_page
        .artifact
        .candidates
        .iter()
        .map(|c| c.id.clone())
        .collect::<Vec<_>>();
    let samples_after_first: i64 = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT COALESCE(SUM(samples),0) FROM batch_performance WHERE task_kind LIKE 'moments.%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(samples_after_first > 0);

    let second = run();
    assert_eq!(second.no_candidates, 1);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "cached discovery must make no provider calls"
    );
    let samples_after_reuse: i64 = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT COALESCE(SUM(samples),0) FROM batch_performance WHERE task_kind LIKE 'moments.%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        samples_after_reuse, samples_after_first,
        "reused evidence must not be measured as new processing throughput"
    );
    let cached_page = catalog
        .moment_evidence_page("moment_asset", 0, 50)
        .unwrap()
        .unwrap();
    assert_eq!(cached_page.artifact.identity, original_identity);
    assert_eq!(
        cached_page
            .artifact
            .candidates
            .iter()
            .map(|c| c.id.clone())
            .collect::<Vec<_>>(),
        original_candidate_ids
    );

    let mut changed_model = options.clone();
    changed_model.model = FALLBACK_MODEL.into();
    changed_model.allowed_models = vec![FALLBACK_MODEL.into()];
    catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &assets,
            &changed_model,
            &provider,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "model change must invalidate reusable steps"
    );
    assert_ne!(
        catalog
            .moment_evidence_page("moment_asset", 0, 50)
            .unwrap()
            .unwrap()
            .artifact
            .identity,
        original_identity
    );

    let mut changed_profile = options.clone();
    changed_profile.profile.select_max_ms -= 1_000;
    catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &assets,
            &changed_profile,
            &provider,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        3,
        "edit profile change must invalidate reusable steps"
    );

    let original_bytes = fs::read(&source).unwrap();
    let write = catalog.begin_proxy_write("moment_asset").unwrap().unwrap();
    let replacement = crate::tool_command("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=640x360:r=24:d=10",
            "-an",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(write.temporary_path())
        .status()
        .unwrap();
    assert!(
        replacement.success(),
        "replacement H.264 proxy must be generated"
    );
    catalog.commit_proxy_write(write, None).unwrap();
    assert_eq!(fs::read(&source).unwrap(), original_bytes);
    catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &assets,
            &options,
            &provider,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        4,
        "proxy replacement must invalidate reusable steps"
    );
    let replaced = catalog
        .moment_evidence_page("moment_asset", 0, 50)
        .unwrap()
        .unwrap();
    assert_ne!(
        replaced.artifact.provenance.representation_fingerprint,
        first_page.artifact.provenance.representation_fingerprint
    );
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET duration_seconds=9.9 WHERE id='moment_asset'",
            [],
        )
        .unwrap();
    assert!(
        !catalog
            .moment_evidence_page("moment_asset", 0, 50)
            .unwrap()
            .unwrap()
            .current
    );
    let corrected = catalog
        .discover_moments_with_provider_for_domain_test(
            &project.id,
            &assets,
            &options,
            &provider,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(corrected.no_candidates, 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 5);
    let corrected = catalog
        .moment_evidence_page("moment_asset", 0, 50)
        .unwrap()
        .unwrap();
    assert_eq!(corrected.artifact.provenance.source_duration_ms, 9900);
    assert_ne!(corrected.artifact.identity, replaced.artifact.identity);
}
