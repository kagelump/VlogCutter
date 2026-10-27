use super::credentials::Credentials;
use super::oauth::refresh;
use super::{
    CredentialStore, Endpoints, failure, http_client, now, response_json, transport, validate_text,
};
use crate::Catalog;
use anyhow::{Context, Result, ensure};
use batch_estimator::BatchEstimator;
use fs2::FileExt;
use reqwest::{
    StatusCode, Url,
    blocking::{Client, Response},
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const CHUNK_BYTES: usize = 8 * 1024 * 1024;
const TASK_KEY: &str = "youtube.upload.bytes.chunk8m.w1.v1";

#[derive(Debug, Clone)]
pub struct UploadOptions {
    pub project_id: String,
    pub file: PathBuf,
    pub title: String,
    pub description: String,
    pub privacy: String,
    pub cut_revision: Option<String>,
    /// Explicitly allow a new session when the prior outcome cannot be recovered.
    pub restart_upload: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadRecord {
    pub id: String,
    pub project_id: String,
    pub credential_id: String,
    pub file_sha256: String,
    pub file_path: String,
    pub file_size: u64,
    pub metadata: Value,
    pub cut_revision: Option<String>,
    pub state: String,
    // A resumable URI is sensitive operational state and never leaves the core.
    #[serde(skip)]
    session_uri: Option<String>,
    pub uploaded_bytes: u64,
    pub video_id: Option<String>,
    pub channel_id: Option<String>,
    pub actual_privacy: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl std::fmt::Debug for UploadRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UploadRecord")
            .field("id", &self.id)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadResult {
    pub upload: UploadRecord,
    pub url: String,
    pub reused: bool,
    pub processing_verified: bool,
}

struct Artifact {
    file: File,
    path: PathBuf,
    size: u64,
    modified: std::time::SystemTime,
    hash: String,
}

impl Artifact {
    fn open(path: &std::path::Path) -> Result<Self> {
        let path = path.canonicalize().context("resolve rendered video file")?;
        let mut file = File::open(&path).context("open rendered video file")?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file() && metadata.len() > 0 && metadata.len() <= 256 * 1024 * 1024 * 1024,
            "invalid input: upload must be a nonempty regular video file no larger than 256 GiB"
        );
        let modified = metadata.modified()?;
        let mut hash = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        loop {
            let length = file.read(&mut buffer)?;
            if length == 0 {
                break;
            }
            hash.update(&buffer[..length]);
        }
        let artifact = Self {
            file,
            path,
            size: metadata.len(),
            modified,
            hash: format!("{:x}", hash.finalize()),
        };
        artifact.check()?;
        Ok(artifact)
    }

    fn check(&self) -> Result<()> {
        let metadata = self.file.metadata()?;
        ensure!(
            metadata.len() == self.size && metadata.modified()? == self.modified,
            "invalid input: rendered video changed during upload; finish rendering before uploading"
        );
        Ok(())
    }
}

impl UploadOptions {
    fn metadata(&self) -> Result<Value> {
        ensure!(
            !self.title.trim().is_empty()
                && self.title.chars().count() <= 100
                && !self.title.contains(['<', '>']),
            "invalid input: title must be 1..100 characters without angle brackets"
        );
        validate_text(&self.title, 400, "title")?;
        validate_text(&self.description, 5000, "description")?;
        ensure!(
            !self.description.contains(['<', '>']),
            "invalid input: description cannot contain angle brackets"
        );
        ensure!(
            ["private", "unlisted", "public"].contains(&self.privacy.as_str()),
            "invalid input: unsupported privacy status"
        );
        if let Some(revision) = &self.cut_revision {
            validate_text(revision, 256, "cut revision")?;
        }
        Ok(
            json!({"snippet":{"title":self.title,"description":self.description,"categoryId":"22"},
            "status":{"privacyStatus":self.privacy}}),
        )
    }
}

impl Catalog {
    /// Local, unsynced history. Reading does not consult Google or expose session URLs.
    pub fn youtube_upload_history(&self, project: &str, limit: u32) -> Result<Vec<UploadRecord>> {
        ensure!(
            (1..=200).contains(&limit),
            "invalid input: history limit must be 1..200"
        );
        let connection = self.connection()?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
            [project],
            |r| r.get(0),
        )?;
        ensure!(exists, "project not found");
        let has_ledger: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='youtube_uploads')", [], |r| r.get(0))?;
        if !has_ledger {
            return failure(
                "schema_incompatible",
                "Upload history requires catalog schema 49; open the catalog with the current VlogCutter to upgrade it first",
            );
        }
        let mut statement = connection.prepare("SELECT id FROM youtube_uploads WHERE project_id=?1 ORDER BY created_at DESC, id DESC LIMIT ?2")?;
        let ids = statement
            .query_map(params![project, limit], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter()
            .map(|id| self.youtube_record(id)?.context("upload record not found"))
            .collect()
    }

    pub fn youtube_upload(
        &self,
        store: &CredentialStore,
        options: &UploadOptions,
        progress: impl FnMut(Value),
    ) -> Result<UploadResult> {
        self.youtube_upload_with(
            store,
            options,
            &http_client()?,
            &Endpoints::default(),
            progress,
        )
    }

    fn youtube_record(&self, id: &str) -> Result<Option<UploadRecord>> {
        self.connection()?.query_row(
            "SELECT id,project_id,credential_id,file_sha256,file_path,file_size,metadata_json,cut_revision,state,session_uri,uploaded_bytes,video_id,channel_id,actual_privacy,created_at,updated_at FROM youtube_uploads WHERE id=?1",
            [id], |r| Ok(UploadRecord {
                id:r.get(0)?, project_id:r.get(1)?, credential_id:r.get(2)?, file_sha256:r.get(3)?,
                file_path:r.get(4)?, file_size:r.get::<_, i64>(5)? as u64, metadata:serde_json::from_str(&r.get::<_,String>(6)?)
                    .map_err(|e| rusqlite::Error::FromSqlConversionFailure(6,rusqlite::types::Type::Text,Box::new(e)))?,
                cut_revision:r.get(7)?, state:r.get(8)?, session_uri:r.get(9)?, uploaded_bytes:r.get::<_, i64>(10)? as u64,
                video_id:r.get(11)?, channel_id:r.get(12)?, actual_privacy:r.get(13)?, created_at:r.get::<_, i64>(14)? as u64, updated_at:r.get::<_, i64>(15)? as u64,
            })).optional().context("read YouTube upload ledger")
    }

    fn save_youtube(&self, record: &UploadRecord) -> Result<()> {
        self.connection()?.execute(
            "UPDATE youtube_uploads SET state=?2,session_uri=?3,uploaded_bytes=?4,video_id=?5,channel_id=?6,actual_privacy=?7,updated_at=?8 WHERE id=?1",
            params![record.id,record.state,record.session_uri,record.uploaded_bytes as i64,record.video_id,record.channel_id,record.actual_privacy,now() as i64],
        )?;
        Ok(())
    }

    pub(super) fn youtube_upload_with(
        &self,
        store: &CredentialStore,
        options: &UploadOptions,
        client: &Client,
        endpoints: &Endpoints,
        mut progress: impl FnMut(Value),
    ) -> Result<UploadResult> {
        ensure!(
            !self.read_only,
            "invalid input: uploads require a writable catalog"
        );
        let metadata = options.metadata()?;
        let exists: bool = self.connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
            [&options.project_id],
            |r| r.get(0),
        )?;
        ensure!(exists, "project not found");
        let _credential_lock = store.lock()?;
        let mut credentials = store.load()?;
        progress(json!({"event":"youtube.upload.hashing","workers":1}));
        let mut artifact = Artifact::open(&options.file)?;
        // Same persistent lock inode even after process termination. Catalog clones
        // also retain UNIQUE protection; no SQLite transaction is held over HTTP.
        let canonical_catalog = self
            .database_path
            .canonicalize()
            .context("resolve upload catalog")?;
        let lock_directory = canonical_catalog
            .parent()
            .context("catalog directory")?
            .join("cache/.youtube-locks");
        fs::create_dir_all(&lock_directory)?;
        let key = format!(
            "{:x}",
            Sha256::digest(format!(
                "{}:{}:{}",
                options.project_id, credentials.id, artifact.hash
            ))
        );
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_directory.join(format!("{key}.lock")))?;
        lock_file
            .try_lock_exclusive()
            .map_err(|_| super::YoutubeError {
                code: "youtube.upload_busy",
                message: "Another process is uploading this artifact",
            })?;
        let connection = self.connection()?;
        let existing: Option<String> = connection.query_row(
            "SELECT id FROM youtube_uploads WHERE project_id=?1 AND credential_id=?2 AND file_sha256=?3",
            params![options.project_id,credentials.id,artifact.hash], |r| r.get(0)).optional()?;
        let mut record = if let Some(id) = existing {
            self.youtube_record(&id)?
                .context("upload record not found")?
        } else {
            let id = uuid::Uuid::new_v4().to_string();
            connection.execute("INSERT INTO youtube_uploads(id,project_id,credential_id,file_sha256,file_path,file_size,metadata_json,cut_revision,state,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'pending',?9,?9)",
                params![id,options.project_id,credentials.id,artifact.hash,artifact.path.to_str().context("invalid input: rendered video path must be UTF-8")?,artifact.size as i64,metadata.to_string(),options.cut_revision,now() as i64])?;
            self.youtube_record(&id)?
                .context("upload record not found")?
        };
        if record.metadata != metadata || record.cut_revision != options.cut_revision {
            return failure(
                "youtube.metadata_conflict",
                "This artifact already has an upload with different metadata or cut revision; reuse its original arguments or upload a new rendered artifact",
            );
        }
        if record.state == "complete" {
            return completed(record, true);
        }
        if record.state == "initiating" && record.session_uri.is_none() && !options.restart_upload {
            return failure(
                "youtube.outcome_unknown",
                "The prior session creation outcome is unknown; check YouTube Studio before explicitly using --restart-upload (which may create a duplicate)",
            );
        }
        let mut replacement_used = record.state == "initiating" && record.session_uri.is_none();
        if record.session_uri.is_none() {
            initiate(
                self,
                client,
                endpoints,
                store,
                &mut credentials,
                &artifact,
                &mut record,
            )?;
        }
        let mut failures = 0u32;
        let started = Instant::now();
        let mut initial_offset = None;
        let mut estimator =
            BatchEstimator::new(&[artifact.size], 1, self.load_batch_history(TASK_KEY)?);
        // Always ask Google where to resume, including after a lost final response.
        let mut probe = true;
        loop {
            artifact.check()?;
            refresh(client, endpoints, store, &mut credentials, false)?;
            let uri = record
                .session_uri
                .as_deref()
                .context("upload session missing")?;
            validate_session(uri, endpoints)?;
            let response = if probe {
                client
                    .put(uri)
                    .bearer_auth(&credentials.access_token)
                    .header("Content-Length", "0")
                    .header("Content-Range", format!("bytes */{}", artifact.size))
                    .send()
            } else {
                let remaining = artifact
                    .size
                    .checked_sub(record.uploaded_bytes)
                    .context("invalid input: upload offset exceeds file size")?;
                let length = remaining.min(CHUNK_BYTES as u64) as usize;
                ensure!(
                    length > 0,
                    "invalid input: upload exhausted without a completion response"
                );
                artifact.file.seek(SeekFrom::Start(record.uploaded_bytes))?;
                let mut chunk = vec![0u8; length];
                artifact.file.read_exact(&mut chunk)?;
                client
                    .put(uri)
                    .bearer_auth(&credentials.access_token)
                    .header("Content-Type", "application/octet-stream")
                    .header(
                        "Content-Range",
                        format!(
                            "bytes {}-{}/{}",
                            record.uploaded_bytes,
                            record.uploaded_bytes + length as u64 - 1,
                            artifact.size
                        ),
                    )
                    .body(chunk)
                    .send()
            };
            let response = match response {
                Ok(response) => response,
                Err(_) => {
                    failures += 1;
                    if failures > 5 {
                        return failure(
                            "youtube.network_error",
                            "Upload interrupted; rerun the same command to resume its saved session",
                        );
                    }
                    backoff(failures);
                    probe = true;
                    continue;
                }
            };
            artifact.check()?;
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED {
                failures += 1;
                if failures > 5 {
                    return failure(
                        "youtube.auth_failed",
                        "YouTube rejected the refreshed credential; authorize again",
                    );
                }
                refresh(client, endpoints, store, &mut credentials, true)?;
                probe = true;
                continue;
            }
            if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
                if !options.restart_upload {
                    return failure(
                        "youtube.session_expired",
                        "Saved upload session expired; check YouTube Studio before explicitly using --restart-upload (which may create a duplicate)",
                    );
                }
                if replacement_used {
                    return failure(
                        "youtube.session_expired",
                        "The replacement session expired too; retry later",
                    );
                }
                record.session_uri = None;
                record.uploaded_bytes = 0;
                record.state = "pending".into();
                self.save_youtube(&record)?;
                initiate(
                    self,
                    client,
                    endpoints,
                    store,
                    &mut credentials,
                    &artifact,
                    &mut record,
                )?;
                replacement_used = true;
                probe = true;
                initial_offset = None;
                failures = 0;
                progress(json!({"event":"youtube.upload.session_restarted","uploadId":record.id}));
                continue;
            }
            if retryable(status) {
                failures += 1;
                if failures > 5 {
                    return failure(
                        "youtube.http_error",
                        "YouTube is unavailable or throttling uploads; rerun later to resume",
                    );
                }
                backoff(failures);
                probe = true;
                continue;
            }
            if status.is_success() {
                let value = response_json(response)?;
                record.video_id = Some(video_id(&value)?);
                record.channel_id = value["snippet"]["channelId"].as_str().map(str::to_owned);
                record.actual_privacy =
                    value["status"]["privacyStatus"].as_str().map(str::to_owned);
                record.uploaded_bytes = artifact.size;
                record.state = "complete".into();
                record.session_uri = None;
                self.save_youtube(&record)?;
                estimator.record_completion(
                    artifact.size - initial_offset.unwrap_or(artifact.size),
                    started.elapsed(),
                );
                self.save_batch_history(TASK_KEY, estimator.history())?;
                progress(
                    json!({"event":"youtube.upload.complete","uploadId":record.id,"uploadedBytes":artifact.size,"totalBytes":artifact.size,"workers":1}),
                );
                record.updated_at = now();
                return completed(record, false);
            }
            if status.as_u16() == 308 {
                let offset = acknowledged(&response, artifact.size)?;
                if initial_offset.is_none() {
                    initial_offset = Some(offset);
                }
                if (!probe && offset <= record.uploaded_bytes) || offset == artifact.size {
                    failures += 1;
                    if failures > 5 {
                        return failure(
                            "youtube.protocol_error",
                            "YouTube upload stopped advancing; rerun to recover the saved session",
                        );
                    }
                    backoff(failures);
                    probe = true;
                } else {
                    // Only a byte increase clears retry accounting. A status probe
                    // that reports the same offset must not mask a stalled chunk.
                    if offset > record.uploaded_bytes {
                        failures = 0;
                    }
                    record.uploaded_bytes = offset;
                    self.save_youtube(&record)?;
                    progress(
                        json!({"event":"youtube.upload.progress","uploadId":record.id,"uploadedBytes":offset,"totalBytes":artifact.size,
                        "estimate":estimator.estimate_with_partial_progress(&[artifact.size-offset],offset)}),
                    );
                    probe = false;
                }
                continue;
            }
            return upload_error(status);
        }
    }
}

