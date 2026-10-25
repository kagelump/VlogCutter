//! Local preparation for one explicitly authorized native cloud speech request.
//!
//! This leaf stops at local media readiness: it validates an already frozen
//! NativeSpeechTranscription policy, extracts the selected proxy exactly once,
//! runs standalone Silero VAD, and returns bounded window maps plus exclusive
//! speech/proxy claims. It never reads credentials, prepares a provider request,
//! calls a transport, reserves budget, or writes canonical speech state.

use anyhow::{Context, Result, bail, ensure};
use hound::{SampleFormat, WavReader};
use serde::Serialize;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::analysis::engine::{VAD_MODEL, ensure_model, model_dir_for};
use crate::analysis::openrouter_stt::{
    NativeSttAudioWindow, NativeSttRequest, ResolvedSpeechBackend, SpeechBackendSelection,
    plan_native_stt_speech_windows, resolve_speech_backend,
};
use crate::analysis::run_policy::{
    AnalysisPayload, RunAssetScope, RunPolicy, RunPolicyParameters,
    load_run_authorization_on_connection, source_scope,
};
use crate::analysis::silero_vad::{SileroVad, SpeechDetector, validate_vad_threads};
use crate::analysis::store::native_stt_transcript_recipe;
use crate::analysis::windows::{PackedWindow, SpeechRegion};
use crate::{
    Asset, AssetVideoValidationStatus, Catalog, RepresentationClaim, RepresentationKind,
    ResourceOverrides, ResourceWaitCancelled, SPEECH_REPRESENTATION_KIND, STT_RECIPE_KEY,
};

/// Stable cancellation category for local native-speech preparation.
#[derive(Debug, thiserror::Error)]
#[error("native speech preparation canceled")]
pub(crate) struct NativeSpeechPreparationCanceled;

/// Distinguishes the two successful local terminal outcomes from prepared
/// speech that has bounded windows available for a future transport slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativeSpeechLocalOutcome {
    NoAudio,
    NoSpeech,
    PreparedSpeech,
}

/// Local VAD/resource facts actually selected for this preparation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeVadEvidence {
    pub model_file: &'static str,
    pub threads: usize,
    pub resource_source: String,
    pub calibration_key: Option<String>,
    pub sample_compatibility: String,
}

/// Exact identities needed by a future accounted native-speech caller. Paths,
/// raw PCM, WAV bytes and credentials are deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeSpeechPreparationEvidence {
    pub policy_id: String,
    pub project_id: String,
    pub asset_id: String,
    pub provider: String,
    pub model: String,
    pub recipe: String,
    pub scope: RunAssetScope,
    pub proxy_full_fingerprint: String,
    pub proxy_sampled_fingerprint: String,
    pub source_duration_ms: u32,
    pub last_safe_sample_ms: u32,
    pub vad: Option<NativeVadEvidence>,
}

#[derive(Debug)]
struct NativeSpeechSourceGuard {
    proxy_path: PathBuf,
    approved_scope: RunAssetScope,
}

impl NativeSpeechSourceGuard {
    fn validate(&self, catalog: &Catalog, asset_id: &str, project_id: &str) -> Result<()> {
        let asset = catalog.get_asset(asset_id)?;
        ensure!(
            asset.project_id == project_id && !asset.removed,
            "native speech asset ownership changed after preparation"
        );
        ensure!(
            catalog.asset_video_validation(asset_id)?.status == AssetVideoValidationStatus::Usable,
            "native speech asset video became unusable after preparation"
        );
        ensure!(
            Catalog::require_proxy(&asset)? == self.proxy_path,
            "native speech proxy path changed after preparation"
        );
        let (current, blockers, _) = source_scope(catalog, &asset, AnalysisPayload::Audio, false);
        ensure!(
            blockers.is_empty() && current == self.approved_scope,
            "native speech source identity changed after preparation"
        );
        Ok(())
    }
}

enum NativeSpeechMedia {
    NoAudio,
    NoSpeech,
    Speech {
        pcm: Vec<f32>,
        windows: Vec<PackedWindow>,
        speech_duration_ms: u32,
        region_count: usize,
    },
}

