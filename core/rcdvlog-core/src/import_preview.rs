//! Lazy, bounded generation of import-preview material — a poster frame,
//! probed duration, and probed capture time — for the preflight rows a
//! dialog is actually showing.
//!
//! `preflight.rs`'s module doc is explicit that ffprobe and ExifTool never
//! run during preflight itself; it calls that "import's problem, and only
//! for files that turn out to be new." This module is exactly that deferred
//! work, invoked separately from preflight and only for the caller's chosen
//! batch (e.g. the rows currently visible in the dialog), never for a whole
//! several-hundred-clip card at once.
//!
//! Nothing here touches the catalog or the project: there is no asset yet
//! for any of these files, so nothing is inserted into `asset_thumbnails` and
//! no project row is written. Poster frames land in a transient, project-
//! local cache the Tauri asset protocol already permits
//! (`<catalog parent>/cache/import-preview/`), named after the file's
//! sampled fingerprint — the same content identity preflight already
//! computed to classify it — so re-opening the dialog on the same card
//! reuses posters instead of re-decoding them.
//!
//! A failure here (an unreadable file, a missing ffmpeg/ffprobe) degrades
//! that one file's row to a placeholder; it is never surfaced as an import
//! blocker.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::{Catalog, probe_media_normalized, resolve_tool_path};

/// Up to this many ffprobe/ffmpeg processes run at once for one call. A
/// batch itself is expected to be small (a dialog asks only for the rows it
/// is showing), so this caps worst-case process/CPU pressure rather than
/// sharding a large job.
const IMPORT_PREVIEW_WORKERS: usize = 4;

/// One file the caller wants preview material for.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreviewRequest {
    pub path: String,
    /// `PreflightItem.fingerprint`, when preflight computed one for this
    /// file. Used as the poster cache key so identical content reuses the
    /// same poster; a request without one still works, it just cannot share
    /// a poster with a different path over the same bytes.
    #[serde(default)]
    pub fingerprint: Option<String>,
}

/// What import-preview could determine for one file, without mutating
/// anything. Any field being absent degrades only that field on the row;
/// `error` explains why, but a row missing its poster or its capture time is
/// still an otherwise-importable file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreviewItem {
    pub path: String,
    /// Absolute path to a generated poster JPEG, when extraction succeeded.
    pub thumbnail_path: Option<String>,
    pub duration_seconds: Option<f64>,
    /// The camera's own probed capture time (ffprobe `creation_time`,
    /// normalized). Distinct from `PreflightItem.filesystem_modified_at`,
    /// which is only ever the file's own filesystem mtime.
    pub captured_at: Option<String>,
    /// Set when probing or poster extraction failed for this file.
    pub error: Option<String>,
}

impl Catalog {
    /// Generates posters/duration/capture-time for a bounded batch of files
    /// — typically the rows an import-preflight dialog currently shows.
    ///
    /// Runs with up to `IMPORT_PREVIEW_WORKERS` concurrent ffprobe/ffmpeg
    /// processes. The bound this promises is concurrency, not request count:
    /// a caller that asks for hundreds of files at once still only runs a
    /// few processes at a time, just for longer. Callers that want a real
    /// bound on total work (so a several-hundred-clip card stays usable)
    /// achieve it by only ever asking for their visible rows.
    pub fn generate_import_preview(
        &self,
        requests: &[ImportPreviewRequest],
    ) -> Result<Vec<ImportPreviewItem>> {
        let cache_dir = self
            .database_path()
            .parent()
            .ok_or_else(|| anyhow::anyhow!("catalog has no parent directory"))?
            .join("cache")
            .join("import-preview");
        generate_import_preview_with_tools(
            requests,
            &cache_dir,
            &resolve_tool_path("ffprobe"),
            &resolve_tool_path("ffmpeg"),
        )
    }
}

