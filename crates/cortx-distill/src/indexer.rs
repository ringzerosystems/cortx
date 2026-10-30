//! Semantic call-graph and contract indexer for Rust codebases.

use crate::error::DistillError;
use quote::ToTokens;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use syn::visit::Visit;
use walkdir::WalkDir;

/// Compact identifier for an interned string in `StringPool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SymbolId(pub u32);

/// Compact representation of a call graph edge using interned string identifiers (12 bytes, zero heap allocations).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CompactCallEdge {
    /// Upstream caller symbol identifier.
    pub caller: SymbolId,
    /// Downstream callee symbol identifier.
    pub callee: SymbolId,
    /// Source file path identifier.
    pub file: SymbolId,
}

impl CompactCallEdge {
    /// Resolves an interned CompactCallEdge back into an owned CallEdge using the StringPool.
    pub fn resolve(&self, pool: &StringPool) -> Option<CallEdge> {
        Some(CallEdge {
            caller: pool.resolve(self.caller)?.to_string(),
            callee: pool.resolve(self.callee)?.to_string(),
            file: pool.resolve(self.file)?.to_string(),
        })
    }
}

/// Contiguous string pool for deduplicating symbols, functions, and file paths.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StringPool {
    /// Contiguous array of interned strings indexed by SymbolId.
    pub strings: Vec<String>,
    /// Fast deduplication lookup mapping string slice to SymbolId.
    pub lookup: HashMap<String, SymbolId>,
}

impl StringPool {
    /// Creates an empty string pool.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an empty string pool with pre-allocated capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            strings: Vec::with_capacity(capacity),
            lookup: HashMap::with_capacity(capacity),
        }
    }

    /// Interns a string slice, returning its unique `SymbolId`.
    pub fn intern(&mut self, s: &str) -> SymbolId {
        if let Some(&id) = self.lookup.get(s) {
            return id;
        }
        let id = SymbolId(self.strings.len() as u32);
        let owned = s.to_string();
        self.strings.push(owned.clone());
        self.lookup.insert(owned, id);
        id
    }

    /// Resolves a `SymbolId` back to its string slice.
    pub fn resolve(&self, id: SymbolId) -> Option<&str> {
        self.strings.get(id.0 as usize).map(|s| s.as_str())
    }

    /// Returns the `SymbolId` for an existing string without interning it.
    pub fn get_id(&self, s: &str) -> Option<SymbolId> {
        self.lookup.get(s).copied()
    }

    /// Returns the number of unique strings interned in the pool.
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// Returns true if the pool contains no interned strings.
    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}

/// Represents a caller-to-callee relation in the indexed call graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CallEdge {
    /// Upstream caller symbol or function.
    pub caller: String,
    /// Downstream callee symbol or function.
    pub callee: String,
    /// Source file path where invocation occurs.
    pub file: String,
}

/// Information describing a trait contract implementation (`impl Trait for Target`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractInfo {
    /// Name of implemented trait.
    pub trait_name: String,
    /// Name of target struct or type.
    pub target_type: String,
    /// List of method signatures provided by this contract.
    pub methods: Vec<String>,
    /// Source file path where implementation resides.
    pub file: String,
}

/// Symbol definition entry in the index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolDef {
    /// Symbol name.
    pub name: String,
    /// Kind ("fn", "struct", "enum", "trait", "type", "interface").
    pub kind: String,
    /// File where symbol is declared.
    pub file: String,
    /// Line number where declared (1-indexed).
    #[serde(default = "default_line")]
    pub line: usize,
    /// Language origin ("rust" or "typescript").
    #[serde(default = "default_rust_lang")]
    pub language: String,
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
    /// HTTP path or command name (e.g. "/api/users", "update_user").
    pub route_path: String,
    /// Handler function or calling function name.
    pub handler_or_caller: String,
    /// Source file path.
    pub file: String,
    /// Line number where route/call is defined (1-indexed).
    pub line: usize,
    /// Whether this is a backend definition (true) or frontend client call site (false).
    pub is_backend: bool,
    /// Pre-computed lowercased route path for zero-allocation cross-boundary queries.
    #[serde(default)]
    pub route_path_lower: String,
    /// Pre-computed lowercased handler name.
    #[serde(default)]
    pub handler_lower: String,
}

impl RouteBinding {
    /// Constructs a new RouteBinding with pre-normalized lowercase lookup keys.
    pub fn new(
        route_path: String,
        handler_or_caller: String,
        file: String,
        line: usize,
        is_backend: bool,
    ) -> Self {
        let route_path_lower = route_path.to_lowercase();
        let handler_lower = handler_or_caller.to_lowercase();
        Self {
            route_path,
            handler_or_caller,
            file,
            line,
            is_backend,
            route_path_lower,
            handler_lower,
        }
    }
}

/// Cross-boundary correlation between Rust backend, Go backend, and TypeScript frontend contracts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossBoundaryImpact {
    /// Inspected target symbol.
    pub symbol: String,
    /// Rust backend definition site, e.g. "crates/server/src/routes/users.rs:42 (UpdateUserPayload)".
    pub rust_definition: Option<String>,
    /// TypeScript consumer interface, e.g. "frontend/src/types/user.ts:18 (UpdateUserPayload)".
    pub ts_interface: Option<String>,
    /// Go backend definition site, e.g. "internal/model/user.go:18 (User)".
    #[serde(default)]
    pub go_definition: Option<String>,
    /// Frontend client call sites.
    pub api_call_sites: Vec<String>,
}

/// Metric summary of the in-memory symbol graph.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolIndexMetrics {
    /// Number of distinct source files containing declared symbols.
    pub distinct_files_count: usize,
    /// Total number of symbols declared in index.
    pub total_symbols_count: usize,
    /// Total number of caller -> callee edges indexed.
    pub total_call_edges_count: usize,
    /// Total number of trait/interface contract implementations recorded.
    pub total_contracts_count: usize,
    /// Total number of HTTP routes and client bindings recorded.
    pub total_routes_count: usize,
}

