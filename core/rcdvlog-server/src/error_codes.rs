/// Stable error-code registry shared by the HTTP adapter and the machine-readable CLI.
/// Adding a client-visible code requires adding it here and updating the contract tests.
pub const STABLE_ERROR_CODES: &[&str] = &[
    "audit_write_failed",
    "backup_busy",
    "backup_canceled",
    "backup_incompatible",
    "backup_verification_failed",
    "baseline_different",
    "baseline_digest_mismatch",
    "baseline_invalid",
    "baseline_version_incompatible",
    "bootstrap_already_used",
    "bootstrap_not_found_or_expired",
    "callback_ack_failed",
    "callback_failed",
    "callback_wait_timeout",
    "conflict",
    "csrf_mismatch",
    "derived_root_unavailable",
    "device_not_found",
    "device_revoked",
    "forbidden",
    "idempotency_conflict",
    "incompatible_protocol",
    "internal_error",
    "invalid_arguments",
    "invalid_configuration",
    "invalid_forwarded_headers",
    "invalid_host",
    "invalid_input",
    "invalid_job_id",
    "invalid_media_object_id",
    "invalid_query",
    "job_canceled",
    "job_failed",
    "job_not_retryable",
    "job_wait_timeout",
    "local_admin_failed",
    "low_disk_space",
    "media_offline",
    "media_tool_failed",
    "mount_path_not_allowed",
    "mount_unavailable",
    "node_reported_failure",
    "not_found",
    "origin_mismatch",
    "pairing_already_used",
    "pairing_not_found_or_expired",
    "project_replica_not_found",
    "project_replica_path_unsafe",
    "rate_limited",
    "retryable_job_failure",
    "scan_already_active",
    "schema_incompatible",
    "server_already_running",
    "server_runtime_failed",
    "server_unreachable",
    "session_invalid",
    "session_revoked",
    "source_not_verified",
    "storage_library_error",
    "sync_page_too_large",
    "sync_project_mismatch",
    "sync_protocol_incompatible",
    "sync_schema_incompatible",
    "sync_sequence_gap",
    "sync_server_mismatch",
    "unauthorized",
];

pub fn is_stable_error_code(code: &str) -> bool {
    STABLE_ERROR_CODES.binary_search(&code).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_sorted_unique_and_contains_required_cross_surface_codes() {
        assert!(STABLE_ERROR_CODES.windows(2).all(|pair| pair[0] < pair[1]));
        for required in [
            "invalid_input",
            "not_found",
            "unauthorized",
            "incompatible_protocol",
            "media_offline",
            "mount_unavailable",
            "conflict",
            "retryable_job_failure",
            "internal_error",
        ] {
            assert!(is_stable_error_code(required), "missing {required}");
        }
    }
}
