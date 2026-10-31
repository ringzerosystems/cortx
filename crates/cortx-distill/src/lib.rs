//! # cortx-distill
//!
//! AST parsing and compiler-aware context distillation engine.
//!
//! Extracts minimal type skeletons, caller/callee hierarchies, and trait contracts
//! to deliver compiler-accurate context out-of-band to AI coding agents.

#![forbid(unsafe_code)]

pub mod bundle;
pub mod error;
pub mod go_skeleton;
pub mod indexer;
pub mod skeleton;
pub mod ts_skeleton;

pub use bundle::{
    AdvisoryDisposition, BudgetStage, BudgetedAdvisory, DistillBundle, MAX_HOOK_WIRE_CHARS,
    PackedAdvisory, decide_stage,
};
pub use error::DistillError;
pub use go_skeleton::{
    GoIndexedData, extract_go_file_skeleton, extract_go_file_skeleton_with_options,
    extract_go_skeleton, extract_go_skeleton_with_options, has_go_test_signature, index_go_ast,
    index_go_ast_with_options, is_go_test_function,
};
pub use indexer::{
    CallEdge, CompactCallEdge, ContractInfo, CrossBoundaryImpact, OverlayView, RouteBinding,
    StringPool, SymbolDef, SymbolId, SymbolIndex, SymbolIndexMetrics, WorkspaceManifests,
    is_ignored_path,
};
pub use skeleton::{
    SkeletonStripper, extract_file_skeleton, extract_file_skeleton_with_options, extract_skeleton,
    extract_skeleton_with_options, is_test_attribute, is_test_item, strip_file_ast,
    strip_file_ast_with_options,
};
pub use ts_skeleton::{
    extract_ts_file_skeleton, extract_ts_file_skeleton_with_options, extract_ts_skeleton,
    extract_ts_skeleton_with_options,
};

pub use cortx_core::model::DistillMode;
pub use cortx_core::{ReadCoverage, ReadSpan};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

/// Encapsulates parameters for a context distillation query.
#[derive(Debug, Clone)]
pub struct DistillQuery<'a> {
    pub file_path: &'a Path,
    pub mode: DistillMode,
    pub coverage: ReadCoverage,
    pub span: Option<ReadSpan>,
    pub preloaded_source: Option<&'a str>,
    pub cross_boundary: bool,
    pub project_cfg: Option<&'a cortx_core::ProjectConfig>,
}

impl<'a> DistillQuery<'a> {
    /// Creates a new query with default full file coverage.
    pub fn new(file_path: &'a Path, mode: DistillMode) -> Self {
        Self {
            file_path,
            mode,
            coverage: ReadCoverage::FullFile,
            span: None,
            preloaded_source: None,
            cross_boundary: false,
            project_cfg: None,
        }
    }
}

/// Distills a target file or symbol using the provided query specification and overlay view.
pub fn distill_with_query(
    query: &DistillQuery<'_>,
    view: OverlayView<'_>,
) -> Result<DistillBundle, DistillError> {
    if query.coverage == ReadCoverage::Micro {
        return Ok(DistillBundle {
            target: query.file_path.display().to_string(),
            skeleton: String::new(),
            contracts: Vec::new(),
            callers: Vec::new(),
            callees: Vec::new(),
            token_reduction_pct: 0.0,
            cross_boundary: None,
            unread_outline: None,
            omitted_callers: 0,
            omitted_callees: 0,
        });
    }

    let owned_source;
    let raw_source = match query.preloaded_source {
        Some(s) => s,
        None => {
            owned_source = std::fs::read_to_string(query.file_path).map_err(DistillError::Io)?;
            &owned_source
        }
    };
    let raw_len = raw_source.len().max(1);

    let ext = query
        .file_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    let omit_tests = query.project_cfg.is_none_or(|p| p.distill.omit_tests);
    let is_target_test = is_test_path(query.file_path);
    let effective_omit_tests = if is_target_test { false } else { omit_tests };

    let skeleton = resolve_or_cache_skeleton(query, &view, raw_source, effective_omit_tests)?;
    let skeleton_len = skeleton.len();

    let reduction_pct = if raw_len > skeleton_len {
        ((raw_len - skeleton_len) as f64 / raw_len as f64) * 100.0
    } else {
        0.0
    };

    let target_name = query
        .file_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("target");
    let call_depth = query.project_cfg.map_or(2, |p| p.distill.max_call_depth);

    let target_defs = view
        .get_file_defs(query.file_path)
        .map(|d| d.to_vec())
        .unwrap_or_else(|| {
            extract_defs_from_source(query.file_path, raw_source, ext, effective_omit_tests)
        });

    let mut contracts = Vec::new();
    if matches!(query.mode, DistillMode::Contracts | DistillMode::Bundle) {
        contracts = collect_contracts_view(&view, query.file_path, &target_defs);
    }

    let mut callers = Vec::new();
    let mut callees = Vec::new();
    let mut omitted_callers = 0;
    let mut omitted_callees = 0;

    let read_window = query
        .span
        .as_ref()
        .and_then(|s| match (s.start_line, s.end_line) {
            (Some(st), Some(en)) => Some((st, en)),
            (Some(st), None) => Some((st, usize::MAX)),
            (None, Some(en)) => Some((1, en)),
            (None, None) => None,
        });

    if matches!(query.mode, DistillMode::Hierarchy | DistillMode::Bundle) {
        let mut raw_callers =
            collect_callers_view(&view, query.file_path, &target_defs, call_depth);
        let mut raw_callees =
            collect_callees_view(&view, query.file_path, &target_defs, call_depth);
        if effective_omit_tests {
            raw_callers.retain(|edge| !is_test_call_edge(edge));
            raw_callees.retain(|edge| !is_test_call_edge(edge));
        }

        let ws_root = view.workspace_root();
        normalize_edge_files(&mut raw_callers, ws_root);
        normalize_edge_files(&mut raw_callees, ws_root);

        let (ranked_callers, o_callers) =
            rank_and_cap_callers(raw_callers, &target_defs, read_window);
        let (ranked_callees, o_callees) =
            rank_and_cap_callees(raw_callees, &target_defs, read_window);

        callers = ranked_callers;
        callees = ranked_callees;
        omitted_callers = o_callers;
        omitted_callees = o_callees;
    }

    let cross_boundary = if query.cross_boundary {
        resolve_cross_boundary_view(&view, query.file_path, target_name)
    } else {
        None
    };

    let mut final_skeleton = skeleton;
    let mut unread_outline = None;

    if query.coverage == ReadCoverage::FullFile {
        final_skeleton.clear();
    } else if query.coverage == ReadCoverage::PartialMacro {
        unread_outline = build_unread_outline(raw_source, &target_defs, read_window);
    }

    Ok(DistillBundle {
        target: query.file_path.display().to_string(),
        skeleton: final_skeleton,
        contracts,
        callers,
        callees,
        token_reduction_pct: reduction_pct,
        cross_boundary,
        unread_outline,
        omitted_callers,
        omitted_callees,
    })
}

