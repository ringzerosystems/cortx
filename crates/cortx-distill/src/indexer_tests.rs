use super::*;

#[test]
fn test_indexing_calls_and_contracts() {
    let code = r#"
            pub trait Engine {
                fn run(&self);
            }

            pub struct Rocket;

            impl Engine for Rocket {
                fn run(&self) {
                    ignite();
                }
            }

            fn ignite() {
                println!("Ignition");
            }
        "#;

    let mut index = SymbolIndex::new();
    index.index_source_str("rocket.rs", code).unwrap();

    let callers = index.get_callers("ignite", 1);
    assert!(!callers.is_empty());
    assert_eq!(callers[0].caller, "run");

    let contracts = index.get_contracts("Rocket");
    assert_eq!(contracts.len(), 1);
    assert_eq!(contracts[0].trait_name, "Engine");
}

#[test]
fn test_cross_boundary_impact_indexing() {
    let rust_code = r#"
            pub struct UpdateUserPayload {
                pub user_id: String,
            }

            #[tauri::command]
            pub fn update_user(payload: UpdateUserPayload) {}
        "#;

    let ts_types = r#"
            export interface UpdateUserPayload {
                userId: string;
            }
        "#;

    let ts_client = r#"
            export async function updateUser(payload: UpdateUserPayload) {
                return fetch("/api/users", { method: "POST" });
            }
        "#;

    let mut index = SymbolIndex::new();
    index
        .index_any_source_str("crates/server/src/routes/users.rs", rust_code)
        .unwrap();
    index
        .index_any_source_str("frontend/src/types/user.ts", ts_types)
        .unwrap();
    index
        .index_any_source_str("frontend/src/api/userClient.ts", ts_client)
        .unwrap();

    let impact = index
        .get_cross_boundary_impact("UpdateUserPayload")
        .expect("impact must be found");
    assert_eq!(impact.symbol, "UpdateUserPayload");
    assert!(
        impact
            .rust_definition
            .as_ref()
            .unwrap()
            .contains("crates/server/src/routes/users.rs")
    );
    assert!(
        impact
            .ts_interface
            .as_ref()
            .unwrap()
            .contains("frontend/src/types/user.ts")
    );
    assert!(!impact.api_call_sites.is_empty());
    assert!(impact.api_call_sites[0].contains("userClient.ts"));
    assert!(impact.api_call_sites[0].contains("updateUser"));
}

#[test]
fn test_metrics_empty() {
    let index = SymbolIndex::new();
    let m = index.metrics();
    assert_eq!(m.distinct_files_count, 0);
    assert_eq!(m.total_symbols_count, 0);
    assert_eq!(m.total_call_edges_count, 0);
    assert_eq!(m.total_contracts_count, 0);
    assert_eq!(m.total_routes_count, 0);
}

#[test]
fn test_metrics_populated() {
    let mut index = SymbolIndex::new();
    let rust_code = r#"
            pub struct Engine {
                pub id: u64,
            }
            pub trait Runner {
                fn run(&self);
            }
            impl Runner for Engine {
                fn run(&self) {}
            }
            pub fn start_all() {
                let e = Engine { id: 1 };
                e.run();
            }
        "#;
    index
        .index_any_source_str("src/engine.rs", rust_code)
        .unwrap();
    let m = index.metrics();
    assert_eq!(m.distinct_files_count, 1);
    assert!(m.total_symbols_count >= 3);
    assert!(m.total_contracts_count >= 1);
    assert!(m.total_call_edges_count >= 1);
}

#[test]
fn test_is_ignored_path() {
    // Ignored relative paths
    assert!(is_ignored_path(Path::new("target/debug/foo.rs")));
    assert!(is_ignored_path(Path::new("node_modules/pkg/index.js")));
    assert!(is_ignored_path(Path::new(".git/HEAD")));
    assert!(is_ignored_path(Path::new("vendor/pkg/foo.go")));
    assert!(is_ignored_path(Path::new("dist/bundle.js")));
    assert!(is_ignored_path(Path::new("build/output.js")));
    assert!(is_ignored_path(Path::new("out/main.js")));
    assert!(is_ignored_path(Path::new(".next/server.js")));
    assert!(is_ignored_path(Path::new(".nuxt/index.js")));
    assert!(is_ignored_path(Path::new(".turbo/cache.bin")));
    assert!(is_ignored_path(Path::new(".yarn/releases.cjs")));
    assert!(is_ignored_path(Path::new(".pnpm-store/v3.bin")));
    assert!(is_ignored_path(Path::new(".output/server/index.mjs")));
    assert!(is_ignored_path(Path::new(".svelte-kit/generated.js")));
    assert!(is_ignored_path(Path::new("coverage/lcov.info")));
    assert!(is_ignored_path(Path::new(".idea/workspace.xml")));
    assert!(is_ignored_path(Path::new(".vscode/settings.json")));
    assert!(!is_ignored_path(Path::new(".worktrees/feat1/src/lib.rs")));

    // Ignored nested subdirectories and absolute paths
    assert!(is_ignored_path(Path::new(
        "/home/user/project/node_modules/foo/index.ts"
    )));
    assert!(is_ignored_path(Path::new(
        "/home/user/project/target/release/lib.rs"
    )));
    assert!(is_ignored_path(Path::new("frontend/nested/vendor/dep.go")));

    // Valid paths that should NOT be ignored
    assert!(!is_ignored_path(Path::new("src/main.rs")));
    assert!(!is_ignored_path(Path::new(
        "crates/cortx-distill/src/indexer.rs"
    )));
    assert!(!is_ignored_path(Path::new(
        "/var/workspace/app/target_metrics.rs"
    )));
    assert!(!is_ignored_path(Path::new("src/validate_target.ts")));
    assert!(!is_ignored_path(Path::new("src/build_helper.rs")));
}

