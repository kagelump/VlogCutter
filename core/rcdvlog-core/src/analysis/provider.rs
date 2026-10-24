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
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// Cheapest measured option: $0.03 for a 273-clip project.
pub const DEFAULT_MODEL: &str = "qwen/qwen3.7-flash";
/// Fastest and most reliable of the cheap tier: 100% first-attempt, p50 3.4 s,
/// $0.08 for the same project.
pub const FALLBACK_MODEL: &str = "google/gemini-2.5-flash-lite";
/// Sustained-failure threshold, per process.
pub const BREAKER_THRESHOLD: u32 = 10;

/// Hard `max_tokens` value sent to OpenRouter for every VLM request. The
/// conservative request bound uses this exact value, so changing it changes the
/// admission quote and therefore the policy identity.
pub const OPENROUTER_MAX_OUTPUT_TOKENS: u32 = 1_500;

/// Version of the provider-routing/cost-bound contract. It is part of request
/// identity because a changed wire cap, model limit or price ceiling is a
/// different paid request even when the prompt and model are byte-identical.
pub const ANALYSIS_COST_BOUND_CONTRACT_VERSION: &str = "openrouter.provider-max-price.v1";

fn analysis_cost_bound_contract_id_for(models: &[ModelSpec]) -> String {
    let mut hasher = blake3::Hasher::new();
    let add = |hasher: &mut blake3::Hasher, bytes: &[u8]| {
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    };
    add(&mut hasher, ANALYSIS_COST_BOUND_CONTRACT_VERSION.as_bytes());
    hasher.update(&(OPENROUTER_MAX_OUTPUT_TOKENS.to_le_bytes()));
    for spec in models {
        add(&mut hasher, spec.id.as_bytes());
        hasher.update(&spec.context_window_tokens.to_le_bytes());
        hasher.update(&spec.max_completion_tokens.to_le_bytes());
        hasher.update(&spec.prompt_price_usd_per_million.to_bits().to_le_bytes());
        hasher.update(
            &spec
                .completion_price_usd_per_million
                .to_bits()
                .to_le_bytes(),
        );
        hasher.update(&spec.image_price_usd_each.to_bits().to_le_bytes());
        hasher.update(&spec.request_price_usd_each.to_bits().to_le_bytes());
    }
    format!(
        "{ANALYSIS_COST_BOUND_CONTRACT_VERSION}:{}",
        hasher.finalize().to_hex()
    )
}

/// Deterministic identity of the exact registered context windows, completion
/// limits and provider price ceilings. Policy and request identities bind this
/// value so a cap change cannot silently reuse an old approval.
pub fn analysis_cost_bound_contract_id() -> String {
    analysis_cost_bound_contract_id_for(MODELS)
}

/// A conservative upper bound for one actual provider transmission.
///
/// This is an admission reservation, not a provider charge. Production
/// transports derive it from enforceable wire caps (OpenRouter
/// `provider.max_price`), the registered model context window and the exact
/// `max_tokens`; synthetic transports may declare a deterministic test-only
/// bound. The fields stay explicit so reports can distinguish the bound from
/// returned/confirmed/uncertain usage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisRequestCostBound {
    pub maximum_cost_usd: f64,
    pub contract_version: String,
    pub prompt_tokens_max: u64,
    pub completion_tokens_max: u32,
    pub image_count: u32,
    pub prompt_price_usd_per_million: f64,
    pub completion_price_usd_per_million: f64,
    pub image_price_usd_each: f64,
    pub request_price_usd_each: f64,
    pub assumptions: Vec<String>,
}

impl AnalysisRequestCostBound {
    /// Deterministic bound for a synthetic transport used only by tests. The
    /// production trait default fails closed, so this cannot become an implicit
    /// fallback for a real transport.
    pub fn declared_for_synthetic_transport(maximum_cost_usd: f64) -> Self {
        Self {
            maximum_cost_usd,
            contract_version: "synthetic.transport-declared.v1".into(),
            prompt_tokens_max: 1,
            completion_tokens_max: 1,
            image_count: 0,
            prompt_price_usd_per_million: 0.0,
            completion_price_usd_per_million: 0.0,
            image_price_usd_each: 0.0,
            request_price_usd_each: maximum_cost_usd,
            assumptions: vec![
                "synthetic transport declares a deterministic test-only bound".into(),
            ],
        }
    }