/// Distills a file or target symbol according to the requested DistillMode.
pub fn distill_target(
    target_path: &Path,
    mode: DistillMode,
    index: Option<&SymbolIndex>,
) -> Result<DistillBundle, DistillError> {
    distill_target_with_overlay(target_path, mode, index, None)
}

/// Distills a file or target symbol according to the requested DistillMode,
/// querying an optional overlay index before falling through to the base index.
pub fn distill_target_with_overlay(
    target_path: &Path,
    mode: DistillMode,
    base_index: Option<&SymbolIndex>,
    overlay_index: Option<&SymbolIndex>,
) -> Result<DistillBundle, DistillError> {
    distill_target_for_project(target_path, mode, base_index, overlay_index, None)
}

/// Distills a file or target symbol according to the requested DistillMode and optional
/// per-project options (`max_call_depth`, `include_private_types`).
pub fn distill_target_for_project(
    target_path: &Path,
    mode: DistillMode,
    base_index: Option<&SymbolIndex>,
    overlay_index: Option<&SymbolIndex>,
    project: Option<&cortx_core::ProjectConfig>,
) -> Result<DistillBundle, DistillError> {
    let view = OverlayView {
        base: base_index,
        overlay: overlay_index,
        shadowed_files: None,
    };
    let query = DistillQuery {
        file_path: target_path,
        mode,
        coverage: ReadCoverage::PartialMacro,
        span: None,
        preloaded_source: None,
        cross_boundary: true,
        project_cfg: project,
    };
    distill_with_query(&query, view)
}

fn resolve_or_cache_skeleton(
    query: &DistillQuery<'_>,
    view: &OverlayView<'_>,
    raw_source: &str,
    omit_tests: bool,
) -> Result<String, DistillError> {
    let mtime = if query.preloaded_source.is_none() {
        std::fs::metadata(query.file_path)
            .and_then(|m| m.modified())
            .ok()
    } else {
        None
    };

    if let Some(mtime) = mtime {
        if let Some(cached) = view.get_cached_skeleton(query.file_path, mtime) {
            return Ok(cached.to_string());
        }
    }

    let ext = query
        .file_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let skeleton = extract_source_skeleton(raw_source, ext, query.project_cfg, omit_tests)?;

    if let Some(mtime) = mtime {
        view.insert_cached_skeleton(
            query.file_path.to_path_buf(),
            mtime,
            Arc::from(skeleton.as_str()),
        );
    }

    Ok(skeleton)
}

fn extract_source_skeleton(
    raw_source: &str,
    ext: &str,
    project: Option<&cortx_core::ProjectConfig>,
    omit_tests: bool,
) -> Result<String, DistillError> {
    if matches!(ext, "ts" | "tsx" | "js" | "jsx") {
        let include_private = project.is_none_or(|p| p.distill.include_private_types);
        extract_ts_skeleton_with_options(raw_source, matches!(ext, "tsx" | "jsx"), include_private)
    } else if ext == "go" {
        let include_private =
            project.is_none_or(|p| p.go.include_private_types && p.distill.include_private_types);
        extract_go_skeleton_with_options(raw_source, include_private, omit_tests)
    } else {
        extract_skeleton_with_options(raw_source, omit_tests)
    }
}

pub(crate) fn is_test_path(path: &Path) -> bool {
    if let Some(file_name) = path.file_name().and_then(|f| f.to_str()) {
        if file_name.ends_with("_test.rs")
            || file_name.ends_with("_tests.rs")
            || file_name.ends_with("_test.go")
            || file_name == "tests.rs"
            || file_name == "test.rs"
            || file_name.ends_with(".test.ts")
            || file_name.ends_with(".test.tsx")
            || file_name.ends_with(".spec.ts")
            || file_name.ends_with(".spec.tsx")
        {
            return true;
        }
    }
    path.components().any(|c| c.as_os_str() == "tests")
}

fn is_test_symbol(sym: &str) -> bool {
    let leaf = sym.rsplit([':', '.']).next().unwrap_or(sym);
    sym.starts_with("tests::")
        || sym.starts_with("tests.")
        || sym.contains("::tests::")
        || sym.contains(".tests.")
        || is_go_test_function(leaf)
        || leaf.starts_with("test_")
        || leaf.starts_with("bench_")
}

fn is_test_call_edge(edge: &CallEdge) -> bool {
    is_test_path(Path::new(&edge.file))
        || is_test_symbol(&edge.caller)
        || is_test_symbol(&edge.callee)
}

pub(crate) fn path_matches(p1: &Path, p2: &Path) -> bool {
    p1 == p2 || p1.ends_with(p2) || p2.ends_with(p1)
}