fn initiate(
    catalog: &Catalog,
    client: &Client,
    endpoints: &Endpoints,
    store: &CredentialStore,
    credentials: &mut Credentials,
    artifact: &Artifact,
    record: &mut UploadRecord,
) -> Result<()> {
    artifact.check()?;
    refresh(client, endpoints, store, credentials, false)?;
    record.state = "initiating".into();
    catalog.save_youtube(record)?;
    // POST is intentionally never retried. A lost response may have created a video.
    let response = client
        .post(&endpoints.upload)
        .query(&[("uploadType", "resumable"), ("part", "snippet,status")])
        .bearer_auth(&credentials.access_token)
        .header("X-Upload-Content-Length", artifact.size)
        .header("X-Upload-Content-Type", "application/octet-stream")
        .json(&record.metadata)
        .send()
        .map_err(transport)?;
    if !response.status().is_success() {
        if response.status().is_client_error() {
            record.state = "pending".into();
            catalog.save_youtube(record)?;
        }
        return upload_error(response.status());
    }
    let uri = response
        .headers()
        .get("location")
        .and_then(|h| h.to_str().ok())
        .context("invalid input: YouTube omitted resumable session location")?;
    validate_session(uri, endpoints)?;
    record.session_uri = Some(uri.into());
    record.state = "uploading".into();
    catalog.save_youtube(record)
}

