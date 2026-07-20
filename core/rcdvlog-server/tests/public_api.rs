use rcdvlog_core::JobId;
use rcdvlog_server_core::{
    ClaimedJob, FailureKind, JobEngine, JobEngineError, JobHandler, JobHandlerFailure,
    JobRequirements, JobResultCommit, JobSnapshot, JobSnapshotError, ProgressEstimateInput,
    ServerCapability,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

struct ExternalHandler;

impl JobHandler for ExternalHandler {
    fn run(
        &self,
        _job: ClaimedJob,
        _cancellation: CancellationToken,
    ) -> Result<JobResultCommit, JobHandlerFailure> {
        Ok(JobResultCommit {
            output_identity: None,
            details: json!({}),
        })
    }
}

#[test]
fn external_handlers_can_construct_the_public_job_contract() {
    fn requires_handler<T: JobHandler>() {}

    requires_handler::<ExternalHandler>();
    let requirements = JobRequirements {
        schema_version: 1,
        requires_original: true,
        requires_derived_write: false,
        capabilities: vec![ServerCapability::Ffmpeg],
    };
    let failure = JobHandlerFailure {
        kind: FailureKind::Retryable,
        code: "retryable".into(),
        details: json!({}),
    };
    let estimate = ProgressEstimateInput {
        worker_count: 1,
        remaining_task_work_units: vec![10],
    };
    let snapshot_error = JobSnapshotError {
        code: "retryable".into(),
        details: json!({}),
    };
    let get_job: fn(&JobEngine, &JobId) -> Result<JobSnapshot, JobEngineError> = JobEngine::get_job;

    assert_eq!(requirements.capabilities, vec![ServerCapability::Ffmpeg]);
    assert_eq!(failure.code, "retryable");
    assert_eq!(estimate.remaining_task_work_units, vec![10]);
    assert_eq!(snapshot_error.code, "retryable");
    let _ = get_job;
}
