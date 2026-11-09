//! Host, tool, model, and device identity components that key calibration results.

use super::*;

/// Opaque process-session identity used only to fence a live job attempt's
/// estimator context. New processes get a new key, so a lease left over from a
/// restarted server cannot keep advertising an ETA from the dead process.
pub fn media_estimator_process_key() -> &'static str {
    static KEY: OnceLock<String> = OnceLock::new();
    KEY.get_or_init(|| {
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let executable = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("unresolved"));
        let mut hasher = blake3::Hasher::new();
        hasher.update(std::process::id().to_le_bytes().as_slice());
        hasher.update(started.to_le_bytes().as_slice());
        hasher.update(executable.to_string_lossy().as_bytes());
        hasher.finalize().to_hex().to_string()[..32].to_string()
    })
}

/// Public wrapper for the host/build identity used by estimator keys.
pub fn calibration_host_identity() -> CalibrationHostIdentity {
    host_identity()
}

/// Public wrapper for the lightweight device identity bundle used by estimator
/// keys. It does not run encoder or media probes.
pub fn calibration_hardware_device_identities() -> Vec<CalibrationDeviceIdentity> {
    hardware_device_identities()
}

/// Identity for a configured media tool path. Bare names are resolved once so
/// a later `PATH` change cannot silently swap the measured executable.
pub fn calibration_tool_identity_for_path(
    role: &str,
    name: &str,
    program: &Path,
) -> CalibrationToolIdentity {
    let resolved = crate::resolve_tool_program(program);
    tool_identity_for_path(role, name, &resolved)
}

/// Opaque, stable-within-a-stat fingerprint. It is deliberately never exposed
/// as a path; callers retain only a digest. A replacement at the same path
/// changes length/mtime/inode and therefore invalidates cached identities.
pub(super) fn file_stat_fingerprint(path: &Path) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok().map(system_time_nanos).unwrap_or(0);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(format!(
            "dev={};ino={};len={};mtime={};mode={};ctime={}.{}",
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            modified,
            metadata.mode(),
            metadata.ctime(),
            metadata.ctime_nsec()
        ))
    }
    #[cfg(not(unix))]
    {
        Some(format!(
            "len={};mtime={};readonly={}",
            metadata.len(),
            modified,
            metadata.permissions().readonly()
        ))
    }
}

pub(super) fn file_identity(path: &Path) -> String {
    let fingerprint = file_stat_fingerprint(path).unwrap_or_else(|| "unresolved".into());
    let mut hasher = blake3::Hasher::new();
    hasher.update(path.as_os_str().to_string_lossy().as_bytes());
    hasher.update(fingerprint.as_bytes());
    hasher.finalize().to_hex().to_string()[..32].to_string()
}

#[derive(Debug, Clone)]
pub(super) struct CachedHostIdentity {
    pub(super) fingerprint: String,
    pub(super) identity: CalibrationHostIdentity,
}

pub(super) fn actual_host_name() -> String {
    if let Ok(value) = std::env::var("RCDVLOG_HOST_ID") {
        return value;
    }
    #[cfg(unix)]
    {
        let mut name = [0_u8; 256];
        if unsafe { libc::gethostname(name.as_mut_ptr().cast(), name.len()) } == 0 {
            name[255] = 0;
            return String::from_utf8_lossy(
                &name[..name.iter().position(|byte| *byte == 0).unwrap_or(255)],
            )
            .into_owned();
        }
    }
    std::env::var("HOSTNAME")
        .unwrap_or_else(|_| format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH))
}

pub(super) fn kernel_identity() -> String {
    #[cfg(unix)]
    {
        let mut value = std::mem::MaybeUninit::<libc::utsname>::uninit();
        if unsafe { libc::uname(value.as_mut_ptr()) } == 0 {
            let value = unsafe { value.assume_init() };
            return unsafe { std::ffi::CStr::from_ptr(value.release.as_ptr()) }
                .to_string_lossy()
                .into_owned();
        }
    }
    std::env::consts::OS.to_string()
}

