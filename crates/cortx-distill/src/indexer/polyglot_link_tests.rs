use super::overlay::OverlayView;
use super::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

#[test]
fn test_rust_macro_call_extraction_and_field_method_chains() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    std::fs::write(
        root.join("Cargo.toml"),
        r#"
[package]
name = "macro_test_pkg"
version = "0.1.0"
"#,
    )
    .unwrap();

    let client_rs = src_dir.join("client.rs");
    std::fs::write(
        &client_rs,
        r#"
pub struct NetworkClient;

impl NetworkClient {
    pub fn send_payload(&self, data: &str) -> bool {
        !data.is_empty()
    }
}
"#,
    )
    .unwrap();

    let service_rs = src_dir.join("service.rs");
    std::fs::write(
        &service_rs,
        r#"
use crate::client::NetworkClient;

pub struct Service {
    pub client: NetworkClient,
}

impl Service {
    pub fn process(&self) {
        self.client.send_payload("data");
    }
}

pub fn dispatch(client: &NetworkClient) {
    tokio::select! {
        _ = client.send_payload("msg") => {
            log_complete();
        }
    }
}

fn log_complete() {}
"#,
    )
    .unwrap();

    let mut index = SymbolIndex::new();
    let count = index.index_directory(root).expect("indexing directory");
    assert_eq!(count, 2, "must index both client.rs and service.rs");

    // 1. Verify struct field method chain resolution: self.client.send_payload() in process
    // and macro call: client.send_payload() in dispatch
    let send_callers = index.get_callers("send_payload", 1);
    assert!(
        !send_callers.is_empty(),
        "send_payload callers should not be empty, got: {:?}",
        send_callers
    );
    let caller_names: Vec<&str> = send_callers.iter().map(|c| c.caller.as_str()).collect();
    assert!(
        caller_names.contains(&"dispatch"),
        "dispatch caller should be found from macro, got: {:?}",
        caller_names
    );
    assert!(
        caller_names.contains(&"process"),
        "process caller should be found from struct field chain, got: {:?}",
        caller_names
    );
    for c in &send_callers {
        assert!(c.call_line > 0, "call_line should be preserved");
    }

    // 2. Verify direct path call inside macro: log_complete()
    let log_callers = index.get_callers("log_complete", 1);
    assert!(
        !log_callers.is_empty(),
        "log_complete callers should not be empty"
    );
    assert_eq!(log_callers[0].caller, "dispatch");
    assert!(log_callers[0].call_line > 0);
}

#[test]
fn test_typescript_barrel_chaining_8_hops() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    std::fs::write(
        root.join("package.json"),
        r#"{ "name": "barrel-test-pkg" }"#,
    )
    .unwrap();

    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();

    // Create leaf module
    std::fs::write(
        src.join("leaf.ts"),
        r#"export function coreWorker() { return 42; }"#,
    )
    .unwrap();

    // Create 8 barrel files chaining re-exports: barrel_1 -> leaf, barrel_2 -> barrel_1, ..., barrel_8 -> barrel_7
    std::fs::write(
        src.join("barrel_1.ts"),
        r#"export { coreWorker } from "./leaf";"#,
    )
    .unwrap();

    for i in 2..=8 {
        let prev = i - 1;
        std::fs::write(
            src.join(format!("barrel_{i}.ts")),
            format!(r#"export {{ coreWorker }} from "./barrel_{prev}";"#),
        )
        .unwrap();
    }

    // Consumer imports through 8-hop barrel_8
    std::fs::write(
        src.join("consumer.ts"),
        r#"
import { coreWorker } from "./barrel_8";

export function execute() {
    coreWorker();
}
"#,
    )
    .unwrap();

    let mut index = SymbolIndex::new();
    let count = index.index_directory(root).expect("indexing directory");
    assert_eq!(count, 10, "1 leaf + 8 barrels + 1 consumer = 10 files");

    // Verify consumer calls coreWorker through the 8-hop barrel chain
    let callers = index.get_callers("coreWorker", 1);
    assert!(
        !callers.is_empty(),
        "coreWorker callers should be resolved through 8 hops, got: {:?}",
        callers
    );
    assert_eq!(callers[0].caller, "consumer.ts:execute");
}

