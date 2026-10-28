//! Published token tariffs and provider-reported usage are distinct from an invoice.
//!
//! Only an explicitly registered quote can release its unused reservation using
//! complete, internally consistent usage. Existing provider-reported charges
//! retain their own settlement contract. Missing facts never become zeroes.
use crate::analysis::provider::AnalysisRequestCostBound;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

pub const PUBLISHED_TOKEN_TARIFF_VERSION: &str = "gemini-direct.token-tariff.2026-10-28.v1";
const PROVIDER: &str = "gemini-direct";
const MODEL: &str = "gemini-3.5-flash-lite";
const PRICE_SOURCE: &str = "https://ai.google.dev/gemini-api/docs/pricing";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PublishedServiceTier {
    Standard,
    Flex,
}
impl PublishedServiceTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Flex => "flex",
        }
    }
}

/// Frozen published-price evidence, included in the full admission identity.
/// Rates are validated against this version's registry, never caller-defined.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublishedTokenTariff {
    pub version: String,
    pub provider: String,
    pub model: String,
    pub tier: PublishedServiceTier,
    pub input_usd_per_million: f64,
    pub cached_input_usd_per_million: f64,
    pub output_including_thinking_usd_per_million: f64,
    pub source_url: String,
}
impl PublishedTokenTariff {
    pub fn gemini_35_flash_lite(tier: PublishedServiceTier) -> Self {
        let (input, cached, output) = match tier {
            PublishedServiceTier::Standard => (0.30, 0.03, 2.50),
            PublishedServiceTier::Flex => (0.15, 0.02, 1.25),
        };
        Self {
            version: PUBLISHED_TOKEN_TARIFF_VERSION.into(),
            provider: PROVIDER.into(),
            model: MODEL.into(),
            tier,
            input_usd_per_million: input,
            cached_input_usd_per_million: cached,
            output_including_thinking_usd_per_million: output,
            source_url: PRICE_SOURCE.into(),
        }
    }
    pub fn fingerprint(&self) -> Result<String> {
        self.validate()?;
        Ok(blake3::hash(&serde_json::to_vec(self)?)
            .to_hex()
            .to_string())
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            *self == Self::gemini_35_flash_lite(self.tier),
            "published token tariff is not an exact registered version/provider/model/tier/rate contract"
        );
        Ok(())
    }
    pub(crate) fn validate_bound(&self, bound: &AnalysisRequestCostBound) -> Result<()> {
        self.validate()?;
        ensure!(
            bound.audio.is_none()
                && bound.image_count == 0
                && bound.image_price_usd_each == 0.0
                && bound.request_price_usd_each == 0.0,
            "published token tariff cannot mix audio-duration, image or per-request charges"
        );
        ensure!(
            bound.prompt_tokens_max <= 1_048_576 && bound.completion_tokens_max <= 65_536,
            "published token quote exceeds the registered model context/output limit"
        );
        ensure!(
            bound.prompt_price_usd_per_million
                >= self
                    .input_usd_per_million
                    .max(self.cached_input_usd_per_million)
                && bound.completion_price_usd_per_million
                    >= self.output_including_thinking_usd_per_million,
            "admission price ceilings are below the registered published tariff"
        );
        Ok(())
    }
}