#[test]
fn test_symbol_index_merge() {
    let mut idx1 = SymbolIndex::new();
    idx1.index_source_str(
        "src/a.rs",
        "pub fn alpha() { beta(); }\npub fn beta() {}\npub struct StructA;",
    )
    .unwrap();

    let mut idx2 = SymbolIndex::new();
    idx2.index_source_str(
        "src/b.rs",
        "pub fn gamma() { delta(); }\npub fn delta() {}\npub struct StructB;",
    )
    .unwrap();

    let mut merged = idx1.clone();
    merged.merge(idx2.clone());

    let m1 = idx1.metrics();
    let m2 = idx2.metrics();
    let mm = merged.metrics();

    assert_eq!(
        mm.distinct_files_count,
        m1.distinct_files_count + m2.distinct_files_count
    );
    assert_eq!(
        mm.total_symbols_count,
        m1.total_symbols_count + m2.total_symbols_count
    );
    assert_eq!(
        mm.total_call_edges_count,
        m1.total_call_edges_count + m2.total_call_edges_count
    );
    assert!(merged.symbols_map.contains_key("StructA"));
    assert!(merged.symbols_map.contains_key("StructB"));
    assert!(!merged.get_callers("beta", 1).is_empty());
    assert!(!merged.get_callers("delta", 1).is_empty());
}

#[test]
fn test_parallel_indexing_equivalence() {
    let temp_dir = tempfile::tempdir().unwrap();
    let root = temp_dir.path();

    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(
        src_dir.join("lib.rs"),
        "pub fn compute() { helper(); }\npub fn helper() {}\npub struct Engine;",
    )
    .unwrap();
    std::fs::write(
            src_dir.join("client.ts"),
            "export interface User { id: string; }\nexport function getUser() { return fetch(\"/api/user\"); }",
        )
        .unwrap();
    std::fs::write(
        src_dir.join("service.go"),
        "package service\ntype Handler struct {}\nfunc (h Handler) Serve() {}",
    )
    .unwrap();

    let nm_dir = root.join("node_modules").join("dummy");
    std::fs::create_dir_all(&nm_dir).unwrap();
    std::fs::write(nm_dir.join("index.js"), "module.exports = {};").unwrap();

    let target_dir = root.join("target").join("debug");
    std::fs::create_dir_all(&target_dir).unwrap();
    std::fs::write(target_dir.join("gen.rs"), "pub fn generated() {}").unwrap();

    let mut seq_idx = SymbolIndex::new();
    let seq_count = seq_idx.index_directory_sequential(root).unwrap();

    let mut par_idx = SymbolIndex::new();
    let par_count = par_idx.index_directory(root).unwrap();

    assert_eq!(seq_count, 3);
    assert_eq!(par_count, 3);

    let seq_metrics = seq_idx.metrics();
    let par_metrics = par_idx.metrics();

    assert_eq!(
        seq_metrics.distinct_files_count,
        par_metrics.distinct_files_count
    );
    assert_eq!(
        seq_metrics.total_symbols_count,
        par_metrics.total_symbols_count
    );
    assert_eq!(
        seq_metrics.total_call_edges_count,
        par_metrics.total_call_edges_count
    );
    assert_eq!(
        seq_metrics.total_contracts_count,
        par_metrics.total_contracts_count
    );
    assert_eq!(
        seq_metrics.total_routes_count,
        par_metrics.total_routes_count
    );

    let seq_callers = seq_idx.get_callers("helper", 1);
    let par_callers = par_idx.get_callers("helper", 1);
    assert_eq!(seq_callers, par_callers);
    assert!(!par_callers.is_empty());
    assert_eq!(par_callers[0].caller, "compute");

    let seq_callees = seq_idx.get_callees("compute", 1);
    let par_callees = par_idx.get_callees("compute", 1);
    assert_eq!(seq_callees, par_callees);
    assert!(!par_callees.is_empty());
    assert_eq!(par_callees[0].callee, "helper");

    assert!(!par_idx.symbols_map.contains_key("generated"));
    assert!(!seq_idx.symbols_map.contains_key("generated"));
}

