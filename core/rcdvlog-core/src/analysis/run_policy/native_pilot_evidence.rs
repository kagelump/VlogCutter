//! Bounded native capability-pilot evidence.
//!
//! A native pilot may pay for exactly one bounded speech window before bulk
//! work. This module retains that parsed window, or a body-free typed unusable
//! outcome, inside the existing `analysis_run_pilots.record_json` owner. It also
//! retains truthful local NoAudio/NoSpeech facts for a future deterministic
//! scope scan.
//!
//! It deliberately does not execute providers, scan assets, write a canonical
//! full-asset transcript, own a receipt/cost balance or create a second request
//! identity. Accounting remains the only owner of financial and serving-identity
//! truth. All SQL below runs on the caller's existing connection so the
//! compare-and-write boundary is one transaction.

use std::collections::HashSet;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::Catalog;
use crate::analysis::accounting::NativeSttAccountedResult;
use crate::analysis::accounting::store::NativeSettledResultProof;
use crate::analysis::openrouter_stt::{
    NativeSttAudioWindow, NativeSttOutcome, NativeSttProtocolError, NativeSttRequest,
    validate_iso639_1,
};
use crate::analysis::quality::DecodedSegment;
use crate::analysis::quality::native_stt::{
    NATIVE_STT_QUALITY_RECIPE_VERSION, NativeSttQualityDecision, NativeSttQualityReason,
    evaluate_native_stt_quality, normalized_native_stt_language,
};
use crate::analysis::store::{
    NativeSttWindowObservation, NativeSttWindowQualityObservation, native_stt_transcript_recipe,
};
use crate::analysis::windows::{PackedWindow, WINDOW_MS};
use crate::transcription::{NativeSpeechLocalOutcome, NativeSpeechPreparation};

use super::{
    AnalysisPayload, AnalysisRunKind, RunAssetScope, RunPilotRecord, RunPolicy,
    RunPolicyParameters, load_run_authorization_on_connection,
};

pub const NATIVE_PILOT_EVIDENCE_SCHEMA_VERSION: &str = "analysis.native-pilot-evidence.v1";
/// The selected policy scope is already bounded by `MAX_SCOPE_ASSETS`; local
/// facts may not exceed that existing bound.
pub const MAX_NATIVE_PILOT_LOCAL_OUTCOMES: usize = super::MAX_SCOPE_ASSETS;
const MAX_NATIVE_PILOT_CANDIDATES: usize = 10_000;
const MAX_NATIVE_PILOT_CANDIDATE_TEXT_BYTES: usize = 64 * 1024;
const MAX_NATIVE_PILOT_REQUEST_KEY_BYTES: usize = 512;

mod persistence;
mod types;
mod validation;

pub use types::*;
use validation::*;
