//! Bounds checks for calibration requests and resource overrides.

use super::*;

pub(super) fn validate_trials(trials: u32) -> Result<()> {
    ensure!(
        (1..=MAX_CALIBRATION_TRIALS).contains(&trials),
        "invalid input: calibration trials must be 1..={MAX_CALIBRATION_TRIALS}"
    );
    Ok(())
}

pub(super) fn validate_generated_duration(seconds: u32) -> Result<()> {
    ensure!(
        (1..=MAX_GENERATED_CALIBRATION_SECONDS).contains(&seconds),
        "invalid input: generated media duration must be 1..={MAX_GENERATED_CALIBRATION_SECONDS} seconds"
    );
    Ok(())
}

pub(super) fn validate_threads(values: &[usize], label: &str) -> Result<()> {
    ensure!(
        !values.is_empty(),
        "invalid input: at least one {label} candidate is required"
    );
    ensure!(
        values
            .iter()
            .all(|value| (1..=MAX_CALIBRATION_THREADS).contains(value)),
        "invalid input: {label} must be 1..={MAX_CALIBRATION_THREADS}"
    );
    Ok(())
}

pub(super) fn validate_worker_counts(values: &[usize]) -> Result<()> {
    ensure!(
        !values.is_empty(),
        "invalid input: at least one worker-count candidate is required"
    );
    ensure!(
        values
            .iter()
            .all(|value| (1..=MAX_CALIBRATION_WORKERS).contains(value)),
        "invalid input: worker counts must be 1..={MAX_CALIBRATION_WORKERS}"
    );
    Ok(())
}

pub(super) fn validate_candidate_budget(candidate_count: usize, trials: u32) -> Result<()> {
    ensure!(
        candidate_count > 0 && candidate_count <= MAX_CALIBRATION_CANDIDATES,
        "invalid input: calibration candidate count must be 1..={MAX_CALIBRATION_CANDIDATES}, got {candidate_count}"
    );
    let total_trials = candidate_count.saturating_mul(trials as usize);
    ensure!(
        total_trials <= MAX_CALIBRATION_TOTAL_TRIALS,
        "invalid input: calibration would run {total_trials} trials; the bounded maximum is {MAX_CALIBRATION_TOTAL_TRIALS}"
    );
    Ok(())
}

pub(super) fn validate_speech_decode_threads(values: &[usize]) -> Result<()> {
    ensure!(
        values == [DEFAULT_DECODE_THREADS],
        "invalid input: VAD calibration measures detection only; decodeThreads must be exactly [{DEFAULT_DECODE_THREADS}] (decode tuning requires an explicit uncalibrated override)"
    );
    Ok(())
}

pub(super) fn validate_representative_media_metadata(path: &Path) -> Result<u64> {
    ensure!(
        path.is_file(),
        "authorized representative media does not exist or is unreadable"
    );
    let bytes = fs::metadata(path)
        .with_context(|| "read representative calibration media metadata")?
        .len();
    ensure!(
        bytes > 0 && bytes <= MAX_REPRESENTATIVE_MEDIA_BYTES,
        "authorized representative media is {bytes} bytes; calibration is bounded to {MAX_REPRESENTATIVE_MEDIA_BYTES} bytes"
    );
    Ok(bytes)
}

pub(super) fn validate_representative_duration(duration_ms: u32) -> Result<()> {
    ensure!(
        duration_ms > 0 && duration_ms <= MAX_REPRESENTATIVE_MEDIA_DURATION_MS,
        "authorized representative media duration is {duration_ms}ms; calibration is bounded to {MAX_REPRESENTATIVE_MEDIA_DURATION_MS}ms"
    );
    Ok(())
}

/// Validate explicit resource settings without model inspection, catalog writes
/// or media work, including for no-work/no-audio fast paths.
pub(crate) fn validate_resource_overrides(
    representation_kind: &str,
    overrides: &ResourceOverrides,
) -> Result<()> {
    if let Some(value) = overrides.worker_count {
        ensure!(
            (1..=MAX_CALIBRATION_WORKERS).contains(&value),
            "invalid input: workerCount must be 1..={MAX_CALIBRATION_WORKERS}"
        );
        if representation_kind == SPEECH_REPRESENTATION_KIND {
            ensure!(
                value == 1,
                "invalid input: speech transcription is serial; workerCount > 1 is not supported and cannot be silently ignored"
            );
        }
    }
    if let Some(value) = overrides.vad_threads {
        ensure!(
            representation_kind == SPEECH_REPRESENTATION_KIND,
            "invalid input: vadThreads is only valid for speech calibration/transcription"
        );
        ensure!(
            (1..=MAX_CALIBRATION_THREADS).contains(&value),
            "invalid input: vadThreads must be 1..={MAX_CALIBRATION_THREADS}"
        );
    }
    if let Some(value) = overrides.decode_threads {
        ensure!(
            representation_kind == SPEECH_REPRESENTATION_KIND,
            "invalid input: decodeThreads is only valid for speech calibration/transcription"
        );
        ensure!(
            (1..=MAX_CALIBRATION_THREADS).contains(&value),
            "invalid input: decodeThreads must be 1..={MAX_CALIBRATION_THREADS}"
        );
    }
    if let Some(encoder) = overrides.encoder.as_deref() {
        ensure!(
            representation_kind == PROXY_REPRESENTATION_KIND,
            "invalid input: encoder is only valid for proxy resource selection"
        );
        // Only an encoder the domain can actually force is accepted. The
        // current proxy domain deliberately has no force-hardware policy:
        // accepting `vaapi`/`videotoolbox` here would let it fall back while
        // still claiming the override was honored.
        ensure!(
            encoder == "software",
            "invalid input: explicit encoder override {encoder:?} is unsupported; only software can be forced (use policy auto for bounded hardware probing)"
        );
    }
    Ok(())
}
