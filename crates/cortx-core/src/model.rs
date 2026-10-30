//! Common domain models and flags.

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Distilling modes available for AST and semantic context extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DistillMode {
    /// Strips implementation bodies from functions and methods, preserving interface contracts.
    #[default]
    Skeleton,
    /// Traces upstream callers and downstream callees up to specified depth.
    Hierarchy,
    /// Extracts all `impl Trait for Target` contracts.
    Contracts,
    /// Bundles skeleton, hierarchy, and trait contracts into an optimized prompt block.
    Bundle,
}

impl fmt::Display for DistillMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Skeleton => write!(f, "skeleton"),
            Self::Hierarchy => write!(f, "hierarchy"),
            Self::Contracts => write!(f, "contracts"),
            Self::Bundle => write!(f, "bundle"),
        }
    }
}
