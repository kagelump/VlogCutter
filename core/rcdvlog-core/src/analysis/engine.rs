//! The boundary between this pipeline and whichever speech engine backs it.
//!
//! The trait lives here and its whisper.cpp implementation lives in
//! `whisper_engine`, so the orchestration in `stt` can be tested against a fake
//! and the external crate's API surface is confined to a single file.
//!
//! Models are fetched on demand rather than vendored: the whisper model alone is
//! 574 MB, against 9.9 GB free disk on the reference machine.

use anyhow::{Context, Result, anyhow, bail};
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
pub trait SttEngine {
    /// Detected speech spans in clip time. An empty result means no speech and
    /// is a successful outcome, not a failure.
    fn detect_speech(&self, pcm_path: &Path) -> Result<Vec<SpeechRegion>>;

    /// Decodes one packed window. The implementation concatenates the window's
    /// regions from `pcm_path` before decoding. Returned segment timestamps are
    /// in **window time** and are translated to clip time by the caller.
    fn decode_window(&self, pcm_path: &Path, window: &PackedWindow) -> Result<Vec<DecodedSegment>>;
}

/// Where downloaded models live: project-local cache, never beside originals.
pub fn model_dir(catalog_path: &Path) -> Result<PathBuf> {
    let parent = catalog_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| anyhow!("catalog has no parent directory"))?;
    Ok(parent.join("cache").join("models"))
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
    fn model_dir_sits_under_the_catalog_cache() {
        let directory = model_dir(Path::new("/projects/expo/catalog.sqlite3")).unwrap();
        assert!(directory.ends_with("cache/models"));
        assert!(directory.starts_with("/projects/expo"));
    }

    #[test]
    fn model_dir_rejects_a_catalog_path_without_a_parent() {
        assert!(model_dir(Path::new("catalog.sqlite3")).is_err());
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