pub(super) fn platform_cpu_model() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        crate::prerequisites::run_bounded_command(
            Path::new("/usr/sbin/sysctl"),
            &[OsString::from("-n"), OsString::from("hw.model")],
            Duration::from_secs(1),
            1024,
        )
        .ok()
        .filter(|result| result.status.success())
        .map(|result| String::from_utf8_lossy(&result.stdout).trim().to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

pub(super) fn host_identity() -> CalibrationHostIdentity {
    static IDENTITY: OnceLock<Mutex<Option<CachedHostIdentity>>> = OnceLock::new();
    let executable = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("unresolved"));
    let raw_host = actual_host_name();
    let fingerprint = format!(
        "exe={};host={:?};build={:?};hostname={:?};epoch={:?};kernel={};cpus={}",
        file_stat_fingerprint(&executable).unwrap_or_else(|| "missing".into()),
        std::env::var("RCDVLOG_HOST_ID").ok(),
        std::env::var("RCDVLOG_BUILD_ID").ok(),
        raw_host,
        std::env::var("RCDVLOG_IDENTITY_EPOCH").ok(),
        kernel_identity(),
        std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1),
    );
    let cache = IDENTITY.get_or_init(|| Mutex::new(None));
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(cached) = guard
        .as_ref()
        .filter(|cached| cached.fingerprint == fingerprint)
    {
        return cached.identity.clone();
    }
    let host_key = blake3::hash(raw_host.trim().as_bytes())
        .to_hex()
        .to_string()[..32]
        .to_string();
    let build_marker = std::env::var("RCDVLOG_BUILD_ID")
        .ok()
        .or_else(|| option_env!("RCDVLOG_BUILD_ID").map(str::to_string))
        .unwrap_or_else(|| "local".into());
    let executable_key = file_identity(&executable);
    let identity = CalibrationHostIdentity {
        host_key,
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        logical_cpus: std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1),
        // Include the actual running executable's file identity. `local`
        // alone would let two different local builds reuse each other's fit.
        application_build: format!(
            "{}+{}+exe-{}",
            env!("CARGO_PKG_VERSION"),
            build_marker,
            &executable_key[..16]
        ),
    };
    *guard = Some(CachedHostIdentity {
        fingerprint,
        identity: identity.clone(),
    });
    identity
}

#[derive(Debug, Clone)]
pub(super) struct CachedToolIdentity {
    pub(super) fingerprint: String,
    pub(super) identity: CalibrationToolIdentity,
}

pub(super) fn tool_identity(role: &str, name: &str) -> CalibrationToolIdentity {
    let path = resolve_tool_path(name);
    tool_identity_for_path(role, name, &path)
}

pub(super) fn tool_identity_for_path(
    role: &str,
    name: &str,
    path: &Path,
) -> CalibrationToolIdentity {
    static IDENTITIES: OnceLock<Mutex<HashMap<String, CachedToolIdentity>>> = OnceLock::new();
    let fingerprint = format!(
        "stat={};epoch={:?}",
        file_stat_fingerprint(path).unwrap_or_else(|| "missing".into()),
        std::env::var("RCDVLOG_IDENTITY_EPOCH").ok(),
    );
    let cache = IDENTITIES.get_or_init(|| Mutex::new(HashMap::new()));
    let key = format!("{role}:{name}:{}", path.to_string_lossy());
    {
        let guard = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cached) = guard
            .get(&key)
            .filter(|cached| cached.fingerprint == fingerprint)
        {
            return cached.identity.clone();
        }
    }
    let version = if path.is_file() {
        crate::prerequisites::run_bounded_command(
            path,
            &[OsString::from("-version")],
            Duration::from_secs(3),
            16 * 1024,
        )
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            let text = String::from_utf8_lossy(&output.stdout);
            bounded_text(
                text.lines().next().unwrap_or("version unavailable").trim(),
                256,
            )
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "version unavailable".to_string())
    } else {
        "unresolved".to_string()
    };
    let identity = CalibrationToolIdentity {
        role: role.to_string(),
        name: name.to_string(),
        version,
        executable_key: file_identity(path),
    };
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.len() >= 16 {
        guard.clear();
    }
    guard.insert(
        key,
        CachedToolIdentity {
            fingerprint,
            identity: identity.clone(),
        },
    );
    identity
}

pub(super) fn tool_identities() -> Vec<CalibrationToolIdentity> {
    vec![
        tool_identity("media_transcode", "ffmpeg"),
        tool_identity("media_probe", "ffprobe"),
    ]
}

#[derive(Debug, Clone)]
pub(super) struct CachedFileDigest {
    pub(super) fingerprint: String,
    pub(super) digest: Option<String>,
    pub(super) bytes: u64,
}

pub(super) fn model_content_digest(path: &Path) -> (Option<String>, u64) {
    static DIGESTS: OnceLock<Mutex<HashMap<PathBuf, CachedFileDigest>>> = OnceLock::new();
    let Some(fingerprint) = file_stat_fingerprint(path) else {
        return (None, 0);
    };
    let bytes = fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let cache = DIGESTS.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let guard = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cached) = guard
            .get(path)
            .filter(|cached| cached.fingerprint == fingerprint)
        {
            return (cached.digest.clone(), cached.bytes);
        }
    }
    let digest = crate::full_file_blake3(path, &|| false)
        .ok()
        .map(|hash| format!("blake3:{}", hash.hex_digest));
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.len() >= 64 {
        guard.clear();
    }
    guard.insert(
        path.to_path_buf(),
        CachedFileDigest {
            fingerprint,
            digest: digest.clone(),
            bytes,
        },
    );
    (digest, bytes)
}

