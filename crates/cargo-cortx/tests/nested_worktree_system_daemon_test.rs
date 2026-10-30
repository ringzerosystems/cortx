//! Integration tests verifying nested Git worktrees inside `<repo>/.worktrees/*`
//! and `<repo>/.claude/worktrees/*` (with `ignored_dirs = [".claude"]`),
//! Copy-on-Write overlay indexing, and live file watcher carve-out/routing.

use cortx_core::CortxConfig;
use cortx_daemon::{
    DaemonRequest, DaemonResponse, ServerOptions, run_server_with_options, send_daemon_request,
};
use cortx_distill::{SymbolIndex, is_ignored_path};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

fn run_git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap_or_else(|e| panic!("failed to run git {:?}: {}", args, e));
    assert!(status.success(), "git {:?} failed", args);
}

fn init_repo(repo_dir: &Path, lib_content: &str) -> PathBuf {
    std::fs::create_dir_all(repo_dir.join("src")).unwrap();
    run_git(repo_dir, &["init"]);
    run_git(repo_dir, &["config", "user.email", "test@example.com"]);
    run_git(repo_dir, &["config", "user.name", "Test User"]);
    run_git(repo_dir, &["config", "commit.gpgsign", "false"]);

    let lib_rs = repo_dir.join("src/lib.rs");
    std::fs::write(&lib_rs, lib_content).unwrap();
    run_git(repo_dir, &["add", "."]);
    run_git(repo_dir, &["commit", "-m", "initial commit"]);
    lib_rs
}

async fn wait_for_log_substring(log_path: &Path, expected: &str) {
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(4) {
        if let Ok(content) = std::fs::read_to_string(log_path) {
            if content.contains(expected) {
                return;
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;
    }
    let actual = std::fs::read_to_string(log_path).unwrap_or_default();
    panic!(
        "Log '{}' did not contain '{}' within timeout. Actual:\n{}",
        log_path.display(),
        expected,
        actual
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_nested_dot_worktrees_overlay_and_post_read_hook() {
    let base = tempdir().unwrap();
    let base_path = std::fs::canonicalize(base.path()).unwrap();

    // 1. Initialize parent repo and nested worktree under `<repo>/.worktrees/agent-auth`
    let repo = base_path.join("auth-service");
    let parent_lib = init_repo(
        &repo,
        r#"pub struct ParentAuth {
    pub id: u64,
}

impl ParentAuth {
    pub fn verify_parent(&self) -> bool {
        let mut check = true;
        for i in 0..100 {
            if i % 2 == 1 {
                println!("Verifying parent authentication sequence step {}", i);
                check = !check;
            }
        }
        check
    }

    pub fn audit_parent(&self) -> usize {
        let mut total = 0;
        for step in 0..50 {
            total += step * 3;
            println!("Auditing parent step {} total {}", step, total);
        }
        total
    }
}
"#,
    );

    let wt_dir = repo.join(".worktrees/agent-auth");
    std::fs::create_dir_all(wt_dir.parent().unwrap()).unwrap();
    run_git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "agent-auth",
            wt_dir.to_str().unwrap(),
        ],
    );

    let wt_lib = wt_dir.join("src/lib.rs");
    std::fs::write(
        &wt_lib,
        r#"pub struct WorktreeAuth {
    pub token: String,
}

impl WorktreeAuth {
    pub fn verify_worktree(&self) -> bool {
        let mut valid = false;
        for ch in "worktree_auth_token_verification_payload_with_extended_validation_chain".chars() {
            if ch == 'a' {
                println!("Validating worktree auth token character match");
                valid = true;
            }
        }
        valid
    }

    pub fn audit_worktree(&self) -> usize {
        let mut counter = 0;
        for idx in 0..60 {
            counter += idx * 7;
            println!("Auditing worktree verification cycle {} count {}", idx, counter);
        }
        counter
    }
}
"#,
    )
    .unwrap();

    // Verify `.worktrees` path is never falsely flagged by `is_ignored_path`
    assert!(
        !is_ignored_path(&wt_lib),
        "Nested worktree file inside .worktrees must not be ignored"
    );

    // 2. Start system-wide daemon
    let socket_path = base_path.join("dot_wt.sock");
    let log_dir = base_path.join("logs");
    let token = CancellationToken::new();
    let s_sock = socket_path.clone();
    let s_log = log_dir.clone();
    let s_tok = token.clone();
    let handle = tokio::spawn(async move {
        let opts = ServerOptions {
            socket_path: s_sock,
            workspace_root: PathBuf::new(),
            watcher_active: false,
            log_dir: Some(s_log),
            idle_ttl_secs: 7200,
            daemon_config: CortxConfig::default(),
            config_path: None,
        };
        run_server_with_options(opts, Arc::new(RwLock::new(SymbolIndex::new())), s_tok).await
    });
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // 3. Request PostReadHook for nested worktree file -> initializes CoW overlay against parent
    let resp = send_daemon_request(
        &socket_path,
        DaemonRequest::PostReadHook {
            file: wt_lib.clone(),
            start_line: None,
            end_line: None,
            raw_read_bytes: None,
            read_kind: None,
        },
    )
    .await
    .unwrap();
    match resp {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(advisory_block.contains("WorktreeAuth"));
            assert!(!advisory_block.contains("ParentAuth"));
        }
        other => panic!("Unexpected response: {:?}", other),
    }

    // 4. Verify parent repo and worktree are isolated in daemon status
    let status_resp = send_daemon_request(&socket_path, DaemonRequest::Status)
        .await
        .unwrap();
    let DaemonResponse::Status(status): DaemonResponse = status_resp else {
        panic!("Expected Status response");
    };
    let canonical_repo = std::fs::canonicalize(&repo).unwrap();
    let canonical_wt = std::fs::canonicalize(&wt_dir).unwrap();
    let wt_summary = status
        .workspaces
        .iter()
        .find(|w| w.root == canonical_wt)
        .expect("worktree registered");
    assert!(wt_summary.is_worktree);
    assert_eq!(wt_summary.parent_repo, Some(canonical_repo));
    assert_eq!(wt_summary.indexed_files, 1);

    // Ensure parent repo still serves ParentAuth
    let parent_resp = send_daemon_request(
        &socket_path,
        DaemonRequest::PostReadHook {
            file: parent_lib,
            start_line: None,
            end_line: None,
            raw_read_bytes: None,
            read_kind: None,
        },
    )
    .await
    .unwrap();
    if let DaemonResponse::AdvisoryContext { advisory_block, .. } = parent_resp {
        assert!(advisory_block.contains("ParentAuth"));
        assert!(!advisory_block.contains("WorktreeAuth"));
    } else {
        panic!("Expected AdvisoryContext for parent");
    }

    let _ = send_daemon_request(&socket_path, DaemonRequest::Shutdown).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_nested_claude_worktree_with_ignored_dirs_and_live_watcher_routing() {
    let base = tempdir().unwrap();
    let base_path = std::fs::canonicalize(base.path()).unwrap();

    // 1. Initialize parent repo and nested worktree at `<repo>/.claude/worktrees/agent-feat`
    let repo = base_path.join("acme-web");
    let parent_lib = init_repo(
        &repo,
        r#"pub struct ParentMain {
    pub v: u32,
}

impl ParentMain {
    pub fn routine(&self) -> bool {
        let mut sum = 0u64;
        for i in 0..100 {
            sum = sum.wrapping_add(i * 13);
            println!("Parent routine step {}", sum);
        }
        sum > 0
    }

    pub fn audit_parent(&self) -> usize {
        let mut total = 0;
        for step in 0..50 {
            total += step * 3;
            println!("Auditing parent step {} total {}", step, total);
        }
        total
    }
}
"#,
    );

    let wt_dir = repo.join(".claude/worktrees/agent-feat");
    std::fs::create_dir_all(wt_dir.parent().unwrap()).unwrap();
    run_git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "agent-feat",
            wt_dir.to_str().unwrap(),
        ],
    );

    let wt_lib = wt_dir.join("src/lib.rs");
    std::fs::write(
        &wt_lib,
        r#"pub struct AgentFeatInitial {
    pub ok: bool,
}

