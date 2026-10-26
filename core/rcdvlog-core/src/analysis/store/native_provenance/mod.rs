//! Canonical native-speech provenance attached to the transcript owner.
//!
//! The transcript header remains the artifact owner. These modules add only the
//! missing relationship between one transcript and the exact accounted provider
//! windows that produced it, plus a read projection over the same accounting
//! ledger. No provider I/O, model loading, media access or financial write lives
//! here.

mod checkpoint;
mod plan;
pub(crate) mod read;
mod types;
mod validation;
mod write;

pub use types::{
    NATIVE_TRANSCRIPT_PROVENANCE_SCHEMA_VERSION, NATIVE_TRANSCRIPT_WINDOW_PLAN_SCHEMA_VERSION,
    NativeSttWindowObservation, NativeSttWindowQualityObservation, NativeTranscriptCoverage,
    NativeTranscriptProvenance, NativeTranscriptWindowPlan, NativeTranscriptWindowProvenance,
};

#[allow(unused_imports)]
pub(crate) use checkpoint::{
    EvaluatedNativeCheckpointWindow, NativeTranscriptCheckpoint, NativeTranscriptCheckpointReuse,
    NativeTranscriptCheckpointWindow, NativeTranscriptCheckpointWindowInput,
    NativeTranscriptWindowReuse, append_native_transcript_checkpoint,
    begin_native_transcript_checkpoint, evaluate_native_checkpoint_result,
    load_native_transcript_checkpoint, probe_native_transcript_checkpoint_for_reuse,
};
pub(crate) use write::save_native_transcript;

#[cfg(test)]
mod tests;