    /// Recomputes the bound from its separately recorded components. This keeps
    /// a persisted maximum from drifting away from the numeric policy that was
    /// approved and sent on the wire.
    pub(crate) fn computed_maximum_cost_usd(&self) -> Result<f64> {
        let prompt =
            self.prompt_tokens_max as f64 * self.prompt_price_usd_per_million / 1_000_000.0;
        let completion =
            self.completion_tokens_max as f64 * self.completion_price_usd_per_million / 1_000_000.0;
        let images = self.image_count as f64 * self.image_price_usd_each;
        let computed = prompt + completion + images + self.request_price_usd_each;
        anyhow::ensure!(
            computed.is_finite(),
            "analysis request cost bound computation must be finite"
        );
        Ok(computed)
    }

    pub fn validate(&self) -> Result<()> {
        let components = [
            self.maximum_cost_usd,
            self.prompt_price_usd_per_million,
            self.completion_price_usd_per_million,
            self.image_price_usd_each,
            self.request_price_usd_each,
        ];
        anyhow::ensure!(
            components
                .iter()
                .all(|value| value.is_finite() && *value >= 0.0),
            "analysis request cost bound components must be finite and non-negative"
        );
        anyhow::ensure!(
            self.maximum_cost_usd.is_finite() && self.maximum_cost_usd > 0.0,
            "analysis request cost bound must be finite and greater than zero"
        );
        anyhow::ensure!(
            !self.contract_version.trim().is_empty(),
            "analysis request cost bound contract version is required"
        );
        anyhow::ensure!(
            self.prompt_tokens_max > 0 && self.completion_tokens_max > 0,
            "analysis request token bounds must be greater than zero"
        );
        let computed = self.computed_maximum_cost_usd()?;
        let tolerance = self.maximum_cost_usd.abs().max(computed.abs()).max(1.0) * 1e-12;
        anyhow::ensure!(
            (self.maximum_cost_usd - computed).abs() <= tolerance,
            "analysis request cost bound maximum is internally inconsistent with its components"
        );
        Ok(())
    }
}

/// What a model costs and what it can be asked to do.
///
/// `image_tokens_per_frame` is **measured**, not derived from published prices:
/// identical images billed 1,235 to 6,026 prompt tokens across models, so a cost
/// model built from frames times price-per-token is wrong by up to 5x.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelSpec {
    pub id: &'static str,
    pub image_tokens_per_frame: u32,
    /// Providers that reject `maxItems` cannot be used at all: bounded arrays
    /// are what make a repetition loop structurally inexpressible.
    pub supports_max_items: bool,
    /// Whether reasoning can be disabled. A model that burns reasoning tokens
    /// and returns empty content is unusable here.
    pub reasoning_disablable: bool,
    /// Whether the model accepts image content blocks.
    pub supports_image_input: bool,
    /// Whether the model accepts audio content blocks. No registered model does
    /// yet; keeping this explicit prevents a configured audio payload from being
    /// silently treated as an image request.
    pub supports_audio_input: bool,
    /// Whether the model can consume accepted transcript text as context.
    pub supports_transcript_input: bool,
    /// Whether the provider/model accepts a JSON Schema response format.
    pub supports_json_schema: bool,
    /// Authoritative model context window in tokens. Prompt tokens cannot
    /// exceed this provider-enforced limit.
    pub context_window_tokens: u64,
    /// Maximum completion length accepted by the model. A registered model
    /// must accept `OPENROUTER_MAX_OUTPUT_TOKENS` before it can be dispatched.
    pub max_completion_tokens: u32,
    /// Routing ceiling mirrored into `provider.max_price.prompt` (USD per
    /// million prompt tokens). OpenRouter only routes to providers at or below
    /// this ceiling; actual charges remain external provider facts.
    pub prompt_price_usd_per_million: f64,
    /// Routing ceiling mirrored into `provider.max_price.completion`.
    pub completion_price_usd_per_million: f64,
    /// Per-image routing ceiling mirrored into `provider.max_price.image`.
    pub image_price_usd_each: f64,
    /// Per-request fee ceiling mirrored into `provider.max_price.request`.
    /// This is a request fee, not a cap on total request price.
    pub request_price_usd_each: f64,
}

pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        // ~2,175 prompt tokens over a median 5-frame pack.
        id: DEFAULT_MODEL,
        image_tokens_per_frame: 435,
        supports_max_items: true,
        reasoning_disablable: true,
        supports_image_input: true,
        supports_audio_input: false,
        supports_transcript_input: true,
        supports_json_schema: true,
        // Public OpenRouter model metadata checked 2026-10-24.
        context_window_tokens: 1_000_000,
        max_completion_tokens: 65_536,
        // Routing ceilings, not live-price claims. The per-million ceilings
        // cover the highest public context-tier metadata.
        prompt_price_usd_per_million: 0.20,
        completion_price_usd_per_million: 0.80,
        image_price_usd_each: 0.01,
        request_price_usd_each: 0.01,
    },
    ModelSpec {
        // ~1,235 prompt tokens over a median 5-frame pack; Google normalises
        // internally, so this is flat with respect to input width.
        id: FALLBACK_MODEL,
        image_tokens_per_frame: 247,
        supports_max_items: true,
        reasoning_disablable: true,
        supports_image_input: true,
        supports_audio_input: false,
        supports_transcript_input: true,
        supports_json_schema: true,
        // Public OpenRouter model metadata checked 2026-10-24.
        context_window_tokens: 1_048_576,
        max_completion_tokens: 65_535,
        // Routing ceilings, not live-price claims.
        prompt_price_usd_per_million: 0.10,
        completion_price_usd_per_million: 0.40,
        image_price_usd_each: 0.01,
        request_price_usd_each: 0.01,
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
    /// Requested model retained for compatibility with existing fallback logic.
    pub model: String,
    pub body: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cost_usd: f64,
    pub latency_ms: u64,
    pub receipt: VlmReceipt,
}

/// Provider-reported execution provenance. Missing values stay missing rather
/// than being inferred from the request.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VlmReceipt {
    pub actual_model: Option<String>,
    pub provider: Option<String>,
    pub generation_id: Option<String>,
    pub usage_complete: bool,
}

/// Stable provider failure categories safe to persist. Provider messages and
/// response bodies may contain prompts, frame bytes or credentials, so they are
/// never copied into policy or pilot diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFailureCategory {
    Transport,
    HttpStatus,
    InvalidResponse,
    Configuration,
    Unclassified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFailure {
    pub category: ProviderFailureCategory,
    pub http_status: Option<u16>,
}

impl ProviderFailure {
    pub fn transport() -> Self {
        Self {
            category: ProviderFailureCategory::Transport,
            http_status: None,
        }
    }

    pub fn http_status(status: u16) -> Self {
        Self {
            category: ProviderFailureCategory::HttpStatus,
            http_status: Some(status),
        }
    }

    pub fn invalid_response() -> Self {
        Self {
            category: ProviderFailureCategory::InvalidResponse,
            http_status: None,
        }
    }

    pub fn unclassified() -> Self {
        Self {
            category: ProviderFailureCategory::Unclassified,
            http_status: None,
        }
    }

    /// Extracts only typed failure metadata from an error chain. Any unknown
    /// error is deliberately collapsed to `unclassified`.
    pub fn from_anyhow(error: &anyhow::Error) -> Self {
        error
            .chain()
            .find_map(|cause| cause.downcast_ref::<ProviderFailure>().cloned())
            .or_else(|| {
                error
                    .chain()
                    .find_map(|cause| cause.downcast_ref::<ProviderChainError>())
                    .map(|chain| Self {
                        category: chain.category,
                        http_status: chain.http_status,
                    })
            })
            .unwrap_or_else(Self::unclassified)
    }

    pub fn safe_detail(&self) -> String {
        match self.category {
            ProviderFailureCategory::Transport => "provider_error:transport".into(),
            ProviderFailureCategory::HttpStatus => {
                format!(
                    "provider_error:http_status:{}",
                    self.http_status.unwrap_or(0)
                )
            }
            ProviderFailureCategory::InvalidResponse => "provider_error:invalid_response".into(),
            ProviderFailureCategory::Configuration => "provider_error:configuration".into(),
            ProviderFailureCategory::Unclassified => "provider_error:unclassified".into(),
        }
    }
}

impl std::fmt::Display for ProviderFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.safe_detail())
    }
}

impl std::error::Error for ProviderFailure {}

/// A transport that can run one request against one named model.
pub trait VlmProvider: Send + Sync {
    /// Local-only credential/client preparation, before recording transmission.
    fn prepare(&self) -> Result<()> {
        Ok(())
    }

