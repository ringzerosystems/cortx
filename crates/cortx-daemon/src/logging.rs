//! Multi-tier file logging architecture for daemon sidecar.
//!
//! Manages a central timestamped daemon log file (`daemon-%Y%m%d-%H%M%S.log`)
//! with a `daemon-latest.log` pointer, and dedicated per-workspace log files
//! (`workspaces/<slug>-<hash>.log`) using non-blocking asynchronous channels.

use crate::error::DaemonError;
use std::path::{Path, PathBuf};

/// Formats a SystemTime into a file-tag string (`YYYYMMDD-HHMMSS`) and an ISO-8601 string.
fn format_system_time(now: std::time::SystemTime) -> (String, String) {
    let dur = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    let millis = dur.subsec_millis();

    let sec = (secs % 60) as u32;
    let min = ((secs / 60) % 60) as u32;
    let hour = ((secs / 3600) % 24) as u32;

    let mut days = (secs / 86400) as i64;
    let mut year = 1970;
    loop {
        let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
        let days_in_year = if leap { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
    let days_in_month = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1;
    for &dim in &days_in_month {
        if days < dim {
            break;
        }
        days -= dim;
        month += 1;
    }
    let day = days as u32 + 1;

    let file_tag = format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        year, month, day, hour, min, sec
    );
    let iso_ts = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year, month, day, hour, min, sec, millis
    );
    (file_tag, iso_ts)
}

/// Derives a length-safe, human-readable log file name for a workspace.
///
/// Output format: `<slug>-<hash>.log`, e.g. `cargo-cortx-cbf29ce48422.log`.
pub fn derive_workspace_log_filename(workspace_root: &Path) -> String {
    let canonical =
        std::fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
    let raw_slug = canonical
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("workspace");

    let mut slug = String::with_capacity(raw_slug.len());
    for ch in raw_slug.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            slug.push(ch);
        } else {
            slug.push('_');
        }
    }
    if slug.is_empty() {
        slug = "workspace".to_string();
    }

    let hash = cortx_core::workspace::splitmix64_str(&canonical.to_string_lossy());
    format!("{}-{:012x}.log", slug, hash & 0x0000_FFFF_FFFF_FFFF)
}

/// Spawns an asynchronous background task to consume messages and write to the target file.
fn spawn_writer_task(path: PathBuf, mut rx: tokio::sync::mpsc::Receiver<String>) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            use tokio::io::AsyncWriteExt;
            if let Ok(mut file) = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await
            {
                while let Some(msg) = rx.recv().await {
                    let _ = file.write_all(msg.as_bytes()).await;
                    let _ = file.flush().await;
                }
            }
        });
    } else {
        std::thread::Builder::new()
            .name("cortx-log-writer".into())
            .spawn(move || {
                use std::io::Write;
                if let Ok(mut file) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                {
                    while let Some(msg) = rx.blocking_recv() {
                        let _ = file.write_all(msg.as_bytes());
                        let _ = file.flush();
                    }
                }
            })
            .ok();
    }
}

/// Dedicated logger attached to an individual active workspace or worktree.
#[derive(Clone, Debug)]
pub struct WorkspaceLogger {
    log_path: PathBuf,
    writer: tokio::sync::mpsc::Sender<String>,
}

impl WorkspaceLogger {
    /// Creates a noop workspace logger for testing or headless operations.
    pub fn noop(log_path: PathBuf) -> Self {
        let (writer, _rx) = tokio::sync::mpsc::channel(1);
        Self { log_path, writer }
    }

    /// Dispatches a non-blocking log event to the workspace log file.
    pub fn log(&self, msg: &str) {
        let (_, iso_ts) = format_system_time(std::time::SystemTime::now());
        let trimmed = msg.trim_end();
        let line = if trimmed.starts_with('[') {
            format!("{} {}\n", iso_ts, trimmed)
        } else {
            format!("{} [INFO] {}\n", iso_ts, trimmed)
        };
        let _ = self.writer.try_send(line);
    }

    /// Returns the absolute path to this workspace's log file.
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }
}

/// Central daemon log manager supervising daemon lifecycles and workspace loggers.
pub struct DaemonLogManager {
    main_log_path: PathBuf,
    workspaces_dir: PathBuf,
    workspace_logging: bool,
    main_writer: tokio::sync::mpsc::Sender<String>,
}

impl DaemonLogManager {
    /// Initializes logging directory structure and starts the main daemon log stream with default options.
    pub fn init(base_dir: &Path) -> Result<Self, DaemonError> {
        Self::init_with_options(base_dir, true)
    }

