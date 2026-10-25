//! Bounded native pilot DTOs and evaluated-result constructors.

use super::*;

/// The only admitted coverage declaration for a paid pilot sample. A partial
/// packed window can never be promoted to full-asset transcript coverage here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativePilotSampleCoverage {
    PartialWindow,
}

/// A truthful local VAD outcome. This is not an observed provider capability,
/// accepted speech, or evidence that the cloud endpoint accepted the asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativePilotLocalOutcome {
    NoAudio,
    NoSpeech,
}

impl NativePilotLocalOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoAudio => "no_audio",
            Self::NoSpeech => "no_speech",
        }
    }
}

/// One distinct, exact policy-scoped local silence/audio-absence fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativePilotLocalOutcomeFact {
    pub scope: RunAssetScope,
    pub recipe_version: String,
    pub outcome: NativePilotLocalOutcome,
}

impl NativePilotLocalOutcomeFact {
    /// Build a local fact from the existing prepared representation. A prepared
    /// speech asset returns `None`; only the two truthful local terminal outcomes
    /// become evidence.
    #[allow(dead_code)] // Consumed by the next native runtime slice.
    pub(crate) fn from_preparation(preparation: &NativeSpeechPreparation) -> Result<Option<Self>> {
        let outcome = match preparation.outcome() {
            NativeSpeechLocalOutcome::NoAudio => NativePilotLocalOutcome::NoAudio,
            NativeSpeechLocalOutcome::NoSpeech => NativePilotLocalOutcome::NoSpeech,
            NativeSpeechLocalOutcome::PreparedSpeech => return Ok(None),
        };
        let evidence = preparation.evidence();
        let fact = Self {
            scope: evidence.scope.clone(),
            recipe_version: evidence.recipe.clone(),
            outcome,
        };
        fact.validate()?;
        Ok(Some(fact))
    }

    pub fn asset_id(&self) -> &str {
        &self.scope.asset_id
    }

    pub fn validate(&self) -> Result<()> {
        validate_scope_structure(&self.scope)?;
        ensure!(
            !self.recipe_version.trim().is_empty()
                && self.recipe_version.trim() == self.recipe_version,
            "native pilot local fact recipe version is missing or malformed"
        );
        Ok(())
    }
}

/// The parsed successful provider result, or a body-free typed protocol failure.
///
/// Candidate text is present only for accepted/review-required quality output;
/// rejected text is already discarded by the pure evaluator and is never copied
/// into this evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativePilotSampleOutcome {
    Evaluated {
        #[serde(default)]
        reported_language: Option<String>,
        quality: NativeSttWindowQualityObservation,
        #[serde(default)]
        candidates: Vec<DecodedSegment>,
    },
    ProtocolUnusable {
        protocol_error: NativeSttProtocolError,
    },
}

/// Exactly one paid, bounded native pilot window and its exact durable request
/// identity. Receipts, cost, actual serving provider/model and uncertainty are
/// intentionally absent; those remain read-only facts in the accounting ledger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativePilotPaidSample {
    pub coverage: NativePilotSampleCoverage,
    pub scope: RunAssetScope,
    pub requested_provider: String,
    pub requested_model: String,
    pub recipe_version: String,
    pub quality_recipe_version: String,
    pub request_key: String,
    pub payload_fingerprint: String,
    pub window: PackedWindow,
    pub latency_ms: u64,
    pub outcome: NativePilotSampleOutcome,
}

impl NativePilotPaidSample {
    /// Build the cacheable typed result from the one already accounted native
    /// result and the exact request/window that produced it. No chat request,
    /// token estimate or fabricated serving identity is constructed.
    pub fn from_accounted_result(
        policy: &RunPolicy,
        scope: &RunAssetScope,
        request: &NativeSttRequest,
        audio: &NativeSttAudioWindow,
        result: &NativeSttAccountedResult,
    ) -> Result<Self> {
        ensure!(
            policy.kind == AnalysisRunKind::NativeSpeechTranscription
                && policy.payload == AnalysisPayload::Audio,
            "native pilot sample requires an audio-only native speech policy"
        );
        ensure!(
            policy
                .scope
                .iter()
                .filter(|candidate| candidate == &scope)
                .count()
                == 1,
            "native pilot sample scope must be one exact frozen policy entry"
        );
        ensure_provider_request_matches_policy(policy, request)?;
        ensure!(
            !result.request_key.trim().is_empty(),
            "native pilot sample requires the accounting request key"
        );
        let payload_fingerprint =
            crate::analysis::accounting::analysis_native_stt_payload_fingerprint(request, audio)?;
        let outcome = match result.outcome() {
            NativeSttOutcome::Usable(transcript) => {
                let evaluation = evaluate_native_stt_quality(transcript, audio);
                let quality = NativeSttWindowQualityObservation::from_evaluation(&evaluation)?;
                let candidates = if evaluation.decision == NativeSttQualityDecision::Rejected {
                    Vec::new()
                } else {
                    evaluation.segments.clone()
                };
                NativePilotSampleOutcome::Evaluated {
                    reported_language: evaluation.reported_language.clone(),
                    quality,
                    candidates,
                }
            }
            NativeSttOutcome::Unusable(protocol_error) => {
                NativePilotSampleOutcome::ProtocolUnusable {
                    protocol_error: *protocol_error,
                }
            }
        };
        let sample = Self {
            coverage: NativePilotSampleCoverage::PartialWindow,
            scope: scope.clone(),
            requested_provider: request.provider().to_string(),
            requested_model: request.model().to_string(),
            recipe_version: policy.recipe_version.clone(),
            quality_recipe_version: NATIVE_STT_QUALITY_RECIPE_VERSION.to_string(),
            request_key: result.request_key.clone(),
            payload_fingerprint,
            window: audio.window().clone(),
            latency_ms: result.latency_ms(),
            outcome,
        };
        sample.validate()?;
        Ok(sample)
    }