/// The testable core: explicit tool paths so a test can point at a missing
/// binary without touching `PATH`, and an explicit cache directory so a test
/// never writes outside its own temp directory.
fn generate_import_preview_with_tools(
    requests: &[ImportPreviewRequest],
    cache_dir: &Path,
    ffprobe_program: &Path,
    ffmpeg_program: &Path,
) -> Result<Vec<ImportPreviewItem>> {
    if requests.is_empty() {
        return Ok(Vec::new());
    }
    std::fs::create_dir_all(cache_dir)
        .with_context(|| format!("create import preview cache {}", cache_dir.display()))?;

    let worker_count = requests.len().clamp(1, IMPORT_PREVIEW_WORKERS);
    let queue: Arc<Mutex<VecDeque<(usize, ImportPreviewRequest)>>> =
        Arc::new(Mutex::new(requests.iter().cloned().enumerate().collect()));
    let (sender, receiver) = mpsc::channel();
    thread::scope(|scope| {
        for _ in 0..worker_count {
            let queue = Arc::clone(&queue);
            let sender = sender.clone();
            scope.spawn(move || {
                loop {
                    let next = queue
                        .lock()
                        .expect("import preview queue lock poisoned")
                        .pop_front();
                    let Some((index, request)) = next else {
                        break;
                    };
                    let item = generate_one(&request, cache_dir, ffprobe_program, ffmpeg_program);
                    if sender.send((index, item)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);
    });

    let mut ordered: Vec<Option<ImportPreviewItem>> = vec![None; requests.len()];
    for (index, item) in receiver {
        ordered[index] = Some(item);
    }
    Ok(ordered.into_iter().flatten().collect())
}

fn generate_one(
    request: &ImportPreviewRequest,
    cache_dir: &Path,
    ffprobe_program: &Path,
    ffmpeg_program: &Path,
) -> ImportPreviewItem {
    let path = Path::new(&request.path);
    let mut item = ImportPreviewItem {
        path: request.path.clone(),
        thumbnail_path: None,
        duration_seconds: None,
        captured_at: None,
        error: None,
    };

    let probe = match probe_media_normalized(path, ffprobe_program, None) {
        Ok(probe) => probe,
        Err(error) => {
            item.error = Some(format!("{error:#}"));
            return item;
        }
    };
    item.duration_seconds = Some(probe.duration_seconds);
    item.captured_at = probe.best_time.or(probe.captured_at_raw);

    let key = request
        .fingerprint
        .clone()
        .unwrap_or_else(|| request.path.clone());
    match extract_poster(
        path,
        cache_dir,
        &key,
        probe.duration_seconds,
        ffmpeg_program,
    ) {
        Ok(poster_path) => item.thumbnail_path = Some(poster_path),
        Err(error) => {
            // Only the poster failed; duration and capture time already
            // succeeded above and stay on the row.
            item.error = Some(format!("{error:#}"));
        }
    }
    item
}

fn extract_poster(
    input: &Path,
    cache_dir: &Path,
    key: &str,
    duration_seconds: f64,
    ffmpeg_program: &Path,
) -> Result<String> {
    let safe_key = sanitize_cache_key(key);
    let output = cache_dir.join(format!("{safe_key}.jpg"));
    if output.is_file() {
        return Ok(output.to_string_lossy().into_owned());
    }
    let temporary = cache_dir.join(format!("{safe_key}.tmp.jpg"));
    let seek = (duration_seconds.max(0.0) * 0.2).to_string();
    let status = std::process::Command::new(ffmpeg_program)
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-ss",
            &seek,
            "-noautorotate",
            "-i",
        ])
        .arg(input)
        .args([
            "-frames:v",
            "1",
            "-vf",
            "scale='min(480,iw)':-2:force_original_aspect_ratio=decrease",
            "-q:v",
            "3",
        ])
        .arg(&temporary)
        .status()
        .context("start ffmpeg import-preview poster process")?;
    if !status.success() {
        let _ = std::fs::remove_file(&temporary);
        anyhow::bail!("ffmpeg import-preview poster generation failed with {status}");
    }
    std::fs::rename(&temporary, &output)?;
    Ok(output.to_string_lossy().into_owned())
}

/// Cache filenames stay path-safe regardless of what a fingerprint or
/// fallback path contains.
fn sanitize_cache_key(key: &str) -> String {
    let cleaned: String = key
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "file".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn ffmpeg_available() -> bool {
        crate::tool_command("ffmpeg")
            .arg("-version")
            .output()
            .is_ok()
            && crate::tool_command("ffprobe")
                .arg("-version")
                .output()
                .is_ok()
    }

    fn write_test_clip(path: &Path) {
        let status = crate::tool_command("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=navy:s=320x180:r=30:d=1",
                "-metadata",
                "creation_time=2026-07-18T09:00:00+09:00",
                "-c:v",
                "mpeg4",
            ])
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn generates_poster_duration_and_capture_time_and_reuses_the_cached_poster() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg/ffprobe unavailable; skipping import preview test");
            return;
        }
        let directory = tempdir().unwrap();
        let clip = directory.path().join("C0001.MP4");
        write_test_clip(&clip);
        let cache_dir = directory.path().join("cache/import-preview");
        let ffprobe = resolve_tool_path("ffprobe");
        let ffmpeg = resolve_tool_path("ffmpeg");

        let requests = vec![ImportPreviewRequest {
            path: clip.to_string_lossy().into_owned(),
            fingerprint: Some("fingerprint_c0001".to_string()),
        }];
        let results =
            generate_import_preview_with_tools(&requests, &cache_dir, &ffprobe, &ffmpeg).unwrap();
        assert_eq!(results.len(), 1);
        let item = &results[0];
        assert!(item.error.is_none());
        assert!(item.duration_seconds.unwrap() > 0.0);
        assert!(item.captured_at.is_some());
        let poster = PathBuf::from(item.thumbnail_path.as_deref().unwrap());
        assert!(poster.is_file());
        assert!(poster.ends_with("fingerprint_c0001.jpg"));

        // A second request for the same fingerprint must reuse the poster
        // rather than re-decode it: touch the source so a re-decode would
        // produce a different, newer file, then confirm the cached file is
        // untouched.
        let first_modified = std::fs::metadata(&poster).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let results_again =
            generate_import_preview_with_tools(&requests, &cache_dir, &ffprobe, &ffmpeg).unwrap();
        let poster_again = PathBuf::from(results_again[0].thumbnail_path.as_deref().unwrap());
        assert_eq!(
            std::fs::metadata(&poster_again)
                .unwrap()
                .modified()
                .unwrap(),
            first_modified,
            "an identical fingerprint must reuse the cached poster, not regenerate it"
        );
    }

    #[test]
    fn an_unreadable_file_degrades_only_its_own_row() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg/ffprobe unavailable; skipping import preview test");
            return;
        }
        let directory = tempdir().unwrap();
        let good_clip = directory.path().join("GOOD.MP4");
        write_test_clip(&good_clip);
        let bad_clip = directory.path().join("BAD.MP4");
        std::fs::write(&bad_clip, b"not actually a video").unwrap();
        let cache_dir = directory.path().join("cache/import-preview");
        let ffprobe = resolve_tool_path("ffprobe");
        let ffmpeg = resolve_tool_path("ffmpeg");

        let requests = vec![
            ImportPreviewRequest {
                path: bad_clip.to_string_lossy().into_owned(),
                fingerprint: Some("fingerprint_bad".to_string()),
            },
            ImportPreviewRequest {
                path: good_clip.to_string_lossy().into_owned(),
                fingerprint: Some("fingerprint_good".to_string()),
            },
        ];
        let results =
            generate_import_preview_with_tools(&requests, &cache_dir, &ffprobe, &ffmpeg).unwrap();
        assert_eq!(results.len(), 2);

        let bad = results
            .iter()
            .find(|item| item.path == requests[0].path)
            .unwrap();
        assert!(bad.error.is_some());
        assert!(bad.thumbnail_path.is_none());
        assert!(bad.duration_seconds.is_none());

        let good = results
            .iter()
            .find(|item| item.path == requests[1].path)
            .unwrap();
        assert!(good.error.is_none(), "one bad file must not fail the batch");
        assert!(good.thumbnail_path.is_some());
    }

    #[test]
    fn a_missing_media_tool_degrades_the_row_instead_of_erroring_the_batch() {
        let directory = tempdir().unwrap();
        let clip = directory.path().join("C0001.MP4");
        std::fs::write(
            &clip,
            b"irrelevant, ffprobe never gets far enough to read it",
        )
        .unwrap();
        let cache_dir = directory.path().join("cache/import-preview");
        let missing_ffprobe = directory.path().join("no-such-ffprobe-binary");
        let missing_ffmpeg = directory.path().join("no-such-ffmpeg-binary");

        let requests = vec![ImportPreviewRequest {
            path: clip.to_string_lossy().into_owned(),
            fingerprint: Some("fingerprint_c0001".to_string()),
        }];
        let results = generate_import_preview_with_tools(
            &requests,
            &cache_dir,
            &missing_ffprobe,
            &missing_ffmpeg,
        )
        .unwrap();
        assert_eq!(results.len(), 1);
        let item = &results[0];
        assert!(item.error.is_some());
        assert!(item.thumbnail_path.is_none());
        assert!(item.duration_seconds.is_none());
    }

    #[test]
    fn empty_batch_touches_nothing() {
        let directory = tempdir().unwrap();
        let cache_dir = directory.path().join("cache/import-preview");
        let results = generate_import_preview_with_tools(
            &[],
            &cache_dir,
            Path::new("ffprobe"),
            Path::new("ffmpeg"),
        )
        .unwrap();
        assert!(results.is_empty());
        assert!(
            !cache_dir.exists(),
            "an empty batch must not even create the cache dir"
        );
    }
}
