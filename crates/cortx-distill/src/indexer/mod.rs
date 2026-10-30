//! Semantic call-graph, contract, and cross-boundary indexer for polyglot codebases.

use crate::error::DistillError;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub mod go_graph;
pub mod manifests;
pub mod overlay;
pub mod pool;
pub mod rust_graph;
pub mod ts_graph;
pub mod types;

pub use go_graph::{GoFactOptions, derive_go_package_path, extract_go_file_facts};
pub use manifests::WorkspaceManifests;
pub use overlay::OverlayView;
pub use pool::{CallEdge, CompactCallEdge, StringPool, SymbolId};
pub use rust_graph::{
    ContainerKind, ExportEntry, FileFacts, RawCallSite, RawCalleeTarget, RustFactOptions, TypeRef,
    derive_base_module_path, extract_rust_file_facts,
};
pub use ts_graph::{TsFactOptions, extract_ts_file_facts, normalize_ts_module_path};
pub use types::*;

/// In-memory semantic index across a codebase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolIndex {
    #[serde(default)]
    pub pool: StringPool,
    #[serde(default)]
    pub callers_map: HashMap<SymbolId, Vec<CompactCallEdge>>,
    #[serde(default)]
    pub callees_map: HashMap<SymbolId, Vec<CompactCallEdge>>,
    pub contracts_map: HashMap<String, Vec<ContractInfo>>,
    pub symbols_map: HashMap<String, Vec<SymbolDef>>,
    #[serde(default)]
    pub file_to_symbols: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub file_symbols: HashMap<PathBuf, Vec<SymbolDef>>,
    #[serde(default)]
    pub routes: Vec<RouteBinding>,
    #[serde(skip)]
    pub file_name_to_paths: HashMap<String, Vec<String>>,
    #[serde(skip)]
    pub file_facts: HashMap<PathBuf, std::sync::Arc<FileFacts>>,
    #[serde(skip, default)]
    pub manifests: std::sync::Arc<WorkspaceManifests>,
    #[serde(skip)]
    pub parse_fallbacks: usize,
}

impl Default for SymbolIndex {
    fn default() -> Self {
        Self {
            pool: StringPool::default(),
            callers_map: HashMap::default(),
            callees_map: HashMap::default(),
            contracts_map: HashMap::default(),
            symbols_map: HashMap::default(),
            file_to_symbols: HashMap::default(),
            file_symbols: HashMap::default(),
            routes: Vec::default(),
            file_name_to_paths: HashMap::default(),
            file_facts: HashMap::default(),
            manifests: std::sync::Arc::new(WorkspaceManifests::discover(Path::new("."), &[])),
            parse_fallbacks: 0,
        }
    }
}