#[test]
fn test_typescript_barrel_chaining_cycle_and_hop_ceiling() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    std::fs::write(root.join("package.json"), r#"{ "name": "cycle-test-pkg" }"#).unwrap();

    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();

    // Mutual cycle between barrel_a and barrel_b
    std::fs::write(
        src.join("barrel_a.ts"),
        r#"export { cycleFunc } from "./barrel_b";"#,
    )
    .unwrap();
    std::fs::write(
        src.join("barrel_b.ts"),
        r#"export { cycleFunc } from "./barrel_a";"#,
    )
    .unwrap();

    std::fs::write(
        src.join("cycle_consumer.ts"),
        r#"
import { cycleFunc } from "./barrel_a";

export function trigger() {
    cycleFunc();
}
"#,
    )
    .unwrap();

    let mut index = SymbolIndex::new();
    // Must complete safely without infinite recursion or stack overflow
    let count = index.index_directory(root).expect("indexing directory");
    assert_eq!(count, 3);

    // Call edge is still recorded with the imported identifier
    let callers = index.get_callers("cycleFunc", 1);
    assert!(!callers.is_empty());
    assert_eq!(callers[0].caller, "cycle_consumer.ts:trigger");
}

#[test]
fn test_go_workspace_package_filtering() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    std::fs::write(
        root.join("go.mod"),
        r#"
module github.com/acme/project

go 1.24
"#,
    )
    .unwrap();

    let util_dir = root.join("pkg/util");
    std::fs::create_dir_all(&util_dir).unwrap();
    std::fs::write(
        util_dir.join("util.go"),
        r#"
package util

func InternalHelper() string {
    return "ok"
}
"#,
    )
    .unwrap();

    let cmd_dir = root.join("cmd/app");
    std::fs::create_dir_all(&cmd_dir).unwrap();
    std::fs::write(
        cmd_dir.join("main.go"),
        r#"
package main

import (
    "fmt"
    "github.com/acme/project/pkg/util"
)

func RunApp() {
    fmt.Println("Starting...")
    util.InternalHelper()
}
"#,
    )
    .unwrap();

    let mut index = SymbolIndex::new();
    let count = index.index_directory(root).expect("indexing directory");
    assert_eq!(count, 2);

    // Workspace package call: InternalHelper should be indexed
    let helper_callers = index.get_callers("InternalHelper", 1);
    assert!(
        !helper_callers.is_empty(),
        "InternalHelper should have callers"
    );
    assert_eq!(helper_callers[0].caller, "RunApp");

    // External package call: fmt.Println should be filtered out
    let println_callers = index.get_callers("Println", 1);
    assert!(
        println_callers.is_empty(),
        "Println from external fmt package must be filtered out by workspace go.mod filter, got: {:?}",
        println_callers
    );
}

#[test]
fn test_apply_watch_batch_file_and_dir_eviction() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    let src = root.join("src");
    let sub = src.join("features");
    std::fs::create_dir_all(&sub).unwrap();

    let main_file = src.join("main.rs");
    let feat1_file = sub.join("feat1.rs");
    let feat2_file = sub.join("feat2.rs");

    std::fs::write(
        &main_file,
        r#"
pub fn run() {
    feat_one();
}
"#,
    )
    .unwrap();

    std::fs::write(
        &feat1_file,
        r#"
pub fn feat_one() {
    feat_two();
}
"#,
    )
    .unwrap();

    std::fs::write(
        &feat2_file,
        r#"
pub fn feat_two() {}
"#,
    )
    .unwrap();

    let mut index = SymbolIndex::new();
    let count = index.index_directory(root).expect("initial indexing");
    assert_eq!(count, 3);

    // Initially all symbols and callers exist
    assert_eq!(index.get_callers("feat_one", 1).len(), 1);
    assert_eq!(index.get_callers("feat_two", 1).len(), 1);

    // 1. Directory deletion eviction: remove `src/features`
    let update = WatchBatchUpdate {
        upserted: &[],
        removed: std::slice::from_ref(&sub),
        project_cfg: None,
    };
    let outcome = index.apply_watch_batch(update);
    let updated_index = outcome.next_index;

    // Features directory files and symbols must be evicted
    assert!(
        updated_index.get_callers("feat_two", 1).is_empty(),
        "feat_two must be evicted after directory removal"
    );
    assert!(
        updated_index
            .symbols_map
            .get("feat_two")
            .is_none_or(|defs| defs.is_empty()),
        "feat_two symbol defs must be removed"
    );
    assert!(
        updated_index
            .symbols_map
            .get("feat_one")
            .is_none_or(|defs| defs.is_empty()),
        "feat_one symbol defs must be removed"
    );

    // main.rs must still be retained
    assert!(
        updated_index
            .symbols_map
            .get("run")
            .is_some_and(|defs| !defs.is_empty())
    );
    assert!(updated_index.file_facts.contains_key(&main_file));
    assert!(!updated_index.file_facts.contains_key(&feat1_file));
    assert!(!updated_index.file_facts.contains_key(&feat2_file));
}

