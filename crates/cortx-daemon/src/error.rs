//! Error primitives for daemon sidecar service.

use std::path::PathBuf;
use thiserror::Error;

/// Errors produced during daemon IPC communication or file watching.
#[derive(Error, Debug)]
pub enum DaemonError {
    /// Socket binding or connection error.
    #[error("IPC socket error on '{path}': {source}")]
    Socket {
        /// Socket path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Daemon socket is already bound and actively serving.
    #[error("daemon socket '{0}' is already bound and actively serving")]
    SocketAlreadyBound(PathBuf),

    /// Protocol framing or deserialization error.
    #[error("protocol framing failure: {0}")]
    Protocol(String),

    /// File watching error.
    #[error("file watcher failure: {0}")]
    Watcher(String),

    /// Distillation error during hook execution.
    #[error("distillation error: {0}")]
    Distill(#[from] cortx_distill::DistillError),

    /// Core error.
    #[error("core error: {0}")]
    Core(#[from] cortx_core::CoreError),

    /// Standard I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