impl fmt::Debug for NativeSpeechPreparation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeSpeechPreparation")
            .field("evidence", &self.evidence)
            .field("outcome", &self.outcome())
            .field("speech_duration_ms", &self.speech_duration_ms())
            .field("window_count", &self.window_count())
            .field("region_count", &self.region_count())
            .finish_non_exhaustive()
    }
}

/// One locally prepared native-speech candidate. The result owns both the
/// selected proxy claim and the per-asset speech claim until the future caller
/// has finished its cloud work or drops the result.
pub(crate) struct NativeSpeechPreparation {
    evidence: NativeSpeechPreparationEvidence,
    media: NativeSpeechMedia,
    source_guard: NativeSpeechSourceGuard,
    _proxy_claim: RepresentationClaim,
    _speech_claim: RepresentationClaim,
}

impl NativeSpeechPreparation {
    pub(crate) fn evidence(&self) -> &NativeSpeechPreparationEvidence {
        &self.evidence
    }

    pub(crate) fn outcome(&self) -> NativeSpeechLocalOutcome {
        match self.media {
            NativeSpeechMedia::NoAudio => NativeSpeechLocalOutcome::NoAudio,
            NativeSpeechMedia::NoSpeech => NativeSpeechLocalOutcome::NoSpeech,
            NativeSpeechMedia::Speech { .. } => NativeSpeechLocalOutcome::PreparedSpeech,
        }
    }

    pub(crate) fn speech_duration_ms(&self) -> u32 {
        match &self.media {
            NativeSpeechMedia::Speech {
                speech_duration_ms, ..
            } => *speech_duration_ms,
            NativeSpeechMedia::NoAudio | NativeSpeechMedia::NoSpeech => 0,
        }
    }

    pub(crate) fn window_count(&self) -> usize {
        match &self.media {
            NativeSpeechMedia::Speech { windows, .. } => windows.len(),
            NativeSpeechMedia::NoAudio | NativeSpeechMedia::NoSpeech => 0,
        }
    }

    pub(crate) fn region_count(&self) -> usize {
        match &self.media {
            NativeSpeechMedia::Speech { region_count, .. } => *region_count,
            NativeSpeechMedia::NoAudio | NativeSpeechMedia::NoSpeech => 0,
        }
    }

    pub(crate) fn windows(&self) -> &[PackedWindow] {
        match &self.media {
            NativeSpeechMedia::Speech { windows, .. } => windows,
            NativeSpeechMedia::NoAudio | NativeSpeechMedia::NoSpeech => &[],
        }
    }

    /// Materializes one strict bounded native WAV on demand. No per-window WAV
    /// buffers are retained while a clip waits for its future caller.
    pub(crate) fn materialize_window(&self, index: usize) -> Result<NativeSttAudioWindow> {
        let NativeSpeechMedia::Speech { pcm, windows, .. } = &self.media else {
            bail!("native speech preparation has no speech windows to materialize");
        };
        let window = windows
            .get(index)
            .ok_or_else(|| anyhow::anyhow!("native speech window index {index} is out of range"))?;
        NativeSttAudioWindow::from_pcm(pcm, window)
    }

    /// Recheck the exact proxy identity before a future caller uploads audio.
    /// Claims remain coordination only; this is the independent source proof.
    pub(crate) fn validate_source_current(&self, catalog: &Catalog) -> Result<()> {
        self.source_guard
            .validate(catalog, &self.evidence.asset_id, &self.evidence.project_id)
    }
}

type DetectorLoader = Box<dyn FnMut(&Path, usize) -> Result<Box<dyn SpeechDetector>>>;

#[cfg(test)]
type TestProbeHook = Box<dyn FnMut(&Path) -> Result<bool>>;
#[cfg(test)]
type TestExtractHook = Box<dyn FnMut(&Path, &Path, &mut dyn FnMut() -> bool) -> Result<()>>;

/// Owns the lazily initialized standalone VAD so a future sequential batch can
/// reuse one loaded context. It deliberately does not own a Whisper ASR engine.
pub(crate) struct NativeSpeechPreparer {
    detector: Option<Box<dyn SpeechDetector>>,
    detector_configuration: Option<(PathBuf, usize)>,
    loader: DetectorLoader,
    #[cfg(test)]
    probe_hook: Option<TestProbeHook>,
    #[cfg(test)]
    extract_hook: Option<TestExtractHook>,
}

