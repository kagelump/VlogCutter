//! Which model serves a request, and what happens when it does not.
//!
//! The default is the cheapest measured model. It is also the least reliable
//! one measured: 3 of 16 requests were upstream 429s and it emitted an
//! out-of-enum value under structured output. The fallback absorbs individual
//! failures; the circuit breaker absorbs a sustained outage, so a 273-clip batch
//! does not pay ten futile requests per clip.
//!
//! Provenance records the model that actually produced the stored result, so a
//! batch legitimately contains mixed provenance and that is visible rather than
//! hidden.

use anyhow::{Result, bail};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// Cheapest measured option: $0.03 for a 273-clip project.
pub const DEFAULT_MODEL: &str = "qwen/qwen3.7-flash";
/// Fastest and most reliable of the cheap tier: 100% first-attempt, p50 3.4 s,
/// $0.08 for the same project.
pub const FALLBACK_MODEL: &str = "google/gemini-2.5-flash-lite";
/// Sustained-failure threshold, per process.
pub const BREAKER_THRESHOLD: u32 = 10;

/// What a model costs and what it can be asked to do.
///
/// `image_tokens_per_frame` is **measured**, not derived from published prices:
/// identical images billed 1,235 to 6,026 prompt tokens across models, so a cost
/// model built from frames times price-per-token is wrong by up to 5x.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSpec {
    pub id: &'static str,
    pub image_tokens_per_frame: u32,
    /// Providers that reject `maxItems` cannot be used at all: bounded arrays
    /// are what make a repetition loop structurally inexpressible.
    pub supports_max_items: bool,
    /// Whether reasoning can be disabled. A model that burns reasoning tokens
    /// and returns empty content is unusable here.
    pub reasoning_disablable: bool,
}

pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        // ~2,175 prompt tokens over a median 5-frame pack.
        id: DEFAULT_MODEL,
        image_tokens_per_frame: 435,
        supports_max_items: true,
        reasoning_disablable: true,
    },
    ModelSpec {
        // ~1,235 prompt tokens over a median 5-frame pack; Google normalises
        // internally, so this is flat with respect to input width.
        id: FALLBACK_MODEL,
        image_tokens_per_frame: 247,
        supports_max_items: true,
        reasoning_disablable: true,
    },
];

pub fn model_spec(id: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|model| model.id == id)
}

/// One analysis request, independent of transport.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VlmRequest {
    pub prompt: String,
    /// Frames as `data:` URIs, already base64-encoded.
    pub frames: Vec<String>,
    pub schema: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VlmResponse {
    pub model: String,
    pub body: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cost_usd: f64,
    pub latency_ms: u64,
}