#[test]
fn test_apply_watch_batch_upsert_relinking() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();

    let main_file = src.join("main.rs");
    std::fs::write(&main_file, "pub fn entry() { initial_callee(); }\n").unwrap();

    let callee_file = src.join("callee.rs");
    std::fs::write(&callee_file, "pub fn initial_callee() {}\n").unwrap();

    let mut index = SymbolIndex::new();
    index.index_directory(root).expect("initial index");
    assert_eq!(index.get_callers("initial_callee", 1).len(), 1);

    // Upsert: update main.rs to call `updated_callee` and add new_target.rs
    std::fs::write(&main_file, "pub fn entry() { updated_callee(); }\n").unwrap();
    let new_target = src.join("new_target.rs");
    std::fs::write(&new_target, "pub fn updated_callee() {}\n").unwrap();

    let upserted_files = [main_file, new_target];
    let update = WatchBatchUpdate {
        upserted: &upserted_files,
        removed: std::slice::from_ref(&callee_file),
        project_cfg: None,
    };
    let outcome = index.apply_watch_batch(update);
    let updated_index = outcome.next_index;

    // Old callee is gone
    assert!(updated_index.get_callers("initial_callee", 1).is_empty());
    // New callee has entry as caller
    let callers = updated_index.get_callers("updated_callee", 1);
    assert_eq!(callers.len(), 1);
    assert_eq!(callers[0].caller, "entry");
}

#[test]
fn test_worktree_tombstones_and_overlay_view() {
    let mut base_idx = SymbolIndex::new();
    base_idx
        .index_source_str(
            "crates/core/src/service.rs",
            r#"
pub struct BaseConfig;
pub fn base_worker() {
    run_action();
}
fn run_action() {}
"#,
        )
        .unwrap();

    base_idx
        .index_source_str(
            "crates/core/src/legacy.rs",
            r#"
pub fn legacy_func() {
    base_worker();
}
"#,
        )
        .unwrap();

    let mut overlay_idx = SymbolIndex::new();
    overlay_idx
        .index_source_str(
            "crates/core/src/service.rs",
            r#"
pub struct BaseConfig;
pub fn base_worker() {
    run_action_v2();
}
fn run_action_v2() {}
"#,
        )
        .unwrap();

    // Worktree tombstones: legacy.rs is deleted, service.rs is modified in overlay
    let mut shadowed = HashSet::new();
    shadowed.insert(PathBuf::from("crates/core/src/legacy.rs"));
    shadowed.insert(PathBuf::from("crates/core/src/service.rs"));

    let view = OverlayView {
        base: Some(&base_idx),
        overlay: Some(&overlay_idx),
        shadowed_files: Some(&shadowed),
    };

    // 1. Shadowed file lookup returns None
    assert!(
        view.get_file_symbols(Path::new("crates/core/src/legacy.rs"))
            .is_none(),
        "Tombstoned legacy.rs file symbols must return None"
    );

    // 2. Shadowed file callers must not be returned
    let callers = view.get_callers("base_worker", 1);
    assert!(
        !callers.iter().any(|c| c.caller == "legacy_func"),
        "legacy_func from shadowed file must not appear in callers: {:?}",
        callers
    );

    // 3. Overlay file takes precedence: run_action_v2 has caller base_worker
    let action_callers = view.get_callers("run_action_v2", 1);
    assert_eq!(action_callers.len(), 1);
    assert_eq!(action_callers[0].caller, "base_worker");

    // 4. Overridden base file's old caller relationship is masked
    let old_action_callers = view.get_callers("run_action", 1);
    assert!(
        old_action_callers.is_empty(),
        "Old run_action should have no callers when overlay overrides file"
    );
}
