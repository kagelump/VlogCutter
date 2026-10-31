//! Local, manifest-last delivery bundles for verified edit-plan review movies.
use super::{PreviewArtifactState, PreviewCutMapping};
use crate::{Catalog, run_captured_media_command_with_cancellation as run_media, tool_command};
use anyhow::{Context, Result, ensure};
use blake3::Hasher;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditPlanBundleRequest {
    pub artifact_id: String,
    pub output_directory: PathBuf,
    /// Stable, portable folder name; restricted to ASCII letters, digits, `_` and `-`.
    pub name: String,
    #[serde(default)]
    pub contact_sheet: bool,
    #[serde(default)]
    pub context_evidence_id: Option<String>,
    #[serde(default)]
    pub picture_coverage: Option<super::coverage::PictureCoverageRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditPlanBundleResult {
    pub bundle_directory: PathBuf,
    pub manifest_path: PathBuf,
    pub reused: bool,
    pub artifact_id: String,
    pub revision_id: String,
    pub movie_path: PathBuf,
    pub movie_digest: String,
    pub files: Vec<BundleFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleFile {
    pub path: String,
    pub blake3: String,
    pub byte_length: u64,
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 96
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn digest_file(path: &Path, cancel: &dyn Fn() -> bool) -> Result<(String, u64)> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Hasher::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        ensure!(!cancel(), "delivery bundle cancelled");
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        bytes += count as u64;
    }
    Ok((hasher.finalize().to_hex().to_string(), bytes))
}

fn copy_create_new(
    source: &Path,
    destination: &Path,
    cancel: &dyn Fn() -> bool,
) -> Result<BundleFile> {
    let mut input = File::open(source)?;
    let mut temporary = destination.with_extension(format!("{}.partial", std::process::id()));
    let mut serial = 0_u32;
    while temporary.exists() {
        serial += 1;
        temporary =
            destination.with_extension(format!("{}.{}.partial", std::process::id(), serial));
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let copied = (|| -> Result<(String, u64)> {
        let mut hasher = Hasher::new();
        let mut bytes = 0_u64;
        let mut buffer = [0_u8; 1024 * 1024];
        loop {
            ensure!(!cancel(), "delivery bundle cancelled");
            let count = input.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            output.write_all(&buffer[..count])?;
            hasher.update(&buffer[..count]);
            bytes += count as u64;
        }
        output.sync_all()?;
        Ok((hasher.finalize().to_hex().to_string(), bytes))
    })();
    drop(output);
    let (blake3, byte_length) = copied?;
    // Hard-link publication is create-only: it cannot replace an unrelated file.
    fs::hard_link(&temporary, destination)
        .with_context(|| format!("publish {}", destination.display()))?;
    fs::remove_file(temporary)?;
    Ok(BundleFile {
        path: destination
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        blake3,
        byte_length,
    })
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<BundleFile> {
    // Every published output is complete, including small JSON/HTML files.
    // An interruption leaves a staging file which the owned resume can ignore.
    publish_create_new_synced(path, bytes)?;
    Ok(BundleFile {
        path: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        blake3: blake3::hash(bytes).to_hex().to_string(),
        byte_length: bytes.len() as u64,
    })
}

fn write_or_verify(path: &Path, bytes: &[u8]) -> Result<BundleFile> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "partial bundle contains a non-regular output file"
        );
        let expected = blake3::hash(bytes).to_hex().to_string();
        let (observed, length) = digest_file(path, &|| false)?;
        ensure!(
            observed == expected && length == bytes.len() as u64,
            "partial bundle output differs from the verified artifact; refusing to replace it"
        );
        return Ok(BundleFile {
            path: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            blake3: expected,
            byte_length: length,
        });
    }
    write_new(path, bytes)
}

fn copy_or_verify(
    source: &Path,
    destination: &Path,
    expected_digest: &str,
    expected_length: u64,
    cancel: &dyn Fn() -> bool,
) -> Result<BundleFile> {
    if let Ok(metadata) = fs::symlink_metadata(destination) {
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "partial bundle movie is not a regular file"
        );
        let (digest, length) = digest_file(destination, cancel)?;
        ensure!(
            digest == expected_digest && length == expected_length,
            "partial bundle movie differs from the verified artifact; refusing to replace it"
        );
        return Ok(BundleFile {
            path: destination
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            blake3: digest,
            byte_length: length,
        });
    }
    let copied = copy_create_new(source, destination, cancel)?;
    ensure!(
        copied.blake3 == expected_digest && copied.byte_length == expected_length,
        "copied file differs from its verified receipt"
    );
    Ok(copied)
}