#[test]
fn test_estimate_memory_mb() {
    let empty_idx = SymbolIndex::new();
    assert!(empty_idx.estimate_memory_mb() > 0.0);

    let mut populated_idx = SymbolIndex::new();
    let rust_code = r#"
            pub struct Engine { pub id: u64 }
            pub trait Runner { fn run(&self); }
            impl Runner for Engine { fn run(&self) {} }
            pub fn start() { let e = Engine { id: 1 }; e.run(); }
        "#;
    populated_idx
        .index_any_source_str("src/engine.rs", rust_code)
        .unwrap();

    assert!(populated_idx.estimate_memory_mb() > empty_idx.estimate_memory_mb());
}

#[test]
fn test_nested_worktree_auto_pruning() {
    let temp_dir = tempfile::tempdir().unwrap();
    let root = temp_dir.path();

    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("main.rs"), "pub fn main() {}").unwrap();

    // Subdirectory representing a nested worktree with a .git file
    let wt_dir = root.join("worktree_a");
    std::fs::create_dir_all(wt_dir.join("src")).unwrap();
    std::fs::write(wt_dir.join(".git"), "gitdir: /path/to/commondir").unwrap();
    std::fs::write(wt_dir.join("src").join("sub.rs"), "pub fn sub() {}").unwrap();

    // Subdirectory matching worktrees_dir
    let custom_wt = root.join(".claude").join("worktrees").join("feat1");
    std::fs::create_dir_all(custom_wt.join("src")).unwrap();
    std::fs::write(custom_wt.join(".git"), "gitdir: /another/path").unwrap();
    std::fs::write(custom_wt.join("src").join("feature.rs"), "pub fn feat() {}").unwrap();

    // Custom ignored directory
    let ign_dir = root.join("ignored_folder").join("src");
    std::fs::create_dir_all(&ign_dir).unwrap();
    std::fs::write(ign_dir.join("ign.rs"), "pub fn ign() {}").unwrap();

    let idx = SymbolIndex::new();
    let files = idx.collect_source_files_with_options(
        root,
        &["ignored_folder".to_string()],
        Some(".claude/worktrees"),
    );

    // Only main.rs must be collected
    assert_eq!(files.len(), 1);
    assert_eq!(files[0], src_dir.join("main.rs"));

    // Default collect_source_files also prunes nested .git directories/files
    let default_files = idx.collect_source_files(root);
    assert!(!default_files.iter().any(|p| p.ends_with("sub.rs")));
    assert!(!default_files.iter().any(|p| p.ends_with("feature.rs")));
}

#[test]
fn test_create_overlay_and_fallback() {
    let temp_dir = tempfile::tempdir().unwrap();
    let file_path = temp_dir.path().join("service.rs");

    std::fs::write(
        &file_path,
        "pub fn handle() { base_handler(); }\npub struct ServiceConfig { pub port: u16, }",
    )
    .unwrap();

    let mut base_idx = SymbolIndex::new();
    base_idx
        .index_source_str(
            &file_path.to_string_lossy(),
            &std::fs::read_to_string(&file_path).unwrap(),
        )
        .unwrap();

    // Modify file on disk with new symbol definition and callers
    std::fs::write(
            &file_path,
            "pub fn handle() { overlay_handler(); }\npub struct ServiceConfig { pub port: u32, }\npub struct NewStruct;",
        )
        .unwrap();

    let overlay_idx = base_idx.create_overlay(std::slice::from_ref(&file_path));

    // Overlay index overrides base symbols
    let defs = overlay_idx
        .get_symbol_fallback(Some(&base_idx), "ServiceConfig")
        .expect("ServiceConfig must be found");
    assert_eq!(defs.len(), 1);

    // Overlay has NewStruct which base does not
    assert!(overlay_idx.symbols_map.contains_key("NewStruct"));
    assert!(!base_idx.symbols_map.contains_key("NewStruct"));

    // Non-existent symbol returns None
    assert!(
        overlay_idx
            .get_symbol_fallback(Some(&base_idx), "NonExistent")
            .is_none()
    );

    // Callers lookup with fallback
    assert!(
        !overlay_idx
            .get_callers_with_fallback(Some(&base_idx), "overlay_handler", 1)
            .is_empty()
    );

    // Verify file_to_symbols mapping
    let file_key = file_path.to_string_lossy().to_string();
    assert!(overlay_idx.file_to_symbols.contains_key(&file_key));
    let syms = &overlay_idx.file_to_symbols[&file_key];
    assert!(syms.contains(&"NewStruct".to_string()));
    assert!(syms.contains(&"ServiceConfig".to_string()));
}

