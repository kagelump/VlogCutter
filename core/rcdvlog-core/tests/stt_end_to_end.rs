//! End-to-end transcription against real footage.
//!
//! Ignored by default: it needs the two downloaded models and a real project
//! with generated proxies. Run explicitly with
//!
//! ```text
//! RCDVLOG_STT_FIXTURE=<project folder> \
//!   cargo test -p rcdvlog-core --test stt_end_to_end -- --ignored --nocapture
//! ```
//!
//! Round 1 measured the pipeline's components but never ran the recommended
//! pipeline end to end; the ~3.5 minute projection for this corpus is arithmetic
//! on per-window costs, not an observed run. This test produces the observed
//! numbers.

use std::path::PathBuf;
use std::time::Instant;

use rcdvlog_core::{Catalog, TranscriptStatus};

#[test]
#[ignore = "requires downloaded models and a real project with proxies"]
fn transcribes_a_real_project_and_reports_its_gate_ratios() {
    let fixture = std::env::var("RCDVLOG_STT_FIXTURE")
        .expect("set RCDVLOG_STT_FIXTURE to a project folder containing catalog.sqlite3");
    let catalog_path = PathBuf::from(&fixture).join("catalog.sqlite3");
    let catalog = Catalog::open(catalog_path).expect("open catalog");
    let projects = catalog.list_projects().expect("list projects");
    let project = projects.first().expect("fixture has at least one project");

    let before = catalog
        .transcript_coverage(&project.id)
        .expect("coverage before");
    println!("clips in project: {}", before.total);
    println!("without a current transcript: {}", before.missing);

    let started = Instant::now();
    let report = catalog
        .transcribe_missing(&project.id)
        .expect("transcribe project");
    let elapsed = started.elapsed();

    let counted = |wanted: TranscriptStatus| {
        report
            .outcomes
            .iter()
            .filter(|outcome| outcome.status == wanted)
            .count()
    };
    let windows: u32 = report
        .outcomes
        .iter()
        .map(|outcome| outcome.window_count)
        .sum();
    let speech_seconds: f64 = report
        .outcomes
        .iter()
        .map(|outcome| outcome.speech_seconds)
        .sum();

    println!("--- observed ---");
    println!("transcribed:  {}", report.transcribed);
    println!("skipped:      {}", report.skipped);
    println!("failed:       {}", report.failures.len());
    println!("speech:       {}", counted(TranscriptStatus::Speech));
    println!("no_speech:    {}", counted(TranscriptStatus::NoSpeech));
    println!("rejected:     {}", counted(TranscriptStatus::Rejected));
    println!("windows decoded: {windows}");
    println!("speech audio:    {speech_seconds:.1}s");
    println!("wall time:       {elapsed:?}");
    for failure in report.failures.iter().take(5) {
        println!("failure: {} -> {}", failure.path, failure.error);
    }

    assert!(
        report.transcribed > 0 || report.skipped > 0,
        "fixture produced no work at all"
    );
    // Round 1 measured 176 of 273 clips with zero speech on this corpus. A run
    // where nothing is gated means the VAD gate is not working, which is the one
    // failure that silently costs both money and correctness.
    if report.transcribed > 0 {
        assert!(
            counted(TranscriptStatus::NoSpeech) > 0,
            "VAD gated nothing across {} clips — the correctness gate is not working",
            report.transcribed
        );
    }
}
