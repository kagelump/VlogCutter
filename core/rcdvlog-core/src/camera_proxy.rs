//! Camera representations are immutable, node-local media, distinct from generated recipes.
//! Association is through a recorded same-asset location, never a filename search.
use crate::*;
use anyhow::ensure;

pub(crate) const REPRESENTATION: &str = "camera.h264-lrf.verified-v1";
const VALIDATOR: &str = "camera-sidecar.cfr-zero-origin.rgb-samples-v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraTimeMapping {
    /// Source and sidecar seconds have the same zero origin and pass at the same speed.
    pub source_offset_seconds: f64,
    pub sidecar_offset_seconds: f64,
    pub seconds_scale: u32,
    pub source_frame_rate: f64,
    pub sidecar_frame_rate: f64,
    /// Nominal sidecar frame n maps to original frame n * frame_reduction.
    pub frame_reduction: u32,
    pub source_frame_count: u64,
    pub sidecar_frame_count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraSidecarProvenance {
    pub original_location: String,
    pub original_relative_path: String,
    pub original_fingerprint: String,
    pub original_full_fingerprint: String,
    pub capture_sequence_id: Option<String>,
    pub validator: String,
    pub sample_times_seconds: Vec<f64>,
    pub max_sample_rgb_error: f64,
    pub time_mapping: CameraTimeMapping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraProxySidecar {
    pub asset_id: String,
    pub path: String,
    pub card_root: String,
    pub status: String,
    pub detail: String,
    pub byte_length: Option<u64>,
    pub fingerprint: Option<String>,
    pub codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_seconds: Option<f64>,
    pub verified_at: Option<String>,
    #[serde(default)]
    pub provenance: Option<CameraSidecarProvenance>,
}

impl CameraProxySidecar {
    fn pending(asset: &Asset, root: &Path, path: &Path) -> Self {
        Self {
            asset_id: asset.id.clone(),
            path: path.to_string_lossy().into_owned(),
            card_root: root.to_string_lossy().into_owned(),
            status: "missing".into(),
            detail: "no same-directory .LRF candidate at the recorded location on this card".into(),
            byte_length: None,
            fingerprint: None,
            codec: None,
            width: None,
            height: None,
            duration_seconds: None,
            verified_at: None,
            provenance: None,
        }
    }

    pub(crate) fn frame_source_identity(&self) -> Result<String> {
        // Bind cache identity to both bytes and the accepted time/association mapping.
        Ok(blake3::hash(&serde_json::to_vec(&(
            REPRESENTATION,
            &self.fingerprint,
            &self.provenance,
        ))?)
        .to_hex()
        .to_string())
    }
}

impl Catalog {
    fn camera_original_locations(&self, asset: &Asset, root: &Path) -> Result<Vec<PathBuf>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT local_path FROM project_asset_local_locations
             WHERE asset_id=?1 AND ((verification_state='verified' AND verified_fingerprint=?2)
                OR (local_path=?3 AND verification_state!='mismatch'))
               AND node_id=(SELECT node_id FROM catalog_identity WHERE singleton_id=1)
             ORDER BY local_path",
        )?;
        let paths = statement
            .query_map(params![asset.id, asset.fingerprint, asset.path], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(paths
            .into_iter()
            .map(PathBuf::from)
            .filter(|p| p.starts_with(root))
            .collect())
    }

    fn verify_camera_original(
        &self,
        asset: &Asset,
        root: &Path,
        original: &Path,
    ) -> Result<PathBuf> {
        let path = fs::canonicalize(original)
            .context("association_mismatch: recorded original is offline")?;
        ensure!(
            path.starts_with(root),
            "association_mismatch: original escapes the exact card root"
        );
        let size = fs::metadata(&path)?.len();
        ensure!(
            size == asset.size_bytes && sampled_fingerprint(&path, size)? == asset.fingerprint,
            "association_mismatch: recorded alternate no longer contains this asset (reused name/card)"
        );
        Ok(path)
    }

    /// Observational discovery, including verified same-asset alternate locations.
    /// Does not scan other cards, register locations, or write a catalog/cache.
    pub fn discover_camera_proxy_sidecars(
        &self,
        project_id: &str,
        card_root: impl AsRef<Path>,
    ) -> Result<Vec<CameraProxySidecar>> {
        let root = fs::canonicalize(card_root.as_ref())?;
        let mut results = Vec::new();
        for asset in self.list_assets(project_id)? {
            for original in self.camera_original_locations(&asset, &root)? {
                let path = original.with_extension("LRF");
                let mut result = CameraProxySidecar::pending(&asset, &root, &path);
                if path.exists() {
                    match self.validate_camera_candidate(&asset, &root, &original, &path) {
                        Ok(mut candidate) => {
                            if let Some(registered) = self.camera_proxy_sidecar(&asset.id)?
                                && registered.status == "registered"
                                && registered.path == candidate.path
                                && registered.card_root == candidate.card_root
                                && registered.provenance == candidate.provenance
                                && registered.fingerprint == candidate.fingerprint
                            {
                                candidate.status = "registered".into();
                                candidate.verified_at = registered.verified_at;
                            }
                            result = candidate;
                        }
                        Err(error) => {
                            result.detail = format!("{error:#}");
                            result.status = rejection_status(&result.detail).into();
                        }
                    }
                }
                results.push(result);
            }
        }
        Ok(results)
    }

    fn validate_camera_candidate(
        &self,
        asset: &Asset,
        root: &Path,
        original: &Path,
        sidecar: &Path,
    ) -> Result<CameraProxySidecar> {
        let original = self.verify_camera_original(asset, root, original)?;
        let path = fs::canonicalize(sidecar)?;
        ensure!(
            path.starts_with(root)
                && path.parent() == original.parent()
                && path.file_stem() == original.file_stem()
                && path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("LRF")),
            "association_mismatch: sidecar must be the same-directory, same-name .LRF on the exact card/run"
        );
        let original_hash = camera_sidecar_fingerprint(&original)?;
        if Path::new(&asset.path) != original && Path::new(&asset.path).is_file() {
            let preferred = Path::new(&asset.path);
            let size = fs::metadata(preferred)?.len();
            if size == asset.size_bytes
                && sampled_fingerprint(preferred, size)? == asset.fingerprint
            {
                ensure!(
                    camera_sidecar_fingerprint(preferred)? == original_hash,
                    "association_mismatch: alternate and available primary differ in full-file content"
                );
            }
        }
        let fingerprint = camera_sidecar_fingerprint(&path)?;
        let info = probe_media_stream(&path)?;
        ensure!(
            info.codec.eq_ignore_ascii_case("h264") && camera_sidecar_has_aac(&path)?,
            "unsupported: expected H.264 video and AAC audio"
        );
        let source_info = probe_media_stream(&original)?;
        ensure!(
            info.width > 0
                && info.height > 0
                && info.width < source_info.width
                && info.height < source_info.height,
            "mismatch: sidecar must have lower resolution than original"
        );
        let aspect_error = (info.width as f64 / info.height as f64
            - source_info.width as f64 / source_info.height as f64)
            .abs();
        ensure!(
            aspect_error < 0.015
                && info.metadata_rotation_degrees == source_info.metadata_rotation_degrees,
            "mismatch: sidecar aspect ratio or display rotation differs"
        );
        let source_timing = inspect_timing(&original)?;
        let sidecar_timing = inspect_timing(&path)?;
        let ratio = source_timing.rate / sidecar_timing.rate;
        ensure!(
            ratio.is_finite()
                && (1.0..=8.0).contains(&ratio)
                && (ratio - ratio.round()).abs() < 0.00001,
            "unsupported_rate_mapping: require equal fps or an integer frame-rate reduction from 1 through 8"
        );
        let reduction = ratio.round() as u32;
        let tolerance = 1.0 / sidecar_timing.rate + 0.002;
        ensure!(
            (source_timing.duration - sidecar_timing.duration).abs() <= tolerance,
            "mismatch: duration exceeds one sidecar frame; retiming or truncation is unsupported"
        );
        ensure!(
            (source_timing.duration - asset.duration_seconds).abs() <= tolerance,
            "mismatch: alternate duration differs from catalog source"
        );
        verify_camera_sidecar_decode(&path)?;
        let duration = source_timing.duration.min(sidecar_timing.duration);
        let mut sample_times = Vec::new();
        let mut max_error = 0.0_f64;
        for fraction in [0.05, 0.25, 0.5, 0.75, 0.95] {
            let time = (duration * fraction * sidecar_timing.rate).floor() / sidecar_timing.rate;
            let a = sample_rgb(&original, time)?;
            let b = sample_rgb(&path, time)?;
            let error = a
                .iter()
                .zip(&b)
                .map(|(a, b)| (*a as f64 - *b as f64).abs())
                .sum::<f64>()
                / a.len() as f64;
            ensure!(
                error <= 16.0,
                "content_mismatch: source and sidecar sample differ at {time:.6}s (RGB mean error {error:.2})"
            );
            max_error = max_error.max(error);
            sample_times.push(time);
        }
        ensure!(
            camera_sidecar_fingerprint(&original)? == original_hash
                && camera_sidecar_fingerprint(&path)? == fingerprint,
            "changed_during_verification: media bytes changed; registration refused"
        );
        let sequence = self
            .connection()?
            .query_row(
                "SELECT capture_sequence_id FROM asset_capture_sequence WHERE asset_id=?1",
                [&asset.id],
                |r| r.get(0),
            )
            .optional()?;
        let mut result = CameraProxySidecar::pending(asset, root, &path);
        result.status = "candidate".into();
        result.detail = "verified association, full decode, CFR time mapping and bounded content samples; explicit registration required".into();
        result.byte_length = Some(fs::metadata(&path)?.len());
        result.fingerprint = Some(fingerprint);
        result.codec = Some(info.codec);
        result.width = Some(info.width);
        result.height = Some(info.height);
        result.duration_seconds = Some(sidecar_timing.duration);
        result.provenance = Some(CameraSidecarProvenance {
            original_location: original.to_string_lossy().into_owned(),
            original_relative_path: original.strip_prefix(root)?.to_string_lossy().into_owned(),
            original_fingerprint: asset.fingerprint.clone(),
            original_full_fingerprint: original_hash,
            capture_sequence_id: sequence,
            validator: VALIDATOR.into(),
            sample_times_seconds: sample_times,
            max_sample_rgb_error: max_error,
            time_mapping: CameraTimeMapping {
                source_offset_seconds: 0.0,
                sidecar_offset_seconds: 0.0,
                seconds_scale: 1,
                source_frame_rate: source_timing.rate,
                sidecar_frame_rate: sidecar_timing.rate,
                frame_reduction: reduction,
                source_frame_count: source_timing.count,
                sidecar_frame_count: sidecar_timing.count,
            },
        });
        Ok(result)
    }

    /// Repeat all verification under the same per-asset claim used by proxy writers,
    /// then atomically replace just the camera representation row.
    pub fn register_camera_proxy_sidecar(
        &self,
        asset_id: &str,
        card_root: impl AsRef<Path>,
        sidecar: impl AsRef<Path>,
    ) -> Result<CameraProxySidecar> {
        self.get_asset(asset_id)?;
        let _claim = self.claim_representation(asset_id, RepresentationKind::Proxy)?;
        let asset = self.get_asset(asset_id)?;
        let root = fs::canonicalize(card_root.as_ref())?;
        let path = fs::canonicalize(sidecar.as_ref())?;
        let original = self.camera_original_locations(&asset, &root)?.into_iter().find(|original|
            original.parent() == path.parent() && original.file_stem() == path.file_stem()
        ).ok_or_else(|| anyhow!("association_mismatch: no verified same-asset original at this exact card/root/run/name"))?;
        let mut result = self.validate_camera_candidate(&asset, &root, &original, &path)?;
        result.verified_at = Some(now());
        self.connection()?.execute(
            "INSERT INTO camera_proxy_sidecars(asset_id,path,card_root,byte_length,fingerprint,codec,width,height,duration_seconds,verified_at,verification,provenance_json)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'verified',?11)
             ON CONFLICT(asset_id) DO UPDATE SET path=excluded.path,card_root=excluded.card_root,byte_length=excluded.byte_length,
             fingerprint=excluded.fingerprint,codec=excluded.codec,width=excluded.width,height=excluded.height,
             duration_seconds=excluded.duration_seconds,verified_at=excluded.verified_at,verification='verified',provenance_json=excluded.provenance_json",
            params![asset_id,result.path,result.card_root,result.byte_length.map(|s| s as i64),result.fingerprint,
                result.codec,result.width,result.height,result.duration_seconds,result.verified_at,serde_json::to_string(&result.provenance)?])?;
        result.status = "registered".into();
        result.detail = "registered verified camera representation with source-time mapping".into();
        Ok(result)
    }

    pub fn camera_proxy_sidecar(&self, asset_id: &str) -> Result<Option<CameraProxySidecar>> {
        self.get_asset(asset_id)?;
        let connection = self.connection()?;
        // Old catalogs stay observational; legacy verification never invents a mapping.
        let provenance_column =
            if table_column_exists(&connection, "camera_proxy_sidecars", "provenance_json")? {
                "provenance_json"
            } else {
                "NULL"
            };
        let row = connection.query_row(&format!(
            "SELECT path,card_root,byte_length,fingerprint,codec,width,height,duration_seconds,verified_at,{provenance_column} FROM camera_proxy_sidecars WHERE asset_id=?1"),
            [asset_id], |r| Ok((CameraProxySidecar {
                asset_id: asset_id.into(), path:r.get(0)?,card_root:r.get(1)?,status:"stale".into(),detail:String::new(),
                byte_length:Some(r.get::<_,i64>(2)? as u64),fingerprint:r.get(3)?,codec:r.get(4)?,width:r.get(5)?,height:r.get(6)?,
                duration_seconds:r.get(7)?,verified_at:r.get(8)?,provenance:None,
            }, r.get::<_,Option<String>>(9)?))).optional()?;
        let Some((mut result, provenance)) = row else {
            return Ok(None);
        };
        result.provenance = provenance.map(|v| serde_json::from_str(&v)).transpose()?;
        let scoped = fs::canonicalize(&result.path)
            .is_ok_and(|p| p == Path::new(&result.path) && p.starts_with(&result.card_root));
        let current = scoped
            && fs::metadata(&result.path)
                .is_ok_and(|m| m.is_file() && Some(m.len()) == result.byte_length)
            && camera_sidecar_fingerprint(Path::new(&result.path)).ok() == result.fingerprint;
        if current {
            result.status = "registered".into();
            result.detail = "registered sidecar verified and available".into();
        } else {
            result.detail =
                "registered sidecar is missing or changed; re-register after verification".into();
        }
        Ok(Some(result))
    }

    pub fn resolve_camera_proxy_for_review(
        &self,
        asset_id: &str,
    ) -> Result<Option<CameraProxySidecar>> {
        Ok(self
            .camera_proxy_sidecar(asset_id)?
            .filter(|s| s.status == "registered"))
    }

    /// Build a local frame pack from the explicitly registered camera representation,
    /// including its own identity. Does not rewrite media or imply RawV3 readiness.
    pub fn camera_proxy_frame_pack(&self, asset_id: &str) -> Result<FramePackManifest> {
        let sidecar = self
            .resolve_camera_proxy_for_review(asset_id)?
            .ok_or_else(|| anyhow!("verified camera sidecar is unavailable"))?;
        ensure!(
            sidecar
                .provenance
                .as_ref()
                .is_some_and(|p| p.validator == VALIDATOR),
            "sidecar needs explicit re-registration with a verified time mapping"
        );
        let mut manifest =
            build_frame_pack(Path::new(&sidecar.path), &sidecar.frame_source_identity()?)?;
        manifest.representation = REPRESENTATION.into();
        ensure!(
            self.resolve_camera_proxy_for_review(asset_id)?
                .is_some_and(
                    |s| s.fingerprint == sidecar.fingerprint && s.provenance == sidecar.provenance
                ),
            "sidecar changed during frame sampling"
        );
        Ok(manifest)
    }
}