impl AgentFeatInitial {
    pub fn feat_step(&self) -> bool {
        let mut check = false;
        for step in 0..100 {
            if step % 5 == 0 {
                println!("Executing agent feat step {}", step);
                check = true;
            }
        }
        check
    }

    pub fn audit_feat(&self) -> usize {
        let mut counter = 0;
        for idx in 0..60 {
            counter += idx * 7;
            println!("Auditing feat cycle {} count {}", idx, counter);
        }
        counter
    }
}
"#,
    )
    .unwrap();

    // Create internal build artifact & internal `.claude` cache inside the worktree that MUST be ignored
    std::fs::create_dir_all(wt_dir.join("target/debug")).unwrap();
    std::fs::write(
        wt_dir.join("target/debug/gen.rs"),
        "pub struct TargetArtifactOne {}\npub struct TargetArtifactTwo {}\n",
    )
    .unwrap();
    std::fs::create_dir_all(wt_dir.join(".claude")).unwrap();
    std::fs::write(
        wt_dir.join(".claude/local_cache.rs"),
        "pub struct ClaudeCacheOne {}\npub struct ClaudeCacheTwo {}\n",
    )
    .unwrap();

    // 2. Configure daemon with `ignored_dirs = [".claude"]` and `has_custom_worktree_path = ".claude/worktrees"`
    let toml_str = format!(
        r#"
[daemon]
system_daemon = true
enable_file_watcher = true

[acme-web]
qualified_path = "{repo}"
has_worktrees = true
has_custom_worktree_path = ".claude/worktrees"
lang = "rust"
ignored_dirs = [".claude"]
"#,
        repo = repo.display()
    );
    let cfg_file = base_path.join("daemon.toml");
    std::fs::write(&cfg_file, &toml_str).unwrap();
    let (daemon_cfg, config_path) = CortxConfig::load_daemon_config(Some(&cfg_file)).unwrap();

    let socket_path = base_path.join("claude_wt.sock");
    let log_dir = base_path.join("logs");
    let token = CancellationToken::new();
    let s_sock = socket_path.clone();
    let s_log = log_dir.clone();
    let s_tok = token.clone();
    let handle = tokio::spawn(async move {
        let opts = ServerOptions {
            socket_path: s_sock,
            workspace_root: PathBuf::new(),
            watcher_active: true,
            log_dir: Some(s_log),
            idle_ttl_secs: 7200,
            daemon_config: daemon_cfg,
            config_path,
        };
        run_server_with_options(opts, Arc::new(RwLock::new(SymbolIndex::new())), s_tok).await
    });
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // 3. Touch both parent and worktree so both are indexed and registered with the file watcher
    let _ = send_daemon_request(
        &socket_path,
        DaemonRequest::PostReadHook {
            file: parent_lib.clone(),
            start_line: None,
            end_line: None,
            raw_read_bytes: None,
            read_kind: None,
        },
    )
    .await
    .unwrap();

    let wt_hook = send_daemon_request(
        &socket_path,
        DaemonRequest::PostReadHook {
            file: wt_lib.clone(),
            start_line: None,
            end_line: None,
            raw_read_bytes: None,
            read_kind: None,
        },
    )
    .await
    .unwrap();
    if let DaemonResponse::AdvisoryContext { advisory_block, .. } = wt_hook {
        assert!(advisory_block.contains("AgentFeatInitial"));
        assert!(!advisory_block.contains("ParentMain"));
    } else {
        panic!("Expected AdvisoryContext");
    }

    // Retrieve workspace summaries from status
    let DaemonResponse::Status(status): DaemonResponse =
        send_daemon_request(&socket_path, DaemonRequest::Status)
            .await
            .unwrap()
    else {
        panic!("Expected Status");
    };
    let canonical_repo = std::fs::canonicalize(&repo).unwrap();
    let canonical_wt = std::fs::canonicalize(&wt_dir).unwrap();
    let parent_ws = status
        .workspaces
        .iter()
        .find(|w| w.root == canonical_repo)
        .unwrap();
    let wt_ws = status
        .workspaces
        .iter()
        .find(|w| w.root == canonical_wt)
        .unwrap();
    assert_eq!(wt_ws.project_name.as_deref(), Some("acme-web"));
    // Only `src/lib.rs` (1 file, 1 symbol) inside the worktree should be indexed;
    // `<worktree>/target/*` and `<worktree>/.claude/*` must be ignored.
    assert_eq!(wt_ws.indexed_files, 1);
    assert_eq!(wt_ws.total_symbols, 1);
    assert_eq!(parent_ws.indexed_files, 1);
    assert_eq!(parent_ws.total_symbols, 1);

    // 4. Perform live file edit inside `<repo>/.claude/worktrees/agent-feat/src/lib.rs`
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    std::fs::write(
        &wt_lib,
        r#"pub struct AgentFeatInitial {
    pub ok: bool,
}

