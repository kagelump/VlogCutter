//! The boundary between this pipeline and whichever speech engine backs it.
//!
//! The trait lives here and its whisper.cpp implementation lives in
//! `whisper_engine`, so the orchestration in `stt` can be tested against a fake
//! and the external crate's API surface is confined to a single file.
//!
//! Models are fetched on demand rather than vendored: the whisper model alone is
//! 574 MB, against 9.9 GB free disk on the reference machine.

use anyhow::{Context, Result, anyhow, bail};
use std::io::{Read, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use crate::analysis::quality::DecodedSegment;
use crate::analysis::windows::{PackedWindow, SpeechRegion};

/// A model file the app downloads and verifies for itself.
#[derive(Debug, Clone, Copy)]
pub struct ModelAsset {
    pub file_name: &'static str,
    pub url: &'static str,
    /// BLAKE3 digest of the published artifact. Empty until the real file has
    /// been fetched once and its digest recorded; `ensure_model` skips
    /// verification while it is empty and enforces it as soon as it is set.
    pub blake3: &'static str,
    pub bytes: u64,
}

/// Quantized large-v3-turbo. Measured at 0.57 aggregate real-time factor and
/// 1.15–1.38 GB peak RSS on CPU with four threads.
pub const WHISPER_MODEL: ModelAsset = ModelAsset {
    file_name: "ggml-large-v3-turbo-q5_0.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
    blake3: "",
    bytes: 574_000_000,
};

/// whisper.cpp's built-in Silero VAD. 885 KB, real-time factor 0.0019, and
/// byte-identical segment lists across repeat runs — no ONNX runtime required.
pub const VAD_MODEL: ModelAsset = ModelAsset {
    file_name: "ggml-silero-v5.1.2.bin",
    url: "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin",
    blake3: "",
    bytes: 885_000,
};

/// Speech detection and decoding, behind one seam.
///
/// Both methods take already-decoded samples rather than a path. A clip is read
/// and converted exactly once by the caller: taking a path here meant every
/// window re-read the whole file, which on a 19-minute clip is millions of
/// samples converted per decode.
pub trait SttEngine {
    /// Detected speech spans in clip time. An empty result means no speech and
    /// is a successful outcome, not a failure.
    fn detect_speech(&self, pcm: &[f32]) -> Result<Vec<SpeechRegion>>;

    /// Decodes one packed window. The implementation concatenates the window's
    /// regions out of `pcm` before decoding. Returned segment timestamps are in
    /// **window time** and are translated to clip time by the caller.
    fn decode_window(&self, pcm: &[f32], window: &PackedWindow) -> Result<Vec<DecodedSegment>>;
}

/// Where installed models live, shared by every project.
///
/// The models are large and identical for all projects, so a per-project copy
/// would cost 574 MB each. They live beside the application's other shared
/// state rather than in a project folder, and never beside originals.
pub fn shared_model_dir() -> Result<PathBuf> {
    let data_dir =
        dirs::data_dir().ok_or_else(|| anyhow!("cannot resolve application data directory"))?;
    Ok(data_dir.join("com.rcd.vlogcutter").join("models"))
}

/// The project-local model directory used before models were shared.
pub fn legacy_model_dir(catalog_path: &Path) -> Result<PathBuf> {
    let parent = catalog_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| anyhow!("catalog has no parent directory"))?;
    Ok(parent.join("cache").join("models"))
}

/// Resolves the directory a model should be read from for this catalog.
///
/// The shared directory wins, but a project that already holds the file keeps
/// working: installs made before models were shared must not silently start a
/// 574 MB re-download.
pub fn model_dir(catalog_path: &Path) -> Result<PathBuf> {
    let shared = shared_model_dir()?;
    if shared.join(WHISPER_MODEL.file_name).is_file() {
        return Ok(shared);
    }
    let legacy = legacy_model_dir(catalog_path)?;
    if legacy.join(WHISPER_MODEL.file_name).is_file() {
        return Ok(legacy);
    }
    Ok(shared)
}

/// Whether a model file is present and, when a digest is published, verifies.
pub fn model_is_installed(directory: &Path, model: &ModelAsset) -> bool {
    ensure_model(directory, model).is_ok()
}

/// Downloads one model into `directory`, verifying it before committing.
///
/// This is the only function that fetches a model, and nothing calls it except
/// an explicit user-initiated install. `on_progress` receives
/// `(downloaded_bytes, total_bytes)`; returning `ControlFlow::Break` abandons
/// the transfer and leaves nothing behind.
pub fn install_model<F>(directory: &Path, model: &ModelAsset, mut on_progress: F) -> Result<PathBuf>
where
    F: FnMut(u64, u64) -> ControlFlow<()>,
{
    let path = directory.join(model.file_name);
    if let Ok(existing) = ensure_model(directory, model) {
        return Ok(existing);
    }
    std::fs::create_dir_all(directory)
        .with_context(|| format!("create model directory {}", directory.display()))?;

    let mut response = reqwest::blocking::Client::builder()
        .timeout(None)
        .build()?
        .get(model.url)
        .send()
        .with_context(|| format!("request {}", model.url))?
        .error_for_status()
        .with_context(|| format!("download {}", model.file_name))?;
    let total = response.content_length().unwrap_or(model.bytes);

    // Written beside the destination and renamed only after verification, so an
    // interrupted or corrupt transfer can never be mistaken for an install.
    let temporary = directory.join(format!("{}.partial", model.file_name));
    let mut file = std::fs::File::create(&temporary)
        .with_context(|| format!("create {}", temporary.display()))?;
    let mut downloaded = 0_u64;
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        let read = response
            .read(&mut buffer)
            .with_context(|| format!("read {}", model.url))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .with_context(|| format!("write {}", temporary.display()))?;
        downloaded += read as u64;
        if on_progress(downloaded, total).is_break() {
            drop(file);
            let _ = std::fs::remove_file(&temporary);
            bail!("model download cancelled");
        }
    }
    file.sync_all()
        .with_context(|| format!("flush {}", temporary.display()))?;
    drop(file);

    if !model.blake3.is_empty() {
        let digest = blake3_file(&temporary)?;
        if digest != model.blake3 {
            let _ = std::fs::remove_file(&temporary);
            bail!(
                "downloaded {} failed verification: expected {}, found {digest}",
                model.file_name,
                model.blake3
            );
        }
    }
    std::fs::rename(&temporary, &path).with_context(|| format!("commit {}", path.display()))?;
    Ok(path)
}

