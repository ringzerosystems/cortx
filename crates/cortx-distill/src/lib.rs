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

pub use bundle::{BudgetStage, BudgetedAdvisory, DistillBundle, decide_stage};
pub use error::DistillError;
pub use go_skeleton::{
    GoIndexedData, extract_go_file_skeleton, extract_go_file_skeleton_with_options,
    extract_go_skeleton, extract_go_skeleton_with_options, has_go_test_signature, index_go_ast,
    index_go_ast_with_options, is_go_test_function,
};
pub use indexer::{
    CallEdge, CompactCallEdge, ContractInfo, CrossBoundaryImpact, RouteBinding, StringPool,
    SymbolDef, SymbolId, SymbolIndex, SymbolIndexMetrics, WorkspaceManifests, is_ignored_path,
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
use std::collections::HashSet;
use std::path::Path;

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
    let raw_source = std::fs::read_to_string(target_path).map_err(DistillError::Io)?;
    let raw_len = raw_source.len().max(1);

    let ext = target_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    let omit_tests = project.is_none_or(|p| p.distill.omit_tests);
    let is_target_test = is_test_path(target_path);
    let effective_omit_tests = if is_target_test { false } else { omit_tests };

    let skeleton = extract_source_skeleton(&raw_source, ext, project, effective_omit_tests)?;
    let skeleton_len = skeleton.len();

    let reduction_pct = if raw_len > skeleton_len {
        ((raw_len - skeleton_len) as f64 / raw_len as f64) * 100.0
    } else {
        0.0
    };

    let target_name = target_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("target");
    let call_depth = project.map_or(2, |p| p.distill.max_call_depth);

    let mut contracts = Vec::new();
    let mut callers = Vec::new();
    let mut callees = Vec::new();

    if overlay_index.is_some() || base_index.is_some() {
        if matches!(mode, DistillMode::Contracts | DistillMode::Bundle) {
            let ov_contracts = overlay_index
                .map(|idx| idx.get_contracts(target_name))
                .unwrap_or_default();
            contracts = if !ov_contracts.is_empty() {
                ov_contracts
            } else {
                base_index
                    .map(|idx| idx.get_contracts(target_name))
                    .unwrap_or_default()
            };
        }
        if matches!(mode, DistillMode::Hierarchy | DistillMode::Bundle) {
            let ov_callers = overlay_index
                .map(|idx| collect_callers(idx, target_path, call_depth, effective_omit_tests))
                .unwrap_or_default();
            callers = if !ov_callers.is_empty() {
                ov_callers
            } else {
                base_index
                    .map(|idx| collect_callers(idx, target_path, call_depth, effective_omit_tests))
                    .unwrap_or_default()
            };

            let ov_callees = overlay_index
                .map(|idx| collect_callees(idx, target_path, call_depth, effective_omit_tests))
                .unwrap_or_default();
            callees = if !ov_callees.is_empty() {
                ov_callees
            } else {
                base_index
                    .map(|idx| collect_callees(idx, target_path, call_depth, effective_omit_tests))
                    .unwrap_or_default()
            };
        }
    }

    let cross_boundary = overlay_index
        .and_then(|ov| resolve_cross_boundary(ov, target_path, target_name))
        .or_else(|| {
            base_index.and_then(|base| resolve_cross_boundary(base, target_path, target_name))
        });

    Ok(DistillBundle {
        target: target_path.display().to_string(),
        skeleton,
        contracts,
        callers,
        callees,
        token_reduction_pct: reduction_pct,
        cross_boundary,
    })
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
    let leaf = sym.rsplit("::").next().unwrap_or(sym);
    sym.starts_with("tests::")
        || sym.contains("::tests::")
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

fn collect_callers(
    idx: &SymbolIndex,
    target_path: &Path,
    depth: usize,
    omit_tests: bool,
) -> Vec<CallEdge> {
    let target_name = target_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("target");
    let mut callers = idx.get_callers(target_name, depth);
    let mut seen: HashSet<CallEdge> = callers.iter().cloned().collect();
    if let Some(sym_names) = idx.get_file_symbols(target_path) {
        for sym in sym_names {
            idx.collect_callers_into(sym, depth, &mut callers, &mut seen);
        }
    }
    if omit_tests {
        callers.retain(|edge| !is_test_call_edge(edge));
    }
    callers
}

fn collect_callees(
    idx: &SymbolIndex,
    target_path: &Path,
    depth: usize,
    omit_tests: bool,
) -> Vec<CallEdge> {
    let target_name = target_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("target");
    let mut callees = idx.get_callees(target_name, depth);
    let mut seen: HashSet<CallEdge> = callees.iter().cloned().collect();
    if let Some(sym_names) = idx.get_file_symbols(target_path) {
        for sym in sym_names {
            idx.collect_callees_into(sym, depth, &mut callees, &mut seen);
        }
    }
    if omit_tests {
        callees.retain(|edge| !is_test_call_edge(edge));
    }
    callees
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

fn resolve_cross_boundary(
    idx: &SymbolIndex,
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

    if let Some(cb) = idx.get_cross_boundary_impact(sym_stem) {
        if is_valid_cross_boundary(&cb, target_path) {
            return Some(cb);
        }
    }
    if let Some(cb) = idx.get_cross_boundary_impact(target_name) {
        if is_valid_cross_boundary(&cb, target_path) {
            return Some(cb);
        }
    }

    if let Some(sym_names) = idx.get_file_symbols(target_path) {
        for sym_name in sym_names {
            if let Some(cb) = idx.get_cross_boundary_impact(sym_name) {
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
        assert!(md.contains("Token Reduction:"));
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
        assert!(md.contains("Token Reduction:"));
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
}
