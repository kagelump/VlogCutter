//! Prompt construction and provider-independent vision orchestration.
//!
//! This module owns no transport and no catalog. It fuses only accepted speech,
//! builds a bounded request, applies the provider chain, and domain-validates
//! before a caller is allowed to persist anything.

use anyhow::Result;
use serde::Serialize;

use crate::analysis::provider::{ProviderChain, VlmProvider, VlmRequest, VlmResponse};
use crate::analysis::schema::{ClipObservation, output_schema, validate};
use crate::analysis::store::TranscriptStatus;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTranscript {
    pub status: TranscriptStatus,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptContext {
    pub frame_timestamps_ms: Vec<u32>,
    pub clip_duration_ms: u32,
    pub transcript: Option<PromptTranscript>,
    pub vocabulary: Vec<String>,
    pub trip_context: Option<String>,
}

pub fn build_prompt(context: &PromptContext) -> String {
    let timestamps = context
        .frame_timestamps_ms
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let vocabulary = if context.vocabulary.is_empty() {
        "(none yet)".to_string()
    } else {
        context.vocabulary.join(", ")
    };
    let transcript = context
        .transcript
        .as_ref()
        .filter(|transcript| transcript.status == TranscriptStatus::Speech)
        .map(|transcript| transcript.text.trim())
        .filter(|text| !text.is_empty())
        .map(|text| {
            format!("\nAccepted speech transcript (supporting context, not visual fact):\n{text}\n")
        })
        .unwrap_or_default();
    let trip = context
        .trip_context
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!("\nExplicit trip context: {value}\n"))
        .unwrap_or_default();

    format!(
        "Observe this video clip from the supplied frames. Return only the requested JSON.\n\
         Frame timestamps in order (milliseconds): [{timestamps}]. Clip duration: {} ms.\n\
         Write title and description in English. Describe only visible content; never infer a \
         landmark or event identity. OCR text must be literally readable—omit uncertain text.\n\
         Prefer this existing project label vocabulary: {vocabulary}. Use a new concise label \
         only when none fits.\n\
         The unusable escape hatch is required: set usable=false and give an unusableReason \
         when frames are black, blank, noise, heavily blurred, overexposed, or otherwise carry \
         no usable visual content. In that case use empty labels and OCR and null moments.\n\
         thumbnailTimestampMs must be null or exactly one supplied timestamp. Best-moment \
         bounds must lie inside the clip.{transcript}{trip}",
        context.clip_duration_ms
    )
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisOutcome {
    pub observation: Option<ClipObservation>,
    pub reject_reason: String,
    pub model: String,
    pub attempts: u32,
    pub fell_back: bool,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cost_usd: f64,
    pub latency_ms: u64,
}

fn rejected(
    response: &VlmResponse,
    attempts: u32,
    fell_back: bool,
    reason: String,
) -> AnalysisOutcome {
    AnalysisOutcome {
        observation: None,
        reject_reason: reason,
        model: response.model.clone(),
        attempts,
        fell_back,
        prompt_tokens: response.prompt_tokens,
        completion_tokens: response.completion_tokens,
        cost_usd: response.cost_usd,
        latency_ms: response.latency_ms,
    }
}

/// Runs one bounded request. A domain-invalid default response is retried once
/// on the fallback just like a transport failure and counts toward the breaker.
pub fn analyze_clip(
    provider: &dyn VlmProvider,
    chain: &ProviderChain,
    context: &PromptContext,
    frames: Vec<String>,
) -> Result<AnalysisOutcome> {
    let request = VlmRequest {
        prompt: build_prompt(context),
        frames,
        schema: output_schema(&context.frame_timestamps_ms, context.clip_duration_ms),
    };
    let first = chain.analyze(provider, &request)?;
    match validate(
        &first.response.body,
        &context.frame_timestamps_ms,
        context.clip_duration_ms,
    ) {
        Ok(observation) => Ok(AnalysisOutcome {
            observation: Some(observation),
            reject_reason: String::new(),
            model: first.response.model,
            attempts: first.attempts,
            fell_back: first.fell_back,
            prompt_tokens: first.response.prompt_tokens,
            completion_tokens: first.response.completion_tokens,
            cost_usd: first.response.cost_usd,
            latency_ms: first.response.latency_ms,
        }),
        Err(error)
            if chain.fallback_model().is_some()
                && chain.fallback_model() != Some(first.response.model.as_str()) =>
        {
            chain.record_default_failure();
            let fallback_model = chain
                .fallback_model()
                .expect("guard checked fallback policy");
            let fallback = provider.analyze(fallback_model, &request)?;
            let aggregate = |fallback: &VlmResponse| {
                (
                    first
                        .response
                        .prompt_tokens
                        .saturating_add(fallback.prompt_tokens),
                    first
                        .response
                        .completion_tokens
                        .saturating_add(fallback.completion_tokens),
                    first.response.cost_usd + fallback.cost_usd,
                    first
                        .response
                        .latency_ms
                        .saturating_add(fallback.latency_ms),
                )
            };
            match validate(
                &fallback.body,
                &context.frame_timestamps_ms,
                context.clip_duration_ms,
            ) {
                Ok(observation) => {
                    let usage = aggregate(&fallback);
                    Ok(AnalysisOutcome {
                        observation: Some(observation),
                        reject_reason: String::new(),
                        model: fallback.model,
                        attempts: first.attempts + 1,
                        fell_back: true,
                        prompt_tokens: usage.0,
                        completion_tokens: usage.1,
                        cost_usd: usage.2,
                        latency_ms: usage.3,
                    })
                }
                Err(fallback_error) => {
                    let usage = aggregate(&fallback);
                    let mut outcome = rejected(
                        &fallback,
                        first.attempts + 1,
                        true,
                        format!(
                            "default: {}; fallback: {}",
                            error.as_reason(),
                            fallback_error.as_reason()
                        ),
                    );
                    outcome.prompt_tokens = usage.0;
                    outcome.completion_tokens = usage.1;
                    outcome.cost_usd = usage.2;
                    outcome.latency_ms = usage.3;
                    Ok(outcome)
                }
            }
        }
        Err(error) => Ok(rejected(
            &first.response,
            first.attempts,
            first.fell_back,
            error.as_reason(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::provider::FALLBACK_MODEL;
    use anyhow::bail;
    use std::sync::Mutex;

    fn valid() -> String {
        r#"{"title":"Expo hall","description":"People walk through an exhibition hall.","labels":["exhibition"],"usable":true,"unusableReason":null,"bestMomentStartMs":0,"bestMomentEndMs":900,"thumbnailTimestampMs":400,"cameraMovement":"handheld","ocrText":[]}"#.into()
    }

    fn context(status: TranscriptStatus) -> PromptContext {
        PromptContext {
            frame_timestamps_ms: vec![400],
            clip_duration_ms: 1_000,
            transcript: Some(PromptTranscript {
                status,
                text: "spoken secret phrase".into(),
            }),
            vocabulary: vec!["exhibition".into()],
            trip_context: None,
        }
    }

    struct Fake {
        bodies: Mutex<Vec<String>>,
        calls: Mutex<Vec<String>>,
    }
    impl VlmProvider for Fake {
        fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
            self.calls.lock().unwrap().push(model.into());
            let mut body = self.bodies.lock().unwrap();
            if body.is_empty() {
                bail!("no scripted response")
            }
            let response = body.remove(0);
            Ok(VlmResponse {
                model: model.into(),
                body: response,
                prompt_tokens: 10,
                completion_tokens: 2,
                cost_usd: 0.001,
                latency_ms: 5,
            })
        }
    }

    #[test]
    fn a_speech_transcript_is_included_in_the_prompt() {
        assert!(build_prompt(&context(TranscriptStatus::Speech)).contains("spoken secret phrase"));
    }

    #[test]
    fn a_rejected_transcript_is_never_included() {
        assert!(
            !build_prompt(&context(TranscriptStatus::Rejected)).contains("spoken secret phrase")
        );
    }

    #[test]
    fn the_existing_vocabulary_is_offered_to_the_model() {
        assert!(build_prompt(&context(TranscriptStatus::NoSpeech)).contains("exhibition"));
    }

    #[test]
    fn the_prompt_keeps_the_unusable_escape_hatch() {
        assert!(build_prompt(&context(TranscriptStatus::NoSpeech)).contains("unusable"));
    }

    #[test]
    fn a_domain_invalid_default_response_retries_the_fallback() {
        let fake = Fake {
            bodies: Mutex::new(vec!["{}".into(), valid()]),
            calls: Mutex::default(),
        };
        let outcome = analyze_clip(
            &fake,
            &ProviderChain::new(),
            &context(TranscriptStatus::NoSpeech),
            vec![],
        )
        .unwrap();
        assert!(outcome.observation.is_some());
        assert_eq!(outcome.model, FALLBACK_MODEL);
        assert_eq!(outcome.attempts, 2);
        assert_eq!(outcome.cost_usd, 0.002);
    }

    #[test]
    fn invalid_responses_are_returned_as_rejected_with_their_reason() {
        let fake = Fake {
            bodies: Mutex::new(vec!["{".into(), "{".into()]),
            calls: Mutex::default(),
        };
        let outcome = analyze_clip(
            &fake,
            &ProviderChain::new(),
            &context(TranscriptStatus::NoSpeech),
            vec![],
        )
        .unwrap();
        assert!(outcome.observation.is_none());
        assert!(outcome.reject_reason.contains("truncated"));
    }
}