fn completed(record: UploadRecord, reused: bool) -> Result<UploadResult> {
    let id = record
        .video_id
        .as_deref()
        .context("completed upload is missing video ID")?;
    Ok(UploadResult {
        url: format!("https://youtu.be/{id}"),
        upload: record,
        reused,
        processing_verified: false,
    })
}

fn video_id(value: &Value) -> Result<String> {
    value["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 64
        && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'))
        .map(str::to_owned).ok_or_else(|| super::YoutubeError {
            code:"youtube.protocol_error",message:"Upload response is missing a valid video ID; rerun to recover the saved session",
        }.into())
}

fn acknowledged(response: &Response, total: u64) -> Result<u64> {
    let Some(range) = response.headers().get("range") else {
        return Ok(0);
    };
    let offset = range
        .to_str()
        .ok()
        .and_then(|r| r.strip_prefix("bytes=0-"))
        .and_then(|n| n.parse::<u64>().ok())
        .and_then(|n| n.checked_add(1))
        .filter(|n| *n <= total);
    offset.ok_or_else(|| {
        super::YoutubeError {
            code: "youtube.protocol_error",
            message: "YouTube returned an invalid resume offset",
        }
        .into()
    })
}

pub(super) fn validate_session(uri: &str, endpoints: &Endpoints) -> Result<()> {
    let url = Url::parse(uri).map_err(|_| super::YoutubeError {
        code: "youtube.protocol_error",
        message: "Invalid upload session URL",
    })?;
    let expected = Url::parse(&endpoints.upload)?;
    if url.origin() != expected.origin()
        || url.path() != expected.path()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return failure(
            "youtube.protocol_error",
            "YouTube returned an unexpected upload session origin or path",
        );
    }
    Ok(())
}

fn retryable(status: StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 500 | 502 | 503 | 504)
}

fn backoff(attempt: u32) {
    std::thread::sleep(Duration::from_secs(1 << (attempt.min(5) - 1)));
}

fn upload_error<T>(status: StatusCode) -> Result<T> {
    match status.as_u16() {
        401 => failure(
            "youtube.auth_failed",
            "YouTube rejected authorization; run youtube auth login again",
        ),
        403 => failure(
            "youtube.upload_forbidden",
            "YouTube denied the upload; check API enablement, quota, channel access, and account upload limits",
        ),
        _ => failure(
            "youtube.http_error",
            "YouTube rejected the upload request; check video metadata and rerun to recover saved state",
        ),
    }
}