fn rejection_status(detail: &str) -> &'static str {
    if detail.contains("association_mismatch:") {
        "association_mismatch"
    } else if detail.contains("unsupported_rate_mapping:") {
        "unsupported_rate_mapping"
    } else if detail.contains("unsupported_timing:") {
        "unsupported_timing"
    } else if detail.contains("content_mismatch:") {
        "content_mismatch"
    } else if detail.contains("mismatch:") {
        "mismatch"
    } else {
        "truncated_or_unreadable"
    }
}

struct Timing {
    rate: f64,
    duration: f64,
    count: u64,
}
fn inspect_timing(path: &Path) -> Result<Timing> {
    let document = probe_media_document(path)?;
    let stream = require_video_stream(&document)?;
    let rate = stream["r_frame_rate"]
        .as_str()
        .and_then(parse_rate)
        .filter(|r| r.is_finite() && *r > 0.0)
        .ok_or_else(|| anyhow!("unsupported_rate_mapping: missing positive frame rate"))?;
    let duration = stream["duration"]
        .as_str()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .or_else(|| video_duration(&document, stream))
        .ok_or_else(|| anyhow!("mismatch: no positive duration"))?;
    let mut command = tool_command("ffprobe");
    command
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=pts_time",
            "-of",
            "csv=p=0",
        ])
        .arg(path);
    let output = crate::stall::output_with_timeout(
        &mut command,
        "verify camera frame timestamps",
        crate::stall::io_stall_timeout(),
    )?;
    ensure!(
        output.status.success(),
        "truncated_or_unreadable: frame timestamp decode failed"
    );
    // Packet PTS scanning avoids decoding a large 4K original just to inspect
    // its timeline. Sort presentation times because compressed packets may be
    // emitted in B-frame decode order. The sidecar is fully decoded separately.
    let mut times = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
    {
        let time = line
            .split(',')
            .next()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .ok_or_else(|| anyhow!("unsupported_timing: unreadable frame timestamp"))?;
        ensure!(
            time.is_finite(),
            "unsupported_timing: non-finite frame timestamp"
        );
        times.push(time);
    }
    times.sort_by(f64::total_cmp);
    for (index, time) in times.iter().enumerate() {
        ensure!(
            (*time - index as f64 / rate).abs() <= 0.002,
            "unsupported_timing: require zero-origin constant-rate timestamps (frame {index})"
        );
    }
    let count = times.len() as u64;
    ensure!(
        count > 0 && (count as f64 / rate - duration).abs() <= 1.0 / rate + 0.002,
        "truncated_or_unreadable: decoded frame count does not cover declared duration"
    );
    Ok(Timing {
        rate,
        duration,
        count,
    })
}

