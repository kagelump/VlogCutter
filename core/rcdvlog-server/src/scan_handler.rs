//! Scan job handler dispatching to root-scan, quick-fingerprint, probe, and full-hash workers.
//!
//! Every file job re-resolves the storage object from its logical identity before touching
//! the filesystem. Fatal root-traversal failures abandon the generation; per-file failures
//! are durable diagnostics that do not abort unrelated files. The observation generation
//! is completed only after every discovered file has a settled outcome.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use rcdvlog_core::{
    MediaObjectId, NodeId, ObservationStatus, RootRelativePath, StorageObjectId, StorageRootId,
    is_supported_media_path, sampled_media_fingerprint,
};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use walkdir::WalkDir;

use crate::catalog::open_read_only_connection;
use crate::jobs::{
    ClaimedJob, FailureKind, JobClass, JobDefinition, JobEngine, JobEngineError, JobHandler,
    JobHandlerFailure, JobRequirements, JobResultCommit, RetryPolicy, ServerCapability,
};
use crate::media_facts::{FullHashFact, ProbeFact, QuickFingerprintFact, ScanFileDiagnostic};
use crate::scan::{
    ScanService, ScanState, insert_scan_run_job, record_scan_observation, safe_path_identity,
    scan_children_settled_read_only,
};
use crate::storage_path::PathAccessIntent;

// ── Recipe keys (versioned, aligned with Task 4) ──────────────────────

pub const ROOT_SCAN_JOB_TYPE: &str = "scan.root.v1";
pub const ROOT_SCAN_RECIPE: &str = "scan.root.discovery-v1";
pub const QUICK_FINGERPRINT_JOB_TYPE: &str = "scan.quick_fingerprint.v1";
pub const QUICK_FINGERPRINT_RECIPE: &str = "scan.quick-fingerprint.sampled-v1";
pub const QUICK_FINGERPRINT_VERSION: &str = "v1";
pub const MEDIA_PROBE_JOB_TYPE: &str = "scan.media_probe.v1";
pub const MEDIA_PROBE_RECIPE: &str = "scan.media-probe.ffprobe-v1";
pub const MEDIA_PROBE_VERSION: &str = "v1";
pub const FULL_HASH_JOB_TYPE: &str = "scan.full_hash.v1";
pub const FULL_HASH_RECIPE: &str = "scan.full-hash.blake3-v1";
pub const FULL_HASH_VERSION: &str = "v1";
pub const SCAN_FINALIZER_JOB_TYPE: &str = "scan.finalizer.v1";
pub const SCAN_FINALIZER_RECIPE: &str = "scan.finalizer-v1";

const EXPECTED_PAYLOAD_VERSION: u32 = 1;

// ── Handler dispatch ──────────────────────────────────────────────────

/// Dispatches scan-related jobs to the correct sub-handler.
pub struct ScanJobHandler {
    library: crate::StorageLibrary,
    scan_service: Arc<ScanService>,
}

impl ScanJobHandler {
    pub fn new(library: crate::StorageLibrary, scan_service: Arc<ScanService>) -> Self {
        Self {
            library,
            scan_service,
        }
    }