fn collect_contracts_view(
    view: &OverlayView<'_>,
    target_path: &Path,
    target_defs: &[SymbolDef],
) -> Vec<ContractInfo> {
    let target_name = target_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("target");
    let mut contracts = view.get_contracts(target_name);
    let mut seen: HashSet<(String, String, String)> = contracts
        .iter()
        .map(|c| (c.trait_name.clone(), c.target_type.clone(), c.file.clone()))
        .collect();

    if let Some(sym_names) = view.get_file_symbols(target_path) {
        for sym in sym_names {
            for c in view.get_contracts(sym) {
                if seen.insert((c.trait_name.clone(), c.target_type.clone(), c.file.clone())) {
                    contracts.push(c);
                }
            }
        }
    }
    for def in target_defs {
        if !def.name.is_empty() {
            for c in view.get_contracts(&def.name) {
                if seen.insert((c.trait_name.clone(), c.target_type.clone(), c.file.clone())) {
                    contracts.push(c);
                }
            }
        }
    }
    contracts
}

fn collect_callers_view(
    view: &OverlayView<'_>,
    target_path: &Path,
    target_defs: &[SymbolDef],
    depth: usize,
) -> Vec<CallEdge> {
    let target_name = target_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("target");
    let mut callers = Vec::new();
    let mut seen = HashSet::new();
    view.collect_callers_into(target_name, depth, &mut callers, &mut seen);
    if let Some(sym_names) = view.get_file_symbols(target_path) {
        for sym in sym_names {
            view.collect_callers_into(sym, depth, &mut callers, &mut seen);
        }
    }
    for def in target_defs {
        if !def.name.is_empty() {
            view.collect_callers_into(&def.name, depth, &mut callers, &mut seen);
        }
        if !def.qualified_name.is_empty() && def.qualified_name != def.name {
            view.collect_callers_into(&def.qualified_name, depth, &mut callers, &mut seen);
        }
    }
    callers
}

fn collect_callees_view(
    view: &OverlayView<'_>,
    target_path: &Path,
    target_defs: &[SymbolDef],
    depth: usize,
) -> Vec<CallEdge> {
    let target_name = target_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("target");
    let mut callees = Vec::new();
    let mut seen = HashSet::new();
    view.collect_callees_into(target_name, depth, &mut callees, &mut seen);
    if let Some(sym_names) = view.get_file_symbols(target_path) {
        for sym in sym_names {
            view.collect_callees_into(sym, depth, &mut callees, &mut seen);
        }
    }
    for def in target_defs {
        if !def.name.is_empty() {
            view.collect_callees_into(&def.name, depth, &mut callees, &mut seen);
        }
        if !def.qualified_name.is_empty() && def.qualified_name != def.name {
            view.collect_callees_into(&def.qualified_name, depth, &mut callees, &mut seen);
        }
    }
    callees
}

fn normalize_edge_files(edges: &mut [CallEdge], ws_root: Option<&Path>) {
    for edge in edges {
        let p = Path::new(&edge.file);
        if let Some(root) = ws_root {
            if let Ok(rel) = p.strip_prefix(root) {
                edge.file = rel.to_string_lossy().to_string();
                continue;
            }
        }
        if let Ok(rel) = p.strip_prefix("./") {
            edge.file = rel.to_string_lossy().to_string();
        }
    }
}

fn distance_to_window(
    anchor_start: usize,
    anchor_end: usize,
    window: Option<(usize, usize)>,
) -> usize {
    let Some((w_start, w_end)) = window else {
        return 0;
    };
    if anchor_start == 0 {
        return usize::MAX;
    }
    if anchor_end < w_start {
        w_start - anchor_end
    } else {
        anchor_start.saturating_sub(w_end)
    }
}

fn rank_and_cap_callers(
    mut callers: Vec<CallEdge>,
    target_defs: &[SymbolDef],
    read_window: Option<(usize, usize)>,
) -> (Vec<CallEdge>, usize) {
    callers.sort_by(|a, b| {
        let key_a = caller_sort_key(a, target_defs, read_window);
        let key_b = caller_sort_key(b, target_defs, read_window);
        key_a.cmp(&key_b)
    });

    let omitted = if callers.len() > 32 {
        let count = callers.len() - 32;
        callers.truncate(32);
        count
    } else {
        0
    };
    (callers, omitted)
}

fn caller_sort_key<'a>(
    edge: &'a CallEdge,
    target_defs: &[SymbolDef],
    read_window: Option<(usize, usize)>,
) -> (usize, usize, &'a str, usize, &'a str, &'a str) {
    let def_match = target_defs.iter().find(|d| {
        d.name == edge.callee
            || d.qualified_name == edge.callee
            || edge.callee.ends_with(&format!("::{}", d.name))
            || edge.callee.ends_with(&format!(".{}", d.name))
    });

    let (hop_depth, anchor) = match def_match {
        Some(d) => (
            1,
            (
                d.line,
                if d.end_line >= d.line {
                    d.end_line
                } else {
                    d.line
                },
            ),
        ),
        None => (2, (0, 0)),
    };

    let dist = if anchor.0 == 0 {
        usize::MAX
    } else {
        distance_to_window(anchor.0, anchor.1, read_window)
    };

    let effective_call_line = if edge.call_line == 0 {
        usize::MAX
    } else {
        edge.call_line
    };

    (
        hop_depth,
        dist,
        edge.file.as_str(),
        effective_call_line,
        edge.caller.as_str(),
        edge.callee.as_str(),
    )
}

fn rank_and_cap_callees(
    mut callees: Vec<CallEdge>,
    target_defs: &[SymbolDef],
    read_window: Option<(usize, usize)>,
) -> (Vec<CallEdge>, usize) {
    let hop1_seeds: std::collections::HashMap<String, usize> = callees
        .iter()
        .filter(|e| {
            target_defs.iter().any(|d| {
                d.name == e.caller
                    || d.qualified_name == e.caller
                    || e.caller.ends_with(&format!("::{}", d.name))
                    || e.caller.ends_with(&format!(".{}", d.name))
            })
        })
        .map(|e| (e.callee.clone(), e.call_line))
        .collect();

    callees.sort_by(|a, b| {
        let key_a = callee_sort_key(a, target_defs, &hop1_seeds, read_window);
        let key_b = callee_sort_key(b, target_defs, &hop1_seeds, read_window);
        key_a.cmp(&key_b)
    });

    let omitted = if callees.len() > 32 {
        let count = callees.len() - 32;
        callees.truncate(32);
        count
    } else {
        0
    };
    (callees, omitted)
}