#[test]
fn test_nested_worktree_and_project_config_indexing() {
    let temp_dir = tempfile::tempdir().unwrap();
    let root = temp_dir.path();

    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "pub fn parent_main() {}").unwrap();

    // 1. Nested worktree inside .worktrees/wt-1
    let wt1 = root.join(".worktrees").join("wt-1");
    std::fs::create_dir_all(wt1.join("src")).unwrap();
    std::fs::write(wt1.join(".git"), "gitdir: /path/to/git/worktrees/wt-1").unwrap();
    std::fs::write(wt1.join("src/wt1.rs"), "pub fn wt1_fn() {}").unwrap();

    assert!(!is_ignored_path(&wt1.join("src/wt1.rs")));
    let idx = SymbolIndex::new();
    let parent_files = idx.collect_source_files(root);
    assert_eq!(parent_files, vec![root.join("src/main.rs")]);
    let wt1_files = idx.collect_source_files(&wt1);
    assert_eq!(wt1_files, vec![wt1.join("src/wt1.rs")]);

    // 2. Nested worktree inside .claude/worktrees/wt-2 with ignored_dirs = [".claude"]
    let wt2 = root.join(".claude").join("worktrees").join("wt-2");
    std::fs::create_dir_all(wt2.join("src")).unwrap();
    std::fs::write(wt2.join(".git"), "gitdir: /path/to/git/worktrees/wt-2").unwrap();
    std::fs::write(wt2.join("src/wt2.rs"), "pub fn wt2_fn() {}").unwrap();
    std::fs::write(wt2.join("src/ui.ts"), "export const x = 1;").unwrap();
    std::fs::write(wt2.join("src/svc.go"), "package main\nfunc Run() {}").unwrap();

    // Internal target/ and .claude/ inside the worktree must still be pruned
    std::fs::create_dir_all(wt2.join("target/debug")).unwrap();
    std::fs::write(wt2.join("target/debug/gen.rs"), "pub fn gen() {}").unwrap();
    std::fs::create_dir_all(wt2.join(".claude")).unwrap();
    std::fs::write(wt2.join(".claude/cache.rs"), "pub fn cache() {}").unwrap();
    std::fs::create_dir_all(wt2.join("custom_ign")).unwrap();
    std::fs::write(wt2.join("custom_ign/skip.rs"), "pub fn skip() {}").unwrap();

    let proj = cortx_core::ProjectConfig {
        qualified_path: root.to_path_buf(),
        has_worktrees: true,
        custom_worktree_path: Some(PathBuf::from(".claude/worktrees")),
        ignored_dirs: vec![".claude".to_string()],
        custom_ignored_dirs: vec!["custom_ign".to_string()],
        lang: vec!["rust".to_string()],
        ..cortx_core::ProjectConfig::default()
    };

    let wt2_files = idx.collect_source_files_for_project(&wt2, &proj);
    assert_eq!(wt2_files, vec![wt2.join("src/wt2.rs")]);

    let mut wt2_idx = SymbolIndex::new();
    let count = wt2_idx.index_directory_with_project(&wt2, &proj).unwrap();
    assert_eq!(count, 1);
    assert!(wt2_idx.symbols_map.contains_key("wt2_fn"));
}