    /// Root scan: walk the scope, upsert storage objects, enqueue child jobs
    /// incrementally, and track counters. The generation is NOT completed here;
    /// a child job graph with dependency-based finalization handles that.
    fn run_root_scan(
        &self,
        job: &ClaimedJob,
        cancellation: &CancellationToken,
    ) -> Result<JobResultCommit, JobHandlerFailure> {
        let payload = &job.job.payload;
        let scan_id = required_string(payload, "scanId")?;
        let generation_id = required_string(payload, "generationId")?;
        let root_id = StorageRootId::parse(required_string(payload, "storageRootId")?)
            .map_err(|e| invalid("storageRootId", e))?;
        let scope_opt = optional_relative_path(payload, "scopeRelativePath")?;

        // Start the scan (generation already exists from request_scan)
        self.scan_service
            .start_scan(&scan_id)
            .map_err(|e| terminal("scan_start_failed", format!("{e}")))?;

        // Ensure the observation generation exists (idempotent across restart)
        self.library
            .start_observation_generation(
                generation_id.clone(),
                self.library.local_node_id(),
                crate::library::ObservationScope {
                    storage_root_id: root_id.clone(),
                    relative_prefix: scope_opt.clone(),
                },
            )
            .map_err(|e| terminal("generation_start_failed", format!("{e}")))?;

        // Resolve the confirmed local binding
        let binding = self
            .library
            .get_local_node_root_binding_admin(&root_id)
            .map_err(|e| terminal("binding_lookup_failed", format!("{e}")))?;

        // Validate scope is within binding — reject symlink escapes
        let scan_root = if let Some(ref scope) = scope_opt {
            let suffix = scope
                .suffix_after(binding.root_subpath.as_ref())
                .ok_or_else(|| {
                    terminal(
                        "scope_outside_binding",
                        "scope is outside binding rootSubpath",
                    )
                })?;
            if suffix.is_empty() {
                binding.local_mount_path.clone()
            } else {
                binding.local_mount_path.join(suffix)
            }
        } else {
            binding.local_mount_path.clone()
        };

        // Canonicalize the scan root and verify it's still within the binding
        let canonical_root = fs::canonicalize(&scan_root).map_err(|e| {
            terminal(
                "scan_root_resolve_failed",
                format!("cannot resolve scan root: {e}"),
            )
        })?;
        let canonical_binding = fs::canonicalize(&binding.local_mount_path).map_err(|e| {
            terminal(
                "binding_resolve_failed",
                format!("cannot resolve binding mount: {e}"),
            )
        })?;
        if !canonical_root.starts_with(&canonical_binding) {
            return Err(terminal(
                "scope_symlink_escape",
                "scan root resolved outside binding mount — possible symlink escape",
            ));
        }

        let engine = JobEngine::new(
            self.library.database_path().clone(),
            self.library.local_node_id().clone(),
        );

        let mut visited: u64 = 0;
        let mut supported: u64 = 0;
        let mut unchanged: u64 = 0;
        let mut queued: u64 = 0;

        for entry in WalkDir::new(&canonical_root).follow_links(false) {
            if cancellation.is_cancelled() {
                self.library
                    .abandon_observation_generation(&generation_id)
                    .ok();
                self.scan_service
                    .complete_scan(&scan_id, ScanState::Canceled, Some("scan_cancelled"))
                    .ok();
                return Err(JobHandlerFailure {
                    kind: FailureKind::Retryable,
                    code: "scan_cancelled".into(),
                    details: json!({"scanId": scan_id}),
                });
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    visited += 1;
                    self.persist_diagnostic(
                        &scan_id,
                        None,
                        "discovery",
                        "walk_error",
                        &safe_identity_from_walkerr(&error),
                        true,
                    );
                    continue;
                }
            };
            visited += 1;

            if !entry.file_type().is_file() || entry.path_is_symlink() {
                continue;
            }
            let path = entry.path();
            if !is_supported_media_path(path) {
                continue;
            }

            // Convert to root-relative path
            let relative_path = match path
                .strip_prefix(&canonical_binding)
                .ok()
                .and_then(|p| p.to_str())
                .and_then(|p| RootRelativePath::parse(p).ok())
            {
                Some(rp) => rp,
                None => {
                    self.persist_diagnostic(
                        &scan_id,
                        None,
                        "discovery",
                        "path_rejected",
                        &safe_path_identity_from_path(path),
                        false,
                    );
                    continue;
                }
            };
            supported += 1;

            // Get or create storage object (assigns stable ID)
            let object_id = match self.get_or_create_storage_object(&root_id, &relative_path, path)
            {
                Ok(id) => id,
                Err(_e) => {
                    self.persist_diagnostic(
                        &scan_id,
                        None,
                        "discovery",
                        "storage_object_failed",
                        &safe_path_identity(&relative_path),
                        false,
                    );
                    continue;
                }
            };

            // Check file evidence: size + mtime
            let disk_meta = match fs::metadata(path) {
                Ok(m) => m,
                Err(_e) => {
                    self.persist_diagnostic(
                        &scan_id,
                        Some(&object_id),
                        "discovery",
                        "metadata_failed",
                        &safe_path_identity(&relative_path),
                        true,
                    );
                    continue;
                }
            };
            let disk_size = disk_meta.len();
            let disk_mtime = modified_rfc3339(&disk_meta);

            // Record a provisional accessible observation for every supported file
            let _ = self.library.record_asset_observation(
                &generation_id,
                crate::library::AssetObservationInput {
                    storage_object_id: object_id.clone(),
                    status: ObservationStatus::Accessible,
                    observed_size_bytes: Some(disk_size),
                    observed_quick_fingerprint: None,
                },
            );

            // Check if unchanged: stored evidence matches disk AND successful facts exist
            let has_valid_facts = self.has_current_quick_fact(&object_id, disk_size, &disk_mtime);
            if has_valid_facts {
                unchanged += 1;
                continue;
            }

            // New/changed file: enqueue child jobs incrementally
            // 1. Quick fingerprint (lightweight)
            let quick_job = build_quick_fingerprint_job(
                &scan_id,
                &generation_id,
                &object_id,
                &root_id,
                &relative_path,
                disk_size,
                disk_mtime.as_deref(),
                self.library.local_node_id(),
            )
            .map_err(|e| terminal("job_build_failed", e))?;
            let quick_job_id = quick_job.job_id.as_str().to_string();
            engine
                .enqueue(quick_job)
                .map_err(|e| retryable("enqueue_failed", format!("{e}")))?;
            // Track the child job for finalizer settlement
            {
                let conn = crate::catalog::open_writable_connection(self.library.database_path())
                    .map_err(|e| retryable("db_open_failed", format!("{e}")))?;
                let tx = conn
                    .unchecked_transaction()
                    .map_err(|e| retryable("tx_failed", format!("{e}")))?;
                let _ = insert_scan_run_job(
                    &tx,
                    &scan_id,
                    &quick_job_id,
                    Some(&object_id),
                    "quick_fingerprint",
                );
                let _ = tx.commit();
            }

            // 2. Probe (lightweight)
            let probe_job = build_probe_job(
                &scan_id,
                &generation_id,
                &object_id,
                &root_id,
                &relative_path,
                self.library.local_node_id(),
            )
            .map_err(|e| terminal("job_build_failed", e))?;
            let probe_job_id = probe_job.job_id.as_str().to_string();
            engine
                .enqueue(probe_job)
                .map_err(|e| retryable("enqueue_failed", format!("{e}")))?;
            {
                let conn = crate::catalog::open_writable_connection(self.library.database_path())
                    .map_err(|e| retryable("db_open_failed", format!("{e}")))?;
                let tx = conn
                    .unchecked_transaction()
                    .map_err(|e| retryable("tx_failed", format!("{e}")))?;
                let _ =
                    insert_scan_run_job(&tx, &scan_id, &probe_job_id, Some(&object_id), "probe");
                let _ = tx.commit();
            }

            // 3. Full hash (full_hash class)
            let full_hash_job = build_full_hash_job(
                &scan_id,
                &generation_id,
                &object_id,
                &root_id,
                disk_size,
                self.library.local_node_id(),
            )
            .map_err(|e| terminal("job_build_failed", e))?;
            let full_hash_job_id = full_hash_job.job_id.as_str().to_string();
            engine
                .enqueue(full_hash_job)
                .map_err(|e| retryable("enqueue_failed", format!("{e}")))?;
            {
                let conn = crate::catalog::open_writable_connection(self.library.database_path())
                    .map_err(|e| retryable("db_open_failed", format!("{e}")))?;
                let tx = conn
                    .unchecked_transaction()
                    .map_err(|e| retryable("tx_failed", format!("{e}")))?;
                let _ = insert_scan_run_job(
                    &tx,
                    &scan_id,
                    &full_hash_job_id,
                    Some(&object_id),
                    "full_hash",
                );
                let _ = tx.commit();
            }

            queued += 1;
        }

        // Update scan counters (absolute set, not increment — idempotent on retry)
        self.scan_service
            .set_scan_counters_absolute(&scan_id, visited, supported, unchanged, queued)
            .map_err(|e| retryable("counter_update_failed", format!("{e}")))?;