pub(super) fn model_identity(models: &[ModelStatus]) -> Vec<CalibrationModelIdentity> {
    models
        .iter()
        .map(|model| {
            let path = Path::new(&model.path);
            let bytes = fs::metadata(path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            CalibrationModelIdentity {
                role: model.role.clone(),
                name: model.name.clone(),
                integrity: format!("{};stat:{}", model.integrity, file_identity(path)),
                bytes,
                content_digest: None,
            }
        })
        .collect()
}

pub(super) fn cpu_device_identity() -> CalibrationDeviceIdentity {
    let model = std::env::var("RCDVLOG_CPU_IDENTITY")
        .ok()
        .or_else(|| {
            fs::read_to_string("/proc/cpuinfo").ok().and_then(|text| {
                text.lines()
                    .find(|line| line.starts_with("model name"))
                    .and_then(|line| line.split_once(':'))
                    .map(|(_, value)| value.trim().to_string())
            })
        })
        .or_else(platform_cpu_model)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS));
    let identity = blake3::hash(
        format!(
            "{model}|{}|{}|{}|{}",
            kernel_identity(),
            std::env::consts::ARCH,
            std::env::consts::OS,
            std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1)
        )
        .as_bytes(),
    )
    .to_hex()
    .to_string()[..32]
        .to_string();
    CalibrationDeviceIdentity {
        kind: "cpu".into(),
        name: model,
        identity,
    }
}

pub(super) fn gpu_device_identities() -> Vec<CalibrationDeviceIdentity> {
    if let Ok(raw) = std::env::var("RCDVLOG_GPU_IDENTITY") {
        let identity = blake3::hash(raw.as_bytes()).to_hex().to_string()[..32].to_string();
        return vec![CalibrationDeviceIdentity {
            kind: "gpu".into(),
            name: bounded_text(&raw, 128),
            identity,
        }];
    }
    let mut entries = Vec::new();
    if let Ok(paths) = fs::read_dir("/sys/class/drm") {
        for path in paths.flatten() {
            let name = path.file_name().to_string_lossy().to_string();
            if !name.starts_with("card") || name.contains('-') {
                continue;
            }
            let uevent = path.path().join("device/uevent");
            let Ok(text) = fs::read_to_string(uevent) else {
                continue;
            };
            let facts = text
                .lines()
                .filter(|line| {
                    line.starts_with("DRIVER=")
                        || line.starts_with("PCI_ID=")
                        || line.starts_with("PCI_SUBSYS_ID=")
                })
                .collect::<Vec<_>>()
                .join(";");
            if facts.is_empty() {
                continue;
            }
            let identity = blake3::hash(facts.as_bytes()).to_hex().to_string()[..32].to_string();
            entries.push(CalibrationDeviceIdentity {
                kind: "gpu".into(),
                name,
                identity,
            });
        }
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    entries
}

/// Lightweight hardware facts only. This function never probes an encoder or
/// runs media, so it is safe from read-only status and selection paths.
pub(super) fn hardware_device_identities() -> Vec<CalibrationDeviceIdentity> {
    let mut devices = vec![cpu_device_identity()];
    let gpus = gpu_device_identities();
    #[cfg(target_os = "linux")]
    {
        let mut paths = vec![
            PathBuf::from("/usr/lib/x86_64-linux-gnu/dri"),
            PathBuf::from("/usr/lib/aarch64-linux-gnu/dri"),
            PathBuf::from("/usr/lib64/dri"),
            PathBuf::from("/usr/lib/dri"),
        ];
        if let Some(path) = std::env::var_os("LIBVA_DRIVERS_PATH") {
            paths.extend(std::env::split_paths(&path));
        }
        let mut files = Vec::new();
        for directory in paths {
            if let Ok(entries) = fs::read_dir(directory) {
                for entry in entries.flatten() {
                    if entry
                        .file_name()
                        .to_string_lossy()
                        .ends_with("_drv_video.so")
                    {
                        files.push(entry.path());
                    }
                }
            }
        }
        files.sort();
        files.dedup();
        let mut hasher = blake3::Hasher::new();
        for file in files {
            hasher.update(file_identity(&file).as_bytes());
        }
        devices.push(CalibrationDeviceIdentity {
            kind: "driver".into(),
            name: "vaapi-userspace".into(),
            identity: hasher.finalize().to_hex().to_string(),
        });
    }

    if gpus.is_empty() {
        devices.push(CalibrationDeviceIdentity {
            kind: "gpu".into(),
            name: "none".into(),
            identity: "none".into(),
        });
    } else {
        devices.extend(gpus);
    }
    devices
}