    /// Conservative maximum charge for this exact model/request. Production
    /// transports must derive this from enforceable wire caps; the default
    /// fails closed so a transport cannot silently receive a generic estimate.
    fn analysis_cost_bound(
        &self,
        _model: &str,
        _request: &VlmRequest,
    ) -> Result<AnalysisRequestCostBound> {
        bail!("provider does not declare a conservative analysis request cost bound")
    }

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
    pub category: ProviderFailureCategory,
    pub http_status: Option<u16>,
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
            Ok(response) => {
                let usage_complete = response.receipt.usage_complete;
                Ok(ChainOutcome {
                    response,
                    attempts: 1,
                    fell_back: false,
                    attempted_models: vec![first.to_string()],
                    usage_complete,
                })
            }
            Err(first_error) => {
                let _ = &first_error;
                let fallback = self.fallback_model();
                if !self.allow_fallback || fallback == Some(first) {
                    // The breaker already routed here; there is nowhere else to go.
                    let failure = ProviderFailure::from_anyhow(&first_error);
                    return Err(ProviderChainError {
                        attempted_models: vec![first.to_string()],
                        message: format!("provider request failed for {first}"),
                        category: failure.category,
                        http_status: failure.http_status,
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
                    Err(fallback_error) => {
                        let failure = ProviderFailure::from_anyhow(&fallback_error);
                        Err(ProviderChainError {
                            attempted_models: vec![first.to_string(), fallback.to_string()],
                            message: format!("both models failed: {first}, {fallback}"),
                            category: failure.category,
                            http_status: failure.http_status,
                        }
                        .into())
                    }
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

    pub fn primary_model(&self) -> &'static str {
        self.primary_model
    }

    pub fn allowed_models(&self) -> &[&'static str] {
        &self.allowed_models
    }

    pub fn allows_fallback(&self) -> bool {
        self.allow_fallback
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
                receipt: crate::analysis::provider::VlmReceipt {
                    actual_model: Some(model.to_string()),
                    provider: Some("fake".into()),
                    generation_id: Some("fake-generation".into()),
                    usage_complete: true,
                },
            })
        }
    }

    struct IncompleteUsageProvider;

    impl VlmProvider for IncompleteUsageProvider {
        fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
            Ok(VlmResponse {
                model: model.to_owned(),
                body: "{}".into(),
                prompt_tokens: 0,
                completion_tokens: 0,
                cost_usd: 0.0,
                latency_ms: 0,
                receipt: VlmReceipt::default(),
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
    fn successful_chain_usage_completeness_comes_from_the_provider_receipt() {
        let outcome = ProviderChain::new()
            .analyze(&IncompleteUsageProvider, &VlmRequest::default())
            .unwrap();
        assert!(!outcome.usage_complete);
        assert_eq!(outcome.response.model, DEFAULT_MODEL);
        assert_eq!(outcome.response.receipt.actual_model, None);
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
        assert!(
            !outcome.usage_complete,
            "a failed first attempt keeps fallback usage incomplete"
        );
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

    #[test]
    fn every_registered_model_has_an_enforceable_cost_contract() {
        for model in MODELS {
            assert!(model.context_window_tokens > 0, "{}", model.id);
            assert!(
                model.max_completion_tokens >= OPENROUTER_MAX_OUTPUT_TOKENS,
                "{} cannot accept the exact max_tokens wire cap",
                model.id
            );
            for value in [
                model.prompt_price_usd_per_million,
                model.completion_price_usd_per_million,
                model.image_price_usd_each,
                model.request_price_usd_each,
            ] {
                assert!(value.is_finite() && value >= 0.0, "{}", model.id);
            }
        }
    }

    #[test]
    fn contract_identity_changes_with_context_and_price_ceilings() {
        let baseline = analysis_cost_bound_contract_id_for(MODELS);
        let mut changed = MODELS.to_vec();
        changed[0].context_window_tokens += 1;
        assert_ne!(analysis_cost_bound_contract_id_for(&changed), baseline);

        let mut changed = MODELS.to_vec();
        changed[1].request_price_usd_each += 0.001;
        assert_ne!(analysis_cost_bound_contract_id_for(&changed), baseline);
    }

    #[test]
    fn cost_bound_validation_rejects_nonfinite_negative_and_inconsistent_quotes() {
        let valid = AnalysisRequestCostBound::declared_for_synthetic_transport(0.05);
        valid.validate().unwrap();

        let mut negative = valid.clone();
        negative.request_price_usd_each = -0.01;
        assert!(negative.validate().is_err());

        let mut nonfinite = valid.clone();
        nonfinite.prompt_price_usd_per_million = f64::INFINITY;
        assert!(nonfinite.validate().is_err());

        let mut inconsistent = valid;
        inconsistent.maximum_cost_usd += 1.0;
        assert!(inconsistent.validate().is_err());
    }
}