        // Enqueue the scan finalizer with dependency on this root job
        let finalizer_job =
            build_scan_finalizer_job(&scan_id, &generation_id, self.library.local_node_id())
                .map_err(|e| terminal("job_build_failed", e))?;
        let finalizer_job_id = finalizer_job.job_id.as_str().to_string();
        engine
            .enqueue(finalizer_job)
            .map_err(|e| retryable("finalizer_enqueue_failed", format!("{e}")))?;
        {
            let conn = crate::catalog::open_writable_connection(self.library.database_path())
                .map_err(|e| retryable("db_open_failed", format!("{e}")))?;
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| retryable("tx_failed", format!("{e}")))?;
            let _ = insert_scan_run_job(&tx, &scan_id, &finalizer_job_id, None, "finalizer");
            let _ = tx.commit();
        }

        // Track the root job itself
        {
            let conn = crate::catalog::open_writable_connection(self.library.database_path())
                .map_err(|e| retryable("db_open_failed", format!("{e}")))?;
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| retryable("tx_failed", format!("{e}")))?;
            let _ = insert_scan_run_job(&tx, &scan_id, job.job.job_id.as_str(), None, "root");
            let _ = tx.commit();
        }

        Ok(JobResultCommit {
            output_identity: Some(format!("scan:{}", scan_id)),
            details: json!({
                "scanId": scan_id,
                "visited": visited,
                "supported": supported,
                "unchanged": unchanged,
                "queued": queued,
            }),
        })
    }

    /// Quick-fingerprint handler: re-resolve, stat pre/post, compute sampled fingerprint.
    fn run_quick_fingerprint(
        &self,
        job: &ClaimedJob,
        _cancellation: &CancellationToken,
    ) -> Result<JobResultCommit, JobHandlerFailure> {
        let payload = &job.job.payload;
        let scan_id = required_string(payload, "scanId")?;
        let object_id = StorageObjectId::parse(required_string(payload, "storageObjectId")?)
            .map_err(|e| invalid("storageObjectId", e))?;

        let resolved = self
            .library
            .resolve_local_storage_object(&object_id, PathAccessIntent::Read)
            .map_err(|e| retryable("file_not_found", format!("{e}")))?;

        // Pre-read evidence
        let pre_meta =
            fs::metadata(&resolved).map_err(|e| retryable("file_not_found", format!("{e}")))?;
        let pre_size = pre_meta.len();
        let pre_mtime = modified_rfc3339(&pre_meta);

        // Compute sampled fingerprint
        let fingerprint = sampled_media_fingerprint(&resolved, pre_size)
            .map_err(|e| retryable("fingerprint_failed", format!("{e}")))?;

        // Post-read evidence
        let post_meta =
            fs::metadata(&resolved).map_err(|e| retryable("file_not_found", format!("{e}")))?;
        let post_size = post_meta.len();
        let post_mtime = modified_rfc3339(&post_meta);

        // If evidence changed during read, reject without committing
        if pre_size != post_size || pre_mtime != post_mtime {
            return Err(retryable(
                "input_changed",
                "file changed during quick fingerprint",
            ));
        }

        Ok(JobResultCommit {
            output_identity: Some(format!("quick:{}", object_id)),
            details: json!({
                "scanId": scan_id,
                "storageObjectId": object_id.as_str(),
                "quickFingerprint": fingerprint,
                "sizeBytes": pre_size,
                "modifiedAt": pre_mtime,
            }),
        })
    }

    /// Probe handler: re-resolve, probe with ffprobe + exiftool.
    fn run_probe(
        &self,
        job: &ClaimedJob,
        _cancellation: &CancellationToken,
    ) -> Result<JobResultCommit, JobHandlerFailure> {
        let payload = &job.job.payload;
        let scan_id = required_string(payload, "scanId")?;
        let object_id = StorageObjectId::parse(required_string(payload, "storageObjectId")?)
            .map_err(|e| invalid("storageObjectId", e))?;

        let resolved = self
            .library
            .resolve_local_storage_object(&object_id, PathAccessIntent::Read)
            .map_err(|e| retryable("file_not_found", format!("{e}")))?;

        let tool_config = self.scan_service.tool_config();
        let probe = rcdvlog_core::probe_media_normalized(
            &resolved,
            &tool_config.ffprobe_program,
            tool_config.exiftool_program.as_deref(),
        )
        .map_err(|e| retryable("probe_failed", format!("{e}")))?;

        Ok(JobResultCommit {
            output_identity: Some(format!("probe:{}", object_id)),
            details: json!({
                "scanId": scan_id,
                "storageObjectId": object_id.as_str(),
                "probe": probe,
            }),
        })
    }

    /// Full-hash handler: re-resolve, stream through BLAKE3 with cooperative cancellation.
    fn run_full_hash(
        &self,
        job: &ClaimedJob,
        cancellation: &CancellationToken,
    ) -> Result<JobResultCommit, JobHandlerFailure> {
        let payload = &job.job.payload;
        let scan_id = required_string(payload, "scanId")?;
        let object_id = StorageObjectId::parse(required_string(payload, "storageObjectId")?)
            .map_err(|e| invalid("storageObjectId", e))?;

        let resolved = self
            .library
            .resolve_local_storage_object(&object_id, PathAccessIntent::Read)
            .map_err(|e| retryable("file_not_found", format!("{e}")))?;

        // Full-file BLAKE3 with cancellation
        let hash_result =
            rcdvlog_core::full_file_blake3(&resolved, &|| cancellation.is_cancelled()).map_err(
                |e| {
                    let msg = format!("{e}");
                    if msg.contains("input_changed") {
                        retryable("input_changed", msg)
                    } else if msg.contains("cancelled") {
                        JobHandlerFailure {
                            kind: FailureKind::Retryable,
                            code: "scan_cancelled".into(),
                            details: json!({"reason": "full_hash_cancelled"}),
                        }
                    } else {
                        retryable("hash_failed", msg)
                    }
                },
            )?;

        Ok(JobResultCommit {
            output_identity: Some(format!("fullhash:{}", object_id)),
            details: json!({
                "scanId": scan_id,
                "storageObjectId": object_id.as_str(),
                "fullContentHash": hash_result.hex_digest,
                "byteCount": hash_result.byte_count,
                "preReadSizeBytes": hash_result.pre_read_size,
                "preReadModifiedNs": hash_result.pre_read_modified_ns,
                "postReadSizeBytes": hash_result.post_read_size,
                "postReadModifiedNs": hash_result.post_read_modified_ns,
            }),
        })
    }

    // ── Scan finalizer ────────────────────────────────────────────────
    //
    // run_scan_finalizer is READ-ONLY: it validates the scan and checks
    // that children have settled. All mutation (generation completion,
    // scan state update, counter finalization) occurs atomically in
    // commit_scan_finalizer, inside the same transaction that marks the
    // finalizer job succeeded (via JobEngine::complete).

    fn run_scan_finalizer(
        &self,
        job: &ClaimedJob,
        _cancellation: &CancellationToken,
    ) -> Result<JobResultCommit, JobHandlerFailure> {
        let payload = &job.job.payload;
        let scan_id = required_string(payload, "scanId")?;
        let generation_id = required_string(payload, "generationId")?;

        // Read-only validation: check children settled
        let conn = crate::catalog::open_read_only_connection(self.library.database_path())
            .map_err(|e| retryable("db_open_failed", format!("{e}")))?;
        let settled = scan_children_settled_read_only(&conn, &scan_id)
            .map_err(|e| retryable("settled_check_failed", format!("{e}")))?;
        if !settled {
            return Err(retryable(
                "scan_not_settled",
                "child jobs are still pending",
            ));
        }

        // Validate scan state: must be running
        let scan_state: String = conn
            .query_row(
                "SELECT state FROM scan_runs WHERE scan_id = ?1",
                rusqlite::params![&scan_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| retryable("scan_state_read_failed", format!("{e}")))?
            .unwrap_or_default();
        if scan_state != "running" {
            return Err(terminal(
                "scan_not_running",
                format!("scan {scan_id} is in state {scan_state}, expected running"),
            ));
        }

        // Validate generation: must be in_progress
        let gen_state: String = conn
            .query_row(
                "SELECT state FROM observation_generations WHERE generation_id = ?1",
                rusqlite::params![&generation_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| retryable("gen_state_read_failed", format!("{e}")))?
            .unwrap_or_default();
        if gen_state != "in_progress" {
            return Err(terminal(
                "generation_not_in_progress",
                format!("generation {generation_id} is in state {gen_state}, expected in_progress"),
            ));
        }

        Ok(JobResultCommit {
            output_identity: Some(format!("finalizer:{}", scan_id)),
            details: json!({"scanId": scan_id, "generationId": generation_id}),
        })
    }

    // ── Helpers ──────────────────────────────────────────────────────

    fn get_or_create_storage_object(
        &self,
        root_id: &StorageRootId,
        relative_path: &RootRelativePath,
        path: &Path,
    ) -> Result<StorageObjectId, JobHandlerFailure> {
        if let Ok(Some(id)) = self.library.find_storage_object(root_id, relative_path) {
            return Ok(id);
        }
        let id = StorageObjectId::parse(format!("storage_{}", Uuid::new_v4().simple()))
            .map_err(|e| terminal("id_generation_failed", format!("{e}")))?;
        let (size, modified) = match fs::metadata(path) {
            Ok(meta) => {
                let sz = meta.len();
                (Some(sz), modified_rfc3339(&meta))
            }
            Err(_) => (None, None),
        };
        self.library
            .upsert_storage_object(&id, root_id, relative_path, size, modified.as_deref())
            .map_err(|e| terminal("storage_object_create_failed", format!("{e}")))?;
        Ok(id)
    }

    /// True when stored size+mtime match disk AND a successful quick-fingerprint fact
    /// exists. Uses read-only connection instead of writable.
    fn has_current_quick_fact(
        &self,
        object_id: &StorageObjectId,
        disk_size: u64,
        disk_mtime: &Option<String>,
    ) -> bool {
        let stored = match self.library.get_storage_object(object_id) {
            Ok(s) => s,
            Err(_) => return false,
        };
        if stored.size_bytes != Some(disk_size)
            || stored.modified_at.as_deref() != disk_mtime.as_deref()
        {
            return false;
        }
        // Use read-only connection for read-only check
        let path = self.library.database_path().clone();
        open_read_only_connection(&path).is_ok_and(|conn| {
            conn.query_row(
                "SELECT 1 FROM media_quick_fingerprint_facts WHERE storage_object_id = ?1 AND recipe_key = ?2",
                rusqlite::params![object_id.as_str(), QUICK_FINGERPRINT_RECIPE],
                |_| Ok(()),
            )
            .is_ok()
        })
    }

    /// Persist a diagnostic row using a proper transaction.
    /// Diagnostic persistence failures emit a structured warning; they do not
    /// fail the caller.
    fn persist_diagnostic(
        &self,
        scan_id: &str,
        object_id: Option<&StorageObjectId>,
        stage: &str,
        code: &str,
        safe_path: &str,
        retryable: bool,
    ) {
        let diagnostic = ScanFileDiagnostic {
            diagnostic_id: format!("diag_{}", Uuid::new_v4().simple()),
            scan_id: scan_id.to_string(),
            storage_object_id: object_id.cloned(),
            stage: stage.to_string(),
            code: code.to_string(),
            safe_path_identity: safe_path.to_string(),
            retryable,
            recorded_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        };
        match crate::catalog::open_writable_connection(self.library.database_path()) {
            Ok(conn) => match conn.unchecked_transaction() {
                Ok(tx) => {
                    if let Err(e) = crate::media_facts::insert_file_diagnostic(&tx, &diagnostic) {
                        tracing::warn!(
                            scan_id = %scan_id,
                            stage = %stage,
                            code = %code,
                            error = %e,
                            "failed to persist file diagnostic"
                        );
                    }
                    let _ = tx.commit();
                }
                Err(e) => {
                    tracing::warn!(
                        scan_id = %scan_id,
                        stage = %stage,
                        code = %code,
                        error = %e,
                        "failed to open transaction for diagnostic persistence"
                    );
                }
            },
            Err(e) => {
                tracing::warn!(
                    scan_id = %scan_id,
                    stage = %stage,
                    code = %code,
                    error = %e,
                    "failed to open connection for diagnostic persistence"
                );
            }
        }
    }
}

impl JobHandler for ScanJobHandler {
    fn run(
        &self,
        job: ClaimedJob,
        cancellation: CancellationToken,
    ) -> Result<JobResultCommit, JobHandlerFailure> {
        // Validate payload version
        if job.job.payload_version != EXPECTED_PAYLOAD_VERSION {
            return Err(JobHandlerFailure {
                kind: FailureKind::Terminal,
                code: "unsupported_payload_version".into(),
                details: json!({
                    "expected": EXPECTED_PAYLOAD_VERSION,
                    "received": job.job.payload_version,
                    "jobType": job.job.job_type,
                }),
            });
        }

        match job.job.job_type.as_str() {
            ROOT_SCAN_JOB_TYPE => self.run_root_scan(&job, &cancellation),
            QUICK_FINGERPRINT_JOB_TYPE => self.run_quick_fingerprint(&job, &cancellation),
            MEDIA_PROBE_JOB_TYPE => self.run_probe(&job, &cancellation),
            FULL_HASH_JOB_TYPE => self.run_full_hash(&job, &cancellation),
            SCAN_FINALIZER_JOB_TYPE => self.run_scan_finalizer(&job, &cancellation),
            unknown => Err(JobHandlerFailure {
                kind: FailureKind::Terminal,
                code: "unknown_job_type".into(),
                details: json!({"jobType": unknown}),
            }),
        }
    }

    fn commit_result(
        &self,
        tx: &rusqlite::Transaction<'_>,
        job: &ClaimedJob,
        result: &JobResultCommit,
    ) -> Result<(), JobEngineError> {
        match job.job.job_type.as_str() {
            QUICK_FINGERPRINT_JOB_TYPE => commit_quick_fingerprint(tx, job, result),
            MEDIA_PROBE_JOB_TYPE => commit_probe(tx, job, result),
            FULL_HASH_JOB_TYPE => commit_full_hash(tx, job, result),
            SCAN_FINALIZER_JOB_TYPE => commit_scan_finalizer(tx, job, result),
            _ => Ok(()),
        }
    }
}

// ── Job builders ──────────────────────────────────────────────────────

/// Build a root scan job definition. Used by request_scan and recovery.
pub fn build_root_scan_job(
    scan_id: &str,
    generation_id: &str,
    root_id: &StorageRootId,
    scope: &Option<RootRelativePath>,
    node_id: &NodeId,
) -> Result<JobDefinition, String> {
    let job_id = rcdvlog_core::JobId::parse(format!("job_root_{}", Uuid::new_v4().simple()))
        .map_err(|e| format!("invalid root job id: {e}"))?;
    Ok(JobDefinition {
        job_id,
        job_type: ROOT_SCAN_JOB_TYPE.into(),
        payload_version: EXPECTED_PAYLOAD_VERSION,
        payload: json!({
            "scanId": scan_id,
            "generationId": generation_id,
            "storageRootId": root_id.as_str(),
            "scopeRelativePath": scope.as_ref().map(RootRelativePath::as_str),
        }),
        immutable_identity: json!({
            "scanId": scan_id,
            "recipeKey": ROOT_SCAN_RECIPE,
        }),
        requirements: JobRequirements {
            schema_version: 1,
            requires_original: true,
            requires_derived_write: false,
            capabilities: vec![],
        },
        target_node_id: node_id.clone(),
        project_id: None,
        media_object_id: None,
        storage_root_id: Some(root_id.clone()),
        representation_id: None,
        recipe_key: ROOT_SCAN_RECIPE.into(),
        job_class: JobClass::Lightweight,
        idempotency_key: format!("root-{}", scan_id),
        priority: 0,
        total_work_units: 1,
        total_items: 1,
        retry_policy: RetryPolicy::default(),
        dependencies: vec![],
    })
}

pub fn build_scan_finalizer_job(
    scan_id: &str,
    generation_id: &str,
    node_id: &NodeId,
) -> Result<JobDefinition, String> {
    let idempotency_key = format!("finalizer-{}", scan_id);
    let job_id = rcdvlog_core::JobId::parse(format!("job_finalizer_{}", Uuid::new_v4().simple()))
        .map_err(|e| format!("invalid finalizer job id: {e}"))?;
    Ok(JobDefinition {
        job_id,
        job_type: SCAN_FINALIZER_JOB_TYPE.into(),
        payload_version: EXPECTED_PAYLOAD_VERSION,
        payload: json!({"scanId": scan_id, "generationId": generation_id}),
        immutable_identity: json!({"scanId": scan_id}),
        requirements: JobRequirements {
            schema_version: 1,
            requires_original: false,
            requires_derived_write: false,
            capabilities: vec![],
        },
        target_node_id: node_id.clone(),
        project_id: None,
        media_object_id: None,
        storage_root_id: None,
        representation_id: None,
        recipe_key: SCAN_FINALIZER_RECIPE.into(),
        job_class: JobClass::Lightweight,
        idempotency_key,
        priority: 0,
        total_work_units: 1,
        total_items: 1,
        retry_policy: RetryPolicy {
            max_attempts: 10,
            initial_backoff_seconds: 30,
            max_backoff_seconds: 600,
        },
        dependencies: vec![],
    })
}

#[allow(clippy::too_many_arguments)]
fn build_quick_fingerprint_job(
    scan_id: &str,
    generation_id: &str,
    object_id: &StorageObjectId,
    root_id: &StorageRootId,
    relative_path: &RootRelativePath,
    file_size: u64,
    file_mtime: Option<&str>,
    node_id: &NodeId,
) -> Result<JobDefinition, String> {
    let idempotency_key = format!("quick-{}-{}", scan_id, object_id.as_str());
    let job_id = rcdvlog_core::JobId::parse(format!("job_quick_{}", Uuid::new_v4().simple()))
        .map_err(|e| format!("invalid quick fingerprint job id: {e}"))?;
    Ok(JobDefinition {
        job_id,
        job_type: QUICK_FINGERPRINT_JOB_TYPE.into(),
        payload_version: EXPECTED_PAYLOAD_VERSION,
        payload: json!({
            "scanId": scan_id,
            "generationId": generation_id,
            "storageObjectId": object_id.as_str(),
            "storageRootId": root_id.as_str(),
            "relativePath": relative_path.as_str(),
        }),
        immutable_identity: json!({
            "scanId": scan_id,
            "storageObjectId": object_id.as_str(),
            "recipeKey": QUICK_FINGERPRINT_RECIPE,
            "recipeVersion": QUICK_FINGERPRINT_VERSION,
            "fileSize": file_size,
            "fileModifiedAt": file_mtime,
        }),
        requirements: JobRequirements {
            schema_version: 1,
            requires_original: true,
            requires_derived_write: false,
            capabilities: vec![],
        },
        target_node_id: node_id.clone(),
        project_id: None,
        media_object_id: None,
        storage_root_id: Some(root_id.clone()),
        representation_id: None,
        recipe_key: QUICK_FINGERPRINT_RECIPE.into(),
        job_class: JobClass::Lightweight,
        idempotency_key,
        priority: 0,
        total_work_units: file_size,
        total_items: 1,
        retry_policy: RetryPolicy::default(),
        dependencies: vec![],
    })
}

fn build_probe_job(
    scan_id: &str,
    generation_id: &str,
    object_id: &StorageObjectId,
    root_id: &StorageRootId,
    relative_path: &RootRelativePath,
    node_id: &NodeId,
) -> Result<JobDefinition, String> {
    let idempotency_key = format!("probe-{}-{}", scan_id, object_id.as_str());
    let job_id = rcdvlog_core::JobId::parse(format!("job_probe_{}", Uuid::new_v4().simple()))
        .map_err(|e| format!("invalid probe job id: {e}"))?;
    Ok(JobDefinition {
        job_id,
        job_type: MEDIA_PROBE_JOB_TYPE.into(),
        payload_version: EXPECTED_PAYLOAD_VERSION,
        payload: json!({
            "scanId": scan_id,
            "generationId": generation_id,
            "storageObjectId": object_id.as_str(),
            "storageRootId": root_id.as_str(),
            "relativePath": relative_path.as_str(),
        }),
        immutable_identity: json!({
            "scanId": scan_id,
            "storageObjectId": object_id.as_str(),
            "recipeKey": MEDIA_PROBE_RECIPE,
            "recipeVersion": MEDIA_PROBE_VERSION,
        }),
        requirements: JobRequirements {
            schema_version: 1,
            requires_original: true,
            requires_derived_write: false,
            capabilities: vec![ServerCapability::Ffprobe],
        },
        target_node_id: node_id.clone(),
        project_id: None,
        media_object_id: None,
        storage_root_id: Some(root_id.clone()),
        representation_id: None,
        recipe_key: MEDIA_PROBE_RECIPE.into(),
        job_class: JobClass::Lightweight,
        idempotency_key,
        priority: 0,
        total_work_units: 1024,
        total_items: 1,
        retry_policy: RetryPolicy::default(),
        dependencies: vec![],
    })
}

pub fn build_full_hash_job(
    scan_id: &str,
    _generation_id: &str,
    object_id: &StorageObjectId,
    root_id: &StorageRootId,
    file_size: u64,
    node_id: &NodeId,
) -> Result<JobDefinition, String> {
    let idempotency_key = format!("fullhash-{}-{}", scan_id, object_id.as_str());
    let job_id = rcdvlog_core::JobId::parse(format!("job_fullhash_{}", Uuid::new_v4().simple()))
        .map_err(|e| format!("invalid full hash job id: {e}"))?;
    Ok(JobDefinition {
        job_id,
        job_type: FULL_HASH_JOB_TYPE.into(),
        payload_version: EXPECTED_PAYLOAD_VERSION,
        payload: json!({
            "scanId": scan_id,
            "storageObjectId": object_id.as_str(),
            "storageRootId": root_id.as_str(),
        }),
        immutable_identity: json!({
            "scanId": scan_id,
            "storageObjectId": object_id.as_str(),
            "recipeKey": FULL_HASH_RECIPE,
            "recipeVersion": FULL_HASH_VERSION,
            "fileSize": file_size,
        }),
        requirements: JobRequirements {
            schema_version: 1,
            requires_original: true,
            requires_derived_write: false,
            capabilities: vec![],
        },
        target_node_id: node_id.clone(),
        project_id: None,
        media_object_id: None,
        storage_root_id: Some(root_id.clone()),
        representation_id: None,
        recipe_key: FULL_HASH_RECIPE.into(),
        job_class: JobClass::FullHash,
        idempotency_key,
        priority: 0,
        total_work_units: file_size,
        total_items: 1,
        retry_policy: RetryPolicy::default(),
        dependencies: vec![],
    })
}

// ── Commit helpers for atomic fact + observation registration ─────────

fn commit_quick_fingerprint(
    tx: &rusqlite::Transaction<'_>,
    job: &ClaimedJob,
    result: &JobResultCommit,
) -> Result<(), JobEngineError> {
    let payload = &job.job.payload;
    let generation_id = payload["generationId"].as_str().unwrap_or_default();
    let object_id = StorageObjectId::parse(payload["storageObjectId"].as_str().unwrap_or_default())
        .map_err(|e| JobEngineError::InvalidInput(format!("{e}")))?;
    let root_id = StorageRootId::parse(payload["storageRootId"].as_str().unwrap_or_default())
        .map_err(|e| JobEngineError::InvalidInput(format!("{e}")))?;

    let details = &result.details;
    let fingerprint = details["quickFingerprint"].as_str().unwrap_or_default();
    let size_bytes = details["sizeBytes"].as_u64().unwrap_or(0);
    let modified_at = details["modifiedAt"].as_str();

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

    // Persist quick fingerprint fact
    let fact = QuickFingerprintFact {
        storage_object_id: object_id.clone(),
        recipe_key: QUICK_FINGERPRINT_RECIPE.into(),
        recipe_version: QUICK_FINGERPRINT_VERSION.into(),
        size_bytes,
        quick_fingerprint: fingerprint.to_string(),
        captured_at: now.clone(),
    };
    crate::media_facts::upsert_quick_fingerprint_fact(tx, &fact)
        .map_err(|e| JobEngineError::InvalidInput(format!("{e}")))?;

    // Update storage object evidence (size + mtime)
    tx.execute(
        "UPDATE storage_objects SET size_bytes = ?2, modified_at = ?3, updated_at = ?4 WHERE storage_object_id = ?1",
        rusqlite::params![object_id.as_str(), size_bytes as i64, modified_at, now],
    )?;

    // Check: does this storage object already have a media link?
    let existing_link: Option<(String, String)> = tx
        .query_row(
            "SELECT media_object_id, verification_state FROM media_object_locations WHERE storage_object_id = ?1",
            rusqlite::params![object_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    // If already linked to a media object, update observation but do NOT
    // create/reassign a new media object. Quick fingerprint alone is not
    // enough evidence to attach a new location to an existing media object.
    if existing_link.is_none() {
        // Create a new provisional media object for this storage object
        let new_media_id = MediaObjectId::parse(format!("media_{}", Uuid::new_v4().simple()))
            .map_err(|e| JobEngineError::InvalidInput(format!("{e}")))?;
        tx.execute(
            r#"INSERT INTO media_objects (media_object_id, size_bytes, quick_fingerprint, verification_state, created_at, updated_at) VALUES (?1, ?2, ?3, 'provisional', ?4, ?4)"#,
            rusqlite::params![new_media_id.as_str(), size_bytes as i64, fingerprint, now],
        )?;
        tx.execute(
            r#"INSERT INTO media_object_locations (media_object_id, storage_object_id, verification_state, linked_at, updated_at) VALUES (?1, ?2, 'provisional', ?3, ?3)"#,
            rusqlite::params![new_media_id.as_str(), object_id.as_str(), now],
        )?;
    }

    // Record accessible observation in the scan generation
    let node_id = NodeId::parse(job.job.target_node_id.as_str())
        .map_err(|e| JobEngineError::InvalidInput(format!("{e}")))?;
    record_scan_observation(
        tx,
        generation_id,
        &node_id,
        &root_id,
        &object_id,
        ObservationStatus::Accessible,
        Some(size_bytes),
        Some(fingerprint),
    )?;

    Ok(())
}

fn commit_probe(
    tx: &rusqlite::Transaction<'_>,
    job: &ClaimedJob,
    result: &JobResultCommit,
) -> Result<(), JobEngineError> {
    let object_id = StorageObjectId::parse(
        job.job.payload["storageObjectId"]
            .as_str()
            .unwrap_or_default(),
    )
    .map_err(|e| JobEngineError::InvalidInput(format!("{e}")))?;

    let details = &result.details;
    let probe = &details["probe"];
    let probe_json = serde_json::to_string(probe).unwrap_or_default();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

    let camera_make = probe["cameraMake"].as_str().map(str::to_string);
    let camera_model = probe["cameraModel"].as_str().map(str::to_string);
    let enrichment_status = probe["enrichmentStatus"]
        .as_str()
        .unwrap_or("ok")
        .to_string();
    let enrichment_warning = probe["enrichmentWarning"].as_str().map(str::to_string);

    let fact = ProbeFact {
        storage_object_id: object_id,
        recipe_key: MEDIA_PROBE_RECIPE.into(),
        recipe_version: MEDIA_PROBE_VERSION.into(),
        probe_json,
        camera_make,
        camera_model,
        enrichment_status,
        enrichment_warning,
        captured_at: now,
    };
    crate::media_facts::upsert_probe_fact(tx, &fact)
        .map_err(|e| JobEngineError::InvalidInput(format!("{e}")))?;
    Ok(())
}

fn commit_full_hash(
    tx: &rusqlite::Transaction<'_>,
    job: &ClaimedJob,
    result: &JobResultCommit,
) -> Result<(), JobEngineError> {
    let object_id = StorageObjectId::parse(
        job.job.payload["storageObjectId"]
            .as_str()
            .unwrap_or_default(),
    )
    .map_err(|e| JobEngineError::InvalidInput(format!("{e}")))?;

    let details = &result.details;
    let full_hash = details["fullContentHash"].as_str().unwrap_or_default();
    let byte_count = details["byteCount"].as_u64().unwrap_or(0);
    let pre_read_size = details["preReadSizeBytes"].as_u64().unwrap_or(0);
    let pre_read_modified_ns = details["preReadModifiedNs"].as_i64();
    let post_read_size = details["postReadSizeBytes"].as_u64().unwrap_or(0);
    let post_read_modified_ns = details["postReadModifiedNs"].as_i64();

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

    // Persist full hash fact
    let fact = FullHashFact {
        storage_object_id: object_id.clone(),
        recipe_key: FULL_HASH_RECIPE.into(),
        recipe_version: FULL_HASH_VERSION.into(),
        full_content_hash: full_hash.to_string(),
        byte_count,
        pre_read_size_bytes: pre_read_size,
        pre_read_modified_ns,
        post_read_size_bytes: post_read_size,
        post_read_modified_ns,
        captured_at: now.clone(),
    };
    crate::media_facts::upsert_full_hash_fact(tx, &fact)
        .map_err(|e| JobEngineError::InvalidInput(format!("{e}")))?;

    // Get the current media link for this storage object
    let current_link: Option<(String, String)> = tx
        .query_row(
            "SELECT media_object_id, verification_state FROM media_object_locations WHERE storage_object_id = ?1",
            rusqlite::params![object_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    // Find a verified media object by full hash + size
    let verified_candidate: Option<String> = tx
        .query_row(
            r#"
            SELECT media_object_id FROM media_objects
            WHERE full_content_hash = ?1 AND size_bytes = ?2
              AND verification_state = 'verified'
            LIMIT 1
            "#,
            rusqlite::params![full_hash, byte_count as i64],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(verified_media_id) = verified_candidate {
        // Converge: relink this storage to the verified media object
        if let Some((current_media, _)) = &current_link {
            if current_media != &verified_media_id {
                // Remove old link and create new one
                tx.execute(
                    "DELETE FROM media_object_locations WHERE storage_object_id = ?1",
                    rusqlite::params![object_id.as_str()],
                )?;
                tx.execute(
                    r#"INSERT INTO media_object_locations (media_object_id, storage_object_id, verification_state, linked_at, updated_at) VALUES (?1, ?2, 'verified', ?3, ?3)"#,
                    rusqlite::params![verified_media_id, object_id.as_str(), now],
                )?;
            } else {
                // Already linked to the correct verified media; just update state
                tx.execute(
                    "UPDATE media_object_locations SET verification_state = 'verified', updated_at = ?2 WHERE storage_object_id = ?1",
                    rusqlite::params![object_id.as_str(), now],
                )?;
            }
        } else {
            tx.execute(
                r#"INSERT INTO media_object_locations (media_object_id, storage_object_id, verification_state, linked_at, updated_at) VALUES (?1, ?2, 'verified', ?3, ?3)"#,
                rusqlite::params![verified_media_id, object_id.as_str(), now],
            )?;
        }
    } else {
        // No verified match: promote the current provisional to verified.
        // Check if this full hash matches a DIFFERENT provisional from the quick
        // candidate (quick fingerprint collision).
        if let Some((current_media, _current_state)) = &current_link {
            // Check if there's another media object with same quick fingerprint but
            // different full hash — record mismatch evidence
            let provisional_media_quick: Option<String> = tx
                .query_row(
                    "SELECT quick_fingerprint FROM media_objects WHERE media_object_id = ?1",
                    rusqlite::params![current_media],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(quick_fp) = provisional_media_quick {
                let colliding: Option<(String, String)> = tx
                    .query_row(
                        r#"
                        SELECT mo.media_object_id, mhf.full_content_hash
                        FROM media_objects mo
                        JOIN media_full_hash_facts mhf ON mhf.storage_object_id IN (
                            SELECT storage_object_id FROM media_object_locations WHERE media_object_id = mo.media_object_id
                        )
                        WHERE mo.quick_fingerprint = ?1 AND mo.size_bytes = ?2
                          AND mo.verification_state = 'verified'
                          AND mo.media_object_id != ?3
                        LIMIT 1
                        "#,
                        rusqlite::params![quick_fp, byte_count as i64, current_media],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                if let Some((colliding_media_id, colliding_full_hash)) = colliding
                    && colliding_full_hash != full_hash
                {
                    // Quick fingerprint collision with different full hash — record mismatch
                    let mismatch_id = format!("mismatch_{}", Uuid::new_v4().simple());
                    tx.execute(
                            r#"INSERT INTO media_mismatch_evidence (mismatch_id, media_object_id, storage_object_id, mismatch_type, related_media_object_id, related_storage_object_id, recorded_at) VALUES (?1, ?2, ?3, 'quick_fingerprint_collision', ?4, NULL, ?5)"#,
                            rusqlite::params![mismatch_id, current_media, object_id.as_str(), colliding_media_id, now],
                        )?;
                }
            }

            // Promote current provisional to verified
            tx.execute(
                r#"UPDATE media_objects SET full_content_hash = ?2, size_bytes = ?3, verification_state = 'verified', updated_at = ?4 WHERE media_object_id = ?1 AND verification_state = 'provisional'"#,
                rusqlite::params![current_media, full_hash, byte_count as i64, now],
            )?;
            tx.execute(
                "UPDATE media_object_locations SET verification_state = 'verified', updated_at = ?2 WHERE storage_object_id = ?1",
                rusqlite::params![object_id.as_str(), now],
            )?;
        }
    }

    Ok(())
}

fn commit_scan_finalizer(
    tx: &rusqlite::Transaction<'_>,
    job: &ClaimedJob,
    result: &JobResultCommit,
) -> Result<(), JobEngineError> {
    let payload = &job.job.payload;
    let scan_id = payload["scanId"].as_str().unwrap_or_default();
    let generation_id = payload["generationId"].as_str().unwrap_or_default();

    // ── Atomic publication: all mutations occur within this single transaction ──

    // 1. Re-check that the scan/generation/root identifiers match expectations
    let (stored_gen_id, _root_job_id, _node_id, _root_id): (
        String,
        String,
        String,
        String,
    ) = tx
        .query_row(
            "SELECT generation_id, root_job_id, node_id, storage_root_id FROM scan_runs WHERE scan_id = ?1",
            rusqlite::params![scan_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?
        .ok_or_else(|| {
            JobEngineError::InvalidInput(format!("scan {scan_id} not found"))
        })?;

    if stored_gen_id != generation_id {
        return Err(JobEngineError::InvalidInput(format!(
            "finalizer generation mismatch: job={generation_id} scan={stored_gen_id}"
        )));
    }

    // 2. Check that the generation is in_progress (or already completed idempotently)
    let gen_state: Option<String> = tx
        .query_row(
            "SELECT state FROM observation_generations WHERE generation_id = ?1",
            rusqlite::params![generation_id],
            |row| row.get(0),
        )
        .optional()?;
    match gen_state.as_deref() {
        Some("completed") => {
            // Idempotent: generation already completed by a previous finalizer success
            // that committed the generation but whose job+scan update may have been
            // interrupted. Still proceed to update scan state below.
        }
        Some("in_progress") => {}
        Some(other) => {
            return Err(JobEngineError::InvalidInput(format!(
                "generation {generation_id} is in unexpected state {other}"
            )));
        }
        None => {
            return Err(JobEngineError::InvalidInput(format!(
                "generation {generation_id} not found"
            )));
        }
    }

    // 3. Re-check that no linked child job is still non-terminal
    let pending: i64 = tx.query_row(
        r#"
        SELECT COUNT(*) FROM scan_run_jobs srj
        JOIN jobs j ON j.job_id = srj.job_id
        WHERE srj.scan_id = ?1
          AND srj.stage IN ('quick_fingerprint', 'probe', 'full_hash')
          AND j.state IN ('queued', 'running', 'retry_wait')
        "#,
        rusqlite::params![scan_id],
        |row| row.get(0),
    )?;
    if pending > 0 {
        return Err(JobEngineError::InvalidInput(format!(
            "scan_not_settled: {pending} child jobs are still pending"
        )));
    }

    // 4. Verify root discovery is durably successful
    let root_succeeded: i64 = tx.query_row(
        r#"
        SELECT COUNT(*) FROM scan_run_jobs srj
        JOIN jobs j ON j.job_id = srj.job_id
        WHERE srj.scan_id = ?1 AND srj.stage = 'root' AND j.state = 'succeeded'
        "#,
        rusqlite::params![scan_id],
        |row| row.get(0),
    )?;
    if root_succeeded != 1 {
        return Err(JobEngineError::InvalidInput(format!(
            "root discovery not succeeded for scan {scan_id}"
        )));
    }

    // 5. Compute settled file counts (distinct storage objects, not job count)
    //    completed_files = distinct objects where NO job is non-terminal
    //    failed_files = subset of completed_files where at least one job terminal-failed
    let completed: i64 = tx.query_row(
        r#"
        SELECT COUNT(DISTINCT srj.storage_object_id)
        FROM scan_run_jobs srj
        WHERE srj.scan_id = ?1
          AND srj.stage IN ('quick_fingerprint', 'probe', 'full_hash')
          AND srj.storage_object_id IS NOT NULL
          AND srj.storage_object_id NOT IN (
              SELECT srj2.storage_object_id
              FROM scan_run_jobs srj2
              JOIN jobs j2 ON j2.job_id = srj2.job_id
              WHERE srj2.scan_id = ?1
                AND srj2.stage IN ('quick_fingerprint', 'probe', 'full_hash')
                AND srj2.storage_object_id IS NOT NULL
                AND j2.state IN ('queued', 'running', 'retry_wait')
          )
        "#,
        rusqlite::params![scan_id],
        |row| row.get(0),
    )?;

    let failed: i64 = tx.query_row(
        r#"
        SELECT COUNT(DISTINCT srj.storage_object_id)
        FROM scan_run_jobs srj
        JOIN jobs j ON j.job_id = srj.job_id
        WHERE srj.scan_id = ?1
          AND srj.stage IN ('quick_fingerprint', 'probe', 'full_hash')
          AND srj.storage_object_id IS NOT NULL
          AND j.state IN ('failed', 'canceled')
          AND srj.storage_object_id NOT IN (
              SELECT srj2.storage_object_id
              FROM scan_run_jobs srj2
              JOIN jobs j2 ON j2.job_id = srj2.job_id
              WHERE srj2.scan_id = ?1
                AND srj2.stage IN ('quick_fingerprint', 'probe', 'full_hash')
                AND srj2.storage_object_id IS NOT NULL
                AND j2.state IN ('queued', 'running', 'retry_wait')
          )
        "#,
        rusqlite::params![scan_id],
        |row| row.get(0),
    )?;

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

    // 6. Complete the generation (if not already completed)
    if gen_state.as_deref() == Some("in_progress") {
        tx.execute(
            r#"
            UPDATE observation_generations
            SET state = 'completed', completed_at = ?2
            WHERE generation_id = ?1 AND state = 'in_progress'
            "#,
            rusqlite::params![generation_id, now],
        )?;
    }

    // 7. Update scan counters and state to succeeded — all in one atomic step
    tx.execute(
        r#"
        UPDATE scan_runs SET
            state = 'succeeded',
            completed_at = ?2,
            completed_files = ?3,
            failed_files = ?4
        WHERE scan_id = ?1 AND state = 'running'
        "#,
        rusqlite::params![scan_id, now, completed, failed],
    )?;

    let _ = result;
    Ok(())
}

// ── Utility helpers ───────────────────────────────────────────────────

fn required_string(value: &Value, key: &str) -> Result<String, JobHandlerFailure> {
    value[key]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| invalid(key, "missing"))
}

fn optional_relative_path(
    value: &Value,
    key: &str,
) -> Result<Option<RootRelativePath>, JobHandlerFailure> {
    match value[key].as_str() {
        Some(s) => RootRelativePath::parse(s)
            .map(Some)
            .map_err(|e| invalid(key, e)),
        None => Ok(None),
    }
}

fn invalid(field: &str, reason: impl std::fmt::Display) -> JobHandlerFailure {
    JobHandlerFailure {
        kind: FailureKind::Terminal,
        code: "invalid_payload".into(),
        details: json!({"field": field, "reason": format!("{reason}")}),
    }
}

fn retryable(code: &str, reason: impl std::fmt::Display) -> JobHandlerFailure {
    JobHandlerFailure {
        kind: FailureKind::Retryable,
        code: code.into(),
        details: json!({"reason": format!("{reason}")}),
    }
}

fn terminal(code: &str, reason: impl std::fmt::Display) -> JobHandlerFailure {
    JobHandlerFailure {
        kind: FailureKind::Terminal,
        code: code.into(),
        details: json!({"reason": format!("{reason}")}),
    }
}

fn safe_path_identity_from_path(path: &Path) -> String {
    path.to_str()
        .map(|s| blake3::hash(s.as_bytes()).to_hex().to_string()[..32].to_string())
        .unwrap_or_else(|| "unknown_path".to_string())
}

fn safe_identity_from_walkerr(error: &walkdir::Error) -> String {
    error
        .path()
        .and_then(|p| p.to_str())
        .map(|s| blake3::hash(s.as_bytes()).to_hex().to_string()[..32].to_string())
        .unwrap_or_else(|| "unknown_path".to_string())
}

#[cfg(unix)]
fn modified_rfc3339(metadata: &std::fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let secs = metadata.mtime();
    let nsecs = metadata.mtime_nsec();
    let dt = chrono::DateTime::from_timestamp(secs, nsecs as u32)?;
    Some(dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

#[cfg(not(unix))]
fn modified_rfc3339(metadata: &std::fs::Metadata) -> Option<String> {
    metadata.modified().ok().and_then(|t| {
        let dt: chrono::DateTime<chrono::Utc> = t.into();
        Some(dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
    })
}