/// In-memory semantic index across a codebase.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SymbolIndex {
    /// Contiguous string pool for deduplicating symbols, functions, and file paths.
    #[serde(default)]
    pub pool: StringPool,
    /// Call-graph edges indexed by callee -> callers.
    #[serde(default)]
    pub callers_map: HashMap<SymbolId, Vec<CompactCallEdge>>,
    /// Call-graph edges indexed by caller -> callees.
    #[serde(default)]
    pub callees_map: HashMap<SymbolId, Vec<CompactCallEdge>>,
    /// Trait implementations indexed by target type name.
    pub contracts_map: HashMap<String, Vec<ContractInfo>>,
    /// Declared symbols indexed by name.
    pub symbols_map: HashMap<String, Vec<SymbolDef>>,
    /// Inverted index mapping file paths to declared symbol names.
    #[serde(default)]
    pub file_to_symbols: HashMap<String, Vec<String>>,
    /// Recorded API routes and client fetch calls.
    #[serde(default)]
    pub routes: Vec<RouteBinding>,
    /// Fast file-name (basename) to file-path mapping for O(1) candidate lookup.
    #[serde(skip)]
    pub file_name_to_paths: HashMap<String, Vec<String>>,
}

impl SymbolIndex {
    /// Creates an empty symbol index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a caller -> callee edge with source file into the interned call graph.
    pub fn record_call_edge(&mut self, caller: &str, callee: &str, file: &str) {
        let caller_id = self.pool.intern(caller);
        let callee_id = self.pool.intern(callee);
        let file_id = self.pool.intern(file);

        let edge = CompactCallEdge {
            caller: caller_id,
            callee: callee_id,
            file: file_id,
        };

        let caller_entry = self.callers_map.entry(callee_id).or_default();
        if !caller_entry.contains(&edge) {
            caller_entry.push(edge);
        }

        let callee_entry = self.callees_map.entry(caller_id).or_default();
        if !callee_entry.contains(&edge) {
            callee_entry.push(edge);
        }
    }

    /// Records a CallEdge into the interned call graph.
    pub fn add_call_edge(&mut self, edge: CallEdge) {
        self.record_call_edge(&edge.caller, &edge.callee, &edge.file);
    }

    fn record_symbol(&mut self, sym: SymbolDef) {
        if let Some(file_name) = Path::new(&sym.file).file_name().and_then(|n| n.to_str()) {
            let paths = self
                .file_name_to_paths
                .entry(file_name.to_string())
                .or_default();
            if !paths.iter().any(|p| p == &sym.file) {
                paths.push(sym.file.clone());
            }
        }
        self.file_to_symbols
            .entry(sym.file.clone())
            .or_default()
            .push(sym.name.clone());
        self.symbols_map
            .entry(sym.name.clone())
            .or_default()
            .push(sym);
    }

    /// Computes summary metrics of the current semantic graph in O(N).
    pub fn metrics(&self) -> SymbolIndexMetrics {
        let mut files = HashSet::new();
        let mut total_symbols = 0;
        for defs in self.symbols_map.values() {
            total_symbols += defs.len();
            for def in defs {
                files.insert(&def.file);
            }
        }
        let total_call_edges: usize = self.callers_map.values().map(|v| v.len()).sum();
        let total_contracts: usize = self.contracts_map.values().map(|v| v.len()).sum();

        SymbolIndexMetrics {
            distinct_files_count: files.len(),
            total_symbols_count: total_symbols,
            total_call_edges_count: total_call_edges,
            total_contracts_count: total_contracts,
            total_routes_count: self.routes.len(),
        }
    }

    /// Estimates in-memory heap consumption of the symbol tables, call edges,
    /// contracts, and route indices in Megabytes.
    pub fn estimate_memory_mb(&self) -> f64 {
        let mut bytes = std::mem::size_of::<Self>();

        // String pool allocation
        bytes += self.pool.strings.capacity() * std::mem::size_of::<String>();
        for s in &self.pool.strings {
            bytes += s.len();
        }
        bytes += self.pool.lookup.capacity()
            * (std::mem::size_of::<String>() + std::mem::size_of::<SymbolId>() + 16);

        // Compact call-graph maps
        for edges in self.callers_map.values() {
            bytes += std::mem::size_of::<SymbolId>() + 16;
            bytes += edges.capacity() * std::mem::size_of::<CompactCallEdge>();
        }
        for edges in self.callees_map.values() {
            bytes += std::mem::size_of::<SymbolId>() + 16;
            bytes += edges.capacity() * std::mem::size_of::<CompactCallEdge>();
        }
        for (target, contracts) in &self.contracts_map {
            bytes += target.len() + 32;
            bytes += contracts.capacity() * std::mem::size_of::<ContractInfo>();
            for c in contracts {
                bytes += c.trait_name.len() + c.target_type.len() + c.file.len();
                for m in &c.methods {
                    bytes += m.len() + std::mem::size_of::<String>();
                }
            }
        }
        for (name, defs) in &self.symbols_map {
            bytes += name.len() + 32;
            bytes += defs.capacity() * std::mem::size_of::<SymbolDef>();
            for d in defs {
                bytes += d.name.len() + d.kind.len() + d.file.len() + d.language.len();
            }
        }
        for (file, syms) in &self.file_to_symbols {
            bytes += file.len() + 32;
            bytes += syms.capacity() * std::mem::size_of::<String>();
            for s in syms {
                bytes += s.len();
            }
        }
        for (name, paths) in &self.file_name_to_paths {
            bytes += name.len() + 32;
            bytes += paths.capacity() * std::mem::size_of::<String>();
            for p in paths {
                bytes += p.len();
            }
        }
        bytes += self.routes.capacity() * std::mem::size_of::<RouteBinding>();
        for r in &self.routes {
            bytes += r.route_path.len()
                + r.handler_or_caller.len()
                + r.file.len()
                + r.route_path_lower.len()
                + r.handler_lower.len();
        }

        bytes as f64 / (1024.0 * 1024.0)
    }

    /// Creates an overlay index containing only symbols from the specified modified files.
    pub fn create_overlay(&self, modified_files: &[PathBuf]) -> SymbolIndex {
        if modified_files.len() <= 1 {
            let mut overlay = SymbolIndex::new();
            for path in modified_files {
                if let Ok(content) = std::fs::read_to_string(path) {
                    let path_str = path.to_string_lossy().to_string();
                    let _ = overlay.index_any_source_str(&path_str, &content);
                }
            }
            overlay
        } else {
            modified_files
                .par_iter()
                .fold(SymbolIndex::new, |mut acc, path| {
                    if let Ok(content) = std::fs::read_to_string(path) {
                        let path_str = path.to_string_lossy().to_string();
                        let _ = acc.index_any_source_str(&path_str, &content);
                    }
                    acc
                })
                .reduce(SymbolIndex::new, |mut a, b| {
                    a.merge(b);
                    a
                })
        }
    }

