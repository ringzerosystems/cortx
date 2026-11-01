//! # cortx-core
//!
//! Shared foundation crate for cargo-cortx.
//!
//! Provides CLI configuration discovery, structured error models, logging
//! initialization, and output formatting.

#![forbid(unsafe_code)]

pub mod config;
pub mod diagnostics;
pub mod error;
pub mod logging;
pub mod model;
pub mod output;
pub mod span;
pub mod workspace;

pub use config::{
    ConfigProvenance, CortxConfig, DistillConfig, ProjectConfig, STANDARD_IGNORED_DIRS,
    canonicalize_best_effort, resolve_default_config_path, resolve_global_config_path,
    resolve_system_config_path,
};
pub use error::CoreError;
pub use logging::init_logging;
pub use model::DistillMode;
pub use output::OutputFormat;
#[allow(deprecated)]
pub use span::{
    AgentVendor, CLAUDE_WHOLE_FILE_MAX_BYTES, DEFAULT_AGY_BYTE_LIMIT, DEFAULT_AGY_END_LINE,
    DEFAULT_AGY_START_LINE, DEFAULT_CLAUDE_LIMIT, DEFAULT_CLAUDE_MAX_READ_TOKENS,
    DEFAULT_CLAUDE_OFFSET, DEFAULT_OPENCODE_LIMIT, DEFAULT_OPENCODE_OFFSET, EST_BYTES_PER_TOKEN,
    MICRO_READ_MAX_LINES, ParsedHookInput, ReadBudgetEnv, ReadCoverage, ReadKind, ReadSpan,
    classify_read_coverage, detect_budget_env, parse_hook_stdin, resolve_read_budget,
};
pub use workspace::{
    derive_socket_path, resolve_active_socket, resolve_workspace_root, splitmix64, splitmix64_str,
};