fn callee_sort_key<'a>(
    edge: &'a CallEdge,
    target_defs: &[SymbolDef],
    hop1_seeds: &std::collections::HashMap<String, usize>,
    read_window: Option<(usize, usize)>,
) -> (usize, usize, &'a str, usize, &'a str, &'a str) {
    let is_hop1 = target_defs.iter().any(|d| {
        d.name == edge.caller
            || d.qualified_name == edge.caller
            || edge.caller.ends_with(&format!("::{}", d.name))
            || edge.caller.ends_with(&format!(".{}", d.name))
    });

    let (hop_depth, anchor_line) = if is_hop1 {
        (1, edge.call_line)
    } else {
        let inherited = hop1_seeds.get(edge.caller.as_str()).copied().unwrap_or(0);
        (2, inherited)
    };

    let dist = if anchor_line == 0 {
        usize::MAX
    } else {
        distance_to_window(anchor_line, anchor_line, read_window)
    };

    let effective_call_line = if edge.call_line == 0 {
        usize::MAX
    } else {
        edge.call_line
    };

    (
        hop_depth,
        dist,
        edge.file.as_str(),
        effective_call_line,
        edge.caller.as_str(),
        edge.callee.as_str(),
    )
}

fn extract_defs_from_source(
    target_path: &Path,
    raw_source: &str,
    ext: &str,
    omit_tests: bool,
) -> Vec<SymbolDef> {
    if matches!(ext, "ts" | "tsx" | "js" | "jsx") {
        let opts = indexer::ts_graph::TsFactOptions {
            file_path: target_path,
            rel_path: target_path,
            package_name: None,
            omit_tests,
        };
        indexer::ts_graph::extract_ts_file_facts(&opts, raw_source)
            .map(|f| f.symbols)
            .unwrap_or_default()
    } else if ext == "go" {
        let opts = indexer::go_graph::GoFactOptions {
            file_path: target_path,
            rel_path: target_path,
            package_path: None,
            omit_tests,
        };
        indexer::go_graph::extract_go_file_facts(&opts, raw_source)
            .map(|f| f.symbols)
            .unwrap_or_default()
    } else if ext == "rs" {
        let opts = indexer::rust_graph::RustFactOptions {
            file_path: target_path,
            rel_path: target_path,
            crate_name: "",
            omit_tests,
        };
        indexer::rust_graph::extract_rust_file_facts(&opts, raw_source)
            .map(|f| f.symbols)
            .unwrap_or_default()
    } else {
        Vec::new()
    }
}

/// Slices the declaration signature directly from `source` starting at line `line` (1-indexed),
/// bounded to at most 40 lines.
pub fn extract_symbol_signature(source: &str, line: usize) -> &str {
    if line == 0 {
        return "";
    }
    let mut current_line = 1;
    let mut start_idx = None;
    for (idx, ch) in source.char_indices() {
        if current_line == line {
            start_idx = Some(idx);
            break;
        }
        if ch == '\n' {
            current_line += 1;
        }
    }
    let Some(start) = start_idx else {
        return "";
    };

    let remainder = &source[start..];
    let mut line_count = 0;
    let mut end_offset = remainder.len();
    let mut paren_depth: usize = 0;
    let mut bracket_depth: usize = 0;

    for (rel_idx, ch) in remainder.char_indices() {
        if ch == '\n' {
            line_count += 1;
            if line_count >= 40 {
                end_offset = rel_idx;
                break;
            }
        }
        match ch {
            '(' => paren_depth += 1,
            ')' => paren_depth = paren_depth.saturating_sub(1),
            '[' => bracket_depth += 1,
            ']' => bracket_depth = bracket_depth.saturating_sub(1),
            '{' if paren_depth == 0 && bracket_depth == 0 => {
                end_offset = rel_idx;
                break;
            }
            ';' if paren_depth == 0 && bracket_depth == 0 => {
                end_offset = rel_idx;
                break;
            }
            _ => {}
        }
    }

    remainder[..end_offset].trim()
}

fn build_unread_outline(
    raw_source: &str,
    target_defs: &[SymbolDef],
    window: Option<(usize, usize)>,
) -> Option<String> {
    let (start_l, end_l) = window?;
    let mut unread_defs: Vec<&SymbolDef> = target_defs
        .iter()
        .filter(|d| {
            let d_end = if d.end_line >= d.line {
                d.end_line
            } else {
                d.line
            };
            d.line > 0 && (d_end < start_l || d.line > end_l)
        })
        .collect();

    unread_defs.sort_by_key(|d| d.line);
    unread_defs.dedup_by_key(|d| d.line);

    let mut outline = String::new();
    for def in unread_defs {
        let sig = extract_symbol_signature(raw_source, def.line);
        if !sig.is_empty() {
            outline.push_str(&format!("L{}: {}\n", def.line, sig));
        }
    }

    if outline.is_empty() {
        None
    } else {
        Some(outline)
    }
}

fn def_matches_path(def_opt: &Option<String>, target_path: &Path) -> bool {
    let Some(def) = def_opt else { return false };
    let file_part = def.split(':').next().unwrap_or("");
    if file_part.is_empty() {
        return false;
    }
    path_matches(target_path, Path::new(file_part))
}

fn is_valid_cross_boundary(cb: &CrossBoundaryImpact, target_path: &Path) -> bool {
    let rust_matches_target = def_matches_path(&cb.rust_definition, target_path);
    let go_matches_target = def_matches_path(&cb.go_definition, target_path);
    let ts_matches_target = def_matches_path(&cb.ts_interface, target_path);

    let has_other_backend = (cb.rust_definition.is_some() && !rust_matches_target)
        || (cb.go_definition.is_some() && !go_matches_target);
    let has_other_ts = cb.ts_interface.is_some() && !ts_matches_target;

    let has_valid_backend = if rust_matches_target || go_matches_target {
        has_other_ts
    } else {
        cb.rust_definition.is_some() || cb.go_definition.is_some()
    };

    let has_valid_ts = if ts_matches_target {
        has_other_backend
    } else {
        cb.ts_interface.is_some()
    };

    (has_valid_backend && has_valid_ts) || !cb.api_call_sites.is_empty()
}