impl SymbolIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_call_edge_with_line(
        &mut self,
        caller: &str,
        callee: &str,
        file: &str,
        call_line: u32,
    ) {
        let caller_id = self.pool.intern(caller);
        let callee_id = self.pool.intern(callee);
        let file_id = self.pool.intern(file);

        let edge = CompactCallEdge {
            caller: caller_id,
            callee: callee_id,
            file: file_id,
            call_line,
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

    pub fn record_call_edge(&mut self, caller: &str, callee: &str, file: &str) {
        self.record_call_edge_with_line(caller, callee, file, 0);
    }

    pub fn add_call_edge(&mut self, edge: CallEdge) {
        self.record_call_edge_with_line(
            &edge.caller,
            &edge.callee,
            &edge.file,
            edge.call_line as u32,
        );
    }

    pub fn record_symbol(&mut self, sym: SymbolDef) {
        let file_path = PathBuf::from(&sym.file);
        self.file_symbols
            .entry(file_path)
            .or_default()
            .push(sym.clone());

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

    pub fn metrics(&self) -> SymbolIndexMetrics {
        let mut files = HashSet::new();
        let mut total_symbols = 0;
        for defs in self.symbols_map.values() {
            total_symbols += defs.len();
            for def in defs {
                files.insert(&def.file);
            }
        }
        SymbolIndexMetrics {
            distinct_files_count: files.len(),
            total_symbols_count: total_symbols,
            total_call_edges_count: self.callers_map.values().map(|v| v.len()).sum(),
            total_contracts_count: self.contracts_map.values().map(|v| v.len()).sum(),
            total_routes_count: self.routes.len(),
        }
    }

    pub fn estimate_memory_mb(&self) -> f64 {
        let mut bytes = std::mem::size_of::<Self>()
            + self.pool.strings.capacity() * std::mem::size_of::<String>()
            + self.pool.lookup.capacity() * 32;
        bytes += self.pool.strings.iter().map(|s| s.len()).sum::<usize>();
        bytes += self
            .callers_map
            .values()
            .chain(self.callees_map.values())
            .map(|e| 24 + e.capacity() * std::mem::size_of::<CompactCallEdge>())
            .sum::<usize>();
        bytes += self
            .contracts_map
            .iter()
            .map(|(t, c)| t.len() + 32 + c.capacity() * std::mem::size_of::<ContractInfo>())
            .sum::<usize>();
        bytes += self
            .symbols_map
            .iter()
            .map(|(n, d)| n.len() + 32 + d.capacity() * std::mem::size_of::<SymbolDef>())
            .sum::<usize>();
        bytes += self
            .file_to_symbols
            .iter()
            .map(|(f, s)| f.len() + 32 + s.capacity() * std::mem::size_of::<String>())
            .sum::<usize>();
        bytes += self
            .file_symbols
            .iter()
            .map(|(p, s)| {
                p.as_os_str().len() + 32 + s.capacity() * std::mem::size_of::<SymbolDef>()
            })
            .sum::<usize>();
        bytes += self
            .file_facts
            .iter()
            .map(|(p, f)| {
                p.as_os_str().len()
                    + std::mem::size_of::<FileFacts>()
                    + 64
                    + f.symbols.len() * std::mem::size_of::<SymbolDef>()
                    + f.calls.len() * std::mem::size_of::<RawCallSite>()
            })
            .sum::<usize>();
        bytes as f64 / (1024.0 * 1024.0)
    }

    pub fn create_overlay(&self, modified_files: &[PathBuf]) -> SymbolIndex {
        let mut overlay = SymbolIndex::new();
        for path in modified_files {
            if let Ok(content) = std::fs::read_to_string(path) {
                let path_str = path.to_string_lossy();
                let _ = overlay.index_any_source_str(&path_str, &content);
            }
        }
        overlay
    }

    pub fn merge(&mut self, other: SymbolIndex) {
        let id_map: Vec<SymbolId> = other
            .pool
            .strings
            .iter()
            .map(|s| self.pool.intern(s))
            .collect();
        let map_edge = |e: &CompactCallEdge| CompactCallEdge {
            caller: id_map[e.caller.0 as usize],
            callee: id_map[e.callee.0 as usize],
            file: id_map[e.file.0 as usize],
            call_line: e.call_line,
        };

        for (k, edges) in other.callers_map {
            self.callers_map
                .entry(id_map[k.0 as usize])
                .or_default()
                .extend(edges.iter().map(&map_edge));
        }
        for (k, edges) in other.callees_map {
            self.callees_map
                .entry(id_map[k.0 as usize])
                .or_default()
                .extend(edges.iter().map(&map_edge));
        }
        for edges in self
            .callers_map
            .values_mut()
            .chain(self.callees_map.values_mut())
        {
            edges.sort_unstable();
            edges.dedup();
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
        for (k, v) in other.file_symbols {
            self.file_symbols.entry(k).or_default().extend(v);
        }
        self.routes.extend(other.routes);
        for (k, v) in other.file_name_to_paths {
            let entry = self.file_name_to_paths.entry(k).or_default();
            for p in v {
                if !entry.contains(&p) {
                    entry.push(p);
                }
            }
        }
        self.file_facts.extend(other.file_facts);
    }

    pub fn collect_source_files(&self, root: &Path) -> Vec<PathBuf> {
        let (cfg, _) = cortx_core::config::CortxConfig::load_daemon_config(None)
            .unwrap_or_else(|_| (cortx_core::config::CortxConfig::default(), None));
        let proj = cfg
            .find_project_for_path(root)
            .map(|(_, p)| p.clone())
            .unwrap_or_else(|| {
                cortx_core::ProjectConfig::from_cortx_config(root.to_path_buf(), &cfg)
            });
        self.collect_source_files_for_project(root, &proj)
    }

    pub fn collect_source_files_with_options(
        &self,
        root: &Path,
        ignored_dirs: &[String],
        worktrees_dir: Option<&str>,
    ) -> Vec<PathBuf> {
        self.collect_source_files_for_project(
            root,
            &cortx_core::ProjectConfig {
                qualified_path: root.to_path_buf(),
                has_worktrees: worktrees_dir.is_some(),
                custom_worktree_path: worktrees_dir.map(PathBuf::from),
                ignored_dirs: ignored_dirs.to_vec(),
                ..cortx_core::ProjectConfig::default()
            },
        )
    }

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
                    if p.join(".git").exists()
                        || rel == Path::new(".worktrees")
                        || rel.starts_with(".worktrees")
                    {
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
                let in_watch = p
                    .strip_prefix(rel_base)
                    .is_ok_and(|rel| project.is_file_in_watch_paths(rel));
                if !in_watch {
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

    pub fn index_directory_with_project(
        &mut self,
        root: &Path,
        project: &cortx_core::ProjectConfig,
    ) -> Result<usize, DistillError> {
        let start = std::time::Instant::now();
        let files = self.collect_source_files_for_project(root, project);
        self.index_discovered_files(root, &files, start, project.distill.omit_tests)
    }

    pub fn index_discovered_files(
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

        let manifests = std::sync::Arc::new(WorkspaceManifests::discover(root, &[]));
        let parse_start = std::time::Instant::now();

        let facts_vec: Vec<(PathBuf, std::sync::Arc<FileFacts>)> = files
            .par_iter()
            .filter_map(|path| {
                let content = std::fs::read_to_string(path).ok()?;
                let rel_path = path.strip_prefix(root).unwrap_or(path).to_path_buf();
                let parse_res =
                    Self::extract_facts_for_file(path, &rel_path, &content, &manifests, omit_tests);
                parse_res
                    .ok()
                    .map(|facts| (path.clone(), std::sync::Arc::new(facts)))
            })
            .collect();

        let mut file_facts = HashMap::with_capacity(facts_vec.len());
        for (p, f) in facts_vec {
            file_facts.insert(p, f);
        }
        let parse_ms = parse_start.elapsed().as_millis();

        let link_start = std::time::Instant::now();
        let linked_index = Self::link_facts(&file_facts, manifests, 0);
        let link_ms = link_start.elapsed().as_millis();

        *self = linked_index;

        let total_elapsed = start.elapsed();
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
            link_ms,
            total_ms = total_elapsed.as_millis(),
            throughput_fps = format!("{:.0}", throughput),
            "Initial indexing complete"
        );
        Ok(total_files)
    }

    fn extract_facts_for_file(
        path: &Path,
        rel_path: &Path,
        content: &str,
        manifests: &WorkspaceManifests,
        omit_tests: bool,
    ) -> Result<FileFacts, DistillError> {
        if path.extension().is_some_and(|e| e == "rs") {
            let crate_name = manifests.rust_crate_for_path(path).unwrap_or("crate");
            let opts = RustFactOptions {
                file_path: path,
                rel_path,
                crate_name,
                omit_tests,
            };
            extract_rust_file_facts(&opts, content)
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e == "ts" || e == "tsx" || e == "js" || e == "jsx")
        {
            let opts = TsFactOptions {
                file_path: path,
                rel_path,
                package_name: None,
                omit_tests,
            };
            extract_ts_file_facts(&opts, content)
        } else if path.extension().is_some_and(|e| e == "go") {
            let pkg_path = manifests.go_package_for_path(path);
            let opts = GoFactOptions {
                file_path: path,
                rel_path,
                package_path: pkg_path.as_deref(),
                omit_tests,
            };
            extract_go_file_facts(&opts, content)
        } else {
            Err(DistillError::SymbolNotFound(format!(
                "unsupported extension: {}",
                path.display()
            )))
        }
    }

    pub fn link_facts(
        file_facts: &HashMap<PathBuf, std::sync::Arc<FileFacts>>,
        manifests: std::sync::Arc<WorkspaceManifests>,
        parse_fallbacks: usize,
    ) -> SymbolIndex {
        let mut idx = SymbolIndex::new();
        idx.manifests = manifests;
        idx.file_facts = file_facts.clone();
        idx.parse_fallbacks = parse_fallbacks;

        for facts in file_facts.values() {
            for sym in &facts.symbols {
                idx.record_symbol(sym.clone());
            }
            for contract in &facts.contracts {
                idx.contracts_map
                    .entry(contract.target_type.clone())
                    .or_default()
                    .push(contract.clone());
            }
            for route in &facts.routes {
                idx.routes.push(route.clone());
            }
        }

        for facts in file_facts.values() {
            let is_rs = facts.file_path.extension().is_some_and(|e| e == "rs");
            let is_go = facts.file_path.extension().is_some_and(|e| e == "go");
            let is_ts = facts
                .file_path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e == "ts" || e == "tsx" || e == "js" || e == "jsx");

            for call in &facts.calls {
                let caller = if is_rs {
                    call.caller_fqn
                        .rsplit("::")
                        .next()
                        .unwrap_or(&call.caller_fqn)
                        .to_string()
                } else if is_go {
                    call.caller_fqn
                        .rsplit('.')
                        .next()
                        .unwrap_or(&call.caller_fqn)
                        .to_string()
                } else if is_ts {
                    let file_name = facts
                        .file_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("");
                    let fn_name = call
                        .caller_fqn
                        .rsplit("::")
                        .next()
                        .unwrap_or(&call.caller_fqn);
                    if fn_name == "<module>" || fn_name.is_empty() {
                        file_name.to_string()
                    } else if !file_name.is_empty() {
                        format!("{file_name}:{fn_name}")
                    } else {
                        fn_name.to_string()
                    }
                } else {
                    call.caller_fqn.clone()
                };
                let line = call.call_line;
                let file = facts.file_path.to_string_lossy().to_string();

                match &call.target {
                    RawCalleeTarget::Path(target_path) => {
                        if target_path.contains("::") && is_ts {
                            let mut parts = target_path.splitn(2, "::");
                            let specifier = parts.next().unwrap_or("");
                            let imported_name = parts.next().unwrap_or("");
                            if let Ok(target_file) =
                                idx.manifests.resolve_ts_import(&facts.file_path, specifier)
                            {
                                let mut visited = HashSet::new();
                                if let Some(callee_fqn) = Self::resolve_ts_barrel_export(
                                    &target_file,
                                    imported_name,
                                    file_facts,
                                    &idx.manifests,
                                    0,
                                    &mut visited,
                                ) {
                                    let callee_name =
                                        callee_fqn.rsplit("::").next().unwrap_or(&callee_fqn);
                                    idx.record_call_edge_with_line(
                                        &caller,
                                        callee_name,
                                        &file,
                                        line,
                                    );
                                } else {
                                    idx.record_call_edge_with_line(
                                        &caller,
                                        imported_name,
                                        &file,
                                        line,
                                    );
                                }
                            } else {
                                idx.record_call_edge_with_line(&caller, imported_name, &file, line);
                            }
                        } else if is_go {
                            let callee = target_path.rsplit('.').next().unwrap_or(target_path);
                            if let Some((pkg, _func)) = target_path.rsplit_once('.') {
                                if !idx.manifests.go_module_prefixes.is_empty() {
                                    if idx.manifests.is_workspace_go_module(pkg) {
                                        idx.record_call_edge_with_line(
                                            &caller, callee, &file, line,
                                        );
                                    }
                                } else {
                                    idx.record_call_edge_with_line(&caller, callee, &file, line);
                                }
                            } else {
                                idx.record_call_edge_with_line(&caller, callee, &file, line);
                            }
                        } else if target_path.starts_with("crate::") {
                            let callee = target_path.rsplit("::").next().unwrap_or(target_path);
                            idx.record_call_edge_with_line(&caller, callee, &file, line);
                        } else if let Some(resolved_fqn) =
                            idx.symbols_map.get(target_path).and_then(|syms| {
                                syms.iter()
                                    .find(|s| s.file == facts.file_path.to_string_lossy())
                                    .map(|s| s.qualified_name.clone())
                                    .or_else(|| syms.first().map(|s| s.qualified_name.clone()))
                            })
                        {
                            let callee = resolved_fqn.rsplit("::").next().unwrap_or(&resolved_fqn);
                            idx.record_call_edge_with_line(&caller, callee, &file, line);
                        } else {
                            let callee = target_path.rsplit("::").next().unwrap_or(target_path);
                            idx.record_call_edge_with_line(&caller, callee, &file, line);
                        }
                    }
                    RawCalleeTarget::Method {
                        receiver,
                        method_name,
                    } => {
                        let unwrapped = receiver.unwrap_container();
                        if let TypeRef::Known { crate_or_pkg, .. } = unwrapped {
                            if is_go
                                && !idx.manifests.go_module_prefixes.is_empty()
                                && !crate_or_pkg.is_empty()
                            {
                                if idx.manifests.is_workspace_go_module(crate_or_pkg) {
                                    idx.record_call_edge_with_line(
                                        &caller,
                                        method_name,
                                        &file,
                                        line,
                                    );
                                }
                            } else {
                                idx.record_call_edge_with_line(&caller, method_name, &file, line);
                            }
                        } else {
                            idx.record_call_edge_with_line(&caller, method_name, &file, line);
                        }
                    }
                    RawCalleeTarget::FieldChain {
                        receiver,
                        fields,
                        method_name,
                    } => {
                        if let Some(TypeRef::Known { crate_or_pkg, .. }) =
                            Self::resolve_struct_field_chain(receiver, fields, file_facts)
                        {
                            if is_go
                                && !idx.manifests.go_module_prefixes.is_empty()
                                && !crate_or_pkg.is_empty()
                            {
                                if idx.manifests.is_workspace_go_module(&crate_or_pkg) {
                                    idx.record_call_edge_with_line(
                                        &caller,
                                        method_name,
                                        &file,
                                        line,
                                    );
                                }
                            } else {
                                idx.record_call_edge_with_line(&caller, method_name, &file, line);
                            }
                        } else {
                            idx.record_call_edge_with_line(&caller, method_name, &file, line);
                        }
                    }
                }
            }
        }

        for edges in idx
            .callers_map
            .values_mut()
            .chain(idx.callees_map.values_mut())
        {
            edges.sort_unstable();
            edges.dedup();
        }

        idx
    }

    fn resolve_ts_barrel_export(
        target_file: &Path,
        name: &str,
        file_facts: &HashMap<PathBuf, std::sync::Arc<FileFacts>>,
        manifests: &WorkspaceManifests,
        depth: usize,
        visited: &mut HashSet<(PathBuf, String)>,
    ) -> Option<String> {
        if depth > 8 || !visited.insert((target_file.to_path_buf(), name.to_string())) {
            return None;
        }

        let facts = file_facts.get(target_file).or_else(|| {
            file_facts.iter().find_map(|(p, f)| {
                if p == target_file || p.ends_with(target_file) || target_file.ends_with(p) {
                    Some(f)
                } else {
                    None
                }
            })
        })?;

        for export in &facts.exports {
            match export {
                ExportEntry::Local(local_name) => {
                    if local_name == name {
                        if let Some(def) = facts.symbols.iter().find(|s| s.name == name) {
                            return Some(def.qualified_name.clone());
                        }
                        return Some(format!(
                            "{}::{}",
                            normalize_ts_module_path(&facts.rel_path),
                            name
                        ));
                    }
                }
                ExportEntry::NamedReExport {
                    specifier,
                    imported,
                    exported,
                } => {
                    if exported == name {
                        if let Ok(next_file) = manifests.resolve_ts_import(target_file, specifier) {
                            return Self::resolve_ts_barrel_export(
                                &next_file,
                                imported,
                                file_facts,
                                manifests,
                                depth + 1,
                                visited,
                            );
                        }
                    }
                }
                ExportEntry::StarReExport { specifier } => {
                    if let Ok(next_file) = manifests.resolve_ts_import(target_file, specifier) {
                        if let Some(res) = Self::resolve_ts_barrel_export(
                            &next_file,
                            name,
                            file_facts,
                            manifests,
                            depth + 1,
                            visited,
                        ) {
                            return Some(res);
                        }
                    }
                }
            }
        }

        if let Some(def) = facts.symbols.iter().find(|s| s.name == name) {
            return Some(def.qualified_name.clone());
        }

        None
    }

    fn resolve_struct_field_chain(
        receiver: &TypeRef,
        fields: &[String],
        file_facts: &HashMap<PathBuf, std::sync::Arc<FileFacts>>,
    ) -> Option<TypeRef> {
        let mut curr = receiver.unwrap_container().clone();
        for field in fields.iter().take(4) {
            match &curr {
                TypeRef::Known { type_name, .. } => {
                    let mut found = None;
                    for facts in file_facts.values() {
                        if let Some(fields_map) = facts.field_types.get(type_name) {
                            if let Some(ft) = fields_map.get(field) {
                                found = Some(ft.unwrap_container().clone());
                                break;
                            }
                        }
                    }
                    curr = found?;
                }
                _ => return None,
            }
        }
        Some(curr)
    }

    pub fn apply_watch_batch(&self, req: WatchBatchUpdate<'_>) -> WatchBatchOutcome {
        let mut new_facts = self.file_facts.clone();
        let mut parse_fallbacks = self.parse_fallbacks;

        let manifests_touched = req
            .upserted
            .iter()
            .chain(req.removed.iter())
            .any(|p| WorkspaceManifests::is_manifest_path(p));

        let ignored_dirs = req
            .project_cfg
            .map(|c| c.ignored_dirs.as_slice())
            .unwrap_or(&[]);

        let manifests = if manifests_touched {
            std::sync::Arc::new(WorkspaceManifests::discover(
                &self.manifests.workspace_root,
                ignored_dirs,
            ))
        } else {
            std::sync::Arc::clone(&self.manifests)
        };

        for rem in req.removed {
            new_facts.retain(|path, _| !(path == rem || path.starts_with(rem)));
        }

        let omit_tests = req
            .project_cfg
            .map(|c| c.distill.omit_tests)
            .unwrap_or(true);

        for ups in req.upserted {
            if !ups.exists() {
                new_facts.retain(|path, _| !(path == ups || path.starts_with(ups)));
                continue;
            }

            if let Some(cfg) = req.project_cfg {
                if let Ok(rel) = ups.strip_prefix(&manifests.workspace_root) {
                    if cfg.is_relative_path_ignored(rel) || !cfg.is_file_in_watch_paths(rel) {
                        continue;
                    }
                }
                if let Some(ext) = ups.extension().and_then(|e| e.to_str()) {
                    if !cfg.is_extension_enabled(ext) {
                        continue;
                    }
                }
            }

            if let Ok(content) = std::fs::read_to_string(ups) {
                let rel_path = ups
                    .strip_prefix(&manifests.workspace_root)
                    .unwrap_or(ups)
                    .to_path_buf();

                match Self::extract_facts_for_file(ups, &rel_path, &content, &manifests, omit_tests)
                {
                    Ok(facts) => {
                        new_facts.insert(ups.clone(), std::sync::Arc::new(facts));
                    }
                    Err(_) => {
                        parse_fallbacks += 1;
                    }
                }
            }
        }

        let next_index = Self::link_facts(&new_facts, manifests, parse_fallbacks);
        WatchBatchOutcome {
            next_index,
            parse_fallbacks,
        }
    }

    pub fn index_directory_sequential(&mut self, root: &Path) -> Result<usize, DistillError> {
        self.index_directory(root)
    }

    pub fn index_any_source_str(
        &mut self,
        file_path: &str,
        content: &str,
    ) -> Result<(), DistillError> {
        self.index_any_source_str_with_options(file_path, content, true)
    }

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

    pub fn index_go_source_str(
        &mut self,
        file_path: &str,
        content: &str,
    ) -> Result<(), DistillError> {
        self.index_go_source_str_with_options(file_path, content, true)
    }

    pub fn index_go_source_str_with_options(
        &mut self,
        file_path: &str,
        content: &str,
        omit_tests: bool,
    ) -> Result<(), DistillError> {
        let path = PathBuf::from(file_path);
        let opts = GoFactOptions {
            file_path: &path,
            rel_path: &path,
            package_path: None,
            omit_tests,
        };
        let facts = extract_go_file_facts(&opts, content)?;
        self.integrate_file_facts(facts);
        Ok(())
    }

    pub fn index_source_str(&mut self, file_path: &str, content: &str) -> Result<(), DistillError> {
        self.index_source_str_with_options(file_path, content, true)
    }

    pub fn index_source_str_with_options(
        &mut self,
        file_path: &str,
        content: &str,
        omit_tests: bool,
    ) -> Result<(), DistillError> {
        let path = PathBuf::from(file_path);
        let opts = RustFactOptions {
            file_path: &path,
            rel_path: &path,
            crate_name: "crate",
            omit_tests,
        };
        let facts = extract_rust_file_facts(&opts, content)?;
        self.integrate_file_facts(facts);
        Ok(())
    }

    pub fn index_ts_source_str(
        &mut self,
        file_path: &str,
        content: &str,
    ) -> Result<(), DistillError> {
        let path = PathBuf::from(file_path);
        let opts = TsFactOptions {
            file_path: &path,
            rel_path: &path,
            package_name: None,
            omit_tests: false,
        };
        let facts = extract_ts_file_facts(&opts, content)?;
        self.integrate_file_facts(facts);
        Ok(())
    }

    fn integrate_file_facts(&mut self, facts: FileFacts) {
        for sym in &facts.symbols {
            self.record_symbol(sym.clone());
        }
        for contract in &facts.contracts {
            self.contracts_map
                .entry(contract.target_type.clone())
                .or_default()
                .push(contract.clone());
        }
        for route in &facts.routes {
            self.routes.push(route.clone());
        }
        let is_rs = facts.file_path.extension().is_some_and(|e| e == "rs");
        let is_go = facts.file_path.extension().is_some_and(|e| e == "go");
        let is_ts = facts
            .file_path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e == "ts" || e == "tsx" || e == "js" || e == "jsx");
        for call in &facts.calls {
            let caller = if is_rs {
                call.caller_fqn
                    .rsplit("::")
                    .next()
                    .unwrap_or(&call.caller_fqn)
                    .to_string()
            } else if is_go {
                call.caller_fqn
                    .rsplit('.')
                    .next()
                    .unwrap_or(&call.caller_fqn)
                    .to_string()
            } else if is_ts {
                let file_name = facts
                    .file_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("");
                let fn_name = call
                    .caller_fqn
                    .rsplit("::")
                    .next()
                    .unwrap_or(&call.caller_fqn);
                if fn_name == "<module>" || fn_name.is_empty() {
                    file_name.to_string()
                } else if !file_name.is_empty() {
                    format!("{file_name}:{fn_name}")
                } else {
                    fn_name.to_string()
                }
            } else {
                call.caller_fqn.clone()
            };
            let file = facts.file_path.to_string_lossy();
            let line = call.call_line;
            match &call.target {
                RawCalleeTarget::Path(p) => {
                    let callee = if is_go {
                        p.rsplit('.').next().unwrap_or(p)
                    } else {
                        p.rsplit("::").next().unwrap_or(p)
                    };
                    self.record_call_edge_with_line(&caller, callee, &file, line);
                }
                RawCalleeTarget::Method { method_name, .. } => {
                    self.record_call_edge_with_line(&caller, method_name, &file, line);
                }
                RawCalleeTarget::FieldChain { method_name, .. } => {
                    self.record_call_edge_with_line(&caller, method_name, &file, line);
                }
            }
        }
    }

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

    fn collect_edges_bfs(
        &self,
        map: &HashMap<SymbolId, Vec<CompactCallEdge>>,
        next_id_fn: fn(&CompactCallEdge) -> SymbolId,
        symbol: &str,
        depth: usize,
        results: &mut Vec<CallEdge>,
        seen: &mut HashSet<CallEdge>,
    ) {
        let mut queue = Vec::new();
        if let Some(start_id) = self.pool.get_id(symbol) {
            queue.push((start_id, 0));
        }
        let short_sym = symbol.rsplit("::").next().unwrap_or(symbol);
        let short_sym = short_sym.rsplit('.').next().unwrap_or(short_sym);
        if short_sym != symbol {
            if let Some(sid) = self.pool.get_id(short_sym) {
                queue.push((sid, 0));
            }
        }
        if let Some(defs) = self.symbols_map.get(symbol) {
            for def in defs {
                if !def.qualified_name.is_empty() && def.qualified_name != symbol {
                    if let Some(qid) = self.pool.get_id(&def.qualified_name) {
                        queue.push((qid, 0));
                    }
                }
            }
        }
        if queue.is_empty() {
            return;
        }

        let mut visited = HashSet::new();
        while let Some((curr, d)) = queue.pop() {
            if d >= depth || !visited.insert(curr) {
                continue;
            }
            if let Some(edges) = map.get(&curr) {
                for edge in edges {
                    if let Some(resolved) = edge.resolve(&self.pool) {
                        if seen.insert(resolved.clone()) {
                            results.push(resolved);
                        }
                        queue.push((next_id_fn(edge), d + 1));
                    }
                }
            }
        }
    }

    pub fn get_callers(&self, symbol: &str, depth: usize) -> Vec<CallEdge> {
        let mut results = Vec::new();
        self.collect_edges_bfs(
            &self.callers_map,
            |e| e.caller,
            symbol,
            depth,
            &mut results,
            &mut HashSet::new(),
        );
        results
    }

    pub fn collect_callers_into(
        &self,
        symbol: &str,
        depth: usize,
        results: &mut Vec<CallEdge>,
        seen: &mut HashSet<CallEdge>,
    ) {
        self.collect_edges_bfs(
            &self.callers_map,
            |e| e.caller,
            symbol,
            depth,
            results,
            seen,
        );
    }

    pub fn get_callees(&self, symbol: &str, depth: usize) -> Vec<CallEdge> {
        let mut results = Vec::new();
        self.collect_edges_bfs(
            &self.callees_map,
            |e| e.callee,
            symbol,
            depth,
            &mut results,
            &mut HashSet::new(),
        );
        results
    }

    pub fn collect_callees_into(
        &self,
        symbol: &str,
        depth: usize,
        results: &mut Vec<CallEdge>,
        seen: &mut HashSet<CallEdge>,
    ) {
        self.collect_edges_bfs(
            &self.callees_map,
            |e| e.callee,
            symbol,
            depth,
            results,
            seen,
        );
    }

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

    pub fn get_satisfied_interfaces(&self, struct_name: &str) -> Vec<ContractInfo> {
        self.get_contracts(struct_name)
    }

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

    pub fn get_callers_with_fallback(
        &self,
        base: Option<&SymbolIndex>,
        symbol: &str,
        depth: usize,
    ) -> Vec<CallEdge> {
        let callers = self.get_callers(symbol, depth);
        if !callers.is_empty() {
            callers
        } else {
            base.map(|b| b.get_callers(symbol, depth))
                .unwrap_or_default()
        }
    }

    pub fn get_callees_with_fallback(
        &self,
        base: Option<&SymbolIndex>,
        symbol: &str,
        depth: usize,
    ) -> Vec<CallEdge> {
        let callees = self.get_callees(symbol, depth);
        if !callees.is_empty() {
            callees
        } else {
            base.map(|b| b.get_callees(symbol, depth))
                .unwrap_or_default()
        }
    }

    pub fn get_contracts_with_fallback(
        &self,
        base: Option<&SymbolIndex>,
        target_type: &str,
    ) -> Vec<ContractInfo> {
        let contracts = self.get_contracts(target_type);
        if !contracts.is_empty() {
            contracts
        } else {
            base.map(|b| b.get_contracts(target_type))
                .unwrap_or_default()
        }
    }

    pub fn get_cross_boundary_impact_with_fallback(
        &self,
        base: Option<&SymbolIndex>,
        symbol: &str,
    ) -> Option<CrossBoundaryImpact> {
        self.get_cross_boundary_impact(symbol)
            .or_else(|| base.and_then(|b| b.get_cross_boundary_impact(symbol)))
    }

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

#[cfg(test)]
#[path = "../indexer_tests.rs"]
mod tests;
