//! Focused cross-protocol boundary regressions for the actual provider wrappers.

use super::*;
use crate::analysis::openrouter_stt::native_stt_request_cost_bound;

struct NativeQuoteVlmProvider {
    quote: AnalysisRequestCostBound,
    bound_calls: AtomicUsize,
    prepare_calls: AtomicUsize,
    analyze_calls: AtomicUsize,
}

impl NativeQuoteVlmProvider {
    fn new(quote: AnalysisRequestCostBound) -> Self {
        Self {
            quote,
            bound_calls: AtomicUsize::new(0),
            prepare_calls: AtomicUsize::new(0),
            analyze_calls: AtomicUsize::new(0),
        }
    }
}

impl VlmProvider for NativeQuoteVlmProvider {
    fn prepare(&self) -> anyhow::Result<()> {
        self.prepare_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn analysis_cost_bound(
        &self,
        _model: &str,
        _request: &VlmRequest,
    ) -> anyhow::Result<AnalysisRequestCostBound> {
        self.bound_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.quote.clone())
    }

    fn analyze(&self, _model: &str, _request: &VlmRequest) -> anyhow::Result<VlmResponse> {
        self.analyze_calls.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("native audio quote must never reach the VLM analyze call")
    }
}

#[test]
fn accounted_vlm_wrapper_rejects_native_audio_quote_for_initial_and_targeted_paths() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(
            &project,
            "boundary-vlm-native-quote",
            1.0,
        ))
        .unwrap();
    let run_id = catalog
        .begin_accounted_analysis_run(&project, "", "pilot")
        .unwrap();

    let native_request = native_request("en", 0.0, SttDataCollection::Deny);
    let native_audio = native_audio_window(0.1, 0, 1_000);
    let native_quote = native_stt_request_cost_bound(&native_request, &native_audio).unwrap();
    assert!(
        native_quote.audio.is_some(),
        "fixture must exercise the native duration component"
    );
    let provider = NativeQuoteVlmProvider::new(native_quote);

    let mut context = operation_context(
        &project,
        "boundary-vlm-native-quote",
        "synthetic-vlm-intent",
        1.0,
    );
    context.run_id = run_id.clone();
    let error = AccountedVlmProvider::new(&catalog, context, &provider)
        .analyze(DEFAULT_MODEL, &boundary_request())
        .unwrap_err();
    assert_eq!(
        ProviderFailure::from_anyhow(&error).category,
        ProviderFailureCategory::LocalAdmission
    );

    let targeted = AnalysisTargetedRetryAdmission {
        project_id: project.clone(),
        prior_request_key: "synthetic-unused-prior".into(),
        expected_next_attempt_epoch: 1,
        run_id,
    };
    let error = AccountedVlmProvider::new_targeted_retry(&catalog, targeted, &provider)
        .analyze(DEFAULT_MODEL, &boundary_request())
        .unwrap_err();
    assert_eq!(
        ProviderFailure::from_anyhow(&error).category,
        ProviderFailureCategory::LocalAdmission
    );

    assert_eq!(provider.bound_calls.load(Ordering::SeqCst), 2);
    assert_eq!(provider.prepare_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.analyze_calls.load(Ordering::SeqCst), 0);
    assert!(
        catalog
            .list_analysis_requests(&project, None, 10)
            .unwrap()
            .is_empty(),
        "typed local rejection must occur before any canonical ledger request"
    );
}