    /// Merges another SymbolIndex into this one in-place.
    pub fn merge(&mut self, other: SymbolIndex) {
        let id_map: Vec<SymbolId> = other
            .pool
            .strings
            .iter()
            .map(|s| self.pool.intern(s))
            .collect();

        for (k, edges) in other.callers_map {
            let mapped_k = id_map[k.0 as usize];
            let target = self.callers_map.entry(mapped_k).or_default();
            for e in edges {
                let mapped_edge = CompactCallEdge {
                    caller: id_map[e.caller.0 as usize],
                    callee: id_map[e.callee.0 as usize],
                    file: id_map[e.file.0 as usize],
                };
                if !target.contains(&mapped_edge) {
                    target.push(mapped_edge);
                }
            }
        }
        for (k, edges) in other.callees_map {
            let mapped_k = id_map[k.0 as usize];
            let target = self.callees_map.entry(mapped_k).or_default();
            for e in edges {
                let mapped_edge = CompactCallEdge {
                    caller: id_map[e.caller.0 as usize],
                    callee: id_map[e.callee.0 as usize],
                    file: id_map[e.file.0 as usize],
                };
                if !target.contains(&mapped_edge) {
                    target.push(mapped_edge);
                }
            }
        }
        for (k, v) in other.contracts_map {
            self.contracts_map.entry(k).or_default().extend(v);
        }
        for (k, v) in other.symbols_map {
            self.symbols_map.entry(k).or_default().extend(v);
        }
        for (k, v) in other.file_to_symbols {
            self.file_to_symbols.entry(k).or_default().extend(v);
        }
        self.routes.extend(other.routes);
        for (k, v) in other.file_name_to_paths {
            let entry = self.file_name_to_paths.entry(k).or_default();
            for p in v {
                if !entry.iter().any(|existing| existing == &p) {
                    entry.push(p);
                }
            }
        }
    }

    /// Discovers candidate source files filtering ignored directories and nested worktrees.
    pub fn collect_source_files(&self, root: &Path) -> Vec<PathBuf> {
        let (cfg, _) = cortx_core::config::CortxConfig::load_daemon_config(None)
            .unwrap_or_else(|_| (cortx_core::config::CortxConfig::default(), None));
        if let Some((_, proj)) = cfg.find_project_for_path(root) {
            return self.collect_source_files_for_project(root, proj);
        }
        let proj = cortx_core::ProjectConfig::from_cortx_config(root.to_path_buf(), &cfg);
        self.collect_source_files_for_project(root, &proj)
    }

    /// Discovers candidate source files with explicit ignored directories and worktrees directory.
    pub fn collect_source_files_with_options(
        &self,
        root: &Path,
        ignored_dirs: &[String],
        worktrees_dir: Option<&str>,
    ) -> Vec<PathBuf> {
        let proj = cortx_core::ProjectConfig {
            qualified_path: root.to_path_buf(),
            has_worktrees: worktrees_dir.is_some(),
            custom_worktree_path: worktrees_dir.map(PathBuf::from),
            ignored_dirs: ignored_dirs.to_vec(),
            ..cortx_core::ProjectConfig::default()
        };
        self.collect_source_files_for_project(root, &proj)
    }