#[test]
fn test_workspace_under_ignored_ancestor_and_watch_paths_scoping() {
    let temp_dir = tempfile::tempdir().unwrap();
    // Workspace root resides inside ancestor directories named `build` and `target`
    let ws_root = temp_dir
        .path()
        .join("build")
        .join("target")
        .join("my_workspace");
    std::fs::create_dir_all(ws_root.join("crates/core/src")).unwrap();
    std::fs::create_dir_all(ws_root.join("unwatched/src")).unwrap();
    std::fs::create_dir_all(ws_root.join("target/debug")).unwrap();

    let watched_file = ws_root.join("crates/core/src/lib.rs");
    let unwatched_file = ws_root.join("unwatched/src/extra.rs");
    let internal_target_file = ws_root.join("target/debug/gen.rs");

    std::fs::write(&watched_file, "pub fn watched_fn() {}").unwrap();
    std::fs::write(&unwatched_file, "pub fn unwatched_fn() {}").unwrap();
    std::fs::write(&internal_target_file, "pub fn gen_fn() {}").unwrap();

    let idx = SymbolIndex::new();

    // 1. Default ProjectConfig (unrestricted watch_paths) must index both crates/ and unwatched/
    //    despite ancestor `build/target/` in ws_root, while still pruning internal `target/debug/gen.rs`.
    let default_proj = cortx_core::ProjectConfig {
        qualified_path: ws_root.clone(),
        ..cortx_core::ProjectConfig::default()
    };
    let mut all_files = idx.collect_source_files_for_project(&ws_root, &default_proj);
    all_files.sort();
    assert_eq!(all_files, vec![watched_file.clone(), unwatched_file]);

    // 2. Explicit watch_paths = ["crates"] restricts discovery strictly to `crates/**`.
    let scoped_proj = cortx_core::ProjectConfig {
        qualified_path: ws_root.clone(),
        watch_paths: vec!["crates".to_string()],
        ..cortx_core::ProjectConfig::default()
    };
    let scoped_files = idx.collect_source_files_for_project(&ws_root, &scoped_proj);
    assert_eq!(scoped_files, vec![watched_file.clone()]);

    // 3. Indexing a subdirectory under `ws_root` (as `handle_distill` does) still evaluates `watch_paths` relative to `qualified_path`.
    let sub_files =
        idx.collect_source_files_for_project(&ws_root.join("crates/core/src"), &scoped_proj);
    assert_eq!(sub_files, vec![watched_file]);
    let unwatched_sub =
        idx.collect_source_files_for_project(&ws_root.join("unwatched/src"), &scoped_proj);
    assert!(unwatched_sub.is_empty());
}

#[test]
fn test_get_file_symbols_fast_path_and_normalization() {
    let mut idx = SymbolIndex::new();
    let file1 = "/home/user/project/crates/core/src/service.rs";
    let file2 = "/home/user/project/crates/api/src/service.rs";

    idx.record_symbol(SymbolDef {
        name: "CoreService".to_string(),
        kind: "struct".to_string(),
        file: file1.to_string(),
        line: 10,
        language: "rust".to_string(),
    });
    idx.record_symbol(SymbolDef {
        name: "ApiService".to_string(),
        kind: "struct".to_string(),
        file: file2.to_string(),
        line: 15,
        language: "rust".to_string(),
    });

    // 1. Exact path lookup
    let syms1 = idx.get_file_symbols(Path::new(file1)).unwrap();
    assert_eq!(syms1, &["CoreService"]);

    // 2. Relative prefix "./" lookup
    let syms_rel_dot = idx
        .get_file_symbols(Path::new(&format!("./{file1}")))
        .unwrap();
    assert_eq!(syms_rel_dot, &["CoreService"]);

    // 3. Fast lookup via file_name_to_paths with suffix match
    let syms_suffix = idx
        .get_file_symbols(Path::new("crates/core/src/service.rs"))
        .unwrap();
    assert_eq!(syms_suffix, &["CoreService"]);

    let syms_suffix2 = idx
        .get_file_symbols(Path::new("crates/api/src/service.rs"))
        .unwrap();
    assert_eq!(syms_suffix2, &["ApiService"]);

    // 4. Missing path returns None
    assert!(idx.get_file_symbols(Path::new("non_existent.rs")).is_none());

    // 5. Fallback when file_name_to_paths is empty
    let mut fallback_idx = SymbolIndex::new();
    fallback_idx.file_to_symbols = idx.file_to_symbols.clone();
    // file_name_to_paths is empty in fallback_idx
    let syms_fb = fallback_idx
        .get_file_symbols(Path::new("crates/core/src/service.rs"))
        .unwrap();
    assert_eq!(syms_fb, &["CoreService"]);
}

