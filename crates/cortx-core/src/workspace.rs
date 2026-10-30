//! Workspace root and dynamic socket discovery.
//!
//! Provides deterministic resolution for Git repositories, linked Git worktrees,
//! subdirectories, and non-Git workspaces, ensuring collision-free IPC sockets.

use std::path::{Path, PathBuf};

pub use crate::config::resolve_global_config_path;

/// 64-bit SplitMix pseudo-random mixer.
#[inline]
pub const fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Hashes a string deterministically using FNV-1a mixed with SplitMix64.
#[inline]
pub fn splitmix64_str(s: &str) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for &b in s.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01B3);
    }
    splitmix64(h)
}

/// Resolves the canonical root directory of the active workspace.
///
/// Hierarchy:
/// 1. Ascend looking for `.git` (file or directory, handling worktrees and standard repos).
/// 2. Fallback to `start_dir`.
pub fn resolve_workspace_root(start_dir: &Path) -> PathBuf {
    let start = if start_dir.is_file() {
        start_dir.parent().unwrap_or(start_dir)
    } else {
        start_dir
    };

    let canonical_start = std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());

    // 1. Check for .git boundary (works for directories and worktree gitdir pointer files)
    for ancestor in canonical_start.ancestors() {
        if ancestor.join(".git").exists() {
            return ancestor.to_path_buf();
        }
    }

    // 2. Fallback to start directory
    canonical_start
}

/// Inspects a directory to determine if it is a linked Git worktree and discovers its parent repository root.
pub fn detect_worktree_parent_repo(worktree_root: &Path) -> Option<PathBuf> {
    let git_path = if worktree_root.join(".git").exists() {
        worktree_root.join(".git")
    } else {
        resolve_workspace_root(worktree_root).join(".git")
    };

    if !git_path.is_file() {
        return None;
    }

    let content = std::fs::read_to_string(&git_path).ok()?;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("gitdir:") {
            let gitdir_str = rest.trim();
            let parent_dir = git_path.parent().unwrap_or(worktree_root);
            let gitdir_path = parent_dir.join(gitdir_str);
            let canonical_gitdir = std::fs::canonicalize(&gitdir_path).unwrap_or(gitdir_path);

            let commondir_file = canonical_gitdir.join("commondir");
            if commondir_file.is_file() {
                if let Ok(common_content) = std::fs::read_to_string(&commondir_file) {
                    let common_str = common_content.trim();
                    let common_path = canonical_gitdir.join(common_str);
                    if let Ok(canonical_common) = std::fs::canonicalize(&common_path) {
                        if let Some(parent) = canonical_common.parent() {
                            return Some(parent.to_path_buf());
                        }
                    }
                }
            }

            for ancestor in canonical_gitdir.ancestors() {
                if ancestor.file_name().is_some_and(|n| n == ".git") {
                    if let Some(parent) = ancestor.parent() {
                        return std::fs::canonicalize(parent)
                            .ok()
                            .or_else(|| Some(parent.to_path_buf()));
                    }
                }
            }
        }
    }

    None
}

/// Derives a deterministic, length-safe Unix domain socket path for a workspace root.
///
/// Guarantees the path remains under POSIX `sun_path` length caps (104 bytes on macOS, 108 bytes on Linux).
pub fn derive_socket_path(workspace_root: &Path) -> PathBuf {
    let canonical =
        std::fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
    let hash = splitmix64_str(&canonical.to_string_lossy());
    let socket_name = format!("cortx-{:012x}.sock", hash & 0x0000_FFFF_FFFF_FFFF);
    std::env::temp_dir().join(socket_name)
}

/// Resolves the default global user-level daemon socket path.
///
/// Uses `$XDG_RUNTIME_DIR/cortx/daemon.sock`, with fallback to
/// `~/Library/Caches/cortx/daemon.sock` (macOS) or `/tmp/cortx-user-<uid>/daemon.sock`.
pub fn resolve_global_socket() -> PathBuf {
    if let Some(xdg_runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        if !xdg_runtime.is_empty() {
            return PathBuf::from(xdg_runtime).join("cortx").join("daemon.sock");
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home_path = PathBuf::from(home);
        #[cfg(target_os = "macos")]
        {
            return home_path
                .join("Library")
                .join("Caches")
                .join("cortx")
                .join("daemon.sock");
        }
        #[cfg(not(target_os = "macos"))]
        let _ = home_path;
    }
    let uid = current_uid();
    std::env::temp_dir()
        .join(format!("cortx-user-{}", uid))
        .join("daemon.sock")
}

#[cfg(unix)]
fn current_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    if let Ok(meta) = std::fs::metadata("/proc/self") {
        return meta.uid();
    }
    if let Some(home) = std::env::var_os("HOME") {
        if let Ok(meta) = std::fs::metadata(home) {
            return meta.uid();
        }
    }
    if let Ok(val) = std::env::var("UID") {
        if let Ok(uid) = val.parse::<u32>() {
            return uid;
        }
    }
    1000
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    1000
}

