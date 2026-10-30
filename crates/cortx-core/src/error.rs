//! Core error definitions for the cargo-cortx workspace.

use std::path::PathBuf;
use thiserror::Error;

/// Core errors encountered within cortx-core operations.
#[derive(Error, Debug)]
pub enum CoreError {
    /// Failed to read a configuration file from disk.
    #[error("failed to read configuration file at '{path}': {source}")]
    ConfigRead {
        /// File path that could not be read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Failed to parse a TOML configuration file.
    #[error("failed to parse TOML configuration from '{path}': {source}")]
    ConfigParse {
        /// File path that could not be parsed.
        path: PathBuf,
        /// Underlying TOML deserialization error.
        #[source]
        source: toml::de::Error,
    },

    /// Failed to serialize or deserialize JSON payload.
    #[error("JSON serialization/deserialization failure: {0}")]
    Json(#[from] serde_json::Error),

    /// Failed to initialize logging or tracing subscriber.
    #[error("failed to initialize tracing subscriber: {0}")]
    LoggingInit(String),

    /// Standard I/O operation error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