impl AgentFeatInitial {
    pub fn feat_step(&self) -> bool {
        let mut check = false;
        for step in 0..100 {
            if step % 5 == 0 {
                println!("Executing agent feat step {}", step);
                check = true;
            }
        }
        check
    }

    pub fn audit_feat(&self) -> usize {
        let mut counter = 0;
        for idx in 0..60 {
            counter += idx * 7;
            println!("Auditing feat cycle {} count {}", idx, counter);
        }
        counter
    }
}

pub struct LiveEditedOne {
    pub a: u64,
}

pub struct LiveEditedTwo {
    pub b: u64,
}
"#,
    )
    .unwrap();

    // Wait for watcher to log the mutation in the worktree's workspace log
    wait_for_log_substring(&wt_ws.log_path, "updated symbol index").await;

    // Verify worktree index now has 3 symbols while parent repo index still has 1 symbol
    let DaemonResponse::Status(status_after): DaemonResponse =
        send_daemon_request(&socket_path, DaemonRequest::Status)
            .await
            .unwrap()
    else {
        panic!("Expected Status");
    };
    let parent_ws_after = status_after
        .workspaces
        .iter()
        .find(|w| w.root == canonical_repo)
        .unwrap();
    let wt_ws_after = status_after
        .workspaces
        .iter()
        .find(|w| w.root == canonical_wt)
        .unwrap();
    assert!(
        wt_ws_after.total_symbols >= 3,
        "Worktree index must be updated with new symbols by live file watcher (got {})",
        wt_ws_after.total_symbols
    );
    assert_eq!(
        parent_ws_after.total_symbols, 1,
        "Parent repo index must not be polluted by worktree edit"
    );
    let parent_log = std::fs::read_to_string(&parent_ws.log_path).unwrap_or_default();
    assert!(
        !parent_log.contains("updated symbol index"),
        "Parent workspace log must not record worktree file mutations"
    );

    let _ = send_daemon_request(&socket_path, DaemonRequest::Shutdown).await;
    let _ = handle.await;
}
