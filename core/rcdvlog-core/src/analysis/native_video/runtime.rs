//! Window planning and one owned execution. Queue/pilot share this exact leaf.
use super::*;
use super::{cache::Claim, transport::Transport};
use crate::analysis::accounting::native_video::AccountedNativeVideoProvider;
use crate::{Catalog, RunPolicy, RunPolicyParameters};
use anyhow::{Result, ensure};

pub(crate) fn claim_policy(catalog: &Catalog, policy_id: &str) -> Result<std::fs::File> {
    ensure!(
        policy_id.len() == 64 && policy_id.bytes().all(|b| b.is_ascii_hexdigit()),
        "native policy identity is malformed"
    );
    let root = super::media::owned_directory(
        catalog
            .database_path()
            .parent()
            .ok_or_else(|| anyhow::anyhow!("catalog has no local root"))?,
        &["cache", "native-video", "claims"],
    )?;
    let path = root.join(format!("policy_{policy_id}.lock"));
    if let Ok(m) = std::fs::symlink_metadata(&path) {
        ensure!(
            !m.file_type().is_symlink(),
            "native policy claim cannot follow symlink"
        );
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    fs2::FileExt::try_lock_exclusive(&file)
        .map_err(|_| anyhow::anyhow!("native policy already has a running owner"))?;
    Ok(file)
}

pub fn plan_native_video_windows(policy: &RunPolicy) -> Result<Vec<NativeVideoWindowRequest>> {
    let RunPolicyParameters::NativeVideoReview { options } = &policy.parameters else {
        anyhow::bail!("native video policy required");
    };
    validate_options(options)?;
    let mut windows = Vec::new();
    for (index, scope) in options.source_scopes.iter().enumerate() {
        let mut start = scope.source_in_seconds;
        while start < scope.source_out_seconds {
            ensure!(
                windows.len() < options.max_requests as usize,
                "native window count exceeds approved request cap"
            );
            let end = (start + f64::from(options.max_window_seconds)).min(scope.source_out_seconds);
            windows.push(NativeVideoWindowRequest {
                policy_id: policy.policy_id.clone(),
                scope_index: index as u32,
                source_in_seconds: start,
                source_out_seconds: end,
                fps: options.fps,
                max_width: options.max_width,
            });
            start = end;
        }
    }
    Ok(windows)
}

/// Local preparation has already released its heavy lease. The budget callback
/// waits only for temporary reservation headroom; it never cancels another
/// already-reserved request or schedules a second paid attempt.
pub(crate) struct ExecutionControl<'a> {
    pub cancelled: &'a dyn Fn() -> bool,
    pub wait_for_headroom: &'a dyn Fn() -> bool,
}
pub(crate) fn execute_window(
    catalog: &Catalog,
    policy: &RunPolicy,
    run_id: &str,
    operation: &str,
    media: &NativeVideoMedia,
    transport: &dyn Transport,
    control: ExecutionControl<'_>,
) -> Result<NativeVideoWindowResult> {
    let ExecutionControl {
        cancelled,
        wait_for_headroom,
    } = control;
    ensure!(
        !cancelled(),
        "native window cancelled before request ownership"
    );
    let RunPolicyParameters::NativeVideoReview { options } = &policy.parameters else {
        anyhow::bail!("native parameters missing");
    };
    let wire = protocol::prepare(options, media)?;
    let owner = match catalog.claim_native_video_window(policy, run_id, media, &wire)? {
        Claim::Cached(result) => return Ok(*result),
        Claim::Ready(owner) => owner,
    };
    let mut context = crate::analysis::run_policy::bulk_request_accounting_context(
        policy,
        run_id,
        &media.view.asset_id,
        format!(
            "native_video_review:{}:{}",
            media.view.asset_id, media.view.identity
        ),
        false,
    )?;
    context.operation = operation.into();
    let provider = AccountedNativeVideoProvider {
        catalog,
        policy,
        context,
        media,
        evidence_id: &owner.evidence_id,
        request: &wire,
        transport,
    };
    loop {
        match provider.transmit(cancelled) {
            Ok((key, call)) => {
                return catalog.complete_native_video_window(&owner, call, key, media);
            }
            Err(error)
                if error
                    .downcast_ref::<crate::AnalysisBudgetExhausted>()
                    .is_some()
                    && !cancelled()
                    && wait_for_headroom() =>
            {
                continue;
            }
            Err(error) => {
                catalog.interrupt_native_video_window(&owner)?;
                return Err(error);
            }
        }
    }
}
