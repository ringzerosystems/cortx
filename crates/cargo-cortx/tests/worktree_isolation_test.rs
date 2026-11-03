//! Integration test verifying dynamic Git worktree isolation, CoW overlay indexing,
//! worktree deletion tombstones, and multi-workspace daemon IPC.

use cortx_core::workspace::{derive_socket_path, resolve_active_socket, resolve_workspace_root};
use cortx_daemon::{
    DaemonRequest, DaemonResponse, ServerOptions, execute_post_read_hook, run_server,
    run_server_with_options, send_daemon_request,
};
use cortx_distill::SymbolIndex;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

fn run_git_cmd(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap_or_else(|e| panic!("failed to execute git {:?}: {}", args, e));
    assert!(status.success(), "git command {:?} failed", args);
}

#[tokio::test]
async fn test_worktree_daemon_isolation_and_lifecycle() {
    let base = tempdir().unwrap();
    let main_repo = base.path().join("main_repo");
    let wt_feature = base.path().join("wt_feature");
    let main_src = main_repo.join("src");
    std::fs::create_dir_all(&main_src).unwrap();

    // 1. Initialize main git repository
    run_git_cmd(&main_repo, &["init"]);
    run_git_cmd(&main_repo, &["config", "user.email", "test@example.com"]);
    run_git_cmd(&main_repo, &["config", "user.name", "Test User"]);
    run_git_cmd(&main_repo, &["config", "commit.gpgsign", "false"]);

    // 2. Add AlphaService and contract to main_repo
    let alpha_file = main_src.join("lib.rs");
    std::fs::write(
        &alpha_file,
        r#"pub trait AlphaContract {
    fn execute(&self) -> bool;
    fn audit_alpha(&self) -> usize;
}

pub struct AlphaService {
    pub id: u64,
}

impl AlphaContract for AlphaService {
    pub fn execute(&self) -> bool {
        let mut ok = false;
        for i in 0..100 {
            if i % 7 == 0 {
                println!("Alpha execute step processing item {}", i);
                ok = true;
            }
        }
        ok
    }

    pub fn audit_alpha(&self) -> usize {
        let mut total = 0;
        for step in 0..60 {
            total += step * 5;
            println!("Auditing alpha service execution cycle {} total {}", step, total);
        }
        total
    }
}
"#,
    )
    .unwrap();

    let alpha_caller = main_src.join("caller.rs");
    std::fs::write(
        &alpha_caller,
        r#"use crate::lib::{AlphaContract, AlphaService};

pub fn trigger_alpha_pipeline(svc: &AlphaService) -> bool {
    svc.execute()
}
"#,
    )
    .unwrap();

    run_git_cmd(&main_repo, &["add", "."]);
    run_git_cmd(&main_repo, &["commit", "-m", "init main repo"]);

    // 3. Create linked worktree for wt_feature
    let wt_str = wt_feature.to_str().unwrap();
    run_git_cmd(
        &main_repo,
        &["worktree", "add", "-b", "feature-beta", wt_str],
    );

    // 4. Modify wt_feature to contain BetaService and BetaContract
    let beta_file = wt_feature.join("src").join("lib.rs");
    std::fs::write(
        &beta_file,
        r#"pub trait BetaContract {
    fn process(&self) -> bool;
    fn audit_beta(&self) -> usize;
}

pub struct BetaService {
    pub token: String,
}

impl BetaContract for BetaService {
    pub fn process(&self) -> bool {
        let mut valid = false;
        for ch in self.token.chars() {
            if ch == 'b' {
                println!("Beta process valid token char matching criteria");
                valid = true;
            }
        }
        valid
    }

