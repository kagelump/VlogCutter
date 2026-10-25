//! Deterministic cache/approval identity for native cloud speech.
//!
//! This leaf is intentionally small and pure. It resolves the already selected
//! request against the registered protocol snapshot and hashes only semantic
//! recipe inputs. It never reads media, creates a catalog, resolves credentials,
//! loads a model or performs provider I/O.

use anyhow::{Context, Result};
use serde::Serialize;

use crate::analysis::engine::VAD_MODEL;
use crate::analysis::openrouter_stt::{
    NativeSttProviderPolicy, NativeSttRequest, resolve_native_stt_model,
};
use crate::analysis::quality::native_stt::NATIVE_STT_QUALITY_RECIPE_VERSION;
use crate::analysis::windows::WINDOW_MS;

/// Stable, recognizable prefix for the domain-separated native transcript
/// recipe digest. A changed recipe yields a different complete version string.
pub const NATIVE_STT_TRANSCRIPT_RECIPE_SCHEMA_VERSION: &str = "native-stt.transcript-recipe.v1";

#[derive(Serialize)]
struct NativeSttTranscriptRecipe<'a> {
    schema_version: &'static str,
    provider: &'a str,
    model: &'a str,
    language: Option<&'a str>,
    temperature_bits: u64,
    provider_policy: NativeSttProviderPolicy,
    endpoint: &'static str,
    response_format: &'static str,
    timestamp_granularities: &'static [&'static str],
    supported_input_formats: &'static [&'static str],
    supports_language: bool,
    max_window_ms: u32,
    vad_model_file: &'static str,
    vad_model_bytes: u64,
    vad_model_blake3: &'static str,
    packing_window_ms: u32,
    packing_algorithm: &'static str,
    quality_recipe_version: &'static str,
}

fn semantics_for(request: &NativeSttRequest) -> Result<NativeSttTranscriptRecipe<'_>> {
    let spec = resolve_native_stt_model(request.provider(), request.model())?;
    request.provider_policy().validate()?;
    Ok(NativeSttTranscriptRecipe {
        schema_version: NATIVE_STT_TRANSCRIPT_RECIPE_SCHEMA_VERSION,
        provider: request.provider(),
        model: request.model(),
        language: request.language(),
        temperature_bits: request.temperature().to_bits(),
        provider_policy: request.provider_policy(),
        endpoint: spec.endpoint,
        response_format: spec.response_format,
        timestamp_granularities: spec.timestamp_granularities,
        supported_input_formats: spec.supported_input_formats,
        supports_language: spec.supports_language,
        max_window_ms: spec.max_window_ms,
        vad_model_file: VAD_MODEL.file_name,
        vad_model_bytes: VAD_MODEL.bytes,
        vad_model_blake3: VAD_MODEL.blake3,
        packing_window_ms: WINDOW_MS,
        packing_algorithm: "greedy-source-order.split-overlong-at-window.v1",
        quality_recipe_version: NATIVE_STT_QUALITY_RECIPE_VERSION,
    })
}

fn recipe_digest(semantics: &NativeSttTranscriptRecipe<'_>) -> Result<String> {
    let serialized =
        serde_json::to_vec(semantics).context("serialize native transcript recipe semantics")?;
    let digest = blake3::hash(&serialized).to_hex();
    Ok(format!(
        "{NATIVE_STT_TRANSCRIPT_RECIPE_SCHEMA_VERSION}:{digest}"
    ))
}