fn resolve_cross_boundary_view(
    view: &OverlayView<'_>,
    target_path: &Path,
    target_name: &str,
) -> Option<CrossBoundaryImpact> {
    let sym_stem = target_name
        .trim_end_matches(".rs")
        .trim_end_matches(".tsx")
        .trim_end_matches(".ts")
        .trim_end_matches(".jsx")
        .trim_end_matches(".js")
        .trim_end_matches(".go");

    if let Some(cb) = view.get_cross_boundary_impact(sym_stem) {
        if is_valid_cross_boundary(&cb, target_path) {
            return Some(cb);
        }
    }
    if let Some(cb) = view.get_cross_boundary_impact(target_name) {
        if is_valid_cross_boundary(&cb, target_path) {
            return Some(cb);
        }
    }

    if let Some(sym_names) = view.get_file_symbols(target_path) {
        for sym_name in sym_names {
            if let Some(cb) = view.get_cross_boundary_impact(sym_name) {
                if is_valid_cross_boundary(&cb, target_path) {
                    return Some(cb);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_distill_target_bundle() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
            pub struct Worker {{
                pub count: usize,
            }}

            impl Worker {{
                pub fn run(&self) {{
                    for i in 0..1000 {{
                        println!("Heavy iteration {{}}", i);
                    }}
                }}
            }}
            "#
        )
        .unwrap();

        let bundle = distill_target(file.path(), DistillMode::Bundle, None).unwrap();
        assert!(bundle.skeleton.contains("pub struct Worker"));
        assert!(bundle.skeleton.contains("pub fn run(&self)"));
        assert!(!bundle.skeleton.contains("Heavy iteration"));
        assert!(bundle.token_reduction_pct > 30.0);
    }

    #[test]
    fn test_distill_target_bundle_tsx() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("Component.tsx");
        std::fs::write(
            &file_path,
            r#"
            export interface Props { title: string; }
            export function Component(props: Props) {
                return <div>{props.title}</div>;
            }
            "#,
        )
        .unwrap();

        let bundle = distill_target(&file_path, DistillMode::Bundle, None).unwrap();
        assert!(bundle.skeleton.contains("export interface Props"));
        assert!(
            bundle
                .skeleton
                .contains("export function Component(props: Props): JSX.Element;")
        );
        let md = bundle.format_markdown();
        assert!(md.contains("```typescript"));
        assert!(bundle.token_reduction_pct > 0.0);
    }

    #[test]
    fn test_distill_target_bundle_with_cross_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let rs_path = dir.path().join("users.rs");
        let ts_path = dir.path().join("user.ts");

        std::fs::write(&rs_path, "pub struct UserPayload { pub id: u64, }").unwrap();
        std::fs::write(&ts_path, "export interface UserPayload { id: number; }").unwrap();

        let mut index = SymbolIndex::new();
        index
            .index_any_source_str(
                &rs_path.to_string_lossy(),
                "pub struct UserPayload { pub id: u64, }",
            )
            .unwrap();
        index
            .index_any_source_str(
                &ts_path.to_string_lossy(),
                "export interface UserPayload { id: number; }",
            )
            .unwrap();

        let bundle = distill_target(&rs_path, DistillMode::Bundle, Some(&index)).unwrap();
        assert!(bundle.cross_boundary.is_some());
        let md = bundle.format_markdown();
        assert!(md.contains("## Cross-Boundary Impact"));
        assert!(md.contains("Rust Backend Definition:"));
        assert!(md.contains("TypeScript Consumer Interface:"));
    }

    #[test]
    fn test_distill_target_bundle_go() {
        let dir = tempfile::tempdir().unwrap();
        let go_path = dir.path().join("user.go");
        let ts_path = dir.path().join("user.ts");

        std::fs::write(
            &go_path,
            r#"
            package model
            type UserPayload struct {
                ID uint64 `json:"user_id"`
            }
            func (u *UserPayload) Validate() bool {
                return u.ID > 0
            }
            "#,
        )
        .unwrap();

        std::fs::write(&ts_path, "export interface UserPayload { id: number; }").unwrap();

        let mut index = SymbolIndex::new();
        index.index_directory(dir.path()).unwrap();

        let bundle = distill_target(&go_path, DistillMode::Bundle, Some(&index)).unwrap();
        assert!(bundle.skeleton.contains("type UserPayload struct"));
        assert!(
            bundle
                .skeleton
                .contains("func (u *UserPayload) Validate() bool;")
        );
        assert!(!bundle.skeleton.contains("return u.ID > 0"));

        let md = bundle.format_markdown();
        assert!(md.contains("```go"));
        assert!(bundle.token_reduction_pct > 0.0);
        assert!(md.contains("## Cross-Boundary Impact"));
        assert!(md.contains("Go Backend Model:"));
    }

    #[test]
    fn test_distill_target_with_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let rs_path = dir.path().join("service.rs");

        // Write initial file and index into base
        std::fs::write(
            &rs_path,
            r#"
            pub struct Service;
            pub trait Handler { fn execute(&self); }
            impl Handler for Service { fn execute(&self) {} }
            pub fn run() { let s = Service; s.execute(); base_call(); }
            pub fn base_call() {}
            "#,
        )
        .unwrap();

        let mut base_index = SymbolIndex::new();
        base_index.index_directory(dir.path()).unwrap();

        // Modify file with new implementation and new callee
        std::fs::write(
            &rs_path,
            r#"
            pub struct Service;
            pub trait Handler { fn execute(&self); }
            impl Handler for Service { fn execute(&self) {} }
            pub fn run() { let s = Service; s.execute(); overlay_call(); }
            pub fn overlay_call() {}
            "#,
        )
        .unwrap();

        let overlay_index = base_index.create_overlay(std::slice::from_ref(&rs_path));

        // Distill with overlay
        let bundle = distill_target_with_overlay(
            &rs_path,
            DistillMode::Bundle,
            Some(&base_index),
            Some(&overlay_index),
        )
        .unwrap();

        assert!(bundle.skeleton.contains("pub struct Service"));
        assert!(bundle.skeleton.contains("pub fn overlay_call()"));

        // Overlay contracts fallback
        let contracts = overlay_index.get_contracts_with_fallback(Some(&base_index), "Service");
        assert!(!contracts.is_empty());
        assert_eq!(contracts[0].trait_name, "Handler");

        // Overridden callers in overlay take precedence
        assert!(!overlay_index.get_callers("overlay_call", 1).is_empty());
        assert!(base_index.get_callers("overlay_call", 1).is_empty());
        assert!(
            !overlay_index
                .get_callers_with_fallback(Some(&base_index), "base_call", 1)
                .is_empty()
        );
    }

    #[test]
    fn test_distill_target_for_project_options() {
        let dir = tempfile::tempdir().unwrap();
        let go_path = dir.path().join("handler.go");
        std::fs::write(
            &go_path,
            "package api\nfunc Exported() { helper() }\nfunc helper() {}\n",
        )
        .unwrap();

        let mut proj = cortx_core::ProjectConfig::default();
        proj.go.include_private_types = false;
        proj.distill.include_private_types = false;
        proj.distill.max_call_depth = 1;

        let bundle_no_priv =
            distill_target_for_project(&go_path, DistillMode::Bundle, None, None, Some(&proj))
                .unwrap();
        assert!(bundle_no_priv.skeleton.contains("func Exported()"));
        assert!(!bundle_no_priv.skeleton.contains("func helper()"));

        proj.go.include_private_types = true;
        proj.distill.include_private_types = true;
        let bundle_with_priv =
            distill_target_for_project(&go_path, DistillMode::Bundle, None, None, Some(&proj))
                .unwrap();
        assert!(bundle_with_priv.skeleton.contains("func helper()"));
    }

    #[test]
    fn test_ts_import_callers_and_no_self_referential_cross_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let utils_path = dir.path().join("utils.ts");
        let component_path = dir.path().join("Button.tsx");

        std::fs::write(
            &utils_path,
            r#"
            export function cn(...inputs: any[]) {
                return inputs.filter(Boolean).join(" ");
            }
            "#,
        )
        .unwrap();

        std::fs::write(
            &component_path,
            r#"
            import { cn } from "./utils";

            export function Button() {
                return <div className={cn("btn", "primary")}>Click</div>;
            }
            "#,
        )
        .unwrap();

        let mut index = SymbolIndex::new();
        index.index_directory(dir.path()).unwrap();

        let bundle = distill_target(&utils_path, DistillMode::Bundle, Some(&index)).unwrap();

        // 1. Must NOT emit self-referential cross-boundary impact
        assert!(
            bundle.cross_boundary.is_none(),
            "Pure TypeScript symbol must not emit cross-boundary impact"
        );

        // 2. Must list Button.tsx as an upstream caller
        assert!(!bundle.callers.is_empty(), "Must index callers of cn");
        assert_eq!(bundle.callers[0].callee, "cn");
        assert!(bundle.callers[0].caller.contains("Button.tsx"));

        // 3. Formatted markdown verification
        let md = bundle.format_markdown();
        assert!(!md.contains("## Cross-Boundary Impact"));
        assert!(md.contains("## Upstream Callers"));
        assert!(md.contains("calls `cn`"));
        assert!(md.contains("Button.tsx"));
    }

    #[test]
    fn test_path_matches_component_isolation() {
        assert!(path_matches(
            Path::new("/path/to/utils.ts"),
            Path::new("utils.ts")
        ));
        assert!(path_matches(
            Path::new("utils.ts"),
            Path::new("/path/to/utils.ts")
        ));
        assert!(!path_matches(
            Path::new("/path/to/other_utils.ts"),
            Path::new("utils.ts")
        ));
        assert!(!path_matches(
            Path::new("my_utils.ts"),
            Path::new("/path/to/utils.ts")
        ));
    }

    #[test]
    fn test_distill_target_call_graph_equivalence_and_ordering() {
        let dir = tempfile::tempdir().unwrap();
        let service_path = dir.path().join("service.rs");
        let caller1_path = dir.path().join("caller1.rs");
        let caller2_path = dir.path().join("caller2.rs");

        std::fs::write(
            &service_path,
            r#"
            pub fn target_fn() { internal_callee(); }
            pub fn internal_callee() {}
            pub fn other_fn() { internal_callee(); }
            "#,
        )
        .unwrap();

        std::fs::write(
            &caller1_path,
            r#"
            use crate::service::target_fn;
            pub fn run_1() { target_fn(); }
            "#,
        )
        .unwrap();

        std::fs::write(
            &caller2_path,
            r#"
            use crate::service::{target_fn, other_fn};
            pub fn run_2() { target_fn(); other_fn(); }
            "#,
        )
        .unwrap();

        let mut index = SymbolIndex::new();
        index.index_directory(dir.path()).unwrap();

        let bundle = distill_target(&service_path, DistillMode::Bundle, Some(&index)).unwrap();

        // Must preserve non-duplicated callers
        assert!(!bundle.callers.is_empty());
        let callers_set: HashSet<_> = bundle.callers.iter().collect();
        assert_eq!(
            callers_set.len(),
            bundle.callers.len(),
            "Callers must have zero duplicate edges"
        );

        // Callees must also have zero duplicates
        let callees_set: HashSet<_> = bundle.callees.iter().collect();
        assert_eq!(
            callees_set.len(),
            bundle.callees.len(),
            "Callees must have zero duplicate edges"
        );
    }

    #[test]
    fn test_distill_target_rust_omit_tests_default_true() {
        let dir = tempfile::tempdir().unwrap();
        let src_path = dir.path().join("service.rs");
        let test_caller_path = dir.path().join("service_test.rs");

        std::fs::write(
            &src_path,
            r#"
            pub fn compute(val: u32) -> u32 {
                val * 2
            }

            #[cfg(test)]
            mod tests {
                use super::*;

                #[test]
                fn test_compute() {
                    assert_eq!(compute(2), 4);
                }
            }
            "#,
        )
        .unwrap();

        std::fs::write(
            &test_caller_path,
            r#"
            use crate::service::compute;

            #[test]
            fn test_external_call() {
                compute(10);
            }
            "#,
        )
        .unwrap();

        let mut index = SymbolIndex::new();
        index
            .index_source_str_with_options(
                &src_path.to_string_lossy(),
                &std::fs::read_to_string(&src_path).unwrap(),
                false,
            )
            .unwrap();
        index
            .index_source_str_with_options(
                &test_caller_path.to_string_lossy(),
                &std::fs::read_to_string(&test_caller_path).unwrap(),
                false,
            )
            .unwrap();

        let proj = cortx_core::ProjectConfig::default();
        let bundle = distill_target_for_project(
            &src_path,
            DistillMode::Bundle,
            Some(&index),
            None,
            Some(&proj),
        )
        .unwrap();

        assert!(bundle.skeleton.contains("pub fn compute(val: u32) -> u32"));
        assert!(!bundle.skeleton.contains("mod tests"));
        assert!(!bundle.skeleton.contains("test_compute"));
        assert!(
            bundle.callers.is_empty(),
            "Test callers must be omitted when omit_tests=true"
        );
    }

    #[test]
    fn test_distill_target_rust_omit_tests_false() {
        let dir = tempfile::tempdir().unwrap();
        let src_path = dir.path().join("service.rs");
        let test_caller_path = dir.path().join("service_test.rs");

        std::fs::write(
            &src_path,
            r#"
            pub fn compute(val: u32) -> u32 {
                val * 2
            }

            #[cfg(test)]
            mod tests {
                use super::*;

                #[test]
                fn test_compute() {
                    assert_eq!(compute(2), 4);
                }
            }
            "#,
        )
        .unwrap();

        std::fs::write(
            &test_caller_path,
            r#"
            use crate::service::compute;

            #[test]
            fn test_external_call() {
                compute(10);
            }
            "#,
        )
        .unwrap();

        let mut index = SymbolIndex::new();
        index
            .index_source_str_with_options(
                &src_path.to_string_lossy(),
                &std::fs::read_to_string(&src_path).unwrap(),
                false,
            )
            .unwrap();
        index
            .index_source_str_with_options(
                &test_caller_path.to_string_lossy(),
                &std::fs::read_to_string(&test_caller_path).unwrap(),
                false,
            )
            .unwrap();

        let mut proj = cortx_core::ProjectConfig::default();
        proj.distill.omit_tests = false;
        proj.omit_tests = false;

        let bundle = distill_target_for_project(
            &src_path,
            DistillMode::Bundle,
            Some(&index),
            None,
            Some(&proj),
        )
        .unwrap();

        assert!(bundle.skeleton.contains("pub fn compute(val: u32) -> u32"));
        assert!(bundle.skeleton.contains("mod tests"));
        assert!(bundle.skeleton.contains("fn test_compute()"));
        assert!(
            !bundle.callers.is_empty(),
            "Test callers must be retained when omit_tests=false"
        );
    }

    #[test]
    fn test_distill_target_go_omit_tests() {
        let dir = tempfile::tempdir().unwrap();
        let go_path = dir.path().join("service.go");

        std::fs::write(
            &go_path,
            r#"
            package service

            func Process(data string) bool {
                return len(data) > 0
            }

            func TestProcess(t *testing.T) {
                if !Process("hello") {
                    t.Fail()
                }
            }

            func BenchmarkProcess(b *testing.B) {
                for i := 0; i < b.N; i++ {
                    Process("bench")
                }
            }
            "#,
        )
        .unwrap();

        let mut proj = cortx_core::ProjectConfig::default();
        proj.distill.omit_tests = true;

        let bundle_omitted =
            distill_target_for_project(&go_path, DistillMode::Bundle, None, None, Some(&proj))
                .unwrap();

        assert!(
            bundle_omitted
                .skeleton
                .contains("func Process(data string) bool;")
        );
        assert!(!bundle_omitted.skeleton.contains("TestProcess"));
        assert!(!bundle_omitted.skeleton.contains("BenchmarkProcess"));

        proj.distill.omit_tests = false;
        let bundle_retained =
            distill_target_for_project(&go_path, DistillMode::Bundle, None, None, Some(&proj))
                .unwrap();

        assert!(
            bundle_retained
                .skeleton
                .contains("func Process(data string) bool;")
        );
        assert!(
            bundle_retained
                .skeleton
                .contains("func TestProcess(t *testing.T);")
        );
        assert!(
            bundle_retained
                .skeleton
                .contains("func BenchmarkProcess(b *testing.B);")
        );
    }

    #[test]
    fn test_distill_target_direct_test_file_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let tests_dir = dir.path().join("tests");
        std::fs::create_dir_all(&tests_dir).unwrap();
        let test_file = tests_dir.join("integration_test.rs");

        std::fs::write(
            &test_file,
            r#"
            #[test]
            fn it_works() {
                assert_eq!(1 + 1, 2);
            }
            "#,
        )
        .unwrap();

        let bundle = distill_target(&test_file, DistillMode::Bundle, None).unwrap();
        assert!(bundle.skeleton.contains("fn it_works()"));
    }

    #[test]
    fn test_extract_symbol_signature_polyglot() {
        let rust_src = "pub fn run(&self) {\n    println!(\"hi\");\n}\npub fn multi(\n    a: i32,\n) -> i32 {\n    a\n}";
        assert_eq!(extract_symbol_signature(rust_src, 1), "pub fn run(&self)");
        assert_eq!(
            extract_symbol_signature(rust_src, 4),
            "pub fn multi(\n    a: i32,\n) -> i32"
        );
        assert_eq!(extract_symbol_signature(rust_src, 0), "");
        assert_eq!(extract_symbol_signature(rust_src, 99), "");

        let ts_src = "export async function fetchData(url: string): Promise<Response> {\n    return fetch(url);\n}\nexport function opt(opts: { flag: boolean }): void {\n}";
        assert_eq!(
            extract_symbol_signature(ts_src, 1),
            "export async function fetchData(url: string): Promise<Response>"
        );
        assert_eq!(
            extract_symbol_signature(ts_src, 4),
            "export function opt(opts: { flag: boolean }): void"
        );

        let go_src = "func (s *Service) Run(ctx context.Context) error {\n    return nil\n}";
        assert_eq!(
            extract_symbol_signature(go_src, 1),
            "func (s *Service) Run(ctx context.Context) error"
        );
    }

    #[test]
    fn test_is_test_symbol_polyglot() {
        assert!(is_test_symbol("my_crate::tests::test_foo"));
        assert!(is_test_symbol("my_crate::foo::tests::bar"));
        assert!(is_test_symbol("test_handler"));
        assert!(is_test_symbol("bench_handler"));
        assert!(is_test_symbol("github.com/org/pkg.TestProcess"));
        assert!(is_test_symbol("github.com/org/pkg.BenchmarkProcess"));

        assert!(!is_test_symbol("my_crate::service::process"));
        assert!(!is_test_symbol("github.com/org/pkg.Process"));
        assert!(!is_test_symbol("UserHandler"));
    }

    #[test]
    fn test_distill_with_query_full_file_omits_skeleton() {
        let dir = tempfile::tempdir().unwrap();
        let rs_path = dir.path().join("leaf.rs");
        std::fs::write(&rs_path, "pub fn leaf() {}").unwrap();

        let query = DistillQuery {
            file_path: &rs_path,
            mode: DistillMode::Bundle,
            coverage: ReadCoverage::FullFile,
            span: None,
            preloaded_source: None,
            cross_boundary: false,
            project_cfg: None,
        };

        let bundle = distill_with_query(&query, OverlayView::empty()).unwrap();
        assert!(bundle.skeleton.is_empty());
        assert!(bundle.is_empty());
        assert_eq!(bundle.format_markdown(), "");
        assert_eq!(bundle.format_text(), "");
    }

    #[test]
    fn test_distill_with_query_partial_macro_unread_outline() {
        let dir = tempfile::tempdir().unwrap();
        let rs_path = dir.path().join("multi.rs");
        let src = r#"pub fn head() {
    println!("1");
}

