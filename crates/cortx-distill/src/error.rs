//! Error primitives for AST and context distillation.

use std::path::PathBuf;
use thiserror::Error;

/// Errors produced during AST parsing, symbol indexing, and context distillation.
#[derive(Error, Debug)]
pub enum DistillError {
    /// Failed to parse source code as a valid Rust syntax tree.
    #[error("failed to parse AST for file '{path}': {source}")]
    ParseFailure {
        /// Source file path.
        path: PathBuf,
        /// Underlying syn error.
        #[source]
        source: syn::Error,
    },

    /// The requested symbol was not found in the indexed symbol graph.
    #[error("symbol '{0}' not found in index")]
    SymbolNotFound(String),

    /// I/O error reading source files.
    #[error("I/O error reading source file: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization error while creating JSON or output payloads.
    #[error("serialization failure: {0}")]
    Serialization(#[from] serde_json::Error),
}
