//! Integration test verifying dynamic Git worktree isolation and multi-workspace daemon IPC.

use cortx_core::workspace::{derive_socket_path, resolve_active_socket, resolve_workspace_root};
use cortx_daemon::{
    DaemonRequest, DaemonResponse, execute_post_read_hook, run_server, send_daemon_request,
};
use cortx_distill::SymbolIndex;
use std::process::Command;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

fn run_git_cmd(cwd: &std::path::Path, args: &[&str]) {
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
    std::fs::create_dir_all(main_repo.join("src")).unwrap();

    // 1. Initialize main git repository
    run_git_cmd(&main_repo, &["init"]);
    run_git_cmd(&main_repo, &["config", "user.email", "test@example.com"]);
    run_git_cmd(&main_repo, &["config", "user.name", "Test User"]);
    run_git_cmd(&main_repo, &["config", "commit.gpgsign", "false"]);

    // 2. Add AlphaService to main_repo
    let alpha_file = main_repo.join("src").join("lib.rs");
    std::fs::write(
        &alpha_file,
        r#"pub struct AlphaService {
    pub id: u64,
}

impl AlphaService {
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

    run_git_cmd(&main_repo, &["add", "."]);
    run_git_cmd(&main_repo, &["commit", "-m", "init main repo"]);

    // 3. Create linked worktree for wt_feature
    let wt_str = wt_feature.to_str().unwrap();
    run_git_cmd(
        &main_repo,
        &["worktree", "add", "-b", "feature-beta", wt_str],
    );

    // 4. Modify wt_feature to contain BetaService
    let beta_file = wt_feature.join("src").join("lib.rs");
    std::fs::write(
        &beta_file,
        r#"pub struct BetaService {
    pub token: String,
}

impl BetaService {
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

    // Verify resolve_active_socket resolves the correct socket from subdirectories
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

    // 8. Query daemon A -> must see AlphaService, NOT BetaService
    let req_main = DaemonRequest::PostReadHook {
        file: alpha_file.clone(),
        start_line: None,
        end_line: None,
        raw_read_bytes: None,
        read_kind: None,
    };
    let resp_main = send_daemon_request(&socket_main, req_main).await.unwrap();
    if let DaemonResponse::AdvisoryContext { advisory_block, .. } = resp_main {
        assert!(
            advisory_block.contains("AlphaService"),
            "Expected AlphaService in main daemon context"
        );
        assert!(
            !advisory_block.contains("BetaService"),
            "Main daemon context contaminated with BetaService"
        );
    } else {
        panic!("Unexpected response from main daemon: {:?}", resp_main);
    }

    // 9. Query daemon B -> must see BetaService, NOT AlphaService
    let req_wt = DaemonRequest::PostReadHook {
        file: beta_file.clone(),
        start_line: None,
        end_line: None,
        raw_read_bytes: None,
        read_kind: None,
    };
    let resp_wt = send_daemon_request(&socket_wt, req_wt).await.unwrap();
    if let DaemonResponse::AdvisoryContext { advisory_block, .. } = resp_wt {
        assert!(
            advisory_block.contains("BetaService"),
            "Expected BetaService in worktree daemon context"
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
        },
    )
    .await
    .unwrap();
    if let DaemonResponse::AdvisoryContext { advisory_block, .. } = resp_wt2 {
        assert!(advisory_block.contains("BetaService"));
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
    assert!(fallback_advisory.contains("AlphaService"));
}