/// Deterministic recipe/cache identity for one validated native request.
///
/// The digest binds only provider-qualified request semantics and the registered
/// protocol, local VAD/packing recipe and native quality recipe. Raw audio,
/// transcript text, paths, credentials, budgets, tariffs and monetary rates are
/// deliberately absent. A future stored native transcript header must reuse this
/// exact helper rather than introducing a parallel cache identity.
pub fn native_stt_transcript_recipe(request: &NativeSttRequest) -> Result<String> {
    recipe_digest(&semantics_for(request)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::openrouter_stt::{
        OPENROUTER_STT_PROVIDER, OPENROUTER_WHISPER_MODEL, SttDataCollection,
    };
    use crate::{FRAMEPACK_RECIPE_KEY, STT_RECIPE_KEY, VLM_PROMPT_VERSION};

    fn base_request() -> NativeSttRequest {
        NativeSttRequest::new(
            OPENROUTER_STT_PROVIDER,
            OPENROUTER_WHISPER_MODEL,
            Some("en".into()),
            0.0,
            NativeSttProviderPolicy::new(true, SttDataCollection::Deny),
        )
        .unwrap()
    }

    #[test]
    fn valid_request_recipe_is_deterministic_versioned_and_private() {
        let request = base_request();
        let baseline = native_stt_transcript_recipe(&request).unwrap();
        assert_eq!(baseline, native_stt_transcript_recipe(&request).unwrap());
        assert!(baseline.starts_with(NATIVE_STT_TRANSCRIPT_RECIPE_SCHEMA_VERSION));
        let digest = baseline
            .strip_prefix(NATIVE_STT_TRANSCRIPT_RECIPE_SCHEMA_VERSION)
            .and_then(|suffix| suffix.strip_prefix(':'))
            .unwrap();
        assert_eq!(digest.len(), 64);
        assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));

        for canary in [
            "input_audio",
            "Synthetic",
            "/private",
            "budget",
            "price",
            "tariff",
            "credential",
        ] {
            assert!(!baseline.contains(canary), "recipe leaked {canary}");
        }

        let changed_language = NativeSttRequest::new(
            OPENROUTER_STT_PROVIDER,
            OPENROUTER_WHISPER_MODEL,
            Some("fr".into()),
            0.0,
            NativeSttProviderPolicy::new(true, SttDataCollection::Deny),
        )
        .unwrap();
        let changed_policy = NativeSttRequest::new(
            OPENROUTER_STT_PROVIDER,
            OPENROUTER_WHISPER_MODEL,
            Some("en".into()),
            0.0,
            NativeSttProviderPolicy::new(true, SttDataCollection::Allow),
        )
        .unwrap();
        let changed_temperature = NativeSttRequest::new(
            OPENROUTER_STT_PROVIDER,
            OPENROUTER_WHISPER_MODEL,
            Some("en".into()),
            0.25,
            NativeSttProviderPolicy::new(true, SttDataCollection::Deny),
        )
        .unwrap();
        for changed in [&changed_language, &changed_policy, &changed_temperature] {
            assert_ne!(
                native_stt_transcript_recipe(changed).unwrap(),
                baseline,
                "changed request semantics must invalidate the native recipe"
            );
        }

        assert!(
            NativeSttRequest::new(
                OPENROUTER_STT_PROVIDER,
                "openai/gpt-4o-mini",
                Some("en".into()),
                0.0,
                NativeSttProviderPolicy::new(true, SttDataCollection::Deny),
            )
            .is_err(),
            "a chat model slug is not an audio-transcription recipe input"
        );
    }

    #[test]
    fn recipe_binds_registered_protocol_vad_window_and_quality_identity() {
        let request = base_request();
        let base_semantics = semantics_for(&request).unwrap();
        let baseline = recipe_digest(&base_semantics).unwrap();

        let mut changed = semantics_for(&request).unwrap();
        changed.provider = "candidate-provider";
        assert_ne!(recipe_digest(&changed).unwrap(), baseline);
        let mut changed = semantics_for(&request).unwrap();
        changed.model = "candidate-model";
        assert_ne!(recipe_digest(&changed).unwrap(), baseline);
        let mut changed = semantics_for(&request).unwrap();
        changed.endpoint = "https://candidate.invalid/audio/transcriptions";
        assert_ne!(recipe_digest(&changed).unwrap(), baseline);
        let mut changed = semantics_for(&request).unwrap();
        changed.response_format = "json";
        assert_ne!(recipe_digest(&changed).unwrap(), baseline);
        let mut changed = semantics_for(&request).unwrap();
        changed.timestamp_granularities = &["word"];
        assert_ne!(recipe_digest(&changed).unwrap(), baseline);
        let mut changed = semantics_for(&request).unwrap();
        changed.supported_input_formats = &["mp3"];
        assert_ne!(recipe_digest(&changed).unwrap(), baseline);
        let mut changed = semantics_for(&request).unwrap();
        changed.max_window_ms += 1;
        assert_ne!(recipe_digest(&changed).unwrap(), baseline);
        let mut changed = semantics_for(&request).unwrap();
        changed.vad_model_file = "candidate-vad.bin";
        assert_ne!(recipe_digest(&changed).unwrap(), baseline);
        let mut changed = semantics_for(&request).unwrap();
        changed.vad_model_bytes += 1;
        assert_ne!(recipe_digest(&changed).unwrap(), baseline);
        let mut changed = semantics_for(&request).unwrap();
        changed.vad_model_blake3 = "candidate-vad-digest";
        assert_ne!(recipe_digest(&changed).unwrap(), baseline);
        let mut changed = semantics_for(&request).unwrap();
        changed.packing_window_ms += 1;
        assert_ne!(recipe_digest(&changed).unwrap(), baseline);
        let mut changed = semantics_for(&request).unwrap();
        changed.packing_algorithm = "candidate-packing";
        assert_ne!(recipe_digest(&changed).unwrap(), baseline);
        let mut changed = semantics_for(&request).unwrap();
        changed.quality_recipe_version = "native-stt.quality.candidate";
        assert_ne!(recipe_digest(&changed).unwrap(), baseline);
    }

    #[test]
    fn legacy_local_and_vlm_recipe_identities_are_unchanged() {
        assert_eq!(
            STT_RECIPE_KEY,
            "stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1"
        );
        assert_eq!(VLM_PROMPT_VERSION, "vlm.observe.v2");
        assert_eq!(
            FRAMEPACK_RECIPE_KEY,
            "framepack.2fps.skip400ms.stride-max40.win3-8.sharp-bright-motion.dhash8-d5.v2"
        );
        let native = native_stt_transcript_recipe(&base_request()).unwrap();
        assert_ne!(native, STT_RECIPE_KEY);
        assert_ne!(native, VLM_PROMPT_VERSION);
        assert_ne!(native, FRAMEPACK_RECIPE_KEY);
    }
}