#[test]
fn test_collect_callers_and_callees_equivalence_and_dedup() {
    let mut idx = SymbolIndex::new();
    let file = "/project/src/lib.rs";

    idx.record_symbol(SymbolDef {
        name: "entrypoint".to_string(),
        kind: "fn".to_string(),
        file: file.to_string(),
        line: 1,
        language: "rust".to_string(),
    });
    idx.record_symbol(SymbolDef {
        name: "helper".to_string(),
        kind: "fn".to_string(),
        file: file.to_string(),
        line: 10,
        language: "rust".to_string(),
    });

    let edge_a = CallEdge {
        caller: "caller_a".into(),
        callee: "entrypoint".into(),
        file: "/project/src/caller_a.rs".into(),
    };
    let edge_b = CallEdge {
        caller: "caller_b".into(),
        callee: "helper".into(),
        file: "/project/src/caller_b.rs".into(),
    };
    let edge_shared = CallEdge {
        caller: "caller_shared".into(),
        callee: "helper".into(),
        file: "/project/src/caller_shared.rs".into(),
    };

    // Add callers
    let id_entrypoint = idx.pool.intern("entrypoint");
    let id_helper = idx.pool.intern("helper");
    let c_edge_a = CompactCallEdge {
        caller: idx.pool.intern(&edge_a.caller),
        callee: idx.pool.intern(&edge_a.callee),
        file: idx.pool.intern(&edge_a.file),
    };
    let c_edge_b = CompactCallEdge {
        caller: idx.pool.intern(&edge_b.caller),
        callee: idx.pool.intern(&edge_b.callee),
        file: idx.pool.intern(&edge_b.file),
    };
    let c_edge_shared = CompactCallEdge {
        caller: idx.pool.intern(&edge_shared.caller),
        callee: idx.pool.intern(&edge_shared.callee),
        file: idx.pool.intern(&edge_shared.file),
    };

    idx.callers_map
        .entry(id_entrypoint)
        .or_default()
        .extend(vec![c_edge_a, c_edge_shared]);
    idx.callers_map
        .entry(id_helper)
        .or_default()
        .extend(vec![c_edge_b, c_edge_shared]);

    let mut callers = Vec::new();
    let mut seen = HashSet::new();
    idx.collect_callers_into("entrypoint", 2, &mut callers, &mut seen);
    assert_eq!(callers.len(), 2);

    // Subsequent collect into callers deduplicates edge_shared
    idx.collect_callers_into("helper", 2, &mut callers, &mut seen);
    assert_eq!(callers.len(), 3);
    assert_eq!(callers, vec![edge_a.clone(), edge_shared.clone(), edge_b]);

    // Verify get_callers directly
    let direct_callers = idx.get_callers("entrypoint", 1);
    assert_eq!(direct_callers, vec![edge_a.clone(), edge_shared.clone()]);

    // Populate callees map and verify collect_callees_into & get_callees
    let id_caller_a = idx.pool.intern("caller_a");
    let id_caller_shared = idx.pool.intern("caller_shared");
    idx.callees_map
        .entry(id_caller_a)
        .or_default()
        .push(c_edge_a);
    idx.callees_map
        .entry(id_caller_shared)
        .or_default()
        .extend(vec![c_edge_a, c_edge_shared]);

    let mut callees = Vec::new();
    let mut seen_callees = HashSet::new();
    idx.collect_callees_into("caller_shared", 2, &mut callees, &mut seen_callees);
    assert_eq!(callees.len(), 2);
    assert_eq!(callees, vec![edge_a.clone(), edge_shared.clone()]);

    // Subsequent collect into callees deduplicates edge_a
    idx.collect_callees_into("caller_a", 2, &mut callees, &mut seen_callees);
    assert_eq!(callees.len(), 2);

    let direct_callees = idx.get_callees("caller_a", 1);
    assert_eq!(direct_callees, vec![edge_a]);
}

#[test]
fn test_string_pool() {
    let mut pool = StringPool::new();
    assert!(pool.is_empty());
    assert_eq!(pool.len(), 0);

    let id_a = pool.intern("my_func");
    let id_b = pool.intern("my_func");
    assert_eq!(
        id_a, id_b,
        "Duplicate interning must return identical SymbolId"
    );

    let id_c = pool.intern("other_func");
    assert_ne!(id_a, id_c);

    assert_eq!(pool.resolve(id_a), Some("my_func"));
    assert_eq!(pool.resolve(id_c), Some("other_func"));
    assert_eq!(pool.resolve(SymbolId(9999)), None);

    assert_eq!(pool.get_id("my_func"), Some(id_a));
    assert_eq!(pool.get_id("non_existent"), None);

    assert_eq!(pool.len(), 2);
    assert!(!pool.is_empty());

    // Verify CompactCallEdge size is exactly 12 bytes
    assert_eq!(std::mem::size_of::<CompactCallEdge>(), 12);
    let edge = CompactCallEdge {
        caller: id_a,
        callee: id_c,
        file: id_a,
    };
    assert_eq!(edge.caller, id_a);
    assert_eq!(edge.callee, id_c);
    assert_eq!(edge.file, id_a);
}

