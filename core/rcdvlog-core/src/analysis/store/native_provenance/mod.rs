//! Canonical native-speech provenance attached to the transcript owner.
//!
//! The transcript header remains the artifact owner. These modules add only the
//! missing relationship between one transcript and the exact accounted provider
//! windows that produced it, plus a read projection over the same accounting
//! ledger. No provider I/O, model loading, media access or financial write lives
//! here.

pub(crate) mod read;
mod types;
mod validation;
mod write;

pub use types::{
    NATIVE_TRANSCRIPT_PROVENANCE_SCHEMA_VERSION, NativeSttWindowObservation,
    NativeSttWindowQualityObservation, NativeTranscriptProvenance,
    NativeTranscriptWindowProvenance,
};

pub(crate) use write::save_native_transcript;

#[cfg(test)]
mod tests;
