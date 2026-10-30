use std::path::{Path, PathBuf};
use std::process::Command;

fn run_git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if output.status.success() {
        let s = String::from_utf8(output.stdout).ok()?;
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

fn is_git_dirty() -> bool {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok();
    if let Some(out) = output {
        if out.status.success() {
            return !out.stdout.is_empty();
        }
    }
    false
}

fn get_git_identity() -> String {
    // 1. Try exact tag
    if let Some(tag) = run_git(&["describe", "--tags", "--exact-match"]) {
        if is_git_dirty() {
            return format!("{tag}-dirty");
        }
        return tag;
    }

    // 2. Try commit hash
    if let Some(hash) = run_git(&["rev-parse", "--short", "HEAD"]) {
        if is_git_dirty() {
            return format!("{hash}-dirty");
        }
        return hash;
    }

    // 3. Fallback for non-git release archives
    "release-archive".to_string()
}

fn format_iso8601(secs: u64) -> String {
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = (secs / 86400) as i64;

    // Civil date from days since 1970-01-01 (Euclidean affine algorithm)
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m_month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m_month <= 2 { y + 1 } else { y };

    format!("{year:04}-{m_month:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

fn get_build_timestamp() -> String {
    if let Ok(epoch_str) = std::env::var("SOURCE_DATE_EPOCH") {
        if let Ok(epoch) = epoch_str.trim().parse::<u64>() {
            return format_iso8601(epoch);
        }
    }

    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_iso8601(secs)
}

fn main() {
    let pkg_version = env!("CARGO_PKG_VERSION");
    let identity = get_git_identity();
    let timestamp = get_build_timestamp();

    let version_string = format!("{pkg_version} ({identity} {timestamp})");
    println!("cargo:rustc-env=CORTX_VERSION_STRING={version_string}");

    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    // Watch git state files if git dir exists
    let git_dir_path = run_git(&["rev-parse", "--git-dir"]).map(PathBuf::from);
    let git_dir = git_dir_path.as_deref().unwrap_or_else(|| Path::new(".git"));
    if git_dir.exists() {
        let head = git_dir.join("HEAD");
        if head.exists() {
            println!("cargo:rerun-if-changed={}", head.display());
        }
        let index = git_dir.join("index");
        if index.exists() {
            println!("cargo:rerun-if-changed={}", index.display());
        }
        let tags = git_dir.join("refs").join("tags");
        if tags.exists() {
            println!("cargo:rerun-if-changed={}", tags.display());
        }
    }
}
