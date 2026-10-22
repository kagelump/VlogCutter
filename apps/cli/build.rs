use std::path::{Path, PathBuf};
use std::process::Command;

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.join("../..");
    // Track source inputs and Git's actual paths (also works in worktrees).
    for path in ["apps/cli", "core", "Cargo.toml", "Cargo.lock"] {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
    for name in ["HEAD", "index"] {
        if let Some(path) = git(&root, &["rev-parse", "--git-path", name]) {
            println!("cargo:rerun-if-changed={}", root.join(path).display());
        }
    }
    if let Some(reference) = git(&root, &["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git(&root, &["rev-parse", "--git-path", &reference])
    {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
    let revision = git(&root, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(
        &root,
        &["status", "--porcelain", "--untracked-files=normal"],
    )
    .map(|status| !status.is_empty())
    .unwrap_or(true);
    let rustc = Command::new(std::env::var_os("RUSTC").unwrap())
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".into());
    for (key, value) in [
        ("RCDVLOG_BUILD_REVISION", revision),
        ("RCDVLOG_BUILD_DIRTY", dirty.to_string()),
        ("RCDVLOG_BUILD_PROFILE", std::env::var("PROFILE").unwrap()),
        ("RCDVLOG_BUILD_TARGET", std::env::var("TARGET").unwrap()),
        ("RCDVLOG_BUILD_RUSTC", rustc),
    ] {
        println!("cargo:rustc-env={key}={value}");
    }
}