#[test]
fn test_call_edge_fidelity_cycles_overlay_and_parallel_merge() {
    // 1. Cyclic Call Graph String Fidelity
    let mut idx_cycle = SymbolIndex::new();
    let edge_ab = CallEdge {
        caller: "node_a".to_string(),
        callee: "node_b".to_string(),
        file: "src/file_a.rs".to_string(),
    };
    let edge_bc = CallEdge {
        caller: "node_b".to_string(),
        callee: "node_c".to_string(),
        file: "src/file_b.rs".to_string(),
    };
    let edge_ca = CallEdge {
        caller: "node_c".to_string(),
        callee: "node_a".to_string(),
        file: "src/file_c.rs".to_string(),
    };

    idx_cycle.add_call_edge(edge_ab.clone());
    idx_cycle.add_call_edge(edge_bc.clone());
    idx_cycle.add_call_edge(edge_ca.clone());

    // BFS callers of node_a with depth 5 must terminate and retain exact strings
    let callers = idx_cycle.get_callers("node_a", 5);
    assert_eq!(callers.len(), 3, "Must find exactly 3 edges in the cycle");
    assert!(callers.contains(&edge_ca));
    assert!(callers.contains(&edge_bc));
    assert!(callers.contains(&edge_ab));

    // BFS callees of node_a with depth 5 must terminate and retain exact strings
    let callees = idx_cycle.get_callees("node_a", 5);
    assert_eq!(callees.len(), 3, "Must find exactly 3 edges in the cycle");
    assert!(callees.contains(&edge_ab));
    assert!(callees.contains(&edge_bc));
    assert!(callees.contains(&edge_ca));

    // 2. Overlay Fallback Fidelity
    let mut base_idx = SymbolIndex::new();
    let base_edge = CallEdge {
        caller: "fn_base_caller".to_string(),
        callee: "fn_shared".to_string(),
        file: "src/base.rs".to_string(),
    };
    base_idx.add_call_edge(base_edge.clone());

    let mut overlay_idx = SymbolIndex::new();
    let overlay_edge = CallEdge {
        caller: "fn_overlay_caller".to_string(),
        callee: "fn_overlay_target".to_string(),
        file: "src/overlay.rs".to_string(),
    };
    overlay_idx.add_call_edge(overlay_edge.clone());

    // Query overlay for symbol only in base
    let fallback_callers = overlay_idx.get_callers_with_fallback(Some(&base_idx), "fn_shared", 1);
    assert_eq!(fallback_callers, vec![base_edge.clone()]);

    let fallback_callees =
        overlay_idx.get_callees_with_fallback(Some(&base_idx), "fn_base_caller", 1);
    assert_eq!(fallback_callees, vec![base_edge]);

    // Query overlay for symbol present in overlay
    let overlay_callers =
        overlay_idx.get_callers_with_fallback(Some(&base_idx), "fn_overlay_target", 1);
    assert_eq!(overlay_callers, vec![overlay_edge]);

    // 3. Parallel Merge Translation Fidelity
    let mut idx1 = SymbolIndex::new();
    let edge_x = CallEdge {
        caller: "caller_x".to_string(),
        callee: "common_fn".to_string(),
        file: "src/x.rs".to_string(),
    };
    let edge_leaf1 = CallEdge {
        caller: "common_fn".to_string(),
        callee: "leaf_1".to_string(),
        file: "src/x.rs".to_string(),
    };
    idx1.add_call_edge(edge_x.clone());
    idx1.add_call_edge(edge_leaf1.clone());

    let mut idx2 = SymbolIndex::new();
    let edge_y = CallEdge {
        caller: "caller_y".to_string(),
        callee: "common_fn".to_string(),
        file: "src/y.rs".to_string(),
    };
    let edge_leaf2 = CallEdge {
        caller: "common_fn".to_string(),
        callee: "leaf_2".to_string(),
        file: "src/y.rs".to_string(),
    };
    idx2.add_call_edge(edge_y.clone());
    idx2.add_call_edge(edge_leaf2.clone());

    // Merge idx2 into idx1
    idx1.merge(idx2);

    let merged_callers = idx1.get_callers("common_fn", 1);
    assert_eq!(merged_callers.len(), 2);
    assert!(merged_callers.contains(&edge_x));
    assert!(merged_callers.contains(&edge_y));

    let merged_callees = idx1.get_callees("common_fn", 1);
    assert_eq!(merged_callees.len(), 2);
    assert!(merged_callees.contains(&edge_leaf1));
    assert!(merged_callees.contains(&edge_leaf2));
}

