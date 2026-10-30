//! Structured logging initialization via tracing and tracing-subscriber.

use crate::error::CoreError;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

/// Initializes tracing subscriber with environment filter support and optional JSON framing.
pub fn init_logging(verbosity: u8, json: bool) -> Result<(), CoreError> {
    let default_directive = match verbosity {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_directive));

    let subscriber_builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_writer(std::io::stderr);

    if json {
        subscriber_builder
            .json()
            .try_init()
            .map_err(|e| CoreError::LoggingInit(e.to_string()))?;
    } else {
        subscriber_builder
            .try_init()
            .map_err(|e| CoreError::LoggingInit(e.to_string()))?;
    }

    Ok(())
}

/// Reads process Resident Set Size (RSS) in Megabytes.
///
/// On Linux, inspects `/proc/self/status` (or `/proc/self/statm`) to read resident memory.
pub fn get_process_rss_mb() -> f64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if let Some(rest) = line.strip_prefix("VmRSS:") {
                    let trimmed = rest.trim();
                    if let Some(kb_str) = trimmed.split_whitespace().next() {
                        if let Ok(kb) = kb_str.parse::<f64>() {
                            return kb / 1024.0;
                        }
                    }
                }
            }
        }
        if let Ok(statm) = std::fs::read_to_string("/proc/self/statm") {
            let mut parts = statm.split_whitespace();
            if let (Some(_size), Some(resident_str)) = (parts.next(), parts.next()) {
                if let Ok(pages) = resident_str.parse::<f64>() {
                    // Standard page size is 4096 bytes (4 KB)
                    return (pages * 4096.0) / (1024.0 * 1024.0);
                }
            }
        }
        0.0
    }
    #[cfg(not(target_os = "linux"))]
    {
        0.0
    }
}

/// Resolves the default base logging directory for cortx:
/// `$XDG_STATE_HOME/cortx/logs/` (defaults to `~/.local/state/cortx/logs/` or `~/Library/Logs/cortx/` on macOS).
pub fn resolve_default_log_dir() -> PathBuf {
    if let Some(xdg_state) = std::env::var_os("XDG_STATE_HOME") {
        if !xdg_state.is_empty() {
            return PathBuf::from(xdg_state).join("cortx").join("logs");
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home_path = PathBuf::from(home);
        #[cfg(target_os = "macos")]
        {
            return home_path.join("Library").join("Logs").join("cortx");
        }
        #[cfg(not(target_os = "macos"))]
        {
            return home_path
                .join(".local")
                .join("state")
                .join("cortx")
                .join("logs");
        }
    }
    std::env::temp_dir().join("cortx").join("logs")
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn test_get_process_rss_mb() {
        let rss = get_process_rss_mb();
        #[cfg(target_os = "linux")]
        assert!(
            rss > 0.0,
            "Linux process RSS should be greater than 0, got {}",
            rss
        );
        #[cfg(not(target_os = "linux"))]
        assert!(rss >= 0.0);
    }

    #[test]
    fn test_resolve_default_log_dir() {
        let log_dir = resolve_default_log_dir();
        assert!(log_dir.to_str().unwrap().contains("cortx"));
        assert!(log_dir.to_str().unwrap().contains("logs"));
    }

    #[test]
    fn test_init_logging_idempotent() {
        // init_logging routes to stderr and returns Ok or LoggingInit error if subscriber was already set
        let _ = init_logging(0, false);
    }
}