fn sample_rgb(path: &Path, time: f64) -> Result<Vec<u8>> {
    let mut command = tool_command("ffmpeg");
    command
        .args([
            "-v",
            "error",
            "-noautorotate",
            "-ss",
            &format!("{time:.9}"),
            "-i",
        ])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            "scale=32:18",
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "-",
        ]);
    let output = crate::stall::output_with_timeout(
        &mut command,
        "verify camera content sample",
        crate::stall::io_stall_timeout(),
    )?;
    ensure!(
        output.status.success() && output.stdout.len() == 32 * 18 * 3,
        "truncated_or_unreadable: content sample could not be decoded"
    );
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{TempDir, tempdir};

    struct Fixture {
        dir: TempDir,
        catalog: Catalog,
        project: Project,
        asset: Asset,
        root: PathBuf,
        original: PathBuf,
        sidecar: PathBuf,
    }

    fn ffmpeg(args: &[&str], path: &Path) {
        let output = tool_command("ffmpeg")
            .args(["-y", "-v", "error"])
            .args(args)
            .args(["-f", "mp4"])
            .arg(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn original(path: &Path, color: &str, rate: &str) {
        ffmpeg(
            &[
                "-f",
                "lavfi",
                "-i",
                &format!(
                    "{color}{}s=640x360:r={rate}:d=2",
                    if color.contains('=') { ":" } else { "=" }
                ),
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=2",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ],
            path,
        );
    }

    fn proxy(source: &Path, path: &Path, filter: &str) {
        ffmpeg(
            &[
                "-i",
                source.to_str().unwrap(),
                "-vf",
                filter,
                "-fps_mode",
                "passthrough",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-movflags",
                "+faststart",
            ],
            path,
        );
    }

    fn fixture(rate: &str) -> Fixture {
        let dir = tempdir().unwrap();
        let catalog = Catalog::open(dir.path().join("catalog.sqlite3")).unwrap();
        let project = catalog
            .create_project("Synthetic alternate sidecars")
            .unwrap();
        let organized = dir.path().join("organized");
        fs::create_dir_all(&organized).unwrap();
        let primary = organized.join("organized-name.MP4");
        original(&primary, "testsrc2", rate);
        catalog.ingest_folder(&project.id, &organized).unwrap();
        let asset = catalog.list_assets(&project.id).unwrap().remove(0);
        let root = dir.path().join("card-a");
        let run = root.join("DCIM/run-001");
        fs::create_dir_all(&run).unwrap();
        let original = run.join("CLIP0001.MP4");
        fs::copy(&primary, &original).unwrap();
        catalog.ingest_folder(&project.id, &root).unwrap();
        assert_eq!(catalog.get_asset(&asset.id).unwrap().path, asset.path);
        let sidecar = run.join("CLIP0001.LRF");
        Fixture {
            dir,
            catalog,
            project,
            asset,
            root,
            original,
            sidecar,
        }
    }

    #[test]
    fn verified_alternate_supports_reduced_rate_offline_frames_and_claims() {
        let f = fixture("60");
        proxy(&f.original, &f.sidecar, "fps=30,scale=320:180");
        let original_bytes = fs::read(&f.original).unwrap();
        let sidecar_bytes = fs::read(&f.sidecar).unwrap();
        let readonly =
            Catalog::open_read_only_existing(f.dir.path().join("catalog.sqlite3")).unwrap();
        let discovered = readonly
            .discover_camera_proxy_sidecars(&f.project.id, &f.root)
            .unwrap();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].status, "candidate", "{discovered:?}");
        assert!(
            readonly
                .camera_proxy_sidecar(&f.asset.id)
                .unwrap()
                .is_none()
        );
        assert!(
            !f.dir.path().join("cache").exists(),
            "discovery is observational"
        );
        let claim = f
            .catalog
            .claim_representation(&f.asset.id, RepresentationKind::Proxy)
            .unwrap();
        let busy = f
            .catalog
            .register_camera_proxy_sidecar(&f.asset.id, &f.root, &f.sidecar)
            .unwrap_err();
        assert!(busy.downcast_ref::<RepresentationBusy>().is_some());
        drop(claim);
        let registered = f
            .catalog
            .register_camera_proxy_sidecar(&f.asset.id, &f.root, &f.sidecar)
            .unwrap();
        let provenance = registered.provenance.as_ref().unwrap();
        assert_eq!(provenance.original_location, f.original.to_string_lossy());
        assert_eq!(
            provenance.original_relative_path,
            "DCIM/run-001/CLIP0001.MP4"
        );
        assert_eq!(provenance.time_mapping.frame_reduction, 2);
        assert_eq!(provenance.time_mapping.source_frame_count, 120);
        assert_eq!(provenance.time_mapping.sidecar_frame_count, 60);
        assert_eq!(provenance.time_mapping.seconds_scale, 1);
        assert_eq!(provenance.sample_times_seconds.len(), 5);
        assert_eq!(fs::read(&f.original).unwrap(), original_bytes);
        assert_eq!(fs::read(&f.sidecar).unwrap(), sidecar_bytes);
        assert_eq!(
            f.catalog
                .discover_camera_proxy_sidecars(&f.project.id, &f.root)
                .unwrap()[0]
                .status,
            "registered"
        );
        let asset = f.catalog.get_asset(&f.asset.id).unwrap();
        assert_eq!(asset.path, f.asset.path);
        assert_eq!(asset.revision, f.asset.revision);
        assert!(asset.proxy_path.is_none());
        // Originals can leave after association has been proven and persisted.
        fs::rename(&f.original, f.original.with_extension("offline")).unwrap();
        fs::rename(
            &asset.path,
            Path::new(&asset.path).with_extension("offline"),
        )
        .unwrap();
        let reopened =
            Catalog::open_read_only_existing(f.dir.path().join("catalog.sqlite3")).unwrap();
        let status = reopened
            .resolve_camera_proxy_for_review(&asset.id)
            .unwrap()
            .unwrap();
        assert_eq!(status.provenance, registered.provenance);
        let manifest = reopened.camera_proxy_frame_pack(&asset.id).unwrap();
        assert_eq!(manifest.representation, REPRESENTATION);
        assert_eq!(
            manifest.source_fingerprint,
            status.frame_source_identity().unwrap()
        );
        assert!(!manifest.timestamps_ms.is_empty());
        assert!(manifest.timestamps_ms.iter().all(|t| *t < 2000));
        let frames = encode_frames(&f.sidecar, &manifest, &f.dir.path().join("frames")).unwrap();
        assert_eq!(frames.len(), manifest.timestamps_ms.len());
        assert!(frames.iter().all(|p| fs::metadata(p).unwrap().len() > 0));
        let mut changed = sidecar_bytes;
        let last = changed.len() - 1;
        changed[last] ^= 1; // Same length still invalidates the full-file identity.
        fs::write(&f.sidecar, changed).unwrap();
        assert_eq!(
            reopened
                .camera_proxy_sidecar(&asset.id)
                .unwrap()
                .unwrap()
                .status,
            "stale"
        );
        assert!(reopened.camera_proxy_frame_pack(&asset.id).is_err());
    }

    #[test]
    fn wrong_card_unrecorded_copy_and_reused_location_are_rejected() {
        let f = fixture("60");
        proxy(&f.original, &f.sidecar, "fps=30,scale=320:180");
        let other_root = f.dir.path().join("card-b");
        let other_run = other_root.join("DCIM/run-001");
        fs::create_dir_all(&other_run).unwrap();
        let other = other_run.join("CLIP0001.MP4");
        let other_sidecar = other_run.join("CLIP0001.LRF");
        original(&other, "color=c=blue", "60");
        proxy(&other, &other_sidecar, "fps=30,scale=320:180");
        f.catalog.ingest_folder(&f.project.id, &other_root).unwrap();
        let err = f
            .catalog
            .register_camera_proxy_sidecar(&f.asset.id, &other_root, &other_sidecar)
            .unwrap_err();
        assert!(err.to_string().contains("association_mismatch"), "{err:#}");
        let discovered = f
            .catalog
            .discover_camera_proxy_sidecars(&f.project.id, &other_root)
            .unwrap();
        assert!(discovered.iter().all(|s| s.asset_id != f.asset.id));
        fs::copy(&f.original, &other).unwrap();
        // Even matching bytes at an unrecorded same-asset location do not confer association.
        assert!(
            f.catalog
                .register_camera_proxy_sidecar(&f.asset.id, &other_root, &other_sidecar)
                .is_err()
        );
        f.catalog.connection().unwrap().execute(
            "UPDATE project_asset_local_locations SET verification_state='unverified' WHERE asset_id=?1 AND local_path=?2",
            params![f.asset.id, f.original.to_string_lossy()]).unwrap();
        let err = f
            .catalog
            .register_camera_proxy_sidecar(&f.asset.id, &f.root, &f.sidecar)
            .unwrap_err();
        assert!(err.to_string().contains("association_mismatch"));
        f.catalog.ingest_folder(&f.project.id, &f.root).unwrap();
        fs::copy(&other_sidecar, &f.sidecar).unwrap();
        let err = f
            .catalog
            .register_camera_proxy_sidecar(&f.asset.id, &f.root, &f.sidecar)
            .unwrap_err();
        assert!(err.to_string().contains("content_mismatch"), "{err:#}");
        original(&f.original, "color=c=red", "60"); // Card reused the same counter/path.
        let discovered = f
            .catalog
            .discover_camera_proxy_sidecars(&f.project.id, &f.root)
            .unwrap();
        assert_eq!(discovered[0].status, "association_mismatch");
        assert!(
            f.catalog
                .register_camera_proxy_sidecar(&f.asset.id, &f.root, &f.sidecar)
                .is_err()
        );
        assert!(
            f.catalog
                .camera_proxy_sidecar(&f.asset.id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn invalid_timing_content_and_truncation_never_replace_a_valid_registration() {
        let f = fixture("60");
        proxy(&f.original, &f.sidecar, "fps=30,scale=320:180");
        let good = f
            .catalog
            .register_camera_proxy_sidecar(&f.asset.id, &f.root, &f.sidecar)
            .unwrap();
        let bytes = fs::read(&f.sidecar).unwrap();
        for (filter, reason) in [
            ("fps=24,scale=320:180", "unsupported_rate_mapping"),
            ("fps=120,scale=320:180", "unsupported_rate_mapping"),
            ("fps=30,trim=end=1,scale=320:180", "mismatch"),
            (
                "fps=30,setpts=PTS+0.5/TB,scale=320:180",
                "unsupported_timing",
            ),
            (
                "fps=30,select=not(eq(n\\,20)),scale=320:180",
                "unsupported_timing",
            ),
        ] {
            proxy(&f.original, &f.sidecar, filter);
            let err = f
                .catalog
                .register_camera_proxy_sidecar(&f.asset.id, &f.root, &f.sidecar)
                .unwrap_err();
            assert!(format!("{err:#}").contains(reason), "{filter}: {err:#}");
            let status = f
                .catalog
                .camera_proxy_sidecar(&f.asset.id)
                .unwrap()
                .unwrap();
            assert_eq!(
                status.fingerprint, good.fingerprint,
                "failed verification replaced a row"
            );
            assert_eq!(status.provenance, good.provenance);
            assert_eq!(status.verified_at, good.verified_at);
        }
        fs::write(&f.sidecar, &bytes[..bytes.len() / 2]).unwrap();
        assert!(
            f.catalog
                .register_camera_proxy_sidecar(&f.asset.id, &f.root, &f.sidecar)
                .is_err()
        );
        assert_ne!(
            f.catalog
                .discover_camera_proxy_sidecars(&f.project.id, &f.root)
                .unwrap()[0]
                .status,
            "candidate"
        );
        fs::write(&f.sidecar, bytes).unwrap();
        assert_eq!(
            f.catalog
                .camera_proxy_sidecar(&f.asset.id)
                .unwrap()
                .unwrap()
                .status,
            "registered"
        );
    }

    #[test]
    fn ntsc_integer_reduction_is_supported_and_legacy_mapping_is_not_invented() {
        let f = fixture("60000/1001");
        proxy(&f.original, &f.sidecar, "fps=30000/1001,scale=320:180");
        let registered = f
            .catalog
            .register_camera_proxy_sidecar(&f.asset.id, &f.root, &f.sidecar)
            .unwrap();
        assert_eq!(
            registered.provenance.unwrap().time_mapping.frame_reduction,
            2
        );
        f.catalog
            .connection()
            .unwrap()
            .execute(
                "UPDATE camera_proxy_sidecars SET provenance_json=NULL WHERE asset_id=?1",
                [&f.asset.id],
            )
            .unwrap();
        assert!(
            f.catalog
                .resolve_camera_proxy_for_review(&f.asset.id)
                .unwrap()
                .is_some()
        );
        let err = f.catalog.camera_proxy_frame_pack(&f.asset.id).unwrap_err();
        assert!(err.to_string().contains("re-registration"));
    }
}
