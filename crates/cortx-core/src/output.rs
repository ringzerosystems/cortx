//! Output format definitions.

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Supported output formats for context distillation and verification reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    /// Plain text human-readable format.
    Text,
    /// JSON formatted payload for machine integration.
    Json,
    /// Markdown format tailored for LLM system context prompt injection.
    #[default]
    Markdown,
}

impl fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text => write!(f, "text"),
            Self::Json => write!(f, "json"),
            Self::Markdown => write!(f, "markdown"),
        }
    }
}
