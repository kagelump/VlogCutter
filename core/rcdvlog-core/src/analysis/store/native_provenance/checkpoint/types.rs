use super::*;

/// Which existing canonical header a checkpoint start may replace.
///
/// The default resume-only intent can only reopen an identical native
/// checkpoint. The explicit native-selection intent exists solely for an
/// adapter that explicitly selected this backend over ordinary local derived
/// output. It may replace free local output but never any native policy, quality,
/// plan, or paid-window association.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeTranscriptCheckpointStart {
    ResumeOnly,
    ExplicitNativeSelection,
}

/// One evaluated provider window retained in the checkpoint projection.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NativeTranscriptCheckpointWindow {
    pub window_index: u32,
    pub observation: NativeSttWindowObservation,
    pub candidates: Vec<DecodedSegment>,
}

/// Snapshot of one incomplete native checkpoint. `windows` contains only the
/// contiguous completed prefix; `coverage` keeps plan length distinct from paid
/// result count.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NativeTranscriptCheckpoint {
    pub asset_id: String,
    pub status: TranscriptStatus,
    pub outcome_reason: TranscriptOutcomeReason,
    pub coverage: NativeTranscriptCoverage,
    pub plan: NativeTranscriptWindowPlan,
    pub windows: Vec<NativeTranscriptCheckpointWindow>,
}

/// The internal forms a paid window result can take at this owner.
///
/// `Evaluated` is for an already quality-evaluated observation plus its mapped
/// candidates. `AccountedResult` reuses the C2 evaluator from the exact raw
/// typed result. `PilotSample` can seed the first checkpoint from C7 without a
/// second transport call, but only when its exact payload/window identity is
/// proved against the live preparation/request/audio.
pub(crate) enum NativeTranscriptCheckpointWindowInput<'a> {
    Evaluated {
        request: &'a NativeSttRequest,
        audio: &'a NativeSttAudioWindow,
        observation: NativeSttWindowObservation,
        candidates: Vec<DecodedSegment>,
    },
    AccountedResult {
        request: &'a NativeSttRequest,
        audio: &'a NativeSttAudioWindow,
        result: &'a NativeSttAccountedResult,
    },
    PilotSample {
        request: &'a NativeSttRequest,
        audio: &'a NativeSttAudioWindow,
        sample: &'a crate::analysis::run_policy::NativePilotPaidSample,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EvaluatedNativeCheckpointWindow {
    pub observation: NativeSttWindowObservation,
    pub candidates: Vec<DecodedSegment>,
}

/// One plan entry's current reuse state against the live preparation/request.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NativeTranscriptWindowReuse {
    /// Exact typed result already retained for this plan index.
    Cached(NativeTranscriptCheckpointWindow),
    /// Genuinely unattempted after accounting release/no row. C9 does not send;
    /// a later bulk owner may treat this as a send candidate.
    Missing { window_index: u32 },
    /// A marker/active/dispatched/completed/uncertain ledger row exists but no
    /// typed checkpoint result does. Default replay must stop and the owning
    /// run must recover/reconcile the exact request first.
    RecoveryRequired {
        window_index: u32,
        request_key: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NativeTranscriptCheckpointReuse {
    pub checkpoint: NativeTranscriptCheckpoint,
    pub windows: Vec<NativeTranscriptWindowReuse>,
}
