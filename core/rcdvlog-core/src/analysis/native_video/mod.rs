//! Bounded direct native-video transport over the shared run-policy/accounting owners.
pub(crate) mod cache;
mod clock;
mod contract;
mod reply;
pub(crate) use contract::{cost_bound, credential_present, tool_fingerprint, validate_options};
mod media;
pub use media::{NativeVideoMedia, NativeVideoMediaView};
mod model;
pub use model::*;
pub(crate) mod moment_export;
pub(crate) mod protocol;
mod queue;
pub(crate) mod runtime;
mod status;
pub(crate) mod transport;
pub use runtime::plan_native_video_windows;
pub const NATIVE_VIDEO_PROVIDER: &str = "gemini-direct";
pub const NATIVE_VIDEO_MODEL: &str = "gemini-3.5-flash-lite";
pub const NATIVE_VIDEO_MEDIA_RECIPE: &str =
    "native-video.source-pts.oriented-h264-24fps.pcm48-aac-explicit.v1";
pub const NATIVE_VIDEO_PROMPT_VERSION: &str = "native-video.local-clock-locators.v1";
pub const NATIVE_VIDEO_SCHEMA_VERSION: &str = "native-video.bounded-moments-review.v1";

#[cfg(test)]
#[path = "accounting_tests.rs"]
mod accounting_tests;

#[cfg(test)]
#[path = "queue_tests.rs"]
mod queue_tests;