fn render_frame_or_verify(
    movie: &Path,
    destination: &Path,
    seconds: f64,
    cancel: &dyn Fn() -> bool,
) -> Result<(String, u64)> {
    let mut temporary = destination.with_extension(format!("rendering-{}.jpg", std::process::id()));
    let mut serial = 0_u32;
    while temporary.exists() {
        serial += 1;
        temporary =
            destination.with_extension(format!("rendering-{}-{serial}.jpg", std::process::id()));
    }
    let mut cmd = tool_command("ffmpeg");
    cmd.args(["-v", "error", "-n", "-ss"])
        .arg(format!("{seconds:.6}"))
        .arg("-i")
        .arg(movie)
        .args(["-frames:v", "1", "-vf", "scale=480:-2", "-q:v", "3"])
        .arg(&temporary);
    let (status, stderr) = run_media(&mut cmd, movie, &temporary, &mut || cancel())?;
    ensure!(
        status.success() && temporary.is_file(),
        "contact frame render failed: {stderr}"
    );
    File::open(&temporary)?.sync_all()?;
    let (digest, length) = digest_file(&temporary, cancel)?;
    if let Ok(metadata) = fs::symlink_metadata(destination) {
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "partial contact frame is not a regular file"
        );
        let (existing_digest, existing_length) = digest_file(destination, cancel)?;
        ensure!(
            digest == existing_digest && length == existing_length,
            "partial contact frame differs from the verified artifact; refusing to replace it"
        );
    } else {
        fs::hard_link(&temporary, destination)?;
    }
    fs::remove_file(temporary)?;
    Ok((digest, length))
}

fn validate_partial_directory(
    path: &Path,
    expected_files: &std::collections::HashSet<String>,
) -> Result<()> {
    for item in fs::read_dir(path)? {
        let item = item?;
        let name = item.file_name().to_string_lossy().into_owned();
        let metadata = fs::symlink_metadata(item.path())?;
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "partial bundle contains a non-regular file"
        );
        let generated_temporary = expected_files.iter().any(|expected| {
            let stem = Path::new(expected)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy();
            (name.starts_with(&format!("{stem}.")) && name.ends_with(".partial"))
                || (name.starts_with(&format!("{stem}.rendering-")) && name.ends_with(".jpg"))
        });
        ensure!(
            expected_files.contains(&name) || generated_temporary,
            "partial bundle contains an unrelated output; refusing to adopt it"
        );
    }
    Ok(())
}