    /// Discovers candidate source files under `root` honoring a declarative `ProjectConfig`.
    pub fn collect_source_files_for_project(
        &self,
        root: &Path,
        project: &cortx_core::ProjectConfig,
    ) -> Vec<PathBuf> {
        let rel_base = if !project.qualified_path.as_os_str().is_empty()
            && root.starts_with(&project.qualified_path)
            && !root.join(".git").exists()
        {
            project.qualified_path.as_path()
        } else {
            root
        };
        WalkDir::new(root)
            .into_iter()
            .filter_entry(|e| {
                let p = e.path();
                if e.depth() == 0 || p == root {
                    return true;
                }
                let Ok(rel) = p.strip_prefix(rel_base) else {
                    return false;
                };
                if project.is_relative_path_ignored(rel) {
                    return false;
                }
                if e.file_type().is_dir() {
                    // 1. Prune nested git repositories/worktrees inside parent
                    if p.join(".git").exists() {
                        return false;
                    }
                    // 2. Prune configured worktrees and directories outside watch_paths relative to root
                    if rel == Path::new(".worktrees") || rel.starts_with(".worktrees") {
                        return false;
                    }
                    if let Some(wt_rel) = project.relative_custom_worktree_path(rel_base) {
                        if rel == wt_rel || rel.starts_with(wt_rel) {
                            return false;
                        }
                    }
                    if !project.is_dir_in_watch_paths(rel) {
                        return false;
                    }
                    if project.distill.omit_tests
                        && rel.components().any(|c| c.as_os_str() == "tests")
                    {
                        return false;
                    }
                }
                true
            })
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| {
                let p = e.path();
                let in_watch_paths = p
                    .strip_prefix(rel_base)
                    .is_ok_and(|rel| project.is_file_in_watch_paths(rel));
                if !in_watch_paths {
                    return false;
                }
                let ext_enabled = p
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| project.is_extension_enabled(ext));
                if !ext_enabled {
                    return false;
                }
                if project.distill.omit_tests {
                    if let Some(name) = p.file_name().and_then(|f| f.to_str()) {
                        if name.ends_with("_test.rs") || name.ends_with("_test.go") {
                            return false;
                        }
                    }
                    if let Ok(rel) = p.strip_prefix(rel_base) {
                        if rel.components().any(|c| c.as_os_str() == "tests") {
                            return false;
                        }
                    }
                }
                true
            })
            .map(|e| e.into_path())
            .collect()
    }

    /// Discovers and indexes all valid source files (.rs, .ts, .tsx, .js, .jsx, .go)
    /// under root in parallel using Rayon worker threads.
    pub fn index_directory(&mut self, root: &Path) -> Result<usize, DistillError> {
        let start = std::time::Instant::now();
        let (cfg, _) = cortx_core::config::CortxConfig::load_daemon_config(None)
            .unwrap_or_else(|_| (cortx_core::config::CortxConfig::default(), None));
        let proj = if let Some((_, proj)) = cfg.find_project_for_path(root) {
            proj.clone()
        } else {
            cortx_core::ProjectConfig::from_cortx_config(root.to_path_buf(), &cfg)
        };
        let files = self.collect_source_files_for_project(root, &proj);
        self.index_discovered_files(root, &files, start, proj.distill.omit_tests)
    }

    /// Discovers and indexes all valid source files under `root` honoring `project` configuration.
    pub fn index_directory_with_project(
        &mut self,
        root: &Path,
        project: &cortx_core::ProjectConfig,
    ) -> Result<usize, DistillError> {
        let start = std::time::Instant::now();
        let files = self.collect_source_files_for_project(root, project);
        self.index_discovered_files(root, &files, start, project.distill.omit_tests)
    }

    fn index_discovered_files(
        &mut self,
        root: &Path,
        files: &[PathBuf],
        start: std::time::Instant,
        omit_tests: bool,
    ) -> Result<usize, DistillError> {
        let discovery_ms = start.elapsed().as_millis();
        let total_files = files.len();
        if total_files == 0 {
            tracing::info!(root = %root.display(), "No source files discovered for indexing");
            return Ok(0);
        }

        tracing::info!(
            root = %root.display(),
            files = total_files,
            discovery_ms,
            "Discovered candidate source files; starting parallel AST indexing"
        );

        let parse_start = std::time::Instant::now();
        let partial_index = files
            .par_iter()
            .fold(SymbolIndex::new, |mut acc, path| {
                if let Ok(content) = std::fs::read_to_string(path) {
                    let path_str = path.to_string_lossy().to_string();
                    let _ = acc.index_any_source_str_with_options(&path_str, &content, omit_tests);
                }
                acc
            })
            .reduce(SymbolIndex::new, |mut a, b| {
                a.merge(b);
                a
            });
        let parse_ms = parse_start.elapsed().as_millis();

        let merge_start = std::time::Instant::now();
        self.merge(partial_index);
        let merge_ms = merge_start.elapsed().as_millis();

        let total_elapsed = start.elapsed();
        let total_ms = total_elapsed.as_millis();
        let throughput = if total_elapsed.as_secs_f64() > 0.0 {
            (total_files as f64) / total_elapsed.as_secs_f64()
        } else {
            0.0
        };

        let metrics = self.metrics();
        tracing::info!(
            root = %root.display(),
            files = total_files,
            symbols = metrics.total_symbols_count,
            edges = metrics.total_call_edges_count,
            contracts = metrics.total_contracts_count,
            routes = metrics.total_routes_count,
            discovery_ms,
            parse_ms,
            merge_ms,
            total_ms,
            throughput_fps = format!("{:.0}", throughput),
            "Initial indexing complete"
        );
        Ok(total_files)
    }

    /// Sequential fallback for single-threaded or deterministic testing.
    pub fn index_directory_sequential(&mut self, root: &Path) -> Result<usize, DistillError> {
        let start = std::time::Instant::now();
        let (cfg, _) = cortx_core::config::CortxConfig::load_daemon_config(None)
            .unwrap_or_else(|_| (cortx_core::config::CortxConfig::default(), None));
        let proj = if let Some((_, proj)) = cfg.find_project_for_path(root) {
            proj.clone()
        } else {
            cortx_core::ProjectConfig::from_cortx_config(root.to_path_buf(), &cfg)
        };
        let files = self.collect_source_files_for_project(root, &proj);
        let total_files = files.len();
        for path in &files {
            if let Ok(content) = std::fs::read_to_string(path) {
                let path_str = path.to_string_lossy().to_string();
                let _ = self.index_any_source_str_with_options(
                    &path_str,
                    &content,
                    proj.distill.omit_tests,
                );
            }
        }
        let total_elapsed = start.elapsed();
        let metrics = self.metrics();
        tracing::info!(
            root = %root.display(),
            files = total_files,
            symbols = metrics.total_symbols_count,
            total_ms = total_elapsed.as_millis(),
            "Sequential indexing complete"
        );
        Ok(total_files)
    }

    /// Indexes source from any supported language (Rust, TypeScript, TSX, JS, Go).
    pub fn index_any_source_str(
        &mut self,
        file_path: &str,
        content: &str,
    ) -> Result<(), DistillError> {
        self.index_any_source_str_with_options(file_path, content, true)
    }

    /// Indexes source with explicit `omit_tests` option.
    pub fn index_any_source_str_with_options(
        &mut self,
        file_path: &str,
        content: &str,
        omit_tests: bool,
    ) -> Result<(), DistillError> {
        if file_path.ends_with(".go") {
            self.index_go_source_str_with_options(file_path, content, omit_tests)
        } else if file_path.ends_with(".ts")
            || file_path.ends_with(".tsx")
            || file_path.ends_with(".js")
            || file_path.ends_with(".jsx")
        {
            self.index_ts_source_str(file_path, content)
        } else {
            self.index_source_str_with_options(file_path, content, omit_tests)
        }
    }

    /// Indexes a single Go source string.
    pub fn index_go_source_str(
        &mut self,
        file_path: &str,
        content: &str,
    ) -> Result<(), DistillError> {
        self.index_go_source_str_with_options(file_path, content, true)
    }

    /// Indexes a single Go source string with explicit `omit_tests` option.
    pub fn index_go_source_str_with_options(
        &mut self,
        file_path: &str,
        content: &str,
        omit_tests: bool,
    ) -> Result<(), DistillError> {
        let data = crate::go_skeleton::index_go_ast_with_options(file_path, content, omit_tests)?;
        for sym in data.symbols {
            self.record_symbol(sym);
        }
        for contract in data.contracts {
            self.contracts_map
                .entry(contract.target_type.clone())
                .or_default()
                .push(contract);
        }
        for route in data.routes {
            self.routes.push(route);
        }
        Ok(())
    }

    /// Indexes a single Rust source string.
    pub fn index_source_str(&mut self, file_path: &str, content: &str) -> Result<(), DistillError> {
        self.index_source_str_with_options(file_path, content, true)
    }

    /// Indexes a single Rust source string with explicit `omit_tests` option.
    pub fn index_source_str_with_options(
        &mut self,
        file_path: &str,
        content: &str,
        omit_tests: bool,
    ) -> Result<(), DistillError> {
        let ast =
            syn::parse_str::<syn::File>(content).map_err(|source| DistillError::ParseFailure {
                path: PathBuf::from(file_path),
                source,
            })?;

        let mut visitor = IndexVisitor::new(file_path, content, omit_tests);
        visitor.visit_file(&ast);

        for edge in visitor.edges {
            self.record_call_edge(&edge.caller, &edge.callee, &edge.file);
        }

        for contract in visitor.contracts {
            self.contracts_map
                .entry(contract.target_type.clone())
                .or_default()
                .push(contract);
        }

        for sym in visitor.symbols {
            self.record_symbol(sym);
        }

        for route in visitor.routes {
            self.routes.push(route);
        }

        Ok(())
    }

    /// Indexes a single TypeScript, TSX, or JavaScript source string.
    pub fn index_ts_source_str(
        &mut self,
        file_path: &str,
        content: &str,
    ) -> Result<(), DistillError> {
        let allocator = oxc_allocator::Allocator::default();
        let is_jsx = file_path.ends_with(".tsx") || file_path.ends_with(".jsx");
        let source_type = oxc_span::SourceType::default()
            .with_typescript(true)
            .with_jsx(is_jsx);
        let ret = oxc_parser::Parser::new(&allocator, content, source_type).parse();

        let get_line = |offset: u32| -> usize {
            1 + content[..(offset as usize).min(content.len())]
                .matches('\n')
                .count()
        };

        for stmt in &ret.program.body {
            match stmt {
                oxc_ast::ast::Statement::ExportNamedDeclaration(decl) => {
                    if let Some(ref d) = decl.declaration {
                        self.index_ts_declaration(d, file_path, &get_line);
                    }
                }
                oxc_ast::ast::Statement::TSInterfaceDeclaration(decl) => {
                    let name = decl.id.name.to_string();
                    let line = get_line(decl.span.start);
                    self.record_symbol(SymbolDef {
                        name,
                        kind: "interface".to_string(),
                        file: file_path.to_string(),
                        line,
                        language: "typescript".to_string(),
                    });
                }
                oxc_ast::ast::Statement::TSTypeAliasDeclaration(decl) => {
                    let name = decl.id.name.to_string();
                    let line = get_line(decl.span.start);
                    self.record_symbol(SymbolDef {
                        name,
                        kind: "type".to_string(),
                        file: file_path.to_string(),
                        line,
                        language: "typescript".to_string(),
                    });
                }
                oxc_ast::ast::Statement::TSEnumDeclaration(decl) => {
                    let name = decl.id.name.to_string();
                    let line = get_line(decl.span.start);
                    self.record_symbol(SymbolDef {
                        name,
                        kind: "enum".to_string(),
                        file: file_path.to_string(),
                        line,
                        language: "typescript".to_string(),
                    });
                }
                oxc_ast::ast::Statement::FunctionDeclaration(func) => {
                    if let Some(ref id) = func.id {
                        let name = id.name.to_string();
                        let line = get_line(func.span.start);
                        self.record_symbol(SymbolDef {
                            name,
                            kind: "fn".to_string(),
                            file: file_path.to_string(),
                            line,
                            language: "typescript".to_string(),
                        });
                    }
                }
                oxc_ast::ast::Statement::VariableDeclaration(var) => {
                    for d in &var.declarations {
                        if let Some(name) = d.id.get_identifier_name() {
                            let line = get_line(d.span.start);
                            self.record_symbol(SymbolDef {
                                name: name.to_string(),
                                kind: "const".to_string(),
                                file: file_path.to_string(),
                                line,
                                language: "typescript".to_string(),
                            });
                        }
                    }
                }
                oxc_ast::ast::Statement::ImportDeclaration(decl) => {
                    if let Some(ref specifiers) = decl.specifiers {
                        for spec in specifiers {
                            let imported_name = match spec {
                                oxc_ast::ast::ImportDeclarationSpecifier::ImportSpecifier(s) => {
                                    s.imported.name().to_string()
                                }
                                oxc_ast::ast::ImportDeclarationSpecifier::ImportDefaultSpecifier(
                                    s,
                                ) => s.local.name.to_string(),
                                oxc_ast::ast::ImportDeclarationSpecifier::ImportNamespaceSpecifier(
                                    s,
                                ) => s.local.name.to_string(),
                            };
                            if !imported_name.is_empty() {
                                self.record_call_edge(file_path, &imported_name, file_path);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        self.extract_ts_route_calls(file_path, content, &get_line);
        Ok(())
    }

    fn index_ts_declaration(
        &mut self,
        decl: &oxc_ast::ast::Declaration<'_>,
        file_path: &str,
        get_line: &impl Fn(u32) -> usize,
    ) {
        match decl {
            oxc_ast::ast::Declaration::TSInterfaceDeclaration(d) => {
                let name = d.id.name.to_string();
                let line = get_line(d.span.start);
                self.record_symbol(SymbolDef {
                    name,
                    kind: "interface".to_string(),
                    file: file_path.to_string(),
                    line,
                    language: "typescript".to_string(),
                });
            }
            oxc_ast::ast::Declaration::TSTypeAliasDeclaration(d) => {
                let name = d.id.name.to_string();
                let line = get_line(d.span.start);
                self.record_symbol(SymbolDef {
                    name,
                    kind: "type".to_string(),
                    file: file_path.to_string(),
                    line,
                    language: "typescript".to_string(),
                });
            }
            oxc_ast::ast::Declaration::TSEnumDeclaration(d) => {
                let name = d.id.name.to_string();
                let line = get_line(d.span.start);
                self.record_symbol(SymbolDef {
                    name,
                    kind: "enum".to_string(),
                    file: file_path.to_string(),
                    line,
                    language: "typescript".to_string(),
                });
            }
            oxc_ast::ast::Declaration::FunctionDeclaration(func) => {
                if let Some(ref id) = func.id {
                    let name = id.name.to_string();
                    let line = get_line(func.span.start);
                    self.record_symbol(SymbolDef {
                        name,
                        kind: "fn".to_string(),
                        file: file_path.to_string(),
                        line,
                        language: "typescript".to_string(),
                    });
                }
            }
            oxc_ast::ast::Declaration::VariableDeclaration(var) => {
                for d in &var.declarations {
                    if let Some(name) = d.id.get_identifier_name() {
                        let line = get_line(d.span.start);
                        self.record_symbol(SymbolDef {
                            name: name.to_string(),
                            kind: "const".to_string(),
                            file: file_path.to_string(),
                            line,
                            language: "typescript".to_string(),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    fn extract_ts_route_calls(
        &mut self,
        file_path: &str,
        content: &str,
        get_line: &impl Fn(u32) -> usize,
    ) {
        let pattern = regex::Regex::new(r#"(?:fetch|invoke|axios\s*\.\s*(?:get|post|put|delete|patch))\s*\(\s*["'`]([^"'`]+)["'`]"#).unwrap();
        for cap in pattern.captures_iter(content) {
            if let Some(m) = cap.get(0) {
                let route = cap.get(1).map(|c| c.as_str()).unwrap_or("");
                let line = get_line(m.start() as u32);
                let caller = find_enclosing_fn(content, m.start());
                self.routes.push(RouteBinding::new(
                    route.to_string(),
                    caller,
                    file_path.to_string(),
                    line,
                    false,
                ));
            }
        }
    }

    /// Computes cross-boundary impact correlating Rust backend models/endpoints
    /// with TypeScript types and frontend client API call sites.
    pub fn get_cross_boundary_impact(&self, symbol: &str) -> Option<CrossBoundaryImpact> {
        let rust_def = self
            .symbols_map
            .get(symbol)
            .and_then(|defs| {
                defs.iter()
                    .find(|d| d.language == "rust" || d.file.ends_with(".rs"))
            })
            .map(|d| format!("{}:{} (`{}`)", d.file, d.line, d.name));

        let ts_def = self
            .symbols_map
            .get(symbol)
            .and_then(|defs| {
                defs.iter().find(|d| {
                    d.language == "typescript"
                        || d.file.ends_with(".ts")
                        || d.file.ends_with(".tsx")
                })
            })
            .map(|d| format!("{}:{} (`{}`)", d.file, d.line, d.name));

        let go_def = self
            .symbols_map
            .get(symbol)
            .and_then(|defs| {
                defs.iter()
                    .find(|d| d.language == "go" || d.file.ends_with(".go"))
            })
            .map(|d| format!("{}:{} (`{}`)", d.file, d.line, d.name));

        let mut call_sites = Vec::new();
        let sym_trimmed = symbol
            .trim_end_matches("Payload")
            .trim_end_matches("Request")
            .trim_end_matches("Response");
        let sym_lower = symbol.to_lowercase();
        let trimmed_lower = sym_trimmed.to_lowercase();

        for r in &self.routes {
            let matched = r.route_path_lower.contains(&sym_lower)
                || r.handler_lower.contains(&sym_lower)
                || r.handler_lower.contains(&trimmed_lower)
                || (!trimmed_lower.is_empty() && r.route_path_lower.contains(&trimmed_lower))
                || (trimmed_lower.len() >= 4
                    && (r.route_path_lower.contains(&trimmed_lower[..4])
                        || r.handler_lower.contains(&trimmed_lower[..4])));
            if matched {
                if r.is_backend && r.file.ends_with(".go") {
                    call_sites.push(format!(
                        "Go Router: {}:{} -> `{}`",
                        r.file, r.line, r.route_path
                    ));
                } else if !r.is_backend {
                    call_sites.push(format!(
                        "{}:{} (`{}()`)",
                        r.file, r.line, r.handler_or_caller
                    ));
                }
            }
        }

        call_sites.sort();
        call_sites.dedup();

        if rust_def.is_some() || ts_def.is_some() || go_def.is_some() || !call_sites.is_empty() {
            Some(CrossBoundaryImpact {
                symbol: symbol.to_string(),
                rust_definition: rust_def,
                ts_interface: ts_def,
                go_definition: go_def,
                api_call_sites: call_sites,
            })
        } else {
            None
        }
    }

    /// Looks up declared symbol names for a file path, checking exact match first,
    /// then fast candidate match via `file_name_to_paths`, and finally fallback.
    pub fn get_file_symbols<'a>(&'a self, target_path: &Path) -> Option<&'a [String]> {
        let file_str = target_path.to_string_lossy();
        if let Some(syms) = self.file_to_symbols.get(file_str.as_ref()) {
            return Some(syms.as_slice());
        }
        if let Some(trimmed) = file_str.strip_prefix("./") {
            if let Some(syms) = self.file_to_symbols.get(trimmed) {
                return Some(syms.as_slice());
            }
        }
        let target_file_name = target_path.file_name()?.to_str()?;
        if let Some(candidates) = self.file_name_to_paths.get(target_file_name) {
            for candidate in candidates {
                if crate::path_matches(target_path, Path::new(candidate)) {
                    if let Some(syms) = self.file_to_symbols.get(candidate) {
                        return Some(syms.as_slice());
                    }
                }
            }
        }
        // Fallback: in case file_name_to_paths was not populated
        self.file_to_symbols.iter().find_map(|(f, s)| {
            let p = Path::new(f);
            if p.file_name().and_then(|n| n.to_str()) == Some(target_file_name)
                && crate::path_matches(target_path, p)
            {
                Some(s.as_slice())
            } else {
                None
            }
        })
    }

    /// Retrieves upstream callers of a symbol up to the requested depth.
    pub fn get_callers(&self, symbol: &str, depth: usize) -> Vec<CallEdge> {
        let mut results = Vec::new();
        let Some(start_id) = self.pool.get_id(symbol) else {
            return results;
        };
        let mut visited = HashSet::new();
        let mut queue = vec![(start_id, 0)];

        while let Some((curr, d)) = queue.pop() {
            if d >= depth || !visited.insert(curr) {
                continue;
            }
            if let Some(edges) = self.callers_map.get(&curr) {
                for edge in edges {
                    if let Some(resolved) = edge.resolve(&self.pool) {
                        results.push(resolved);
                        queue.push((edge.caller, d + 1));
                    }
                }
            }
        }
        results
    }

    /// Collects upstream callers directly into a results vector, deduplicating against `seen`.
    pub fn collect_callers_into(
        &self,
        symbol: &str,
        depth: usize,
        results: &mut Vec<CallEdge>,
        seen: &mut HashSet<CallEdge>,
    ) {
        let Some(start_id) = self.pool.get_id(symbol) else {
            return;
        };
        let mut visited = HashSet::new();
        let mut queue = vec![(start_id, 0)];

        while let Some((curr, d)) = queue.pop() {
            if d >= depth || !visited.insert(curr) {
                continue;
            }
            if let Some(edges) = self.callers_map.get(&curr) {
                for edge in edges {
                    if let Some(resolved) = edge.resolve(&self.pool) {
                        if seen.insert(resolved.clone()) {
                            results.push(resolved);
                        }
                        queue.push((edge.caller, d + 1));
                    }
                }
            }
        }
    }

    /// Retrieves downstream callees called by a symbol up to the requested depth.
    pub fn get_callees(&self, symbol: &str, depth: usize) -> Vec<CallEdge> {
        let mut results = Vec::new();
        let Some(start_id) = self.pool.get_id(symbol) else {
            return results;
        };
        let mut visited = HashSet::new();
        let mut queue = vec![(start_id, 0)];

        while let Some((curr, d)) = queue.pop() {
            if d >= depth || !visited.insert(curr) {
                continue;
            }
            if let Some(edges) = self.callees_map.get(&curr) {
                for edge in edges {
                    if let Some(resolved) = edge.resolve(&self.pool) {
                        results.push(resolved);
                        queue.push((edge.callee, d + 1));
                    }
                }
            }
        }
        results
    }

    /// Collects downstream callees directly into a results vector, deduplicating against `seen`.
    pub fn collect_callees_into(
        &self,
        symbol: &str,
        depth: usize,
        results: &mut Vec<CallEdge>,
        seen: &mut HashSet<CallEdge>,
    ) {
        let Some(start_id) = self.pool.get_id(symbol) else {
            return;
        };
        let mut visited = HashSet::new();
        let mut queue = vec![(start_id, 0)];

        while let Some((curr, d)) = queue.pop() {
            if d >= depth || !visited.insert(curr) {
                continue;
            }
            if let Some(edges) = self.callees_map.get(&curr) {
                for edge in edges {
                    if let Some(resolved) = edge.resolve(&self.pool) {
                        if seen.insert(resolved.clone()) {
                            results.push(resolved);
                        }
                        queue.push((edge.callee, d + 1));
                    }
                }
            }
        }
    }

    /// Retrieves trait contracts associated with a target type.
    pub fn get_contracts(&self, target_type: &str) -> Vec<ContractInfo> {
        let mut list = self
            .contracts_map
            .get(target_type)
            .cloned()
            .unwrap_or_default();

        if !target_type.starts_with('*') {
            let ptr_name = format!("*{}", target_type);
            if let Some(ptr_contracts) = self.contracts_map.get(&ptr_name) {
                list.extend(ptr_contracts.clone());
            }
        }

        if list.is_empty() {
            for contracts in self.contracts_map.values() {
                for c in contracts {
                    if c.trait_name == target_type
                        || c.trait_name.ends_with(&format!(".{target_type}"))
                    {
                        list.push(c.clone());
                    }
                }
            }
        }
        list
    }

    /// Returns satisfied interfaces for a Go struct type (both value and pointer receiver sets).
    pub fn get_satisfied_interfaces(&self, struct_name: &str) -> Vec<ContractInfo> {
        self.get_contracts(struct_name)
    }

    /// Returns concrete implementor type names of an interface.
    pub fn get_interface_implementors(&self, interface_name: &str) -> Vec<String> {
        let mut implementors = Vec::new();
        for (target_type, contracts) in &self.contracts_map {
            for c in contracts {
                if c.trait_name == interface_name
                    || c.trait_name.ends_with(&format!(".{interface_name}"))
                {
                    implementors.push(target_type.clone());
                }
            }
        }
        implementors.sort();
        implementors.dedup();
        implementors
    }

    /// Retrieves upstream callers of a symbol, falling back to base index if absent in self.
    pub fn get_callers_with_fallback(
        &self,
        base: Option<&SymbolIndex>,
        symbol: &str,
        depth: usize,
    ) -> Vec<CallEdge> {
        let callers = self.get_callers(symbol, depth);
        if !callers.is_empty() {
            callers
        } else if let Some(base_idx) = base {
            base_idx.get_callers(symbol, depth)
        } else {
            Vec::new()
        }
    }

    /// Retrieves downstream callees called by a symbol, falling back to base index if absent in self.
    pub fn get_callees_with_fallback(
        &self,
        base: Option<&SymbolIndex>,
        symbol: &str,
        depth: usize,
    ) -> Vec<CallEdge> {
        let callees = self.get_callees(symbol, depth);
        if !callees.is_empty() {
            callees
        } else if let Some(base_idx) = base {
            base_idx.get_callees(symbol, depth)
        } else {
            Vec::new()
        }
    }

    /// Retrieves trait contracts associated with a target type, falling back to base index if absent in self.
    pub fn get_contracts_with_fallback(
        &self,
        base: Option<&SymbolIndex>,
        target_type: &str,
    ) -> Vec<ContractInfo> {
        let contracts = self.get_contracts(target_type);
        if !contracts.is_empty() {
            contracts
        } else if let Some(base_idx) = base {
            base_idx.get_contracts(target_type)
        } else {
            Vec::new()
        }
    }

    /// Computes cross-boundary impact correlating Rust/Go backend with TypeScript frontend,
    /// falling back to base index if absent in self.
    pub fn get_cross_boundary_impact_with_fallback(
        &self,
        base: Option<&SymbolIndex>,
        symbol: &str,
    ) -> Option<CrossBoundaryImpact> {
        self.get_cross_boundary_impact(symbol)
            .or_else(|| base.and_then(|b| b.get_cross_boundary_impact(symbol)))
    }

    /// Looks up symbol definitions, falling back to base index if absent in self.
    pub fn get_symbol_fallback<'a>(
        &'a self,
        base: Option<&'a SymbolIndex>,
        symbol: &str,
    ) -> Option<&'a [SymbolDef]> {
        if let Some(defs) = self.symbols_map.get(symbol) {
            if !defs.is_empty() {
                return Some(defs.as_slice());
            }
        }
        base.and_then(|b| b.symbols_map.get(symbol).map(|v| v.as_slice()))
    }
}

/// Returns true if any path segment corresponds to a known dependency,
/// build artifact, VCS metadata, or cache directory.
pub fn is_ignored_path(path: &Path) -> bool {
    for component in path.components() {
        if let std::path::Component::Normal(c) = component {
            if let Some(s) = c.to_str() {
                if cortx_core::STANDARD_IGNORED_DIRS.contains(&s) {
                    return true;
                }
            }
        }
    }
    false
}

fn find_line(content: &str, pattern: &str) -> usize {
    if let Some(pos) = content.find(pattern) {
        1 + content[..pos].matches('\n').count()
    } else {
        1
    }
}

fn extract_string_literal(input: &str) -> Option<String> {
    let start = input.find('"')?;
    let end = input[start + 1..].find('"')?;
    Some(input[start + 1..start + 1 + end].to_string())
}

fn find_enclosing_fn(content: &str, offset: usize) -> String {
    let before = &content[..offset];
    for line in before.lines().rev() {
        let trimmed = line.trim();
        for prefix in &[
            "export async function ",
            "async function ",
            "export function ",
            "function ",
        ] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                if let Some(name) = rest.split(['(', ' ']).next() {
                    let cleaned = name.trim();
                    if !cleaned.is_empty() {
                        return cleaned.to_string();
                    }
                }
            }
        }
        for prefix in &["export const ", "const ", "export let ", "let "] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                if let Some(name) = rest.split(['=', ':', '(']).next() {
                    let cleaned = name.trim();
                    if !cleaned.is_empty() {
                        return cleaned.to_string();
                    }
                }
            }
        }
    }
    "apiCall".to_string()
}

struct IndexVisitor<'a> {
    file_path: &'a str,
    content: &'a str,
    omit_tests: bool,
    current_fn: Option<String>,
    edges: Vec<CallEdge>,
    contracts: Vec<ContractInfo>,
    symbols: Vec<SymbolDef>,
    routes: Vec<RouteBinding>,
}

impl<'a> IndexVisitor<'a> {
    fn new(file_path: &'a str, content: &'a str, omit_tests: bool) -> Self {
        Self {
            file_path,
            content,
            omit_tests,
            current_fn: None,
            edges: Vec::new(),
            contracts: Vec::new(),
            symbols: Vec::new(),
            routes: Vec::new(),
        }
    }
}

impl<'a> Visit<'_> for IndexVisitor<'a> {
    fn visit_item_mod(&mut self, node: &syn::ItemMod) {
        if self.omit_tests && node.attrs.iter().any(crate::skeleton::is_test_attribute) {
            return;
        }
        syn::visit::visit_item_mod(self, node);
    }

    fn visit_item_fn(&mut self, node: &syn::ItemFn) {
        if self.omit_tests && node.attrs.iter().any(crate::skeleton::is_test_attribute) {
            return;
        }

        let name = node.sig.ident.to_string();
        let pattern = format!("fn {}", name);
        let line = find_line(self.content, &pattern);
        self.symbols.push(SymbolDef {
            name: name.clone(),
            kind: "fn".to_string(),
            file: self.file_path.to_string(),
            line,
            language: "rust".to_string(),
        });

        for attr in &node.attrs {
            let attr_str = attr.to_token_stream().to_string();
            if attr_str.contains("tauri :: command") || attr_str.contains("tauri::command") {
                self.routes.push(RouteBinding::new(
                    name.clone(),
                    name.clone(),
                    self.file_path.to_string(),
                    line,
                    true,
                ));
            } else if attr_str.contains("get")
                || attr_str.contains("post")
                || attr_str.contains("put")
                || attr_str.contains("delete")
            {
                let route_path = extract_string_literal(&attr_str).unwrap_or_else(|| name.clone());
                self.routes.push(RouteBinding::new(
                    route_path,
                    name.clone(),
                    self.file_path.to_string(),
                    line,
                    true,
                ));
            }
        }

        let prev = self.current_fn.replace(name);
        syn::visit::visit_item_fn(self, node);
        self.current_fn = prev;
    }

    fn visit_item_struct(&mut self, node: &syn::ItemStruct) {
        if self.omit_tests && node.attrs.iter().any(crate::skeleton::is_test_attribute) {
            return;
        }
        let name = node.ident.to_string();
        let pattern = format!("struct {}", name);
        let line = find_line(self.content, &pattern);
        self.symbols.push(SymbolDef {
            name,
            kind: "struct".to_string(),
            file: self.file_path.to_string(),
            line,
            language: "rust".to_string(),
        });
        syn::visit::visit_item_struct(self, node);
    }

    fn visit_item_enum(&mut self, node: &syn::ItemEnum) {
        if self.omit_tests && node.attrs.iter().any(crate::skeleton::is_test_attribute) {
            return;
        }
        let name = node.ident.to_string();
        let pattern = format!("enum {}", name);
        let line = find_line(self.content, &pattern);
        self.symbols.push(SymbolDef {
            name,
            kind: "enum".to_string(),
            file: self.file_path.to_string(),
            line,
            language: "rust".to_string(),
        });
        syn::visit::visit_item_enum(self, node);
    }

    fn visit_item_trait(&mut self, node: &syn::ItemTrait) {
        if self.omit_tests && node.attrs.iter().any(crate::skeleton::is_test_attribute) {
            return;
        }
        let name = node.ident.to_string();
        let pattern = format!("trait {}", name);
        let line = find_line(self.content, &pattern);
        self.symbols.push(SymbolDef {
            name,
            kind: "trait".to_string(),
            file: self.file_path.to_string(),
            line,
            language: "rust".to_string(),
        });
        syn::visit::visit_item_trait(self, node);
    }

    fn visit_item_impl(&mut self, node: &syn::ItemImpl) {
        if self.omit_tests && node.attrs.iter().any(crate::skeleton::is_test_attribute) {
            return;
        }
        let target_type = node.self_ty.to_token_stream().to_string().replace(' ', "");
        if let Some((_, ref trait_path, _)) = node.trait_ {
            let trait_name = trait_path.to_token_stream().to_string().replace(' ', "");
            let mut methods = Vec::new();
            for item in &node.items {
                if let syn::ImplItem::Fn(m) = item {
                    if !self.omit_tests || !m.attrs.iter().any(crate::skeleton::is_test_attribute) {
                        methods.push(m.sig.to_token_stream().to_string());
                    }
                }
            }
            self.contracts.push(ContractInfo {
                trait_name,
                target_type,
                methods,
                file: self.file_path.to_string(),
            });
        }
        syn::visit::visit_item_impl(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &syn::ImplItemFn) {
        if self.omit_tests && node.attrs.iter().any(crate::skeleton::is_test_attribute) {
            return;
        }
        let name = node.sig.ident.to_string();
        let prev = self.current_fn.replace(name);
        syn::visit::visit_impl_item_fn(self, node);
        self.current_fn = prev;
    }

    fn visit_expr_call(&mut self, node: &syn::ExprCall) {
        if let Some(ref caller) = self.current_fn {
            let callee = node.func.to_token_stream().to_string();
            // Take the last path segment identifier
            let short_callee = callee
                .split("::")
                .last()
                .unwrap_or(&callee)
                .trim()
                .to_string();
            self.edges.push(CallEdge {
                caller: caller.clone(),
                callee: short_callee,
                file: self.file_path.to_string(),
            });
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &syn::ExprMethodCall) {
        if let Some(ref caller) = self.current_fn {
            self.edges.push(CallEdge {
                caller: caller.clone(),
                callee: node.method.to_string(),
                file: self.file_path.to_string(),
            });
        }
        syn::visit::visit_expr_method_call(self, node);
    }
}

#[cfg(test)]
#[path = "indexer_tests.rs"]
mod tests;
