//! Deferred secure credentials for the one accounted native transmission.

use super::*;

/// Production native transport that deliberately resolves the existing secure
/// OpenRouter credential only from `NativeSttTransport::prepare`, i.e. after the
/// accounting reservation and immediately before the durable dispatch marker.
/// All-local silence and cached results never construct this client or read a key.
pub(crate) struct DeferredEnvironmentNativeSttTransport {
    client: OnceLock<std::result::Result<OpenRouterSttClient, ProviderFailure>>,
}

impl DeferredEnvironmentNativeSttTransport {
    pub(crate) fn new() -> Self {
        Self {
            client: OnceLock::new(),
        }
    }

    fn client(&self) -> Result<&OpenRouterSttClient> {
        match self.client.get_or_init(|| {
            OpenRouterSttClient::from_environment().map_err(|_| ProviderFailure::configuration())
        }) {
            Ok(client) => Ok(client),
            Err(failure) => Err(failure.clone().into()),
        }
    }
}

impl NativeSttTransport for DeferredEnvironmentNativeSttTransport {
    fn prepare(&self) -> Result<()> {
        NativeSttTransport::prepare(self.client()?)
    }

    fn transcribe(
        &self,
        request: &NativeSttRequest,
        audio: &crate::analysis::openrouter_stt::NativeSttAudioWindow,
    ) -> Result<crate::analysis::openrouter_stt::NativeSttCallResult> {
        self.client()?.transcribe(request, audio)
    }
}
