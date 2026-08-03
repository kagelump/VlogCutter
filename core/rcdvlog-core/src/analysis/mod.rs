//! Derived per-clip analysis: local speech-to-text today, cloud vision later.
//!
//! Results are derived artifacts with recipe identity, in the same style as
//! proxies and thumbnails: re-running an unchanged recipe against an unchanged
//! source is idempotent, and a changed recipe makes stored results detectably
//! stale rather than silently mixed.

pub mod audio;
pub mod coverage;
pub mod engine;
pub mod quality;
pub mod store;
pub mod stt;
pub mod whisper_engine;
pub mod windows;

/// Identity of the speech-to-text recipe. Changing any component of the
/// pipeline — model, quantization, VAD model, or window strategy — requires a
/// new key so existing transcripts become detectably stale.
pub const STT_RECIPE_KEY: &str = "stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1";

/// Batch-estimator task key. Suffixed with the worker count because throughput
/// is not comparable across concurrency levels.
pub const STT_TASK_KEY: &str = "stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1-w1";
