//! Worktree overlay union view (`overlay ∪ (base \ shadowed_files)`).

use super::{CallEdge, ContractInfo, CrossBoundaryImpact, SymbolDef, SymbolIndex};
use rustc_hash::FxHashSet;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

/// Worktree overlay union view (`overlay ∪ (base \ shadowed_files)`).
#[derive(Debug, Clone, Copy)]
pub struct OverlayView<'a> {
    pub base: Option<&'a SymbolIndex>,
    pub overlay: Option<&'a SymbolIndex>,
    pub shadowed_files: Option<&'a HashSet<PathBuf>>,
}

impl<'a> OverlayView<'a> {
    /// Creates an empty overlay view without base, overlay, or tombstones.
    pub fn empty() -> Self {
        Self {
            base: None,
            overlay: None,
            shadowed_files: None,
        }
    }

    /// Returns true if the file path is masked by a worktree deletion tombstone.
    pub fn is_shadowed(&self, file_path: &str) -> bool {
        let Some(shadowed) = self.shadowed_files else {
            return false;
        };
        if shadowed.is_empty() {
            return false;
        }
        let p = Path::new(file_path);
        if shadowed.contains(p) || p.strip_prefix("./").is_ok_and(|rel| shadowed.contains(rel)) {
            return true;
        }
        shadowed
            .iter()
            .any(|sf| p.ends_with(sf) || p.starts_with(sf))
    }

    /// Returns true if the given path is masked by a worktree deletion tombstone.
    pub fn is_shadowed_path(&self, path: &Path) -> bool {
        self.is_shadowed(&path.to_string_lossy())
    }

    fn collect_edges<S: std::hash::BuildHasher>(
        &self,
        is_callers: bool,
        symbol: &str,
        depth: usize,
        results: &mut Vec<CallEdge>,
        seen: &mut HashSet<CallEdge, S>,
    ) {
        if self.overlay.is_none() && self.shadowed_files.is_none_or(|s| s.is_empty()) {
            if let Some(base_idx) = self.base {
                if is_callers {
                    base_idx.collect_callers_into(symbol, depth, results, seen);
                } else {
                    base_idx.collect_callees_into(symbol, depth, results, seen);
                }
            }
            return;
        }

        if let Some(ov) = self.overlay {
            if is_callers {
                ov.collect_callers_into(symbol, depth, results, seen);
            } else {
                ov.collect_callees_into(symbol, depth, results, seen);
            }
        }
        if let Some(base_idx) = self.base {
            let mut base_edges = Vec::new();
            let mut base_seen = FxHashSet::default();
            if is_callers {
                base_idx.collect_callers_into(symbol, depth, &mut base_edges, &mut base_seen);
            } else {
                base_idx.collect_callees_into(symbol, depth, &mut base_edges, &mut base_seen);
            }
            for edge in base_edges {
                if !self.is_shadowed(&edge.file) && seen.insert(edge.clone()) {
                    results.push(edge);
                }
            }
        }
    }

    /// Retrieves upstream callers across overlay and base, filtering shadowed files.
    pub fn get_callers(&self, symbol: &str, depth: usize) -> Vec<CallEdge> {
        let mut results = Vec::new();
        self.collect_edges(true, symbol, depth, &mut results, &mut FxHashSet::default());
        results
    }

    /// Collects upstream callers across overlay and base, filtering shadowed files.
    pub fn collect_callers_into<S: std::hash::BuildHasher>(
        &self,
        symbol: &str,
        depth: usize,
        results: &mut Vec<CallEdge>,
        seen: &mut HashSet<CallEdge, S>,
    ) {
        self.collect_edges(true, symbol, depth, results, seen);
    }

    /// Retrieves downstream callees across overlay and base, filtering shadowed files.
    pub fn get_callees(&self, symbol: &str, depth: usize) -> Vec<CallEdge> {
        let mut results = Vec::new();
        self.collect_edges(false, symbol, depth, &mut results, &mut FxHashSet::default());
        results
    }

    /// Collects downstream callees across overlay and base, filtering shadowed files.
    pub fn collect_callees_into<S: std::hash::BuildHasher>(
        &self,
        symbol: &str,
        depth: usize,
        results: &mut Vec<CallEdge>,
        seen: &mut HashSet<CallEdge, S>,
    ) {
        self.collect_edges(false, symbol, depth, results, seen);
    }

    /// Retrieves trait contracts associated with a target type, filtering shadowed files.
    pub fn get_contracts(&self, target_type: &str) -> Vec<ContractInfo> {
        let mut results = self
            .overlay
            .map(|o| o.get_contracts(target_type))
            .unwrap_or_default();
        let mut seen = FxHashSet::default();
        for c in &results {
            seen.insert((c.trait_name.as_str(), c.target_type.as_str(), c.file.as_str()));
        }

        if let Some(base_idx) = self.base {
            let base_contracts = base_idx.get_contracts(target_type);
            let mut keep = vec![false; base_contracts.len()];
            for (i, c) in base_contracts.iter().enumerate() {
                if !self.is_shadowed(&c.file)
                    && seen.insert((c.trait_name.as_str(), c.target_type.as_str(), c.file.as_str()))
                {
                    keep[i] = true;
                }
            }
            drop(seen);
            for (c, kept) in base_contracts.into_iter().zip(keep) {
                if kept {
                    results.push(c);
                }
            }
        }
        results
    }