/// Returned token/tier evidence. None means unknown; zero must be reported.
/// No raw prompt, video bytes, thought text, or credential is retained here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsagePricedReceipt {
    pub tariff_fingerprint: String,
    pub reported_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_tier_body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_tier_header: Option<String>,
    pub prompt_tokens: Option<u64>,
    pub cached_prompt_tokens: Option<u64>,
    pub candidate_tokens: Option<u64>,
    pub thinking_tokens: Option<u64>,
    pub tool_prompt_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub latency_ms: u64,
    /// Raw missing counters above remain unknown. This registered derivation
    /// permits zero *billable auxiliary* tokens only when returned total equals
    /// all known nonnegative components. Missing cache counts earn no discount.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conservative_derivation: Option<UsageConservativeDerivation>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UsageConservativeDerivation {
    GeminiGenerateContentV1BetaConservedAuxiliaryCountersV1,
}
impl UsagePricedReceipt {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.tariff_fingerprint.len() == 64
                && self
                    .tariff_fingerprint
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit()),
            "usage-priced receipt requires a tariff fingerprint"
        );
        ensure!(
            self.reported_tier
                .as_ref()
                .is_none_or(|s| s.len() <= 64 && s == s.trim())
                && [&self.reported_tier_body, &self.reported_tier_header]
                    .iter()
                    .all(|tier| tier.as_ref().is_none_or(|s| s.len() <= 64)),
            "reported service tier is invalid"
        );
        Ok(())
    }
    pub(crate) fn accepts_partial_metadata(&self, newer: &Self) -> bool {
        let same_or_new_tier = |known: &Option<String>, incoming: &Option<String>| {
            incoming.as_ref().is_none_or(|value| {
                known.as_ref().map_or_else(
                    || Some(value) == self.reported_tier.as_ref(),
                    |prior| prior == value,
                )
            })
        };
        let derived_zero = |known: Option<u64>, incoming: Option<u64>| {
            incoming.is_none_or(|value| {
                Some(value) == known
                    || (known.is_none() && self.conservative_derivation.is_some() && value == 0)
            })
        };
        self.tariff_fingerprint == newer.tariff_fingerprint
            && same_or_new_tier(&self.reported_tier_body, &newer.reported_tier_body)
            && same_or_new_tier(&self.reported_tier_header, &newer.reported_tier_header)
            && derived_zero(self.thinking_tokens, newer.thinking_tokens)
            && derived_zero(self.tool_prompt_tokens, newer.tool_prompt_tokens)
            && newer.cached_prompt_tokens.is_none_or(|value| {
                Some(value) == self.cached_prompt_tokens
                    || (self.cached_prompt_tokens.is_none()
                        && self.conservative_derivation.is_some()
                        && self.prompt_tokens.is_some_and(|prompt| value <= prompt))
            })
            && newer
                .conservative_derivation
                .is_none_or(|proof| Some(proof) == self.conservative_derivation)
            && newer
                .reported_tier
                .as_ref()
                .is_none_or(|tier| Some(tier) == self.reported_tier.as_ref())
            && [
                (self.prompt_tokens, newer.prompt_tokens),
                (self.candidate_tokens, newer.candidate_tokens),
                (self.total_tokens, newer.total_tokens),
            ]
            .iter()
            .all(|(known, incoming)| incoming.is_none_or(|value| Some(value) == *known))
    }

    /// A usable estimate requires exact quote provenance and every billing
    /// component. Contradictory/over-limit facts remain uncertain evidence.
    pub(crate) fn estimate(
        &self,
        bound: &AnalysisRequestCostBound,
        provider: &str,
        requested_model: &str,
        actual_model: &str,
    ) -> Result<Option<f64>> {
        self.validate()?;
        let Some(tariff) = &bound.published_token_tariff else {
            return Ok(None);
        };
        tariff.validate_bound(bound)?;
        if provider != tariff.provider
            || requested_model != tariff.model
            || actual_model != tariff.model
            || self.tariff_fingerprint != tariff.fingerprint()?
            || self.reported_tier.as_deref() != Some(tariff.tier.as_str())
            || [&self.reported_tier_body, &self.reported_tier_header]
                .iter()
                .any(|raw| raw.as_deref().is_some_and(|v| v != tariff.tier.as_str()))
        {
            return Ok(None);
        }
        // Google GenerateContent's documented total is prompt + tool prompt +
        // thoughts + candidates. If known components already equal total, any
        // omitted nonnegative tool/thought component is necessarily zero. This
        // is a versioned conservation proof, not an assertion of raw presence.
        // Unknown cache usage is priced without a credit (conservative).
        let derived = self.conservative_derivation.is_some()
            && bound
                .contract_version
                .starts_with("gemini-direct.native-video.token-bound.v1:");
        let auxiliary = |raw: Option<u64>| raw.or(if derived { Some(0) } else { None });
        let (Some(prompt), Some(cached), Some(candidate), Some(thinking), Some(tool), Some(total)) = (
            self.prompt_tokens,
            auxiliary(self.cached_prompt_tokens),
            self.candidate_tokens,
            auxiliary(self.thinking_tokens),
            auxiliary(self.tool_prompt_tokens),
            self.total_tokens,
        ) else {
            return Ok(None);
        };
        let Some(input) = prompt.checked_add(tool) else {
            return Ok(None);
        };
        let Some(output) = candidate.checked_add(thinking) else {
            return Ok(None);
        };
        if input.checked_add(output) != Some(total)
            || cached > prompt
            || input > bound.prompt_tokens_max
            || output > u64::from(bound.completion_tokens_max)
        {
            return Ok(None);
        }
        let estimated = ((input - cached) as f64 * tariff.input_usd_per_million
            + cached as f64 * tariff.cached_input_usd_per_million
            + output as f64 * tariff.output_including_thinking_usd_per_million)
            / 1_000_000.0;
        ensure!(
            estimated.is_finite() && estimated >= 0.0,
            "published usage estimate is not finite"
        );
        Ok(Some(estimated))
    }
}