    /// Initializes logging directory structure with explicit workspace logging toggle.
    pub fn init_with_options(
        base_dir: &Path,
        workspace_logging: bool,
    ) -> Result<Self, DaemonError> {
        std::fs::create_dir_all(base_dir)?;
        let workspaces_dir = base_dir.join("workspaces");
        std::fs::create_dir_all(&workspaces_dir)?;

        let (tag, _) = format_system_time(std::time::SystemTime::now());
        let main_log_path = base_dir.join(format!("daemon-{}.log", tag));
        // Touch main log file immediately so symlink target exists
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&main_log_path);

        let latest_path = base_dir.join("daemon-latest.log");
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(&latest_path);
            if let Some(file_name) = main_log_path.file_name() {
                let _ = std::os::unix::fs::symlink(file_name, &latest_path);
            }
        }
        #[cfg(not(unix))]
        {
            let _ = std::fs::write(&latest_path, main_log_path.to_string_lossy().as_bytes());
        }

        let (tx, rx) = tokio::sync::mpsc::channel::<String>(2048);
        spawn_writer_task(main_log_path.clone(), rx);

        Ok(Self {
            main_log_path,
            workspaces_dir,
            workspace_logging,
            main_writer: tx,
        })
    }

    /// Emits a log event to the central daemon log file.
    pub fn log_daemon(&self, msg: &str) {
        let (_, iso_ts) = format_system_time(std::time::SystemTime::now());
        let trimmed = msg.trim_end();
        let line = if trimmed.starts_with('[') {
            format!("{} {}\n", iso_ts, trimmed)
        } else {
            format!("{} [INFO] {}\n", iso_ts, trimmed)
        };
        let _ = self.main_writer.try_send(line);
    }

    /// Creates and registers an isolated logger for the specified workspace root.
    pub fn create_workspace_logger(&self, root: &Path) -> Result<WorkspaceLogger, DaemonError> {
        let filename = derive_workspace_log_filename(root);
        let log_path = self.workspaces_dir.join(filename);

        if !self.workspace_logging {
            return Ok(WorkspaceLogger::noop(log_path));
        }

        let (tx, rx) = tokio::sync::mpsc::channel::<String>(1024);
        spawn_writer_task(log_path.clone(), rx);

        Ok(WorkspaceLogger {
            log_path,
            writer: tx,
        })
    }

    /// Returns the path to the current main daemon log file.
    pub fn main_log_path(&self) -> &Path {
        &self.main_log_path
    }

    /// Returns the directory containing all workspace log files.
    pub fn workspaces_dir(&self) -> &Path {
        &self.workspaces_dir
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_daemon_log_manager_init() {
        let dir = tempdir().unwrap();
        let manager = DaemonLogManager::init(dir.path()).expect("init should succeed");

        assert!(manager.main_log_path().exists());
        assert!(
            manager
                .main_log_path()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("daemon-")
        );
        assert!(manager.workspaces_dir().is_dir());

        let latest = dir.path().join("daemon-latest.log");
        assert!(latest.exists());

        manager.log_daemon("Daemon server started on socket");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let content = std::fs::read_to_string(manager.main_log_path()).unwrap_or_default();
        assert!(
            content.contains("Daemon server started on socket"),
            "Log content should contain logged line, got: {}",
            content
        );
    }

    #[tokio::test]
    async fn test_workspace_logger_creation_and_write() {
        let dir = tempdir().unwrap();
        let manager = DaemonLogManager::init(dir.path()).unwrap();

        let ws_root = PathBuf::from("/test/workspace/alpha");
        let ws_logger = manager
            .create_workspace_logger(&ws_root)
            .expect("should create logger");

        assert!(ws_logger.log_path().to_str().unwrap().contains("alpha-"));
        assert!(ws_logger.log_path().to_str().unwrap().ends_with(".log"));

        ws_logger.log("[INFO] Indexed 120 files successfully");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let content = std::fs::read_to_string(ws_logger.log_path()).unwrap_or_default();
        assert!(
            content.contains("Indexed 120 files successfully"),
            "Workspace log must contain logged message, got: {}",
            content
        );
    }

    #[test]
    fn test_derive_workspace_log_filename() {
        let p = Path::new("/var/projects/my-app");
        let filename = derive_workspace_log_filename(p);
        assert!(filename.starts_with("my-app-"));
        assert!(filename.ends_with(".log"));
    }

    #[tokio::test]
    async fn test_workspace_logging_disabled_respects_config() {
        let log_dir = tempdir().unwrap();
        let ws_dir = tempdir().unwrap();

        let manager = DaemonLogManager::init_with_options(log_dir.path(), false).unwrap();
        let ws_logger = manager.create_workspace_logger(ws_dir.path()).unwrap();
        ws_logger.log("Should not be written to disk");
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(!ws_logger.log_path().exists());
    }
}