    pub fn audit_beta(&self) -> usize {
        let mut count = 0;
        for idx in 0..70 {
            count += idx * 11;
            println!("Auditing beta worker operational loop {} count {}", idx, count);
        }
        count
    }
}
"#,
    )
    .unwrap();

    let beta_caller = wt_feature.join("src").join("caller.rs");
    std::fs::write(
        &beta_caller,
        r#"use crate::lib::{BetaContract, BetaService};

pub fn trigger_beta_pipeline(svc: &BetaService) -> bool {
    svc.process()
}
"#,
    )
    .unwrap();

    // 5. Verify workspace root resolution
    let root_main = resolve_workspace_root(&main_repo.join("src"));
    let root_wt = resolve_workspace_root(&wt_feature.join("src"));

    let canonical_main = std::fs::canonicalize(&main_repo).unwrap();
    let canonical_wt = std::fs::canonicalize(&wt_feature).unwrap();

    assert_eq!(root_main, canonical_main);
    assert_eq!(root_wt, canonical_wt);
    assert_ne!(root_main, root_wt);

    // 6. Verify derived socket paths are distinct and length-compliant
    let socket_main = derive_socket_path(&root_main);
    let socket_wt = derive_socket_path(&root_wt);

    assert_ne!(socket_main, socket_wt);
    assert!(socket_main.to_str().unwrap().len() < 104);
    assert!(socket_wt.to_str().unwrap().len() < 104);

    let active_from_sub = resolve_active_socket(None, &main_repo.join("src"));
    assert_eq!(active_from_sub, socket_main);

    // 7. Start isolated daemons in both workspaces
    let mut index_main = SymbolIndex::new();
    index_main.index_directory(&root_main).unwrap();
    let token_main = CancellationToken::new();
    let s_main = socket_main.clone();
    let tok_main = token_main.clone();
    let handle_main = tokio::spawn(async move {
        run_server(&s_main, Arc::new(RwLock::new(index_main)), tok_main).await
    });

    let mut index_wt = SymbolIndex::new();
    index_wt.index_directory(&root_wt).unwrap();
    let token_wt = CancellationToken::new();
    let s_wt = socket_wt.clone();
    let tok_wt = token_wt.clone();
    let handle_wt =
        tokio::spawn(
            async move { run_server(&s_wt, Arc::new(RwLock::new(index_wt)), tok_wt).await },
        );

    // Wait for daemons to bind
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    assert!(socket_main.exists());
    assert!(socket_wt.exists());

    // 8. Query daemon A -> must see AlphaService / AlphaContract, NOT BetaService
    let req_main = DaemonRequest::PostReadHook {
        file: alpha_file.clone(),
        start_line: None,
        end_line: None,
        raw_read_bytes: None,
        read_kind: None,
        total_lines: None,
        vendor: None,
        is_full_read: None,
        session_id: None,
    };
    let resp_main = send_daemon_request(&socket_main, req_main).await.unwrap();
    if let DaemonResponse::AdvisoryContext { advisory_block, .. } = resp_main {
        assert!(
            advisory_block.contains("AlphaService")
                || advisory_block.contains("AlphaContract")
                || advisory_block.contains("trigger_alpha_pipeline"),
            "Expected AlphaService/AlphaContract/caller in main daemon context:\n{}",
            advisory_block
        );
        assert!(
            !advisory_block.contains("BetaService"),
            "Main daemon context contaminated with BetaService"
        );
    } else {
        panic!("Unexpected response from main daemon: {:?}", resp_main);
    }

    // 9. Query daemon B -> must see BetaService / BetaContract, NOT AlphaService
    let req_wt = DaemonRequest::PostReadHook {
        file: beta_file.clone(),
        start_line: None,
        end_line: None,
        raw_read_bytes: None,
        read_kind: None,
        total_lines: None,
        vendor: None,
        is_full_read: None,
        session_id: None,
    };
    let resp_wt = send_daemon_request(&socket_wt, req_wt).await.unwrap();
    if let DaemonResponse::AdvisoryContext { advisory_block, .. } = resp_wt {
        assert!(
            advisory_block.contains("BetaService")
                || advisory_block.contains("BetaContract")
                || advisory_block.contains("trigger_beta_pipeline"),
            "Expected BetaService/BetaContract/caller in worktree daemon context:\n{}",
            advisory_block
        );
        assert!(
            !advisory_block.contains("AlphaService"),
            "Worktree daemon context contaminated with AlphaService"
        );
    } else {
        panic!("Unexpected response from worktree daemon: {:?}", resp_wt);
    }

    // 10. Terminate daemon A and verify socket A is cleanly removed
    let shut_a = send_daemon_request(&socket_main, DaemonRequest::Shutdown)
        .await
        .unwrap();
    assert!(matches!(shut_a, DaemonResponse::ShutdownAck));
    let res_a = handle_main.await.unwrap();
    assert!(res_a.is_ok());
    assert!(
        !socket_main.exists(),
        "Socket A should be unlinked after shutdown"
    );

    // 11. Verify daemon B continues running undisturbed
    let ping_b = send_daemon_request(&socket_wt, DaemonRequest::Ping)
        .await
        .unwrap();
    assert!(matches!(ping_b, DaemonResponse::Pong));

    let resp_wt2 = send_daemon_request(
        &socket_wt,
        DaemonRequest::PostReadHook {
            file: beta_file.clone(),
            start_line: None,
            end_line: None,
            raw_read_bytes: None,
            read_kind: None,
            total_lines: None,
            vendor: None,
            is_full_read: None,
            session_id: None,
        },
    )
    .await
    .unwrap();
    if let DaemonResponse::AdvisoryContext { advisory_block, .. } = resp_wt2 {
        assert!(
            advisory_block.contains("BetaService")
                || advisory_block.contains("BetaContract")
                || advisory_block.contains("trigger_beta_pipeline")
        );
    } else {
        panic!("Unexpected response from worktree daemon: {:?}", resp_wt2);
    }

    // 12. Terminate daemon B
    let shut_b = send_daemon_request(&socket_wt, DaemonRequest::Shutdown)
        .await
        .unwrap();
    assert!(matches!(shut_b, DaemonResponse::ShutdownAck));
    let res_b = handle_wt.await.unwrap();
    assert!(res_b.is_ok());
    assert!(
        !socket_wt.exists(),
        "Socket B should be unlinked after shutdown"
    );

    // 13. Test in-process fallback when daemon is offline
    let fallback_advisory = execute_post_read_hook(&alpha_file, None).unwrap();
    assert!(
        fallback_advisory.contains("AlphaService")
            || fallback_advisory.contains("AlphaContract")
            || fallback_advisory.contains("trigger_alpha_pipeline")
    );
}

