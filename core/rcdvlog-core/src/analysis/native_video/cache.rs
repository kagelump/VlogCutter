//! Durable, local request evidence; canonical money remains in accounting.
use super::{media::NativeVideoMedia, protocol::PreparedRequest, *};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use std::fs;

pub(crate) struct WindowOwner {
    pub evidence_id: String,
    pub run_id: String,
    pub provenance: NativeVideoProvenance,
    pub request: NativeVideoWindowRequest,
    _claim: fs::File,
}
pub(crate) enum Claim {
    Cached(Box<NativeVideoWindowResult>),
    Ready(Box<WindowOwner>),
}
impl crate::Catalog {
    /// Retain an observed response before financial settlement and editorial
    /// validation. This is raw evidence only; it cannot become a completed
    /// candidate artifact until the shared receipt owner succeeds.
    pub(crate) fn stage_native_video_response(
        &self,
        evidence_id: &str,
        run_id: &str,
        call: &super::protocol::CallResult,
    ) -> Result<()> {
        ensure!(
            call.raw_response.len() <= super::protocol::MAX_RESPONSE_BYTES,
            "native response exceeds bounded size"
        );
        let (body,key):(String,Option<String>)=self.connection()?.query_row("SELECT provenance_json,request_key FROM native_video_calls WHERE evidence_id=?1 AND run_id=?2 AND state='reserved'",params![evidence_id,run_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let mut provenance: NativeVideoProvenance = serde_json::from_str(&body)?;
        provenance.request_key = key;
        provenance.http_status = Some(call.status);
        provenance.actual_model =
            (!call.receipt.actual_model.is_empty()).then(|| call.receipt.actual_model.clone());
        provenance.reported_tier = call
            .receipt
            .usage_pricing
            .as_ref()
            .and_then(|u| u.reported_tier.clone());
        provenance.usage = call.receipt.usage_pricing.clone();
        let written=self.connection()?.execute("UPDATE native_video_calls SET raw_response=?3,provenance_json=?4,updated_at=?5 WHERE evidence_id=?1 AND run_id=?2 AND state='reserved'",params![evidence_id,run_id,call.raw_response,serde_json::to_string(&provenance)?,crate::now()])?;
        ensure!(
            written == 1,
            "native response no longer owns its durable window"
        );
        Ok(())
    }
    pub(crate) fn claim_native_video_window(
        &self,
        policy: &crate::RunPolicy,
        run_id: &str,
        media: &NativeVideoMedia,
        wire: &PreparedRequest,
    ) -> Result<Claim> {
        let crate::RunPolicyParameters::NativeVideoReview { options } = &policy.parameters else {
            anyhow::bail!("native policy missing");
        };
        let asset_scope = policy
            .scope
            .iter()
            .find(|s| s.asset_id == media.view.asset_id)
            .context("native source scope missing")?;
        let semantic=blake3::hash(&serde_json::to_vec(&serde_json::json!({"projectId":policy.project_id,"sourceIdentity":asset_scope.source_identity,
            "assetId":media.view.asset_id,"sourceIn":media.request.source_in_seconds,"sourceOut":media.request.source_out_seconds,"fps":media.request.fps,"maxWidth":media.request.max_width,
            "purpose":options.purpose,"provider":policy.provider,"model":policy.primary_model,"tier":options.tier,"thinking":options.thinking,"maxOutputTokens":options.max_output_tokens,
            "payload":policy.payload,"recipe":NATIVE_VIDEO_MEDIA_RECIPE,"prompt":NATIVE_VIDEO_PROMPT_VERSION,"schema":NATIVE_VIDEO_SCHEMA_VERSION}))?).to_hex().to_string();
        let input = blake3::hash(&serde_json::to_vec(&(&semantic, &wire.fingerprint))?)
            .to_hex()
            .to_string();
        let evidence_id = format!("native_video_{input}");
        let parent = self
            .database_path()
            .parent()
            .context("catalog has no media root")?;
        let root = super::media::owned_directory(parent, &["cache", "native-video", "claims"])?;
        let path = root.join(format!("{semantic}.lock"));
        if let Ok(m) = fs::symlink_metadata(&path) {
            ensure!(
                !m.file_type().is_symlink(),
                "native call claim cannot follow symlink"
            );
        }
        let claim = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        fs2::FileExt::try_lock_exclusive(&claim)
            .context("native window already has a running owner")?;
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prior:Option<(String,String,Option<String>,Option<String>)>=tx.query_row("SELECT input_identity,state,CASE WHEN length(CAST(result_json AS BLOB))<=3145728 THEN result_json END,request_key FROM native_video_calls WHERE project_id=?1 AND semantic_identity=?2 ORDER BY created_at LIMIT 1",params![policy.project_id,semantic],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        if let Some((prior_input, state, result, request_key)) = prior {
            ensure!(
                prior_input == input,
                "native media changed for prior request evidence; explicit recovery is required before any resend"
            );
            if matches!(state.as_str(), "completed" | "rejected") {
                let mut result: NativeVideoWindowResult =
                    serde_json::from_str(&result.context(
                        "native completed request body is unavailable; recovery required",
                    )?)?;
                ensure!(
                    result.evidence_id == evidence_id
                        && result.provenance.media_fingerprint == media.view.output_fingerprint,
                    "native cached request identity is inconsistent; recovery required"
                );
                result.reused = true;
                tx.commit()?;
                return Ok(Claim::Cached(Box::new(result)));
            }
            // No linked reservation and no output is an unadmitted local
            // attempt only. The canonical orphan check below proves that no
            // marker/receipt exists before reclaiming it. A released linked
            // reservation still requires the accounting retry owner.
            ensure!(
                request_key.is_none()
                    && result.is_none()
                    && matches!(state.as_str(), "prepared" | "interrupted"),
                "native request is incomplete or previously attempted; inspect/reconcile its canonical receipt before explicit recovery"
            );
        }
        // Covers a crash after reservation but before this evidence row was
        // linked. A fresh run or policy cannot erase that paid request.
        let orphan:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM analysis_request_reservations WHERE project_id=?1 AND payload_fingerprint=?2)",params![policy.project_id,wire.fingerprint],|r|r.get(0))?;
        ensure!(
            !orphan,
            "prior native request accounting exists without recoverable output; receipt recovery is required"
        );
        let provenance = NativeVideoProvenance {
            policy_id: policy.policy_id.clone(),
            asset_id: media.view.asset_id.clone(),
            source_fingerprint: asset_scope.source_fingerprint.clone(),
            source_identity: asset_scope.source_identity.clone(),
            representation_fingerprint: media.selected.fingerprint.clone(),
            media_fingerprint: media.view.output_fingerprint.clone(),
            media_recipe: NATIVE_VIDEO_MEDIA_RECIPE.into(),
            prompt_version: NATIVE_VIDEO_PROMPT_VERSION.into(),
            schema_version: NATIVE_VIDEO_SCHEMA_VERSION.into(),
            clock: media.view.clock.clone(),
            requested_model: policy.primary_model.clone(),
            actual_model: None,
            requested_tier: options.tier,
            reported_tier: None,
            http_status: None,
            request_key: None,
            usage: None,
            limitations: media.view.limitations.clone(),
        };
        let owned=tx.execute("INSERT INTO native_video_calls(evidence_id,project_id,policy_id,asset_id,input_identity,semantic_identity,run_id,state,request_json,provenance_json,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,'prepared',?8,?9,?10,?10) ON CONFLICT(evidence_id) DO UPDATE SET policy_id=excluded.policy_id,run_id=excluded.run_id,state='prepared',request_json=excluded.request_json,provenance_json=excluded.provenance_json,cancel_requested=0,failure_code=NULL,updated_at=excluded.updated_at WHERE native_video_calls.request_key IS NULL AND native_video_calls.result_json IS NULL AND native_video_calls.raw_response IS NULL",
            params![evidence_id,policy.project_id,policy.policy_id,media.view.asset_id,input,semantic,run_id,serde_json::to_string(&media.request)?,serde_json::to_string(&provenance)?,crate::now()])?;
        ensure!(owned == 1, "native unadmitted window ownership changed");
        tx.commit()?;
        Ok(Claim::Ready(Box::new(WindowOwner {
            evidence_id,
            run_id: run_id.into(),
            provenance,
            request: media.request.clone(),
            _claim: claim,
        })))
    }
    pub(crate) fn complete_native_video_window(
        &self,
        owner: &WindowOwner,
        call: super::protocol::CallResult,
        request_key: String,
        media: &NativeVideoMedia,
    ) -> Result<NativeVideoWindowResult> {
        self.complete_native_video_window_mode(owner, call, request_key, media, false)
    }
    fn complete_native_video_window_mode(
        &self,
        owner: &WindowOwner,
        call: super::protocol::CallResult,
        request_key: String,
        media: &NativeVideoMedia,
        explicit_review: bool,
    ) -> Result<NativeVideoWindowResult> {
        let mut provenance = owner.provenance.clone();
        provenance.http_status = Some(call.status);
        provenance.actual_model =
            (!call.receipt.actual_model.is_empty()).then_some(call.receipt.actual_model.clone());
        provenance.reported_tier = call
            .receipt
            .usage_pricing
            .as_ref()
            .and_then(|u| u.reported_tier.clone());
        provenance.usage = call.receipt.usage_pricing.clone();
        provenance.request_key = Some(request_key.clone());
        let current = {
            let _heavy = self.acquire_local_heavy()?;
            crate::analysis::run_policy::validate_native_video_asset_mode(
                self,
                &self.load_run_authorization(&provenance.policy_id)?.policy,
                &provenance.asset_id,
                explicit_review,
                &|| false,
            )
        };
        let parsed = current
            .and_then(|_| call.output.map_err(anyhow::Error::msg))
            .and_then(|body| super::reply::parse(&body, &provenance.clock, &media.witness));
        let (mut candidates, flags, rejection_reason) = match parsed {
            Ok(reply) => (reply.candidates, reply.flags, None),
            Err(error) => (
                vec![],
                vec![],
                Some(if !(200..300).contains(&call.status) {
                    "provider_http_failure".into()
                } else {
                    format!(
                        "native_response_rejected: {}",
                        error.to_string().chars().take(240).collect::<String>()
                    )
                }),
            ),
        };
        // Provider-local ids are not globally stable across source windows.
        for candidate in &mut candidates {
            candidate.id = format!(
                "nv_{}",
                blake3::hash(&serde_json::to_vec(&(&owner.evidence_id, &candidate.id))?).to_hex()
            );
        }
        let result = NativeVideoWindowResult {
            evidence_id: owner.evidence_id.clone(),
            state: if rejection_reason.is_none() {
                "completed".into()
            } else {
                "rejected".into()
            },
            request: owner.request.clone(),
            provenance,
            candidates,
            flags,
            rejection_reason,
            reused: false,
        };
        let changed=self.connection()?.execute("UPDATE native_video_calls SET state=?3,request_key=?4,provenance_json=?5,raw_response=?6,result_json=?7,updated_at=?8,failure_code=NULL WHERE evidence_id=?1 AND run_id=?2 AND state IN ('prepared','reserved','interrupted')",
            params![owner.evidence_id,owner.run_id,result.state,request_key,serde_json::to_string(&result.provenance)?,call.raw_response,serde_json::to_string(&result)?,crate::now()])?;
        ensure!(changed == 1, "native request lost its durable result owner");
        Ok(result)
    }
    pub(crate) fn interrupt_native_video_window(&self, owner: &WindowOwner) -> Result<()> {
        self.connection()?.execute("UPDATE native_video_calls SET state='interrupted',failure_code='native_request_incomplete',updated_at=?3 WHERE evidence_id=?1 AND run_id=?2 AND state IN ('prepared','reserved','interrupted')",params![owner.evidence_id,owner.run_id,crate::now()])?;
        Ok(())
    }
    /// Bounded persisted evidence lookup. It does not recover a process, hash
    /// media, mutate states or claim that an old result is current.
    pub fn native_video_window_result(
        &self,
        project_id: &str,
        evidence_id: &str,
    ) -> Result<Option<NativeVideoWindowResult>> {
        self.require_project(project_id)?;
        let row: Option<String> = self
            .connection()?
            .query_row(
                "SELECT result_json FROM native_video_calls WHERE project_id=?1 AND evidence_id=?2 AND length(CAST(result_json AS BLOB))<=3145728",
                params![project_id, evidence_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        row.map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }
    /// Explicit bounded inspection of the unchanged returned body. It contains
    /// provider output, never the uploaded media, prompt body or credentials.
    pub fn native_video_raw_response(
        &self,
        project_id: &str,
        evidence_id: &str,
    ) -> Result<Option<NativeVideoRawResponse>> {
        self.require_project(project_id)?;
        let row:Option<(Option<String>,String)>=self.connection()?.query_row("SELECT raw_response,provenance_json FROM native_video_calls WHERE project_id=?1 AND evidence_id=?2 AND length(CAST(raw_response AS BLOB))<=2097152 AND length(CAST(provenance_json AS BLOB))<=1048576",params![project_id,evidence_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((Some(body), provenance)) = row else {
            return Ok(None);
        };
        ensure!(
            body.len() <= super::protocol::MAX_RESPONSE_BYTES && provenance.len() <= 1024 * 1024,
            "native response exceeds bounded size"
        );
        let provenance: NativeVideoProvenance = serde_json::from_str(&provenance)?;
        Ok(Some(NativeVideoRawResponse {
            evidence_id: evidence_id.into(),
            http_status: provenance.http_status,
            body,
        }))
    }
    /// Recover only already observed bytes. This creates no transport, resolves
    /// no key, admits no request and never releases uncertain usage by inference.
    pub fn recover_native_video_window(
        &self,
        project_id: &str,
        evidence_id: &str,
    ) -> Result<NativeVideoWindowResult> {
        self.require_project(project_id)?;
        let (policy_id,run_id,request,provenance,semantic):(String,String,String,String,String)=self.connection()?.query_row("SELECT policy_id,run_id,request_json,provenance_json,semantic_identity FROM native_video_calls WHERE project_id=?1 AND evidence_id=?2 AND length(CAST(request_json AS BLOB))<=65536 AND length(CAST(provenance_json AS BLOB))<=1048576",params![project_id,evidence_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        let _policy = super::runtime::claim_policy(self, &policy_id)?;
        ensure!(
            semantic.len() == 64 && semantic.bytes().all(|b| b.is_ascii_hexdigit()),
            "native recovery identity is malformed"
        );
        let parent = self
            .database_path()
            .parent()
            .context("catalog has no local root")?;
        let path = super::media::owned_directory(parent, &["cache", "native-video", "claims"])?
            .join(format!("{semantic}.lock"));
        if let Ok(m) = fs::symlink_metadata(&path) {
            ensure!(
                !m.file_type().is_symlink(),
                "native recovery cannot follow claim symlink"
            );
        }
        let claim = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        fs2::FileExt::try_lock_exclusive(&claim)
            .context("native response still has a running owner")?;
        let policy = self.load_run_authorization(&policy_id)?.policy;
        ensure!(
            policy.project_id == project_id,
            "native response is outside the project"
        );
        let provenance: NativeVideoProvenance = serde_json::from_str(&provenance)?;
        let request: NativeVideoWindowRequest = serde_json::from_str(&request)?;
        let media = self.prepare_native_video_window_mode(&policy, &request, true, &|| false)?;
        ensure!(
            media.view.output_fingerprint == provenance.media_fingerprint
                && media.view.clock == provenance.clock,
            "native recovery cannot reconstruct the exact observed media/clock; no resend was performed"
        );
        if let Some(mut existing) = self.native_video_window_result(project_id, evidence_id)? {
            self.refresh_native_video_pilot_after_recovery(&policy, &run_id, &existing)?;
            existing.reused = true;
            return Ok(existing);
        }
        let raw = self
            .native_video_raw_response(project_id, evidence_id)?
            .context("native response bytes are unavailable; no resend was performed")?;
        let key = provenance
            .request_key
            .clone()
            .context("native response has no canonical request key")?;
        let ledger = self
            .analysis_request(&key)?
            .context("native canonical request is missing")?;
        ensure!(
            ledger.policy_id == policy_id
                && ledger.run_id == run_id
                && ledger.dispatch_at.is_some()
                && ledger.lifecycle != crate::AnalysisRequestLifecycle::Released,
            "native recovery lacks its exact dispatched request"
        );
        let crate::RunPolicyParameters::NativeVideoReview { options } = &policy.parameters else {
            anyhow::bail!("native policy missing");
        };
        let mut call = super::protocol::decode_response(
            raw.http_status
                .context("native HTTP status evidence is missing")?,
            raw.body.into_bytes(),
            provenance
                .usage
                .as_ref()
                .and_then(|u| u.reported_tier_header.as_deref()),
            provenance.usage.as_ref().map_or(0, |u| u.latency_ms),
            options,
        )?;
        if call
            .receipt
            .usage_pricing
            .as_ref()
            .and_then(|u| u.reported_tier.as_ref())
            != provenance.reported_tier.as_ref()
            || (!call.receipt.actual_model.is_empty()).then_some(&call.receipt.actual_model)
                != provenance.actual_model.as_ref()
        {
            call.output = Err("stored_native_serving_evidence_is_not_recoverable");
        }
        call.receipt.usage_pricing = provenance.usage.clone();
        call.receipt.actual_model = provenance.actual_model.clone().unwrap_or_default();
        if ledger.lifecycle != crate::AnalysisRequestLifecycle::Completed {
            let settled = self.record_analysis_request_receipt(&key, &call.receipt)?;
            ensure!(
                settled.lifecycle == crate::AnalysisRequestLifecycle::Completed,
                "native response usage remains uncertain; reconcile its canonical receipt before domain recovery"
            );
        }
        let owner = WindowOwner {
            evidence_id: evidence_id.into(),
            run_id: run_id.clone(),
            request,
            provenance,
            _claim: claim,
        };
        let mut result = self.complete_native_video_window_mode(&owner, call, key, &media, true)?;
        self.connection()?.execute("UPDATE native_video_run_windows SET state=?3,evidence_id=?4,reused=1,failure_code='native_response_recovered_locally' WHERE run_id=?1 AND request_json=?2",params![run_id,serde_json::to_string(&owner.request)?,format!("recovered_{}",result.state),evidence_id])?;
        self.refresh_native_video_pilot_after_recovery(&policy, &run_id, &result)?;
        let _ = self.publish_native_video_moments(&policy, &result.provenance.asset_id);
        result.reused = true;
        Ok(result)
    }
}