fn publish_create_new_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut temporary = path.with_extension(format!("{}.partial", std::process::id()));
    let mut serial = 0_u32;
    while temporary.exists() {
        serial += 1;
        temporary = path.with_extension(format!("{}.{}.partial", std::process::id(), serial));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::hard_link(&temporary, path).context("publish create-only bundle file")?;
    fs::remove_file(temporary)?;
    Ok(())
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn measured_movie_mapping(
    mapping: &PreviewCutMapping,
    movie_time_base_numerator: i64,
    movie_time_base_denominator: i64,
    movie_origin_pts: i64,
    video_time_base_numerator: i64,
    video_time_base_denominator: i64,
) -> Value {
    let video_pts = |movie_pts: i64| -> Option<i64> {
        if movie_time_base_denominator <= 0 || video_time_base_numerator <= 0 {
            return None;
        }
        let numerator = i128::from(movie_pts)
            * i128::from(movie_time_base_numerator)
            * i128::from(video_time_base_denominator);
        let denominator =
            i128::from(movie_time_base_denominator) * i128::from(video_time_base_numerator);
        (numerator % denominator == 0)
            .then(|| i64::try_from(numerator / denominator).ok())
            .flatten()
    };
    json!({
        "mapping": mapping,
        "movieStartSeconds": (mapping.movie_in_pts - movie_origin_pts) as f64
            * movie_time_base_numerator as f64 / movie_time_base_denominator as f64,
        "movieEndSeconds": (mapping.movie_out_pts - movie_origin_pts) as f64
            * movie_time_base_numerator as f64 / movie_time_base_denominator as f64,
        "videoStartPts": video_pts(mapping.movie_in_pts),
        "videoEndPts": video_pts(mapping.movie_out_pts)
    })
}

fn bundle_html(
    title: &str,
    manifest_name: &str,
    cuts: &[PreviewCutMapping],
    context: Option<&Value>,
    contacts: &[Value],
    handles: &[Value],
    movie_time_base_numerator: i64,
    movie_time_base_denominator: i64,
    movie_origin_pts: i64,
) -> String {
    let rows = cuts
        .iter()
        .map(|cut| {
            format!(
                "<tr><td>{}</td><td>{}</td><td>{:.3}–{:.3}s</td><td>{:.3}–{:.3}s</td></tr>",
                html_escape(&cut.operation_id),
                html_escape(&cut.asset_id),
                cut.source_in_seconds,
                cut.source_out_seconds,
                (cut.movie_in_pts - movie_origin_pts) as f64 * movie_time_base_numerator as f64
                    / movie_time_base_denominator as f64,
                (cut.movie_out_pts - movie_origin_pts) as f64 * movie_time_base_numerator as f64
                    / movie_time_base_denominator as f64
            )
        })
        .collect::<String>();
    let contact_section = if contacts.is_empty() {
        String::new()
    } else {
        let images = contacts.iter().map(|entry| format!(
            "<figure><img src=\"{}\" alt=\"{} {} frame\"><figcaption>Cut {} · source {} · {} · {}s</figcaption></figure>",
            html_escape(entry["file"].as_str().unwrap_or("")),
            html_escape(entry["operationId"].as_str().unwrap_or("cut")),
            html_escape(entry["position"].as_str().unwrap_or("")),
            html_escape(entry["operationId"].as_str().unwrap_or("cut")),
            html_escape(entry["assetId"].as_str().unwrap_or("")),
            html_escape(entry["position"].as_str().unwrap_or("")),
            entry["movieSeconds"].as_f64().unwrap_or_default()
        )).collect::<String>();
        format!("<section><h2>Cut contact frames</h2><div class=\"grid\">{images}</div></section>")
    };
    let evidence = context
        .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
        .map(|s| {
            format!(
                "<details><summary>Context evidence</summary><pre>{}</pre></details>",
                html_escape(&s)
            )
        })
        .unwrap_or_default();
    let handle_section = if handles.is_empty() {
        String::new()
    } else {
        let body = handles.iter().map(|handle| {
            let frames = handle["frames"].as_array().into_iter().flatten().map(|frame| format!(
                "<figure><img src=\"{}\" alt=\"{} source frame\"><figcaption>Source {} · {:.3}s</figcaption></figure>",
                html_escape(frame["file"].as_str().unwrap_or("")),
                html_escape(handle["assetId"].as_str().unwrap_or("")),
                html_escape(handle["assetId"].as_str().unwrap_or("")),
                frame["sourceSeconds"].as_f64().unwrap_or_default()
            )).collect::<String>();
            let audio = handle["audioFile"].as_str().map(|file| format!(
                "<p>Source audio · {} · {:.3}–{:.3}s</p><audio controls preload=none src=\"{}\"></audio>",
                html_escape(handle["assetId"].as_str().unwrap_or("")),
                handle["sourceInSeconds"].as_f64().unwrap_or_default(),
                handle["sourceOutSeconds"].as_f64().unwrap_or_default(),
                html_escape(file)
            )).unwrap_or_default();
            format!("<article><h3>Source handle · {}</h3>{}<div class=\"grid\">{}</div></article>",
                html_escape(handle["assetId"].as_str().unwrap_or("")), audio, frames)
        }).collect::<String>();
        format!("<section><h2>Separately labeled source handles</h2>{body}</section>")
    };
    format!(
        "<!doctype html><html><head><meta charset=utf-8><meta name=viewport content='width=device-width'><title>{}</title><style>body{{font:16px system-ui;max-width:1100px;margin:2rem auto;padding:0 1rem;color:#222}}video,audio{{width:100%;background:#111}}table{{border-collapse:collapse;width:100%}}td,th{{border-bottom:1px solid #ddd;text-align:left;padding:.5rem}}.grid{{display:grid;grid-template-columns:repeat(3,1fr);gap:.6rem}}.grid img{{width:100%}}small{{color:#555}}</style></head><body><h1>{}</h1><video controls preload=metadata src='movie.mp4'></video><p><small>Local verified review copy. Playback uses the rendered movie clock; source times are declared locators and are not exact source-frame identity.</small></p><h2>Cut and source mapping</h2><table><thead><tr><th>Cut ID</th><th>Source handle</th><th>Source in/out</th><th>Movie in/out</th></tr></thead><tbody>{}</tbody></table>{}<p><a href='{}'>Bundle manifest</a></p>{}{}</body></html>",
        html_escape(title),
        html_escape(title),
        rows,
        contact_section,
        manifest_name,
        evidence,
        handle_section
    )
}

fn existing_bundle(
    path: &Path,
    request: &EditPlanBundleRequest,
    artifact_id: &str,
    revision_id: &str,
    movie_digest: &str,
    movie_length: u64,
) -> Result<Option<EditPlanBundleResult>> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "bundle path must be a real directory"
    );
    let manifest = path.join("manifest.json");
    if !fs::symlink_metadata(&manifest).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink()) {
        return Ok(None);
    }
    let value: Value = serde_json::from_slice(&fs::read(&manifest)?)?;
    ensure!(
        value["schemaVersion"] == 1
            && value["verified"] == true
            && value["artifactId"] == artifact_id,
        "output bundle name already belongs to a different artifact"
    );
    ensure!(
        value["revisionId"] == revision_id
            && value["contactSheet"] == request.contact_sheet
            && value["contextEvidenceId"].as_str() == request.context_evidence_id.as_deref(),
        "existing bundle does not match this request"
    );
    let owner_path = path.join("bundle-owner.json");
    let owner_metadata = fs::symlink_metadata(&owner_path)?;
    ensure!(
        owner_metadata.is_file() && !owner_metadata.file_type().is_symlink(),
        "existing bundle ownership marker is not a regular file"
    );
    let owner: Value = serde_json::from_slice(&fs::read(owner_path)?)?;
    ensure!(
        owner
            == expected_owner(
                request,
                artifact_id,
                revision_id,
                movie_digest,
                movie_length
            ),
        "existing bundle ownership marker does not match this request"
    );
    let files: Vec<BundleFile> = serde_json::from_value(value["files"].clone())?;
    for required in [
        "bundle-owner.json",
        "movie.mp4",
        "mapping.json",
        "provenance.json",
        "coverage.json",
        "index.html",
    ] {
        ensure!(
            files.iter().any(|f| f.path == required),
            "existing bundle manifest is incomplete"
        );
    }
    ensure!(
        files
            .iter()
            .any(|f| f.path == "movie.mp4" && f.blake3 == movie_digest),
        "existing movie digest differs from verified artifact"
    );
    for entry in &files {
        let rel = Path::new(&entry.path);
        ensure!(
            rel.components().count() <= 3
                && rel.components().all(|c| matches!(c, Component::Normal(_))),
            "bundle manifest contains unsafe file path"
        );
        let file_path = path.join(rel);
        let file_meta = fs::symlink_metadata(&file_path)?;
        ensure!(
            file_meta.is_file() && !file_meta.file_type().is_symlink(),
            "bundle contains a non-regular file"
        );
        let (hash, len) = digest_file(&file_path, &|| false)?;
        ensure!(
            hash == entry.blake3 && len == entry.byte_length,
            "existing bundle file does not match manifest"
        );
    }
    let movie = path.join("movie.mp4");
    Ok(Some(EditPlanBundleResult {
        bundle_directory: path.into(),
        manifest_path: manifest,
        reused: true,
        artifact_id: artifact_id.into(),
        revision_id: value["revisionId"].as_str().unwrap_or_default().into(),
        movie_path: movie.clone(),
        movie_digest: files
            .iter()
            .find(|f| f.path == "movie.mp4")
            .map(|f| f.blake3.clone())
            .unwrap_or_default(),
        files,
    }))
}

