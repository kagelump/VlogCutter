//! Derived per-clip analysis: local speech-to-text today, cloud vision later.
//!
//! Results are derived artifacts with recipe identity, in the same style as
//! proxies and thumbnails: re-running an unchanged recipe against an unchanged
//! source is idempotent, and a changed recipe makes stored results detectably
//! stale rather than silently mixed.

// The pipeline is assembled bottom-up: the pure-logic units below are built and
// tested before the orchestration that consumes them exists. This allowance is
// removed once `stt` wires them together.
#[allow(dead_code)]
pub mod audio;
#[allow(dead_code)]
pub mod coverage;
#[allow(dead_code)]
pub mod engine;
#[allow(dead_code)]
pub mod quality;
#[allow(dead_code)]
pub mod windows;

/// Identity of the speech-to-text recipe. Changing any component of the
/// pipeline — model, quantization, VAD model, or window strategy — requires a
/// new key so existing transcripts become detectably stale.
pub const STT_RECIPE_KEY: &str = "stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1";

/// Batch-estimator task key. Suffixed with the worker count because throughput
/// is not comparable across concurrency levels.
pub const STT_TASK_KEY: &str = "stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1-w1";