    /// Computes cross-boundary impact correlating backend models with frontend contracts.
    pub fn get_cross_boundary_impact(&self, symbol: &str) -> Option<CrossBoundaryImpact> {
        let ov_cb = self
            .overlay
            .and_then(|o| o.get_cross_boundary_impact(symbol));
        let base_cb = self.base.and_then(|b| b.get_cross_boundary_impact(symbol));

        match (ov_cb, base_cb) {
            (Some(ov), Some(base)) => {
                let filter_def = |d: Option<String>| {
                    d.filter(|s| !self.is_shadowed(s.split(':').next().unwrap_or("")))
                };
                let rust_def = ov
                    .rust_definition
                    .or_else(|| filter_def(base.rust_definition));
                let ts_interface = ov.ts_interface.or_else(|| filter_def(base.ts_interface));
                let go_def = ov.go_definition.or_else(|| filter_def(base.go_definition));
                let mut call_sites = ov.api_call_sites;
                for cs in base.api_call_sites {
                    if !self.is_shadowed(cs.split(':').next().unwrap_or("")) {
                        call_sites.push(cs);
                    }
                }
                call_sites.sort();
                call_sites.dedup();
                Some(CrossBoundaryImpact {
                    symbol: symbol.to_string(),
                    rust_definition: rust_def,
                    ts_interface,
                    go_definition: go_def,
                    api_call_sites: call_sites,
                })
            }
            (Some(ov), None) => Some(ov),
            (None, Some(base)) => {
                let filter_def = |d: Option<String>| {
                    d.filter(|s| !self.is_shadowed(s.split(':').next().unwrap_or("")))
                };
                let rust_def = filter_def(base.rust_definition);
                let ts_interface = filter_def(base.ts_interface);
                let go_def = filter_def(base.go_definition);
                let mut call_sites = Vec::new();
                for cs in base.api_call_sites {
                    if !self.is_shadowed(cs.split(':').next().unwrap_or("")) {
                        call_sites.push(cs);
                    }
                }
                if rust_def.is_some()
                    || ts_interface.is_some()
                    || go_def.is_some()
                    || !call_sites.is_empty()
                {
                    Some(CrossBoundaryImpact {
                        symbol: symbol.to_string(),
                        rust_definition: rust_def,
                        ts_interface,
                        go_definition: go_def,
                        api_call_sites: call_sites,
                    })
                } else {
                    None
                }
            }
            (None, None) => None,
        }
    }

    /// Looks up declared symbol names for a file path, checking overlay first, then base.
    pub fn get_file_symbols<'b>(&'b self, target_path: &Path) -> Option<&'b [String]> {
        if let Some(syms) = self.overlay.and_then(|o| o.get_file_symbols(target_path)) {
            return Some(syms);
        }
        if !self.is_shadowed_path(target_path) {
            return self.base.and_then(|b| b.get_file_symbols(target_path));
        }
        None
    }

    /// Looks up declared symbol definitions for a file path, checking overlay first, then base.
    pub fn get_file_defs<'b>(&'b self, target_path: &Path) -> Option<&'b [SymbolDef]> {
        if let Some(defs) = self.overlay.and_then(|o| o.get_file_defs(target_path)) {
            return Some(defs);
        }
        if !self.is_shadowed_path(target_path) {
            return self.base.and_then(|b| b.get_file_defs(target_path));
        }
        None
    }

    /// Returns workspace root from overlay or base index.
    pub fn workspace_root(&self) -> Option<&Path> {
        self.overlay
            .map(|o| o.manifests.workspace_root.as_path())
            .or_else(|| self.base.map(|b| b.manifests.workspace_root.as_path()))
    }

    /// Looks up symbol definitions, combining overlay and non-shadowed base entries.
    pub fn get_symbol_defs<'b>(&'b self, symbol: &str) -> Vec<&'b SymbolDef> {
        let mut defs = Vec::new();
        if let Some(ov) = self.overlay {
            if let Some(ov_defs) = ov.symbols_map.get(symbol) {
                defs.extend(ov_defs);
            }
        }
        if let Some(base_idx) = self.base {
            if let Some(base_defs) = base_idx.symbols_map.get(symbol) {
                defs.extend(base_defs.iter().filter(|d| !self.is_shadowed(&d.file)));
            }
        }
        defs
    }

    /// Looks up a cached distillation skeleton for the given path, validating mtime.
    pub fn get_cached_skeleton(&self, path: &Path, mtime: SystemTime) -> Option<Arc<str>> {
        if let Some(ov) = self.overlay {
            if let Some((cached_mtime, skeleton)) = ov.skeleton_cache.read().get(path) {
                if *cached_mtime == mtime {
                    return Some(Arc::clone(skeleton));
                }
            }
        }
        if !self.is_shadowed_path(path) {
            if let Some(base_idx) = self.base {
                if let Some((cached_mtime, skeleton)) = base_idx.skeleton_cache.read().get(path) {
                    if *cached_mtime == mtime {
                        return Some(Arc::clone(skeleton));
                    }
                }
            }
        }
        None
    }

    /// Inserts a distillation skeleton into the active overlay or base cache.
    pub fn insert_cached_skeleton(&self, path: PathBuf, mtime: SystemTime, skeleton: Arc<str>) {
        if let Some(ov) = self.overlay {
            ov.skeleton_cache.write().insert(path, (mtime, skeleton));
        } else if let Some(base_idx) = self.base {
            base_idx
                .skeleton_cache
                .write()
                .insert(path, (mtime, skeleton));
        }
    }
}