pub fn mid() {
    println!("2");
}

pub fn tail() {
    println!("3");
}
"#;
        std::fs::write(&rs_path, src).unwrap();

        let mut idx = SymbolIndex::new();
        idx.index_any_source_str(&rs_path.to_string_lossy(), src)
            .unwrap();
        let view = OverlayView {
            base: Some(&idx),
            overlay: None,
            shadowed_files: None,
        };

        let span = ReadSpan {
            start_line: Some(5),
            end_line: Some(8),
            ..Default::default()
        };

        let query = DistillQuery {
            file_path: &rs_path,
            mode: DistillMode::Bundle,
            coverage: ReadCoverage::PartialMacro,
            span: Some(span),
            preloaded_source: None,
            cross_boundary: false,
            project_cfg: None,
        };

        let bundle = distill_with_query(&query, view).unwrap();
        assert!(!bundle.skeleton.is_empty());
        let outline = bundle.unread_outline.expect("outline should be generated");
        assert!(outline.contains("L1: pub fn head()"));
        assert!(outline.contains("L9: pub fn tail()"));
        assert!(!outline.contains("mid"));
    }

    #[test]
    fn test_distill_with_query_proximity_ranking() {
        let dir = tempfile::tempdir().unwrap();
        let svc_path = dir.path().join("service.rs");
        let caller1_path = dir.path().join("caller1.rs");
        let caller2_path = dir.path().join("caller2.rs");

        let src = r#"pub fn alpha() {}

pub fn omega() {}
"#;
        std::fs::write(&svc_path, src).unwrap();

        let mut idx = SymbolIndex::new();
        idx.index_any_source_str(&svc_path.to_string_lossy(), src)
            .unwrap();
        idx.add_call_edge(CallEdge {
            caller: "call_alpha".into(),
            callee: "alpha".into(),
            file: caller1_path.to_string_lossy().to_string(),
            call_line: 10,
        });
        idx.add_call_edge(CallEdge {
            caller: "call_omega".into(),
            callee: "omega".into(),
            file: caller2_path.to_string_lossy().to_string(),
            call_line: 20,
        });

        let view = OverlayView {
            base: Some(&idx),
            overlay: None,
            shadowed_files: None,
        };

        // Window close to omega (line 3)
        let span = ReadSpan {
            start_line: Some(3),
            end_line: Some(5),
            ..Default::default()
        };

        let query = DistillQuery {
            file_path: &svc_path,
            mode: DistillMode::Bundle,
            coverage: ReadCoverage::PartialMacro,
            span: Some(span),
            preloaded_source: None,
            cross_boundary: false,
            project_cfg: None,
        };

        let bundle = distill_with_query(&query, view).unwrap();
        assert_eq!(bundle.callers.len(), 2);
        // call_omega should be ranked first because omega is at line 3, inside the read window
        assert_eq!(bundle.callers[0].callee, "omega");
        assert_eq!(bundle.callers[1].callee, "alpha");
    }

    #[test]
    fn test_distill_with_query_micro_read() {
        let dir = tempfile::tempdir().unwrap();
        let rs_path = dir.path().join("file.rs");
        std::fs::write(&rs_path, "pub fn foo() {}").unwrap();

        let query = DistillQuery {
            file_path: &rs_path,
            mode: DistillMode::Bundle,
            coverage: ReadCoverage::Micro,
            span: None,
            preloaded_source: None,
            cross_boundary: false,
            project_cfg: None,
        };

        let bundle = distill_with_query(&query, OverlayView::empty()).unwrap();
        assert!(bundle.is_empty());
        assert_eq!(bundle.format_markdown(), "");
    }
}