fn expected_owner(
    request: &EditPlanBundleRequest,
    artifact_id: &str,
    revision_id: &str,
    movie_digest: &str,
    movie_length: u64,
) -> Value {
    let picture_digest = request
        .picture_coverage
        .as_ref()
        .and_then(|value| serde_json::to_vec(value).ok())
        .map(|bytes| blake3::hash(&bytes).to_hex().to_string());
    json!({
        "schemaVersion":1,"artifactId":artifact_id,"revisionId":revision_id,
        "movieDigest":movie_digest,"movieByteLength":movie_length,"name":request.name,
        "contactSheet":request.contact_sheet,"contextEvidenceId":request.context_evidence_id,
        "pictureCoverageDigest":picture_digest
    })
}

impl Catalog {
    /// Publish a standalone local review bundle from a freshly verified exact preview artifact.
    pub fn export_edit_plan_bundle(
        &self,
        request: &EditPlanBundleRequest,
        cancel: &dyn Fn() -> bool,
    ) -> Result<EditPlanBundleResult> {
        ensure!(
            safe_name(&request.name),
            "invalid input: bundle name must use 1-96 ASCII letters, digits, '_' or '-'"
        );
        ensure!(
            request.artifact_id.len() <= 255 && !request.artifact_id.is_empty(),
            "invalid input: artifact id"
        );
        ensure!(!cancel(), "delivery bundle cancelled");
        let status =
            self.preview_artifact_status_cancellable(&request.artifact_id, true, cancel)?;
        ensure!(
            status.observed_state == PreviewArtifactState::Complete
                && status.current_for_request == Some(true),
            "stale evidence: delivery requires a currently verified complete artifact"
        );
        let artifact = status.artifact;
        let validation = artifact
            .validation
            .as_ref()
            .context("verified artifact lacks media validation")?;
        let result = artifact
            .result
            .as_ref()
            .context("verified artifact lacks movie")?;
        ensure!(
            validation.fully_decoded && validation.output_fingerprint.len() == 64,
            "invalid receipt: movie lacks complete decode or full-file digest"
        );
        let source_movie = PathBuf::from(&result.output_path);
        let plan = self.edit_plan_revision(&artifact.revision_id)?;
        let mut context_value = if let Some(id) = request.context_evidence_id.as_deref() {
            ensure!(
                id.len() <= 255 && !id.is_empty(),
                "invalid input: context evidence id"
            );
            let body: Option<String> = self.connection()?.query_row(
                "SELECT body_json FROM assembled_review_contexts WHERE evidence_id=?1 AND artifact_id=?2",
                rusqlite::params![id, artifact.artifact_id], |row| row.get(0)).optional()?;
            let mut evidence = serde_json::from_str::<Value>(
                &body.context("context evidence does not belong to this artifact")?,
            )?;
            evidence["movie"]["outputPath"] = Value::String("movie.mp4".into());
            Some(evidence)
        } else {
            None
        };
        let parent = if request.output_directory.exists() {
            fs::canonicalize(&request.output_directory)?
        } else {
            fs::create_dir_all(&request.output_directory)?;
            fs::canonicalize(&request.output_directory)?
        };
        let bundle = parent.join(&request.name);
        let bundle_preexisting = fs::symlink_metadata(&bundle).is_ok();
        if bundle_preexisting {
            let metadata = fs::symlink_metadata(&bundle)?;
            ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "output bundle path must be a real directory"
            );
        } else {
            fs::create_dir(&bundle)
                .with_context(|| format!("create bundle directory {}", bundle.display()))?;
        }
        // Lock the directory inode itself. The OS releases this lock if the
        // process exits, so an interrupted owned bundle remains resumable.
        let directory_lock = File::open(&bundle)?;
        directory_lock
            .try_lock()
            .context("another bundle publisher owns this output directory")?;
        if bundle_preexisting {
            if let Some(existing) = existing_bundle(
                &bundle,
                request,
                &artifact.artifact_id,
                &artifact.revision_id,
                &validation.output_fingerprint,
                validation.byte_length,
            )? {
                return Ok(existing);
            }
            ensure!(
                fs::symlink_metadata(bundle.join("manifest.json")).is_err(),
                "existing bundle manifest is damaged; refusing to replace it"
            );
        }
        let mut files = Vec::new();
        let owner_value = expected_owner(
            request,
            &artifact.artifact_id,
            &artifact.revision_id,
            &validation.output_fingerprint,
            validation.byte_length,
        );
        let owner_bytes = serde_json::to_vec_pretty(&owner_value)?;
        let owner_path = bundle.join("bundle-owner.json");
        if bundle_preexisting {
            let metadata = fs::symlink_metadata(&owner_path)
                .context("existing partial directory is not owned by a bundle request")?;
            ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "partial bundle ownership marker is not a regular file"
            );
            let existing_owner: Value = serde_json::from_slice(
                &fs::read(&owner_path)
                    .context("existing partial directory is not owned by this bundle request")?,
            )?;
            ensure!(
                existing_owner == owner_value,
                "existing partial directory belongs to a different bundle request"
            );
            files.push(write_or_verify(&owner_path, &owner_bytes)?);
        } else {
            publish_create_new_synced(&owner_path, &owner_bytes)?;
            files.push(BundleFile {
                path: "bundle-owner.json".into(),
                blake3: blake3::hash(&owner_bytes).to_hex().to_string(),
                byte_length: owner_bytes.len() as u64,
            });
        }
        // Make ownership durable before any movie, image, or context file is
        // published into this directory.
        File::open(&bundle)?.sync_all()?;

        let mut expected_files = std::collections::HashSet::from([
            "bundle-owner.json".to_owned(),
            "movie.mp4".to_owned(),
            "mapping.json".to_owned(),
            "provenance.json".to_owned(),
            "coverage.json".to_owned(),
            "index.html".to_owned(),
            "manifest.json".to_owned(),
        ]);
        if request.contact_sheet && artifact.kind == "cutNeighborhood" {
            for (index, cut) in validation.cuts.iter().enumerate() {
                let safe_id: String = cut
                    .operation_id
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect();
                for label in ["start", "middle", "end"] {
                    expected_files.insert(format!("frame-{index:04}-{safe_id}-{label}.jpg"));
                }
            }
            expected_files.insert("contact-sheet.json".to_owned());
        }
        if let Some(context) = context_value.as_ref() {
            for (handle_index, handle) in context["sourceHandles"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                if handle["state"] != "complete" {
                    continue;
                }
                let inspection = &handle["inspection"];
                for frame_index in 0..inspection["frames"].as_array().map_or(0, Vec::len) {
                    expected_files.insert(format!(
                        "handle-{handle_index:02}-frame-{frame_index:02}.jpg"
                    ));
                }
                if inspection["audio"]["playable"].is_object() {
                    expected_files.insert(format!("handle-{handle_index:02}-audio.wav"));
                }
            }
        }
        if bundle_preexisting {
            validate_partial_directory(&bundle, &expected_files)?;
        }
        let movie = bundle.join("movie.mp4");
        let movie_file = copy_or_verify(
            &source_movie,
            &movie,
            &validation.output_fingerprint,
            validation.byte_length,
            cancel,
        )?;
        ensure!(
            movie_file.blake3 == validation.output_fingerprint
                && movie_file.byte_length == validation.byte_length,
            "verified movie changed while copying"
        );
        files.push(movie_file.clone());

        let selected_cut_ids = validation
            .cuts
            .iter()
            .map(|m| m.operation_id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let selected_operations = plan
            .operations
            .iter()
            .filter(|cut| selected_cut_ids.contains(cut.operation_id.as_str()))
            .collect::<Vec<_>>();
        let source_inventory = artifact.identity.as_ref().context("verified artifact has no input identity")?.sources.iter()
            .map(|source| self.get_asset(&source.asset_id).map(|asset| json!({
                "assetId": asset.id, "filename": asset.filename, "sourceFingerprint": asset.fingerprint,
                "sizeBytes": asset.size_bytes, "selectedRepresentation": source.representation,
                "representationFingerprint": source.representation_fingerprint, "sourceClock": source.clocks,
                "originalAvailable": asset.original_available, "proxyAvailable": asset.proxy_available
            }))).collect::<Result<Vec<_>>>()?;
        let mut contact_records = Vec::new();
        if request.contact_sheet && artifact.kind == "cutNeighborhood" {
            ensure!(
                validation.cuts.len() <= 12,
                "invalid receipt: contact sheet neighborhood exceeds 12 cuts"
            );
            for (index, cut) in validation.cuts.iter().enumerate() {
                let start = (cut.movie_in_pts - validation.movie_origin_pts) as f64
                    * validation.movie_time_base_numerator as f64
                    / validation.movie_time_base_denominator as f64;
                let end = (cut.movie_out_pts - validation.movie_origin_pts) as f64
                    * validation.movie_time_base_numerator as f64
                    / validation.movie_time_base_denominator as f64;
                let last_frame_start = end - (end - start) / cut.output_frame_count.max(1) as f64;
                for (label, seconds) in [
                    ("start", start),
                    ("middle", (start + end) / 2.0),
                    ("end", last_frame_start.max(start)),
                ] {
                    let safe_id: String = cut
                        .operation_id
                        .chars()
                        .map(|c| {
                            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                                c
                            } else {
                                '_'
                            }
                        })
                        .collect();
                    let filename = format!("frame-{index:04}-{safe_id}-{label}.jpg");
                    let dest = bundle.join(&filename);
                    let (hash, len) = render_frame_or_verify(&movie, &dest, seconds, cancel)?;
                    files.push(BundleFile {
                        path: filename.clone(),
                        blake3: hash,
                        byte_length: len,
                    });
                    contact_records.push(json!({"operationId":cut.operation_id,"assetId":cut.asset_id,"position":label,"movieSeconds":seconds,"file":filename}));
                }
            }
        }
        let mut handle_records = Vec::new();
        if let Some(context) = context_value.as_mut()
            && let Some(handles) = context["sourceHandles"].as_array_mut()
        {
            for (handle_index, handle) in handles.iter_mut().enumerate() {
                if handle["state"] != "complete" {
                    continue;
                }
                let inspection = &mut handle["inspection"];
                let asset_id = inspection["request"]["assetId"]
                    .as_str()
                    .unwrap_or("source")
                    .to_owned();
                let request_in = inspection["request"]["sourceInSeconds"]
                    .as_f64()
                    .unwrap_or_default();
                let request_out = inspection["request"]["sourceOutSeconds"]
                    .as_f64()
                    .unwrap_or_default();
                let mut frames = Vec::new();
                if let Some(source_frames) = inspection["frames"].as_array_mut() {
                    for (frame_index, frame) in source_frames.iter_mut().enumerate() {
                        let receipt = frame["image"].clone();
                        let source_path = PathBuf::from(
                            receipt["path"]
                                .as_str()
                                .context("source frame receipt lacks path")?,
                        );
                        let expected_digest = receipt["blake3"]
                            .as_str()
                            .context("source frame receipt lacks digest")?;
                        let expected_length = receipt["byteLength"]
                            .as_u64()
                            .context("source frame receipt lacks length")?;
                        let filename =
                            format!("handle-{handle_index:02}-frame-{frame_index:02}.jpg");
                        let copied = copy_or_verify(
                            &source_path,
                            &bundle.join(&filename),
                            expected_digest,
                            expected_length,
                            cancel,
                        )?;
                        ensure!(
                            copied.blake3 == expected_digest
                                && copied.byte_length == expected_length,
                            "source frame no longer matches its context receipt"
                        );
                        files.push(copied);
                        frame["image"]["path"] = Value::String(filename.clone());
                        frames.push(json!({"file":filename,"sourceSeconds":frame["sourceSeconds"],"representationPts":frame["representationPts"]}));
                    }
                }
                let audio_file = if inspection["audio"]["playable"].is_object() {
                    let receipt = inspection["audio"]["playable"].clone();
                    let source_path = PathBuf::from(
                        receipt["path"]
                            .as_str()
                            .context("source audio receipt lacks path")?,
                    );
                    let expected_digest = receipt["blake3"]
                        .as_str()
                        .context("source audio receipt lacks digest")?;
                    let expected_length = receipt["byteLength"]
                        .as_u64()
                        .context("source audio receipt lacks length")?;
                    let filename = format!("handle-{handle_index:02}-audio.wav");
                    let copied = copy_or_verify(
                        &source_path,
                        &bundle.join(&filename),
                        expected_digest,
                        expected_length,
                        cancel,
                    )?;
                    ensure!(
                        copied.blake3 == expected_digest && copied.byte_length == expected_length,
                        "source audio no longer matches its context receipt"
                    );
                    files.push(copied);
                    inspection["audio"]["playable"]["path"] = Value::String(filename.clone());
                    Some(filename)
                } else {
                    None
                };
                handle_records.push(json!({"assetId":asset_id,"sourceInSeconds":request_in,"sourceOutSeconds":request_out,
                    "frames":frames,"audioFile":audio_file}));
            }
        }
        let mut cut_rows = Vec::new();
        for cut in &selected_operations {
            let mapping = validation
                .cuts
                .iter()
                .find(|m| m.operation_id == cut.operation_id);
            let source = artifact
                .identity
                .as_ref()
                .and_then(|i| i.sources.iter().find(|s| s.asset_id == cut.asset_id));
            let measured = mapping.map(|m| {
                measured_movie_mapping(
                    m,
                    validation.movie_time_base_numerator,
                    validation.movie_time_base_denominator,
                    validation.movie_origin_pts,
                    validation.video_time_base_numerator,
                    validation.video_time_base_denominator,
                )
            });
            cut_rows.push(json!({"operationId":cut.operation_id,"position":cut.position,"assetId":cut.asset_id,
                "sourceInSeconds":cut.source_in_seconds,"sourceOutSeconds":cut.source_out_seconds,
                "sourceClock":source.map(|s|&s.clocks),"selectedRepresentation":source.map(|s|&s.representation),
                "movieMapping":measured}));
        }
        let review_coverage =
            serde_json::to_value(self.review_coverage_report(&artifact.revision_id, false)?)?;
        let picture_coverage = request
            .picture_coverage
            .as_ref()
            .map(|coverage_request| {
                ensure!(
                    coverage_request.revision_id == artifact.revision_id,
                    "invalid input: picture coverage revision must match the verified artifact"
                );
                self.picture_coverage(coverage_request)
            })
            .transpose()?;
        let mut chapters: Vec<Value> = Vec::new();
        let metadata = super::selection::load_metadata(&self.connection()?, &artifact.revision_id)?;
        if let Some(metadata) = metadata {
            for sequence in &metadata.input.sequences {
                let mut members = metadata
                    .assignments
                    .iter()
                    .filter(|a| a.sequence_id == sequence.sequence_id)
                    .filter_map(|a| {
                        plan.operations
                            .iter()
                            .find(|op| op.operation_id == a.operation_id)
                    })
                    .filter_map(|op| {
                        validation
                            .cuts
                            .iter()
                            .find(|m| m.operation_id == op.operation_id)
                            .map(|m| (op.position, m))
                    })
                    .collect::<Vec<_>>();
                members.sort_by_key(|(position, _)| *position);
                let mut start = 0;
                let mut segment = 0;
                while start < members.len() {
                    let mut end = start + 1;
                    while end < members.len() && members[end - 1].0 + 1 == members[end].0 {
                        end += 1;
                    }
                    let first = members[start].1;
                    let last = members[end - 1].1;
                    let time = |pts: i64| {
                        (pts - validation.movie_origin_pts) as f64
                            * validation.movie_time_base_numerator as f64
                            / validation.movie_time_base_denominator as f64
                    };
                    chapters.push(json!({"sequenceId":sequence.sequence_id,"title":sequence.goal,
                        "selectionSegment":segment,"movieStartSeconds":time(first.movie_in_pts),
                        "movieEndSeconds":time(last.movie_out_pts),"firstOperationId":first.operation_id,
                        "lastOperationId":last.operation_id,"scope":"canonical selected sequence span"}));
                    start = end;
                    segment += 1;
                }
            }
        }
        let mapping_doc = json!({"schemaVersion":1,"artifactId":artifact.artifact_id,"revisionId":artifact.revision_id,
            "movieClock":{"timeBaseNumerator":validation.movie_time_base_numerator,"timeBaseDenominator":validation.movie_time_base_denominator,"originPts":validation.movie_origin_pts,"durationSeconds":validation.duration_seconds},
            "videoClock":{"timeBaseNumerator":validation.video_time_base_numerator,"timeBaseDenominator":validation.video_time_base_denominator,"originPts":validation.video_origin_pts},
            "cuts":cut_rows,"audioMappings":context_value.as_ref().map(|v|&v["audioMappings"]),
            "chapters":chapters,"limitations":["Source locators describe declared source ranges; preview resampling and frame holds do not establish exact source-frame identity.","Chapter spans and titles come from canonical selection metadata; no boundaries or titles are inferred."]});
        files.push(write_or_verify(
            &bundle.join("mapping.json"),
            &serde_json::to_vec_pretty(&mapping_doc)?,
        )?);
        let inventory = json!({"schemaVersion":1,"projectId":artifact.project_id,"revisionId":artifact.revision_id,
            "artifactId":artifact.artifact_id,"previewIdentity":artifact.identity,"inputSources":source_inventory,
            "movie":{"path":"movie.mp4","blake3":movie_file.blake3,"byteLength":movie_file.byte_length},"provenance":{
                "verifiedCurrentForRequest":status.current_for_request,"fullyDecoded":validation.fully_decoded,
                "limitations":validation.limitations,"catalogProjectId":artifact.project_id}});
        files.push(write_or_verify(
            &bundle.join("provenance.json"),
            &serde_json::to_vec_pretty(&inventory)?,
        )?);
        let coverage_doc = json!({"schemaVersion":1,"revisionId":artifact.revision_id,"verifiedArtifactId":artifact.artifact_id,
            "reviewCoverage":review_coverage,"pictureCoverage":picture_coverage,
            "limitations":["Review coverage summarizes declared local evidence and is not human approval.",
                "Picture inventory coverage is included only when explicitly requested with a canonical inventory scope."]});
        files.push(write_or_verify(
            &bundle.join("coverage.json"),
            &serde_json::to_vec_pretty(&coverage_doc)?,
        )?);
        if !contact_records.is_empty() {
            let contact_doc = json!({"schemaVersion":1,"frames":contact_records,"maxFrames":36});
            files.push(write_or_verify(
                &bundle.join("contact-sheet.json"),
                &serde_json::to_vec_pretty(&contact_doc)?,
            )?);
        }
        let html = bundle_html(
            &request.name,
            "manifest.json",
            &validation.cuts,
            context_value.as_ref(),
            &contact_records,
            &handle_records,
            validation.movie_time_base_numerator,
            validation.movie_time_base_denominator,
            validation.movie_origin_pts,
        );
        files.push(write_or_verify(
            &bundle.join("index.html"),
            html.as_bytes(),
        )?);
        let manifest_value = json!({"schemaVersion":1,"artifactId":artifact.artifact_id,"revisionId":artifact.revision_id,
            "projectId":artifact.project_id,"kind":artifact.kind,"excerpt":artifact.excerpt,"createdAt":crate::now(),
            "verified":status.current_for_request==Some(true),"contactSheet":request.contact_sheet,
            "contextEvidenceId":request.context_evidence_id,"files":files});
        let manifest_bytes = serde_json::to_vec_pretty(&manifest_value)?;
        publish_create_new_synced(&bundle.join("manifest.json"), &manifest_bytes)?;
        File::open(&bundle)?.sync_all()?;
        Ok(EditPlanBundleResult {
            bundle_directory: bundle.clone(),
            manifest_path: bundle.join("manifest.json"),
            reused: bundle_preexisting,
            artifact_id: artifact.artifact_id,
            revision_id: plan.revision_id,
            movie_path: movie,
            movie_digest: movie_file.blake3,
            files,
        })
    }
}

#[cfg(test)]
#[path = "delivery_tests.rs"]
mod tests;
