//! Derived per-clip analysis: local speech-to-text today, cloud vision later.
//!
//! Results are derived artifacts with recipe identity, in the same style as
//! proxies and thumbnails: re-running an unchanged recipe against an unchanged
//! source is idempotent, and a changed recipe makes stored results detectably
//! stale rather than silently mixed.

pub mod audio;
pub mod chronology_suggestions;
pub mod consolidate;
pub mod coverage;
pub mod engine;
pub mod framepack;
pub mod frames;
pub mod moments;
pub mod openrouter;
pub mod provider;
pub mod quality;
pub mod run_policy;
pub mod schema;
pub mod store;
pub mod stt;
pub mod vlm;
pub mod vlm_store;
pub mod whisper_engine;
pub mod windows;

/// Identity of the speech-to-text recipe. Changing any component of the
/// pipeline — model, quantization, VAD model, or window strategy — requires a
/// new key so existing transcripts become detectably stale.
pub const STT_RECIPE_KEY: &str = "stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1";

/// Batch-estimator task key. Suffixed with the worker count because throughput
/// is not comparable across concurrency levels.
pub const STT_TASK_KEY: &str = "stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1-w1";

/// Identity of the frame-pack recipe. Always qualified at use by the
/// representation and its fingerprint: the same recipe over a proxy and over the
/// original selects different frames, because sharpness and motion are
/// resolution- and codec-dependent.
pub const FRAMEPACK_RECIPE_KEY: &str =
    "framepack.2fps.skip400ms.stride-max40.win3-8.sharp-bright-motion.dhash8-d5.v2";

/// Version of the vision prompt. Bumping it makes stored analyses stale.
pub const VLM_PROMPT_VERSION: &str = "vlm.observe.v2";
