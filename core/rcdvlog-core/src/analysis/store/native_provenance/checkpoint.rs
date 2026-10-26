//! Durable incomplete native speech-window checkpoints.
//!
//! This is a persistence boundary, not bulk orchestration. It starts a positive
//! VAD/packing plan on the existing transcript header and appends one contiguous
//! quality-evaluated paid window result at a time. Candidate text stays solely
//! in `transcript_segments`; request/cost/uncertainty truth stays solely in the
//! accounting ledger. No provider, model, media or credential work happens here.

// C9 exposes internal persistence/read primitives intentionally; production
// bulk/adapters are later reviewed slices. Tests exercise this boundary now.
#![allow(dead_code)]

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};

use crate::Catalog;
use crate::analysis::accounting::ANALYSIS_ACCOUNTING_SCHEMA_VERSION;
use crate::analysis::accounting::NativeSttAccountedResult;
use crate::analysis::accounting::analysis_native_stt_payload_fingerprint;
use crate::analysis::accounting::store as accounting_store;
use crate::analysis::accounting::store::NativeSettledResultProof;
use crate::analysis::openrouter_stt::{NativeSttAudioWindow, NativeSttOutcome, NativeSttRequest};
use crate::analysis::quality::DecodedSegment;
use crate::analysis::quality::native_stt::{
    NATIVE_STT_QUALITY_RECIPE_VERSION, NativeSttQualityDecision, NativeSttQualityEvaluation,
    NativeSttQualityReason, evaluate_native_stt_quality, normalized_native_stt_language,
};
use crate::analysis::run_policy::{
    AnalysisRunKind, RunAssetScope, RunPolicy, RunPolicyParameters,
    load_run_authorization_on_connection,
};
use crate::analysis::store::native_provenance::types::{
    NATIVE_TRANSCRIPT_WINDOW_PLAN_SCHEMA_VERSION, NativeSttWindowObservation,
    NativeSttWindowQualityObservation, NativeTranscriptCoverage, NativeTranscriptProvenance,
    NativeTranscriptWindowPlan,
};
use crate::analysis::store::native_stt_transcript_recipe;
use crate::transcription::NativeSpeechPreparationEvidence;
use crate::transcription::{NativeSpeechLocalOutcome, NativeSpeechPreparation};

use crate::analysis::store::native_provenance::plan::{
    plan_matches_windows, semantic_fingerprint, validate_plan,
};
use crate::analysis::store::native_provenance::read::load_native_transcript_provenance;
use crate::analysis::store::native_provenance::validation::validate_native_policy_recipe;
use crate::analysis::store::native_provenance::write::{
    insert_native_window_observation, validate_asset_scope,
};
use crate::analysis::store::{
    NativeTranscriptWindowPlanRow, StoredTranscript, TranscriptOutcomeReason, TranscriptStatus,
};

mod evaluation;
mod read;
mod reuse;
mod types;
mod validation;
mod write;

pub(crate) use evaluation::evaluate_native_checkpoint_result;
use evaluation::*;
pub(crate) use read::{checkpoint_from_provenance, load_native_transcript_checkpoint};
use read::{load_checkpoint_snapshot, load_transcript_header};
pub(crate) use reuse::probe_native_transcript_checkpoint_for_reuse;
pub(crate) use types::*;
use validation::*;
pub(crate) use write::{append_native_transcript_checkpoint, begin_native_transcript_checkpoint};

impl Catalog {
    /// Internal C9 entry point: open or idempotently return the current
    /// incomplete native checkpoint for one live prepared speech asset.
    pub(crate) fn begin_native_transcript_checkpoint(
        &self,
        preparation: &NativeSpeechPreparation,
    ) -> Result<NativeTranscriptCheckpoint> {
        begin_native_transcript_checkpoint(self, preparation)
    }

    /// Internal C9 entry point: append one exact quality-evaluated paid window.
    pub(crate) fn append_native_transcript_checkpoint(
        &self,
        preparation: &NativeSpeechPreparation,
        input: NativeTranscriptCheckpointWindowInput<'_>,
    ) -> Result<NativeTranscriptCheckpoint> {
        append_native_transcript_checkpoint(self, preparation, input)
    }

    /// Internal C9 read projection. Historical callers do not need today's
    /// tariff/readiness and therefore cannot mistake this for current reuse.
    pub(crate) fn native_transcript_checkpoint(
        &self,
        asset_id: &str,
    ) -> Result<Option<NativeTranscriptCheckpoint>> {
        load_native_transcript_checkpoint(self, asset_id)
    }

    /// Internal C9 current-reuse probe. No provider call is made.
    pub(crate) fn probe_native_transcript_checkpoint_reuse(
        &self,
        preparation: &NativeSpeechPreparation,
        request: &NativeSttRequest,
    ) -> Result<NativeTranscriptCheckpointReuse> {
        probe_native_transcript_checkpoint_for_reuse(self, preparation, request)
    }
}