/// Probes whether a Unix domain socket at `path` is active and accepting connections.
pub fn is_socket_alive(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixStream;
        UnixStream::connect(path).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// Resolves the active socket path, giving precedence to explicit overrides.
pub fn resolve_active_socket(explicit_socket: Option<&str>, start_dir: &Path) -> PathBuf {
    if let Some(sock) = explicit_socket {
        if !sock.trim().is_empty() {
            return PathBuf::from(sock);
        }
    }
    let root = resolve_workspace_root(start_dir);
    derive_socket_path(&root)
}

/// Resolves an active connected socket path following precedence hierarchy:
/// explicit override -> global user socket -> local workspace socket.
pub fn resolve_connected_socket(
    explicit_socket: Option<&str>,
    target_path: &Path,
) -> Option<PathBuf> {
    if let Some(sock) = explicit_socket {
        if !sock.trim().is_empty() {
            let p = PathBuf::from(sock);
            if is_socket_alive(&p) {
                return Some(p);
            }
            return None;
        }
    }

    let global = resolve_global_socket();
    if is_socket_alive(&global) {
        return Some(global);
    }

    let root = resolve_workspace_root(target_path);
    let local = derive_socket_path(&root);
    if is_socket_alive(&local) {
        return Some(local);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_resolve_workspace_root_git_repo() {
        let dir = tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        std::fs::create_dir(&git_dir).unwrap();

        let sub = dir.path().join("crates").join("cortx-core");
        std::fs::create_dir_all(&sub).unwrap();

        let resolved = resolve_workspace_root(&sub);
        let expected = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(resolved, expected);
    }

    #[test]
    fn test_resolve_workspace_root_git_worktree() {
        let dir = tempdir().unwrap();
        let git_file = dir.path().join(".git");
        std::fs::write(&git_file, "gitdir: /path/to/main/.git/worktrees/wt\n").unwrap();

        let sub = dir.path().join("frontend").join("src");
        std::fs::create_dir_all(&sub).unwrap();

        let resolved = resolve_workspace_root(&sub);
        let expected = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(resolved, expected);
    }

    #[test]
    fn test_resolve_workspace_root_no_markers() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        let resolved = resolve_workspace_root(&sub);
        let expected = std::fs::canonicalize(&sub).unwrap();
        assert_eq!(resolved, expected);
    }

    #[test]
    fn test_derive_socket_path_length_and_format() {
        let dir = tempdir().unwrap();
        let socket_path = derive_socket_path(dir.path());
        let path_str = socket_path.to_str().unwrap();

        // Must fit within POSIX sun_path limit (104 bytes)
        assert!(
            path_str.len() < 104,
            "socket path '{}' exceeds 104 bytes ({})",
            path_str,
            path_str.len()
        );

        let file_name = socket_path.file_name().unwrap().to_str().unwrap();
        assert!(file_name.starts_with("cortx-"));
        assert!(file_name.ends_with(".sock"));
        assert_eq!(file_name.len(), 23); // cortx- (6) + 12 hex + .sock (5)
    }

    #[test]
    fn test_derive_socket_path_deterministic() {
        let dir1 = tempdir().unwrap();
        let dir2 = tempdir().unwrap();

        let sock1_a = derive_socket_path(dir1.path());
        let sock1_b = derive_socket_path(dir1.path());
        assert_eq!(sock1_a, sock1_b);

        let sock2 = derive_socket_path(dir2.path());
        assert_ne!(sock1_a, sock2);
    }

    #[test]
    fn test_resolve_active_socket_explicit_override() {
        let dir = tempdir().unwrap();
        let resolved = resolve_active_socket(Some("/tmp/custom-socket.sock"), dir.path());
        assert_eq!(resolved, PathBuf::from("/tmp/custom-socket.sock"));

        let derived = resolve_active_socket(None, dir.path());
        assert_ne!(derived, PathBuf::from("/tmp/custom-socket.sock"));
        assert!(
            derived
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("cortx-")
        );
    }

    #[test]
    fn test_resolve_global_socket() {
        let sock = resolve_global_socket();
        assert!(sock.to_str().unwrap().contains("cortx"));
        assert!(sock.to_str().unwrap().ends_with(".sock"));
    }

    #[test]
    fn test_detect_worktree_parent_repo_commondir() {
        let parent_repo = tempdir().unwrap();
        let main_git = parent_repo.path().join(".git");
        let worktrees_git = main_git.join("worktrees").join("feature-x");
        std::fs::create_dir_all(&worktrees_git).unwrap();
        std::fs::write(worktrees_git.join("commondir"), "../..\n").unwrap();

        let worktree_dir = tempdir().unwrap();
        let git_file = worktree_dir.path().join(".git");
        let gitdir_content = format!("gitdir: {}\n", worktrees_git.display());
        std::fs::write(&git_file, gitdir_content).unwrap();

        let detected = detect_worktree_parent_repo(worktree_dir.path()).unwrap();
        let expected = std::fs::canonicalize(parent_repo.path()).unwrap();
        assert_eq!(detected, expected);
    }

    #[test]
    fn test_detect_worktree_parent_repo_not_worktree() {
        let dir = tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        std::fs::create_dir(&git_dir).unwrap();

        assert_eq!(detect_worktree_parent_repo(dir.path()), None);
    }

    #[test]
    fn test_is_socket_alive_nonexistent() {
        let p = PathBuf::from("/tmp/cortx-definitely-nonexistent-socket.sock");
        assert!(!is_socket_alive(&p));
    }
}