#[test]
fn test_indexer_rust_omit_tests_filtering() {
    let code = r#"
        pub fn execute_job() {
            println!("job executed");
        }

        pub fn trigger_job() {
            execute_job();
        }

        #[tokio::test]
        async fn test_standalone_caller() {
            execute_job();
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn test_internal_caller() {
                execute_job();
            }
        }
    "#;

    // 1. With omit_tests = true (default)
    let mut idx_omitted = SymbolIndex::new();
    idx_omitted
        .index_source_str_with_options("src/job.rs", code, true)
        .unwrap();

    let callers_omitted = idx_omitted.get_callers("execute_job", 1);
    assert_eq!(callers_omitted.len(), 1);
    assert_eq!(callers_omitted[0].caller, "trigger_job");
    assert!(idx_omitted.symbols_map.contains_key("execute_job"));
    assert!(idx_omitted.symbols_map.contains_key("trigger_job"));
    assert!(
        !idx_omitted
            .symbols_map
            .contains_key("test_standalone_caller")
    );
    assert!(!idx_omitted.symbols_map.contains_key("test_internal_caller"));

    // 2. With omit_tests = false
    let mut idx_retained = SymbolIndex::new();
    idx_retained
        .index_source_str_with_options("src/job.rs", code, false)
        .unwrap();

    let callers_retained = idx_retained.get_callers("execute_job", 1);
    let caller_names: HashSet<&str> = callers_retained.iter().map(|c| c.caller.as_str()).collect();
    assert!(caller_names.contains("trigger_job"));
    assert!(caller_names.contains("test_standalone_caller"));
    assert!(caller_names.contains("test_internal_caller"));
    assert!(
        idx_retained
            .symbols_map
            .contains_key("test_standalone_caller")
    );
    assert!(
        idx_retained
            .symbols_map
            .contains_key("test_internal_caller")
    );
}

#[test]
fn test_indexer_go_omit_tests_filtering() {
    let go_code = r#"
package service

import "testing"

func ProcessOrder() {}

func TestProcessOrder(t *testing.T) {
    ProcessOrder()
}

func BenchmarkProcessOrder(b *testing.B) {}

func RegisterTestRoute(r *gin.Engine) {
    r.GET("/test", TestProcessOrder)
    r.POST("/process", ProcessOrder)
}
"#;

    // 1. With omit_tests = true (default)
    let mut idx_omitted = SymbolIndex::new();
    idx_omitted
        .index_go_source_str_with_options("service.go", go_code, true)
        .unwrap();

    assert!(idx_omitted.symbols_map.contains_key("ProcessOrder"));
    assert!(!idx_omitted.symbols_map.contains_key("TestProcessOrder"));
    assert!(
        !idx_omitted
            .symbols_map
            .contains_key("BenchmarkProcessOrder")
    );
    assert_eq!(idx_omitted.routes.len(), 1);
    assert_eq!(idx_omitted.routes[0].route_path, "/process");

    // 2. With omit_tests = false
    let mut idx_retained = SymbolIndex::new();
    idx_retained
        .index_go_source_str_with_options("service.go", go_code, false)
        .unwrap();

    assert!(idx_retained.symbols_map.contains_key("ProcessOrder"));
    assert!(idx_retained.symbols_map.contains_key("TestProcessOrder"));
    assert!(
        idx_retained
            .symbols_map
            .contains_key("BenchmarkProcessOrder")
    );
    assert_eq!(idx_retained.routes.len(), 2);
    let route_paths: HashSet<&str> = idx_retained
        .routes
        .iter()
        .map(|r| r.route_path.as_str())
        .collect();
    assert!(route_paths.contains("/process"));
    assert!(route_paths.contains("/test"));
}

#[test]
fn test_collect_source_files_omit_tests_filtering() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("tests/common")).unwrap();

    std::fs::write(root.join("src/lib.rs"), "pub fn run() {}").unwrap();
    std::fs::write(root.join("src/foo_test.rs"), "fn test_foo() {}").unwrap();
    std::fs::write(root.join("src/bar_test.go"), "package main").unwrap();
    std::fs::write(root.join("tests/integration_test.rs"), "fn test_all() {}").unwrap();
    std::fs::write(root.join("tests/common/mod.rs"), "pub fn helper() {}").unwrap();

    let indexer = SymbolIndex::new();

    // 1. With omit_tests = true
    let proj_omit = cortx_core::ProjectConfig {
        qualified_path: root.to_path_buf(),
        distill: cortx_core::config::DistillConfig {
            omit_tests: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let files_omit = indexer.collect_source_files_for_project(root, &proj_omit);
    let file_names_omit: Vec<String> = files_omit
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert_eq!(file_names_omit, vec!["lib.rs"]);

    // 2. With omit_tests = false
    let proj_retain = cortx_core::ProjectConfig {
        qualified_path: root.to_path_buf(),
        distill: cortx_core::config::DistillConfig {
            omit_tests: false,
            ..Default::default()
        },
        ..Default::default()
    };
    let files_retain = indexer.collect_source_files_for_project(root, &proj_retain);
    let mut file_names_retain: Vec<String> = files_retain
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    file_names_retain.sort();
    assert_eq!(
        file_names_retain,
        vec![
            "bar_test.go",
            "foo_test.rs",
            "integration_test.rs",
            "lib.rs",
            "mod.rs"
        ]
    );
}