/// Resolves a model file, verifying it if present.
///
/// Downloading is a separate, explicit provisioning step: a transcription batch
/// must not silently start a 574 MB transfer. When the file is absent this
/// returns an actionable error naming the file, its size, and where to put it.
pub fn ensure_model(directory: &Path, model: &ModelAsset) -> Result<PathBuf> {
    let path = directory.join(model.file_name);
    if path.is_file() {
        if !model.blake3.is_empty() {
            let digest = blake3_file(&path)?;
            if digest != model.blake3 {
                bail!(
                    "model {} failed verification: expected {}, found {digest}",
                    model.file_name,
                    model.blake3
                );
            }
        }
        return Ok(path);
    }
    bail!(
        "model {} is not installed ({} MB, from {}). Install it into {} before transcribing.",
        model.file_name,
        model.bytes / 1_000_000,
        model.url,
        directory.display()
    )
}

fn blake3_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn the_legacy_model_dir_sits_under_the_catalog_cache() {
        let directory = legacy_model_dir(Path::new("/projects/expo/catalog.sqlite3")).unwrap();
        assert!(directory.ends_with("cache/models"));
        assert!(directory.starts_with("/projects/expo"));
    }

    #[test]
    fn legacy_model_dir_rejects_a_catalog_path_without_a_parent() {
        assert!(legacy_model_dir(Path::new("catalog.sqlite3")).is_err());
    }

    #[test]
    fn the_shared_model_dir_is_not_project_scoped() {
        let shared = shared_model_dir().unwrap();
        assert!(shared.ends_with("com.rcd.vlogcutter/models"));
        assert!(!shared.starts_with("/projects/expo"));
    }

    #[test]
    fn model_dir_prefers_a_project_local_install_over_an_empty_shared_dir() {
        // A catalog that already holds the model keeps reading it there, so
        // moving to a shared directory never forces a 574 MB re-download.
        let project = tempdir().unwrap();
        let catalog = project.path().join("catalog.sqlite3");
        let legacy = legacy_model_dir(&catalog).unwrap();
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join(WHISPER_MODEL.file_name), b"installed here").unwrap();
        assert_eq!(model_dir(&catalog).unwrap(), legacy);
    }

    #[test]
    fn model_dir_falls_back_to_the_shared_dir_when_nothing_is_installed() {
        let project = tempdir().unwrap();
        let catalog = project.path().join("catalog.sqlite3");
        assert_eq!(model_dir(&catalog).unwrap(), shared_model_dir().unwrap());
    }

    #[test]
    fn model_status_reports_a_missing_model_without_touching_the_network() {
        let directory = tempdir().unwrap();
        assert!(!model_is_installed(directory.path(), &WHISPER_MODEL));
        fs::write(directory.path().join(WHISPER_MODEL.file_name), b"installed").unwrap();
        assert!(model_is_installed(directory.path(), &WHISPER_MODEL));
    }

    #[test]
    fn installing_an_already_present_model_does_not_download() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join(VAD_MODEL.file_name), b"already here").unwrap();
        // The URL is unreachable in tests, so returning Ok proves no transfer
        // was attempted.
        let installed = install_model(directory.path(), &VAD_MODEL, |_, _| {
            panic!("an installed model must not be downloaded again")
        })
        .unwrap();
        assert_eq!(installed, directory.path().join(VAD_MODEL.file_name));
    }

    #[test]
    fn ensure_model_returns_an_existing_file_without_downloading() {
        let directory = tempdir().unwrap();
        let path = directory.path().join(VAD_MODEL.file_name);
        fs::write(&path, b"already here").unwrap();
        // No network is available in tests; succeeding proves no download ran.
        let resolved = ensure_model(directory.path(), &VAD_MODEL).unwrap();
        assert_eq!(resolved, path);
    }

    #[test]
    fn ensure_model_reports_the_missing_file_and_its_size() {
        let directory = tempdir().unwrap();
        let error = ensure_model(directory.path(), &WHISPER_MODEL).unwrap_err();
        let message = error.to_string();
        assert!(message.contains(WHISPER_MODEL.file_name), "{message}");
        assert!(message.contains("574"), "{message}");
    }

    #[test]
    fn a_model_whose_digest_is_pinned_and_wrong_is_rejected() {
        let directory = tempdir().unwrap();
        let pinned = ModelAsset {
            file_name: "pinned.bin",
            url: "https://example.invalid/pinned.bin",
            blake3: "0000000000000000000000000000000000000000000000000000000000000000",
            bytes: 4,
        };
        fs::write(directory.path().join("pinned.bin"), b"data").unwrap();
        let error = ensure_model(directory.path(), &pinned).unwrap_err();
        assert!(error.to_string().contains("failed verification"), "{error}");
    }

    #[test]
    fn model_constants_name_the_measured_artifacts() {
        assert_eq!(WHISPER_MODEL.file_name, "ggml-large-v3-turbo-q5_0.bin");
        assert_eq!(VAD_MODEL.file_name, "ggml-silero-v5.1.2.bin");
    }
}