struct WorktreeOverlayFixture {
    main_lib: PathBuf,
    wt_lib: PathBuf,
}

fn setup_worktree_overlay_fixture(base_dir: &Path) -> WorktreeOverlayFixture {
    let main_repo = base_dir.join("main_repo_overlay");
    let wt_dir = base_dir.join("wt_overlay");
    let main_src = main_repo.join("src");
    std::fs::create_dir_all(&main_src).unwrap();

    run_git_cmd(&main_repo, &["init"]);
    run_git_cmd(&main_repo, &["config", "user.email", "test@example.com"]);
    run_git_cmd(&main_repo, &["config", "user.name", "Test User"]);
    run_git_cmd(&main_repo, &["config", "commit.gpgsign", "false"]);

    let main_lib = main_src.join("lib.rs");
    std::fs::write(
        &main_lib,
        r#"pub struct ServiceEngine {
    pub capacity: usize,
}

impl ServiceEngine {
    pub fn run_engine(&self) -> bool {
        self.capacity > 0
    }
}
"#,
    )
    .unwrap();

    std::fs::write(
        main_src.join("caller_one.rs"),
        r#"use crate::lib::ServiceEngine;

pub fn base_caller_one(engine: &ServiceEngine) -> bool {
    engine.run_engine()
}
"#,
    )
    .unwrap();

    std::fs::write(
        main_src.join("caller_two.rs"),
        r#"use crate::lib::ServiceEngine;

pub fn base_caller_two(engine: &ServiceEngine) -> bool {
    engine.run_engine()
}
"#,
    )
    .unwrap();

    std::fs::write(
        main_src.join("deleted_caller.rs"),
        r#"use crate::lib::ServiceEngine;

pub fn soon_deleted_caller(engine: &ServiceEngine) -> bool {
    engine.run_engine()
}
"#,
    )
    .unwrap();

    run_git_cmd(&main_repo, &["add", "."]);
    run_git_cmd(&main_repo, &["commit", "-m", "init overlay fixture"]);

    let wt_str = wt_dir.to_str().unwrap();
    run_git_cmd(
        &main_repo,
        &["worktree", "add", "-b", "feat-overlay", wt_str],
    );

    // Modify caller_one.rs in worktree
    std::fs::write(
        wt_dir.join("src").join("caller_one.rs"),
        r#"use crate::lib::ServiceEngine;

pub fn wt_modified_caller(engine: &ServiceEngine) -> bool {
    engine.run_engine()
}
"#,
    )
    .unwrap();

    // Delete deleted_caller.rs in worktree
    std::fs::remove_file(wt_dir.join("src").join("deleted_caller.rs")).unwrap();

    let wt_lib = wt_dir.join("src").join("lib.rs");
    WorktreeOverlayFixture { main_lib, wt_lib }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_worktree_overlay_union_and_deletion_tombstones() {
    let base = tempdir().unwrap();
    let fixture = setup_worktree_overlay_fixture(base.path());

    let socket_path = base.path().join("system_daemon.sock");
    let log_dir = base.path().join("logs");
    let server_token = CancellationToken::new();
    let s_sock = socket_path.clone();
    let tok = server_token.clone();
    let dummy_index = Arc::new(RwLock::new(SymbolIndex::new()));

    let server_handle = tokio::spawn(async move {
        let opts = ServerOptions {
            socket_path: s_sock,
            workspace_root: PathBuf::new(),
            watcher_active: false,
            log_dir: Some(log_dir),
            idle_ttl_secs: 60,
            daemon_config: Default::default(),
            config_path: None,
            ..Default::default()
        };
        let _ = run_server_with_options(opts, dummy_index, tok).await;
    });

    let mut ready = false;
    for _ in 0..20 {
        if send_daemon_request(&socket_path, DaemonRequest::Ping)
            .await
            .is_ok()
        {
            ready = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(ready, "Daemon server failed to bind and respond to ping");

    // 1. Query wt_lib -> overlay ∪ (base \ shadowed_files)
    let req_wt = DaemonRequest::PostReadHook {
        file: fixture.wt_lib.clone(),
        start_line: None,
        end_line: None,
        raw_read_bytes: None,
        read_kind: None,
        total_lines: None,
        vendor: None,
        is_full_read: None,
        session_id: None,
    };
    let resp_wt = send_daemon_request(&socket_path, req_wt).await.unwrap();
    if let DaemonResponse::AdvisoryContext { advisory_block, .. } = resp_wt {
        assert!(
            advisory_block.contains("wt_modified_caller"),
            "Expected wt_modified_caller in worktree advisory, got:\n{}",
            advisory_block
        );
        assert!(
            advisory_block.contains("base_caller_two"),
            "Expected unmodified base_caller_two preserved from base index in worktree advisory, got:\n{}",
            advisory_block
        );
        assert!(
            !advisory_block.contains("base_caller_one"),
            "Shadowed base_caller_one should be replaced by overlay in worktree advisory, got:\n{}",
            advisory_block
        );
        assert!(
            !advisory_block.contains("soon_deleted_caller"),
            "Deleted file caller soon_deleted_caller must be suppressed by worktree tombstone, got:\n{}",
            advisory_block
        );
    } else {
        panic!(
            "Unexpected response from daemon for worktree: {:?}",
            resp_wt
        );
    }

    // 2. Query main_lib -> parent repository index remains intact
    let req_parent = DaemonRequest::PostReadHook {
        file: fixture.main_lib.clone(),
        start_line: None,
        end_line: None,
        raw_read_bytes: None,
        read_kind: None,
        total_lines: None,
        vendor: None,
        is_full_read: None,
        session_id: None,
    };
    let resp_parent = send_daemon_request(&socket_path, req_parent).await.unwrap();
    if let DaemonResponse::AdvisoryContext { advisory_block, .. } = resp_parent {
        assert!(
            advisory_block.contains("base_caller_one"),
            "Parent repo must retain original base_caller_one, got:\n{}",
            advisory_block
        );
        assert!(
            advisory_block.contains("base_caller_two"),
            "Parent repo must retain base_caller_two, got:\n{}",
            advisory_block
        );
        assert!(
            advisory_block.contains("soon_deleted_caller"),
            "Parent repo must retain soon_deleted_caller, got:\n{}",
            advisory_block
        );
        assert!(
            !advisory_block.contains("wt_modified_caller"),
            "Parent repo must not be contaminated with wt_modified_caller, got:\n{}",
            advisory_block
        );
    } else {
        panic!(
            "Unexpected response from daemon for parent: {:?}",
            resp_parent
        );
    }

    // 3. Graceful shutdown
    let shut = send_daemon_request(&socket_path, DaemonRequest::Shutdown)
        .await
        .unwrap();
    assert!(matches!(shut, DaemonResponse::ShutdownAck));
    let res = server_handle.await;
    assert!(res.is_ok());
    assert!(!socket_path.exists());
}