/// A transport that can run one request against one named model.
pub trait VlmProvider: Send + Sync {
    fn analyze(&self, model: &str, request: &VlmRequest) -> Result<VlmResponse>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChainOutcome {
    pub response: VlmResponse,
    /// How many models were tried, including the one that succeeded.
    pub attempts: u32,
    /// Whether the default model failed on this request.
    pub fell_back: bool,
    pub attempted_models: Vec<String>,
    /// False when a provider attempt failed without returning usage.
    pub usage_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderChainError {
    pub attempted_models: Vec<String>,
    pub message: String,
}

impl std::fmt::Display for ProviderChainError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProviderChainError {}

/// Default → fallback → breaker, for the lifetime of one process.
#[derive(Debug, Clone)]
pub struct ProviderChain {
    default_failures: Arc<AtomicU32>,
    primary_model: &'static str,
    allowed_models: Vec<&'static str>,
    allow_fallback: bool,
}

impl Default for ProviderChain {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderChain {
    pub fn new() -> Self {
        Self::with_policy(
            DEFAULT_MODEL,
            &[DEFAULT_MODEL.to_string(), FALLBACK_MODEL.to_string()],
            true,
        )
        .expect("built-in provider policy is valid")
    }

    /// Creates a run-scoped chain after validating all model choices locally.
    pub fn with_policy(primary: &str, allowed: &[String], allow_fallback: bool) -> Result<Self> {
        let primary =
            model_spec(primary).ok_or_else(|| anyhow::anyhow!("unsupported model: {primary}"))?;
        let allowed = allowed
            .iter()
            .map(|id| {
                model_spec(id).ok_or_else(|| anyhow::anyhow!("unsupported allowed model: {id}"))
            })
            .collect::<Result<Vec<_>>>()?;
        if !allowed.iter().any(|model| model.id == primary.id) {
            bail!(
                "primary model {} is not in the allowed model list",
                primary.id
            );
        }
        let fallback = if primary.id == DEFAULT_MODEL {
            FALLBACK_MODEL
        } else {
            DEFAULT_MODEL
        };
        if allow_fallback && !allowed.iter().any(|model| model.id == fallback) {
            bail!("fallback model {fallback} is not in the allowed model list");
        }
        Ok(Self {
            default_failures: Arc::new(AtomicU32::new(0)),
            primary_model: primary.id,
            allowed_models: allowed.iter().map(|model| model.id).collect(),
            allow_fallback,
        })
    }

    /// Once tripped the default model is not attempted again in this process.
    pub fn breaker_tripped(&self) -> bool {
        self.default_failures() >= BREAKER_THRESHOLD
    }

    pub fn default_failures(&self) -> u32 {
        self.default_failures.load(Ordering::Relaxed)
    }

    /// The model this chain would try first right now.
    pub fn current_default(&self) -> &'static str {
        if self.breaker_tripped() {
            self.fallback_model().unwrap_or(self.primary_model)
        } else {
            self.primary_model
        }
    }

    /// Runs one request, falling back once on failure.
    ///
    /// A rejected-by-domain-validation result counts as a failure the same way a
    /// 429 does: both mean this model did not produce a usable answer, and both
    /// should push toward the model that does.
    pub fn analyze(
        &self,
        provider: &dyn VlmProvider,
        request: &VlmRequest,
    ) -> Result<ChainOutcome> {
        let first = self.current_default();
        match provider.analyze(first, request) {
            Ok(response) => Ok(ChainOutcome {
                response,
                attempts: 1,
                fell_back: false,
                attempted_models: vec![first.to_string()],
                usage_complete: true,
            }),
            Err(first_error) => {
                let fallback = self.fallback_model();
                if !self.allow_fallback || fallback == Some(first) {
                    // The breaker already routed here; there is nowhere else to go.
                    return Err(ProviderChainError {
                        attempted_models: vec![first.to_string()],
                        message: format!("{first}: {first_error:#}"),
                    }
                    .into());
                }
                self.default_failures.fetch_add(1, Ordering::Relaxed);
                let fallback = fallback.expect("fallback policy checked");
                match provider.analyze(fallback, request) {
                    Ok(response) => Ok(ChainOutcome {
                        response,
                        attempts: 2,
                        fell_back: true,
                        attempted_models: vec![first.to_string(), fallback.to_string()],
                        usage_complete: false,
                    }),
                    Err(fallback_error) => Err(ProviderChainError {
                        attempted_models: vec![first.to_string(), fallback.to_string()],
                        message: format!("both models failed: {first}: {first_error:#}; {fallback}: {fallback_error:#}"),
                    }.into()),
                }
            }
        }
    }

    /// Records a failure attributable to the default model that did not come
    /// from the transport — a domain-invalid response, for instance.
    pub fn record_default_failure(&self) {
        self.default_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn fallback_model(&self) -> Option<&'static str> {
        if !self.allow_fallback {
            return None;
        }
        let candidate = if self.primary_model == DEFAULT_MODEL {
            FALLBACK_MODEL
        } else {
            DEFAULT_MODEL
        };
        self.allowed_models
            .contains(&candidate)
            .then_some(candidate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct ScriptedProvider {
        failing: Vec<&'static str>,
        calls: Mutex<Vec<String>>,
    }

    impl ScriptedProvider {
        fn new(failing: Vec<&'static str>) -> Self {
            Self {
                failing,
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl VlmProvider for ScriptedProvider {
        fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
            self.calls.lock().unwrap().push(model.to_string());
            if self.failing.contains(&model) {
                bail!("upstream 429");
            }
            Ok(VlmResponse {
                model: model.to_string(),
                body: "{}".to_string(),
                prompt_tokens: 1,
                completion_tokens: 1,
                cost_usd: 0.0,
                latency_ms: 1,
            })
        }
    }

    #[test]
    fn the_default_model_serves_when_it_works() {
        let scripted = ScriptedProvider::new(vec![]);
        let chain = ProviderChain::new();
        let outcome = chain.analyze(&scripted, &VlmRequest::default()).unwrap();
        assert_eq!(outcome.response.model, DEFAULT_MODEL);
        assert_eq!(outcome.attempts, 1);
        assert!(!outcome.fell_back);
        assert_eq!(*scripted.calls.lock().unwrap(), vec![DEFAULT_MODEL]);
    }

    #[test]
    fn default_constructs_the_same_usable_policy_as_new() {
        let from_default = ProviderChain::default();
        assert_eq!(from_default.current_default(), DEFAULT_MODEL);
        assert_eq!(from_default.fallback_model(), Some(FALLBACK_MODEL));
    }

    #[test]
    fn a_failure_falls_back_once_and_records_the_serving_model() {
        let scripted = ScriptedProvider::new(vec![DEFAULT_MODEL]);
        let chain = ProviderChain::new();
        let outcome = chain.analyze(&scripted, &VlmRequest::default()).unwrap();
        assert_eq!(outcome.response.model, FALLBACK_MODEL);
        assert_eq!(outcome.attempts, 2);
        assert!(outcome.fell_back);
    }

    #[test]
    fn both_failing_is_an_error_naming_both_models() {
        let scripted = ScriptedProvider::new(vec![DEFAULT_MODEL, FALLBACK_MODEL]);
        let chain = ProviderChain::new();
        let error = chain
            .analyze(&scripted, &VlmRequest::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains(DEFAULT_MODEL), "{error}");
        assert!(error.contains(FALLBACK_MODEL), "{error}");
    }

    #[test]
    fn explicit_policy_refuses_models_before_calling_the_provider() {
        let provider = ScriptedProvider::new(vec![]);
        assert!(ProviderChain::with_policy("unknown/model", &[], false).is_err());
        assert!(provider.calls.lock().unwrap().is_empty());
        assert!(
            ProviderChain::with_policy(DEFAULT_MODEL, &[FALLBACK_MODEL.to_string()], true).is_err()
        );
    }

    #[test]
    fn disabled_fallback_makes_only_the_primary_request() {
        let provider = ScriptedProvider::new(vec![DEFAULT_MODEL]);
        let chain =
            ProviderChain::with_policy(DEFAULT_MODEL, &[DEFAULT_MODEL.to_string()], false).unwrap();
        assert!(chain.analyze(&provider, &VlmRequest::default()).is_err());
        assert_eq!(*provider.calls.lock().unwrap(), vec![DEFAULT_MODEL]);
    }

    #[test]
    fn an_explicit_alternate_primary_falls_back_to_the_other_allowed_model() {
        let scripted = ScriptedProvider::new(vec![FALLBACK_MODEL]);
        let chain = ProviderChain::with_policy(
            FALLBACK_MODEL,
            &[FALLBACK_MODEL.to_string(), DEFAULT_MODEL.to_string()],
            true,
        )
        .unwrap();
        let outcome = chain.analyze(&scripted, &VlmRequest::default()).unwrap();
        assert_eq!(outcome.response.model, DEFAULT_MODEL);
        assert_eq!(
            *scripted.calls.lock().unwrap(),
            vec![FALLBACK_MODEL, DEFAULT_MODEL]
        );
    }

    #[test]
    fn breaker_routes_gemini_primary_to_its_qwen_alternate() {
        let chain = ProviderChain::with_policy(
            FALLBACK_MODEL,
            &[FALLBACK_MODEL.to_string(), DEFAULT_MODEL.to_string()],
            true,
        )
        .unwrap();
        for _ in 0..BREAKER_THRESHOLD {
            chain.record_default_failure();
        }
        assert_eq!(chain.current_default(), DEFAULT_MODEL);
    }

    #[test]
    fn ten_default_failures_flip_the_default_for_the_rest_of_the_process() {
        let scripted = ScriptedProvider::new(vec![DEFAULT_MODEL]);
        let chain = ProviderChain::new();
        for _ in 0..BREAKER_THRESHOLD {
            chain.analyze(&scripted, &VlmRequest::default()).unwrap();
        }
        assert!(chain.breaker_tripped());
        assert_eq!(chain.current_default(), FALLBACK_MODEL);

        scripted.calls.lock().unwrap().clear();
        chain.analyze(&scripted, &VlmRequest::default()).unwrap();
        // The default model is not attempted at all any more.
        assert_eq!(*scripted.calls.lock().unwrap(), vec![FALLBACK_MODEL]);
    }

    #[test]
    fn the_breaker_does_not_trip_below_the_threshold() {
        let scripted = ScriptedProvider::new(vec![DEFAULT_MODEL]);
        let chain = ProviderChain::new();
        for _ in 0..BREAKER_THRESHOLD - 1 {
            chain.analyze(&scripted, &VlmRequest::default()).unwrap();
        }
        assert!(!chain.breaker_tripped());
        assert_eq!(chain.current_default(), DEFAULT_MODEL);
    }

    #[test]
    fn a_domain_rejection_can_be_charged_to_the_default_model() {
        let chain = ProviderChain::new();
        for _ in 0..BREAKER_THRESHOLD {
            chain.record_default_failure();
        }
        assert!(chain.breaker_tripped());
    }

    #[test]
    fn cloned_chains_share_one_process_wide_breaker() {
        let chain = ProviderChain::new();
        let worker = chain.clone();
        for _ in 0..BREAKER_THRESHOLD {
            worker.record_default_failure();
        }
        assert!(chain.breaker_tripped());
        assert_eq!(chain.current_default(), FALLBACK_MODEL);
    }

    #[test]
    fn once_the_breaker_is_tripped_a_fallback_failure_is_not_retried() {
        let scripted = ScriptedProvider::new(vec![FALLBACK_MODEL]);
        let chain = ProviderChain::new();
        for _ in 0..BREAKER_THRESHOLD {
            chain.record_default_failure();
        }
        scripted.calls.lock().unwrap().clear();
        assert!(chain.analyze(&scripted, &VlmRequest::default()).is_err());
        assert_eq!(
            scripted.calls.lock().unwrap().len(),
            1,
            "a tripped breaker must not double-request the fallback"
        );
    }

    #[test]
    fn every_registered_model_declares_a_measured_image_token_ratio() {
        // Deriving cost from published per-token prices is wrong by up to 5x.
        for model in MODELS {
            assert!(model.image_tokens_per_frame > 0, "{}", model.id);
            assert!(model.supports_max_items, "{} cannot bound arrays", model.id);
        }
        assert!(model_spec(DEFAULT_MODEL).is_some());
        assert!(model_spec(FALLBACK_MODEL).is_some());
        assert!(model_spec("nonexistent/model").is_none());
    }
}