impl Default for NativeSpeechPreparer {
    fn default() -> Self {
        Self {
            detector: None,
            detector_configuration: None,
            loader: Box::new(|path, threads| Ok(Box::new(SileroVad::load(path, threads)?))),
            #[cfg(test)]
            probe_hook: None,
            #[cfg(test)]
            extract_hook: None,
        }
    }
}

impl NativeSpeechPreparer {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Prepare one selected proxy for a frozen, already authorized native speech
    /// policy. All validation before local effects is independent of credentials
    /// and provider transport.
    pub(crate) fn prepare(
        &mut self,
        catalog: &Catalog,
        policy_id: &str,
        asset_id: &str,
        overrides: &ResourceOverrides,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<NativeSpeechPreparation> {
        check_cancelled(is_cancelled)?;
        ensure!(
            !catalog.read_only,
            "native speech preparation requires an explicitly writable catalog"
        );

        crate::calibration::validate_resource_overrides(SPEECH_REPRESENTATION_KIND, overrides)?;
        let connection = catalog.connection()?;
        let (authorization, _) = load_run_authorization_on_connection(&connection, policy_id)?;
        drop(connection);
        let policy = &authorization.policy;
        let request = validate_native_policy(policy, asset_id)?;
        let recipe = native_stt_transcript_recipe(&request)?;
        ensure!(
            recipe == policy.recipe_version,
            "authorized native speech recipe does not match the selected provider/model"
        );
        let backend = resolve_speech_backend(&SpeechBackendSelection::NativeOpenRouter {
            provider: request.provider().to_string(),
            model: request.model().to_string(),
        })?;
        ensure!(
            matches!(backend, ResolvedSpeechBackend::NativeOpenRouter { .. }),
            "native speech backend registry did not resolve the selected endpoint"
        );

        let asset = catalog.get_asset(asset_id)?;
        validate_asset_scope(catalog, policy, &asset)?;
        check_cancelled(is_cancelled)?;

        let proxy_claim = catalog.claim_representation(asset_id, RepresentationKind::Proxy)?;
        let speech_claim = catalog.claim_representation(asset_id, RepresentationKind::Speech)?;
        // The selected source must still be exactly what the frozen policy
        // approved after both coordination claims are in place.
        check_cancelled(is_cancelled)?;
        validate_asset_scope(catalog, policy, &asset)?;

        let source = catalog.analysis_proxy_source(&asset)?;
        let approved_scope = policy
            .scope
            .iter()
            .find(|scope| scope.asset_id == asset_id)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!("asset is outside the authorized native speech scope")
            })?;
        let source_guard = NativeSpeechSourceGuard {
            proxy_path: source.proxy.clone(),
            approved_scope: approved_scope.clone(),
        };
        let mut evidence = NativeSpeechPreparationEvidence {
            policy_id: policy.policy_id.clone(),
            project_id: policy.project_id.clone(),
            asset_id: asset.id.clone(),
            provider: request.provider().to_string(),
            model: request.model().to_string(),
            recipe,
            scope: approved_scope,
            proxy_full_fingerprint: source.fingerprint.clone(),
            proxy_sampled_fingerprint: source.sampled_fingerprint.clone(),
            source_duration_ms: source.duration_ms,
            last_safe_sample_ms: source.last_safe_sample_ms,
            vad: None,
        };