    pub fn request_key(&self) -> &str {
        &self.request_key
    }

    pub fn latency_ms(&self) -> u64 {
        self.latency_ms
    }

    /// Reuse the existing window-observation DTO for a quality-evaluated sample.
    /// Protocol-unusable output has no quality observation and returns `None`.
    pub fn as_window_observation(&self) -> Option<NativeSttWindowObservation> {
        match &self.outcome {
            NativePilotSampleOutcome::Evaluated {
                reported_language,
                quality,
                ..
            } => Some(NativeSttWindowObservation {
                request_key: self.request_key.clone(),
                latency_ms: self.latency_ms,
                reported_language: reported_language.clone(),
                quality: quality.clone(),
            }),
            NativePilotSampleOutcome::ProtocolUnusable { .. } => None,
        }
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.coverage == NativePilotSampleCoverage::PartialWindow,
            "native pilot sample must be explicitly a partial window"
        );
        validate_scope_structure(&self.scope)?;
        ensure!(
            !self.requested_provider.trim().is_empty()
                && self.requested_provider.trim() == self.requested_provider
                && !self.requested_model.trim().is_empty()
                && self.requested_model.trim() == self.requested_model,
            "native pilot sample requested provider/model is missing or malformed"
        );
        ensure!(
            !self.recipe_version.trim().is_empty()
                && self.recipe_version.trim() == self.recipe_version
                && self.quality_recipe_version == NATIVE_STT_QUALITY_RECIPE_VERSION,
            "native pilot sample recipe/quality version is missing or unsupported"
        );
        validate_request_key(&self.request_key)?;
        validate_payload_fingerprint(&self.payload_fingerprint)?;
        validate_window_mapping(&self.window, self.scope.source_duration_ms)?;
        ensure!(
            i64::try_from(self.latency_ms).is_ok(),
            "native pilot sample latency does not fit canonical SQLite storage"
        );

        match &self.outcome {
            NativePilotSampleOutcome::Evaluated {
                reported_language,
                quality,
                candidates,
            } => {
                self.as_window_observation()
                    .expect("evaluated observation")
                    .validate()?;
                validate_candidates(reported_language, quality, candidates, &self.window)?;
            }
            NativePilotSampleOutcome::ProtocolUnusable { .. } => {}
        }
        Ok(())
    }
}

/// Bounded pilot evidence carried by the existing pilot JSON owner. `None` at
/// the record level preserves the historical VLM wire shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativePilotEvidence {
    pub schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample: Option<NativePilotPaidSample>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_outcomes: Vec<NativePilotLocalOutcomeFact>,
}

impl NativePilotEvidence {
    pub fn with_sample(sample: NativePilotPaidSample) -> Self {
        Self {
            schema_version: NATIVE_PILOT_EVIDENCE_SCHEMA_VERSION.to_string(),
            sample: Some(sample),
            local_outcomes: Vec::new(),
        }
    }

    pub fn with_local_outcomes(local_outcomes: Vec<NativePilotLocalOutcomeFact>) -> Self {
        Self {
            schema_version: NATIVE_PILOT_EVIDENCE_SCHEMA_VERSION.to_string(),
            sample: None,
            local_outcomes,
        }
    }

    pub(super) fn empty() -> Self {
        Self {
            schema_version: NATIVE_PILOT_EVIDENCE_SCHEMA_VERSION.to_string(),
            sample: None,
            local_outcomes: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.sample.is_none() && self.local_outcomes.is_empty()
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == NATIVE_PILOT_EVIDENCE_SCHEMA_VERSION,
            "native pilot evidence schema version is unsupported"
        );
        ensure!(
            self.local_outcomes.len() <= MAX_NATIVE_PILOT_LOCAL_OUTCOMES,
            "native pilot local outcome list exceeds the selected-scope bound"
        );
        ensure!(
            !self.is_empty(),
            "native pilot evidence must contain one paid sample or bounded local facts"
        );
        if let Some(sample) = &self.sample {
            sample.validate()?;
        }
        let mut asset_ids = HashSet::new();
        let mut source_identities = HashSet::new();
        for (index, fact) in self.local_outcomes.iter().enumerate() {
            fact.validate()
                .with_context(|| format!("validate native pilot local fact {index}"))?;
            ensure!(
                asset_ids.insert(fact.scope.asset_id.clone()),
                "native pilot local facts contain a duplicate asset"
            );
            ensure!(
                source_identities.insert(fact.scope.source_identity.clone()),
                "native pilot local facts contain a duplicate source identity"
            );
            if let Some(sample) = &self.sample {
                ensure!(
                    fact.scope.asset_id != sample.scope.asset_id,
                    "native pilot sample asset cannot also be a local NoAudio/NoSpeech fact"
                );
            }
        }
        Ok(())
    }
}

impl RunPilotRecord {
    /// Historical inspection keeps recorded facts even after a policy/source
    /// change. Reuse callers must still run the currentness proof below before
    /// treating any recorded sample as current.
    pub fn native_pilot_evidence(&self) -> Option<&NativePilotEvidence> {
        self.native_evidence.as_ref()
    }
}
