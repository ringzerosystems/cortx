use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Information describing a trait contract implementation (`impl Trait for Target`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractInfo {
    pub trait_name: String,
    pub target_type: String,
    pub methods: Vec<String>,
    pub file: String,
}

/// Symbol definition entry in the index.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolDef {
    pub name: String,
    pub kind: String,
    pub file: String,
    #[serde(default = "default_line")]
    pub line: usize,
    #[serde(default = "default_rust_lang")]
    pub language: String,
    #[serde(default)]
    pub end_line: usize,
    #[serde(default)]
    pub qualified_name: String,
}

fn default_line() -> usize {
    1
}

fn default_rust_lang() -> String {
    "rust".to_string()
}

/// Information describing an API route, IPC command, or client fetch call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteBinding {
    pub route_path: String,
    pub handler_or_caller: String,
    pub file: String,
    pub line: usize,
    pub is_backend: bool,
    #[serde(default)]
    pub route_path_lower: String,
    #[serde(default)]
    pub handler_lower: String,
}

impl RouteBinding {
    pub fn new(
        route_path: String,
        handler_or_caller: String,
        file: String,
        line: usize,
        is_backend: bool,
    ) -> Self {
        Self {
            route_path_lower: route_path.to_lowercase(),
            handler_lower: handler_or_caller.to_lowercase(),
            route_path,
            handler_or_caller,
            file,
            line,
            is_backend,
        }
    }
}

/// Cross-boundary correlation between backend and frontend contracts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossBoundaryImpact {
    pub symbol: String,
    pub rust_definition: Option<String>,
    pub ts_interface: Option<String>,
    #[serde(default)]
    pub go_definition: Option<String>,
    pub api_call_sites: Vec<String>,
}

/// Metric summary of the in-memory symbol graph.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolIndexMetrics {
    pub distinct_files_count: usize,
    pub total_symbols_count: usize,
    pub total_call_edges_count: usize,
    pub total_contracts_count: usize,
    pub total_routes_count: usize,
}

/// Batch update request for live watcher events.
pub struct WatchBatchUpdate<'a> {
    pub upserted: &'a [PathBuf],
    pub removed: &'a [PathBuf],
    pub project_cfg: Option<&'a cortx_core::ProjectConfig>,
}

/// Outcome of applying a live watch batch update.
pub struct WatchBatchOutcome {
    pub next_index: crate::indexer::SymbolIndex,
    pub parse_fallbacks: usize,
}