        let (media, vad) =
            self.prepare_media(catalog, &source, asset_id, overrides, is_cancelled)?;
        source_guard.validate(catalog, asset_id, &policy.project_id)?;
        evidence.vad = vad;
        Ok(NativeSpeechPreparation {
            evidence,
            media,
            source_guard,
            _proxy_claim: proxy_claim,
            _speech_claim: speech_claim,
        })
    }

    fn prepare_media(
        &mut self,
        catalog: &Catalog,
        source: &crate::representation::AnalysisProxySource,
        asset_id: &str,
        overrides: &ResourceOverrides,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(NativeSpeechMedia, Option<NativeVadEvidence>)> {
        check_cancelled(is_cancelled)?;
        let audio_present = self.probe_audio(catalog, &source.proxy)?;
        check_cancelled(is_cancelled)?;
        if !audio_present {
            return Ok((NativeSpeechMedia::NoAudio, None));
        }

        let selection = catalog.resolve_resource_selection(
            SPEECH_REPRESENTATION_KIND,
            STT_RECIPE_KEY,
            None,
            None,
            overrides,
        )?;
        let vad_threads = validate_vad_threads(selection.vad_threads)? as usize;
        check_cancelled(is_cancelled)?;

        let heavy = match catalog.acquire_local_heavy_cancellable(&mut *is_cancelled) {
            Ok(lease) => lease,
            Err(error) if error.is::<ResourceWaitCancelled>() => {
                return Err(NativeSpeechPreparationCanceled.into());
            }
            Err(error) => return Err(error),
        };
        let scratch = NativeAudioScratch::new(catalog, asset_id)?;
        check_cancelled(is_cancelled)?;
        let extraction = self.extract_audio(&source.proxy, scratch.path(), is_cancelled);
        check_cancelled(is_cancelled)?;
        extraction?;
        let pcm = read_extracted_pcm(scratch.path())?;
        check_cancelled(is_cancelled)?;

        let (mut regions, vad) =
            self.run_vad(catalog, vad_threads, &pcm, is_cancelled, &selection)?;
        check_cancelled(is_cancelled)?;

        let windows = plan_native_stt_speech_windows(&pcm, |_| Ok(std::mem::take(&mut regions)))?;
        ensure!(
            windows
                .iter()
                .flat_map(|window| &window.regions)
                .all(|region| region
                    .source_start_ms
                    .checked_add(region.duration_ms)
                    .is_some_and(|end| end <= source.duration_ms)),
            "native speech window exceeds the selected source duration"
        );
        let media = if windows.is_empty() {
            NativeSpeechMedia::NoSpeech
        } else {
            let speech_duration_ms = windows
                .iter()
                .flat_map(|window| window.regions.iter())
                .try_fold(0_u32, |total, region| total.checked_add(region.duration_ms))
                .ok_or_else(|| anyhow::anyhow!("native speech duration overflowed"))?;
            let region_count = windows.iter().map(|window| window.regions.len()).sum();
            NativeSpeechMedia::Speech {
                pcm,
                windows,
                speech_duration_ms,
                region_count,
            }
        };

        // This lease covers local extraction/VAD only, and is gone before the
        // prepared guard is exposed to a future cloud caller.
        drop(heavy);
        Ok((media, Some(vad)))
    }

    fn probe_audio(&mut self, catalog: &Catalog, proxy_path: &Path) -> Result<bool> {
        #[cfg(test)]
        if let Some(hook) = self.probe_hook.as_mut() {
            return hook(proxy_path);
        }
        catalog.transcription_audio_present(proxy_path)
    }

    fn extract_audio(
        &mut self,
        source: &Path,
        destination: &Path,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<()> {
        #[cfg(test)]
        if let Some(hook) = self.extract_hook.as_mut() {
            return hook(source, destination, is_cancelled);
        }
        crate::analysis::audio::FfmpegAudioExtractor.extract_pcm16k_cancellable(
            source,
            destination,
            is_cancelled,
        )
    }

    fn run_vad(
        &mut self,
        catalog: &Catalog,
        threads: usize,
        pcm: &[f32],
        is_cancelled: &mut dyn FnMut() -> bool,
        selection: &crate::ResourceSelection,
    ) -> Result<(Vec<SpeechRegion>, NativeVadEvidence)> {
        check_cancelled(is_cancelled)?;
        let directory = model_dir_for(catalog.database_path(), &VAD_MODEL)?;
        let model_path = ensure_model(&directory, &VAD_MODEL)?;
        let configuration = (model_path, threads);
        if self.detector_configuration.as_ref() != Some(&configuration) {
            self.detector = Some((self.loader)(&configuration.0, threads)?);
            self.detector_configuration = Some(configuration);
        }
        check_cancelled(is_cancelled)?;
        let regions = self
            .detector
            .as_mut()
            .expect("VAD detector initialized")
            .detect_speech(pcm)?;
        check_cancelled(is_cancelled)?;
        Ok((
            regions,
            NativeVadEvidence {
                model_file: VAD_MODEL.file_name,
                threads,
                resource_source: selection.source.clone(),
                calibration_key: selection.calibration_key.clone(),
                sample_compatibility: selection.sample_compatibility.clone(),
            },
        ))
    }
}

fn check_cancelled(is_cancelled: &mut dyn FnMut() -> bool) -> Result<()> {
    if is_cancelled() {
        return Err(NativeSpeechPreparationCanceled.into());
    }
    Ok(())
}

fn validate_native_policy(policy: &RunPolicy, asset_id: &str) -> Result<NativeSttRequest> {
    ensure!(
        policy.kind == crate::AnalysisRunKind::NativeSpeechTranscription,
        "authorized policy is not native speech transcription"
    );
    ensure!(
        policy.payload == AnalysisPayload::Audio,
        "authorized native speech policy payload is not audio"
    );
    ensure!(
        policy
            .scope
            .iter()
            .filter(|scope| scope.asset_id == asset_id)
            .count()
            == 1,
        "authorized native speech policy must contain exactly one entry for the requested asset"
    );
    ensure!(
        !policy.allow_fallback
            && policy.permitted_fallbacks.is_empty()
            && policy.allowed_models == vec![policy.primary_model.clone()],
        "authorized native speech policy must select exactly one model without fallback"
    );
    ensure!(
        policy.readiness.ready_for_authorization && policy.readiness.budget_ready,
        "authorized native speech policy is not ready"
    );
    ensure!(
        policy.budget_usd.is_finite() && policy.budget_usd > 0.0,
        "authorized native speech budget is invalid"
    );
    ensure!(
        policy.expected_cost_usd.is_finite()
            && policy.expected_cost_usd >= 0.0
            && policy.expected_cost_usd <= policy.budget_usd,
        "authorized native speech cost exceeds its budget"
    );
    ensure!(
        policy.provider_cost_bound_contract == crate::native_stt_cost_bound_contract_id(),
        "authorized native speech provider cost contract changed"
    );
    let RunPolicyParameters::NativeSpeechTranscription {
        language,
        provider_policy,
        ..
    } = &policy.parameters
    else {
        bail!("authorized policy is missing native speech parameters");
    };
    let request = NativeSttRequest::new(
        policy.provider.clone(),
        policy.primary_model.clone(),
        language.clone(),
        0.0,
        *provider_policy,
    )?;
    Ok(request)
}

fn validate_asset_scope(catalog: &Catalog, policy: &RunPolicy, asset: &Asset) -> Result<()> {
    ensure!(
        asset.project_id == policy.project_id && !asset.removed,
        "native speech asset is outside the authorized project or removed"
    );
    ensure!(
        catalog.asset_video_validation(&asset.id)?.status == AssetVideoValidationStatus::Usable,
        "native speech asset video is not usable"
    );
    catalog.validate_run_policy_asset(policy, &asset.id)
}

fn read_extracted_pcm(path: &Path) -> Result<Vec<f32>> {
    let mut reader = WavReader::open(path).context("open extracted speech audio")?;
    let spec = reader.spec();
    ensure!(
        spec.sample_rate == crate::analysis::speech_audio::SPEECH_SAMPLE_RATE_HZ,
        "extracted speech audio must be 16 kHz"
    );
    ensure!(
        spec.channels == crate::analysis::speech_audio::SPEECH_CHANNELS,
        "extracted speech audio must be mono"
    );
    ensure!(
        spec.bits_per_sample == crate::analysis::speech_audio::SPEECH_BITS_PER_SAMPLE
            && spec.sample_format == SampleFormat::Int,
        "extracted speech audio must be signed 16-bit PCM"
    );
    let samples = reader
        .samples::<i16>()
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("read extracted PCM samples")?;
    ensure!(
        !samples.is_empty(),
        "audio stream decoded to no PCM samples"
    );
    Ok(samples
        .into_iter()
        .map(|sample| f32::from(sample) / f32::from(i16::MAX))
        .collect())
}

struct NativeAudioScratch {
    path: PathBuf,
}

impl NativeAudioScratch {
    fn new(catalog: &Catalog, asset_id: &str) -> Result<Self> {
        let parent = catalog
            .database_path()
            .parent()
            .ok_or_else(|| anyhow::anyhow!("catalog has no parent directory"))?;
        let audio_dir = parent.join("cache").join("audio");
        std::fs::create_dir_all(&audio_dir).context("create native speech scratch")?;
        Ok(Self {
            path: audio_dir.join(format!(
                "{asset_id}.{}.native-16k.wav",
                uuid::Uuid::new_v4().simple()
            )),
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for NativeAudioScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests;
