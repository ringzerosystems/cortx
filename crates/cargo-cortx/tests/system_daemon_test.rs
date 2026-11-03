//! End-to-end integration test verifying multi-tenant system-wide daemon operations,
//! lazy on-demand cold starts, Git worktree overlay indexing, multi-tier file logging,
//! and memory consumption telemetry in megabytes.

use cortx_daemon::{DaemonStatusInfo, ServerOptions, run_server_with_options};
use cortx_distill::SymbolIndex;
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
        .unwrap_or_else(|e| {
            panic!(
                "failed to execute git {:?} in {}: {}",
                args,
                cwd.display(),
                e
            )
        });
    assert!(status.success(), "git command {:?} failed", args);
}

fn cortx_cmd(bin: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_CHILD_SESSION")
        .env_remove("CLAUDE_CODE_FILE_READ_MAX_OUTPUT_TOKENS");
    cmd
}

fn init_test_repo(path: &Path, file_name: &str, content: &str) -> PathBuf {
    std::fs::create_dir_all(path.join("src")).unwrap();
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "test@example.com"]);
    run_git(path, &["config", "user.name", "Test User"]);
    run_git(path, &["config", "commit.gpgsign", "false"]);

    let target_file = path.join("src").join(file_name);
    std::fs::write(&target_file, content).unwrap();
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", "initial commit"]);
    target_file
}

async fn assert_file_contains_eventually(path: &Path, expected: &str) {
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(3) {
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(path) {
                if content.contains(expected) {
                    return;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let actual = std::fs::read_to_string(path).unwrap_or_default();
    panic!(
        "File '{}' did not contain '{}' within timeout. Actual content:\n{}",
        path.display(),
        expected,
        actual
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_system_daemon_multi_tenant_lifecycle() {
    let cortx_bin = env!("CARGO_BIN_EXE_cortx");
    let base = tempdir().unwrap();
    let base_path = std::fs::canonicalize(base.path()).unwrap();

    // 1. Initialize two independent Git repositories
    let repo_alpha = base_path.join("repo_alpha");
    let alpha_file = init_test_repo(
        &repo_alpha,
        "lib.rs",
        r#"pub struct AlphaService {
    pub id: u64,
}

pub trait AlphaRunner {
    fn run_alpha(&self) -> bool;
    fn process_record(&self, key: &str, value: &[u8]) -> Result<usize, String>;
    fn compute_checksum(&self, seed: u64) -> u64;
}

impl AlphaRunner for AlphaService {
    fn run_alpha(&self) -> bool {
        let active = self.id > 0;
        if active {
            println!("AlphaService {} running active pipeline", self.id);
            true
        } else {
            eprintln!("AlphaService zero-id inactive");
            false
        }
    }

    fn process_record(&self, key: &str, value: &[u8]) -> Result<usize, String> {
        if key.is_empty() {
            return Err("Empty key provided to alpha pipeline".to_string());
        }
        let mut total = 0usize;
        for &byte in value {
            total = total.wrapping_add(byte as usize);
            if total > 1_000_000 {
                println!("Large payload processed for key {}", key);
            }
        }
        Ok(total)
    }

    fn compute_checksum(&self, seed: u64) -> u64 {
        let mut hash = seed ^ self.id;
        for i in 0..64 {
            hash = hash.rotate_left(3).wrapping_add(i as u64);
        }
        hash
    }
}
"#,
    );

    let repo_beta = base_path.join("repo_beta");
    let beta_file = init_test_repo(
        &repo_beta,
        "lib.rs",
        r#"pub struct BetaService {
    pub name: String,
}

pub trait BetaRunner {
    fn run_beta(&self) -> bool;
    fn execute_query(&self, query: &str) -> Result<Vec<String>, String>;
    fn summarize_state(&self) -> String;
}

impl BetaRunner for BetaService {
    fn run_beta(&self) -> bool {
        let valid = !self.name.is_empty();
        if valid {
            println!("BetaService {} running active worker", self.name);
            true
        } else {
            eprintln!("Invalid BetaService configuration");
            false
        }
    }

    fn execute_query(&self, query: &str) -> Result<Vec<String>, String> {
        if query.trim().is_empty() {
            return Err("Empty query specified for beta runner".to_string());
        }
        let mut results = Vec::new();
        for part in query.split_whitespace() {
            results.push(format!("{}:{}", self.name, part));
            if results.len() > 100 {
                break;
            }
        }
        Ok(results)
    }

    fn summarize_state(&self) -> String {
        format!("BetaWorker[name={}, len={}]", self.name, self.name.len())
    }
}
"#,
    );

    // 2. Create nested Git worktree at repo_alpha/.claude/worktrees/feat1
    let wt_parent = repo_alpha.join(".claude").join("worktrees");
    std::fs::create_dir_all(&wt_parent).unwrap();
    let wt_feat1 = wt_parent.join("feat1");
    let wt_feat1_str = wt_feat1.to_str().unwrap();
    run_git(
        &repo_alpha,
        &["worktree", "add", "-b", "feat1", wt_feat1_str],
    );

    let feat1_file = wt_feat1.join("src").join("lib.rs");
    std::fs::write(
        &feat1_file,
        r#"pub struct AlphaService {
    pub id: u64,
    pub extra: bool,
}

pub struct FeatOneService {
    pub flag: bool,
}

pub trait FeatRunner {
    fn execute_feat(&self) -> bool;
    fn audit_status(&self, tag: &str) -> Result<String, String>;
}

impl FeatRunner for FeatOneService {
    fn execute_feat(&self) -> bool {
        if self.flag {
            println!("Feature 1 execution active");
            true
        } else {
            println!("Feature 1 disabled by flag");
            false
        }
    }

    fn audit_status(&self, tag: &str) -> Result<String, String> {
        if tag.is_empty() {
            return Err("Missing audit tag".to_string());
        }
        let report = format!("FeatOne[flag={}, tag={}]", self.flag, tag);
        println!("Audit log generated: {}", report);
        Ok(report)
    }
}
"#,
    )
    .unwrap();

    let feat1_caller = wt_feat1.join("src").join("caller.rs");
    std::fs::write(
        &feat1_caller,
        r#"use crate::lib::{FeatOneService, FeatRunner};

pub fn trigger_feat_pipeline(svc: &FeatOneService) -> bool {
    svc.execute_feat()
}
"#,
    )
    .unwrap();

    // 3. Launch single system-wide daemon instance
    let socket_path = base_path.join("system_daemon.sock");
    let log_dir = base_path.join("logs");
    let token = CancellationToken::new();
    let server_token = token.clone();
    let s_sock = socket_path.clone();
    let s_log = log_dir.clone();
    let dummy_index = Arc::new(RwLock::new(SymbolIndex::new()));

    let server_handle = tokio::spawn(async move {
        let opts = ServerOptions {
            socket_path: s_sock,
            workspace_root: PathBuf::new(),
            watcher_active: false,
            log_dir: Some(s_log),
            idle_ttl_secs: 7200,
            daemon_config: Default::default(),
            config_path: None,
            ..Default::default()
        };
        let _ = run_server_with_options(opts, dummy_index, server_token).await;
    });

    // Wait for server socket to bind
    tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

    // Probe liveness via --ping
    let ping_out = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--ping",
            "--socket",
            socket_path.to_str().unwrap(),
        ])
        .current_dir(&base_path)
        .output()
        .await
        .expect("failed to execute cortx daemon --ping");
    assert!(ping_out.status.success(), "daemon ping must succeed");

    // 4. Issue post-read hook for repo_alpha file -> assert lazy cold-start and context distillation
    let hook_alpha = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--hook-post-read",
            "--file",
            alpha_file.to_str().unwrap(),
            "--socket",
            socket_path.to_str().unwrap(),
        ])
        .current_dir(&repo_alpha)
        .output()
        .await
        .expect("failed to execute hook for repo_alpha");
    assert!(hook_alpha.status.success());
    let hook_alpha_str = String::from_utf8_lossy(&hook_alpha.stdout);
    assert!(hook_alpha_str.contains("<!-- cortx-context-provenance: daemon (socket:"));
    assert!(hook_alpha_str.contains("AlphaService"));
    assert!(hook_alpha_str.contains("AlphaRunner"));
    assert!(!hook_alpha_str.contains("## Type Skeleton"));
    assert!(!hook_alpha_str.contains("Token Reduction"));

    // 5. Issue post-read hook for repo_beta file -> assert lazy cold-start and context distillation
    let hook_beta = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--hook-post-read",
            "--file",
            beta_file.to_str().unwrap(),
            "--socket",
            socket_path.to_str().unwrap(),
        ])
        .current_dir(&repo_beta)
        .output()
        .await
        .expect("failed to execute hook for repo_beta");
    assert!(hook_beta.status.success());
    let hook_beta_str = String::from_utf8_lossy(&hook_beta.stdout);
    assert!(hook_beta_str.contains("<!-- cortx-context-provenance: daemon (socket:"));
    assert!(hook_beta_str.contains("BetaService"));
    assert!(hook_beta_str.contains("BetaRunner"));
    assert!(!hook_beta_str.contains("## Type Skeleton"));
    assert!(!hook_beta_str.contains("Token Reduction"));

    // 6. Issue post-read hook for worktree feat1 -> assert worktree overlay resolution
    let hook_wt = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--hook-post-read",
            "--file",
            feat1_file.to_str().unwrap(),
            "--socket",
            socket_path.to_str().unwrap(),
        ])
        .current_dir(&wt_feat1)
        .output()
        .await
        .expect("failed to execute hook for feat1");
    assert!(hook_wt.status.success());
    let hook_wt_str = String::from_utf8_lossy(&hook_wt.stdout);
    assert!(hook_wt_str.contains("<!-- cortx-context-provenance: daemon (socket:"));
    assert!(hook_wt_str.contains("FeatOneService"));
    assert!(hook_wt_str.contains("FeatRunner"));
    assert!(hook_wt_str.contains("## Upstream Callers"));
    assert!(hook_wt_str.contains("trigger_feat_pipeline"));
    assert!(hook_wt_str.contains("caller.rs"));
    assert!(!hook_wt_str.contains("## Type Skeleton"));
    assert!(!hook_wt_str.contains("Token Reduction"));

    // 7. Query daemon status via JSON to verify telemetry and workspace breakdown
    let status_json_out = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--status",
            "--socket",
            socket_path.to_str().unwrap(),
            "--json",
        ])
        .current_dir(&base_path)
        .output()
        .await
        .expect("failed to execute cortx daemon --status --json");
    assert!(status_json_out.status.success());
    let json_str = String::from_utf8_lossy(&status_json_out.stdout);
    let status: DaemonStatusInfo =
        serde_json::from_str(&json_str).expect("status output must be valid JSON");

    assert_eq!(status.hooks_served, 3);
    assert_eq!(status.submissions, 3);
    assert_eq!(status.callgraph_drops, 0);
    assert_eq!(status.ast_drops, 0);
    assert!(
        status.memory_rss_mb > 0.0,
        "process RSS memory must be > 0.0 MB"
    );
    assert_eq!(status.active_workspaces_count, 3);
    assert_eq!(status.workspaces.len(), 3);

    // Verify main daemon log exists and contains logged events
    assert!(status.main_log_path.exists(), "main daemon log must exist");
    assert_file_contains_eventually(&status.main_log_path, "Cortx daemon IPC server started").await;
    assert_file_contains_eventually(&status.main_log_path, "Cold start indexing initiated").await;
    assert_file_contains_eventually(&status.main_log_path, "PostReadHook served").await;

    // Verify symlink daemon-latest.log exists
    let latest_symlink = log_dir.join("daemon-latest.log");
    assert!(
        latest_symlink.exists(),
        "daemon-latest.log pointer must exist"
    );

    // Canonicalize paths for workspace comparison
    let canonical_alpha = std::fs::canonicalize(&repo_alpha).unwrap();
    let canonical_beta = std::fs::canonicalize(&repo_beta).unwrap();
    let canonical_wt = std::fs::canonicalize(&wt_feat1).unwrap();

    let alpha_summary = status
        .workspaces
        .iter()
        .find(|w| w.root == canonical_alpha)
        .expect("repo_alpha must be registered in workspace summary");
    assert!(!alpha_summary.is_worktree);
    assert_eq!(alpha_summary.parent_repo, None);
    assert_eq!(alpha_summary.submissions, 1);
    assert_eq!(alpha_summary.callgraph_drops, 0);
    assert_eq!(alpha_summary.ast_drops, 0);
    assert!(
        alpha_summary.memory_mb > 0.0,
        "alpha memory must be > 0.0 MB"
    );
    assert!(alpha_summary.log_path.exists(), "alpha log file must exist");
    assert_file_contains_eventually(&alpha_summary.log_path, "Initial indexing started").await;
    assert_file_contains_eventually(&alpha_summary.log_path, "PostReadHook served").await;

    let beta_summary = status
        .workspaces
        .iter()
        .find(|w| w.root == canonical_beta)
        .expect("repo_beta must be registered in workspace summary");
    assert!(!beta_summary.is_worktree);
    assert_eq!(beta_summary.parent_repo, None);
    assert_eq!(beta_summary.submissions, 1);
    assert_eq!(beta_summary.callgraph_drops, 0);
    assert_eq!(beta_summary.ast_drops, 0);
    assert!(beta_summary.memory_mb > 0.0, "beta memory must be > 0.0 MB");
    assert!(beta_summary.log_path.exists(), "beta log file must exist");
    assert_file_contains_eventually(&beta_summary.log_path, "Initial indexing started").await;
    assert_file_contains_eventually(&beta_summary.log_path, "PostReadHook served").await;

    let wt_summary = status
        .workspaces
        .iter()
        .find(|w| w.root == canonical_wt)
        .expect("feat1 worktree must be registered in workspace summary");
    assert!(wt_summary.is_worktree);
    assert_eq!(wt_summary.parent_repo, Some(canonical_alpha.clone()));
    assert_eq!(wt_summary.submissions, 1);
    assert_eq!(wt_summary.callgraph_drops, 0);
    assert_eq!(wt_summary.ast_drops, 0);
    assert!(
        wt_summary.memory_mb > 0.0,
        "worktree memory must be > 0.0 MB"
    );
    assert!(wt_summary.log_path.exists(), "worktree log file must exist");
    assert_file_contains_eventually(&wt_summary.log_path, "Overlay indexed").await;
    assert_file_contains_eventually(&wt_summary.log_path, "PostReadHook served").await;

    // 8. Query daemon status via formatted human table
    let status_table_out = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--status",
            "--socket",
            socket_path.to_str().unwrap(),
        ])
        .current_dir(&base_path)
        .output()
        .await
        .expect("failed to execute cortx daemon --status");
    assert!(status_table_out.status.success());
    let table_str = String::from_utf8_lossy(&status_table_out.stdout);
    assert!(table_str.contains("Cortx Daemon Status:  Active"));
    assert!(table_str.contains("Memory RSS:"));
    assert!(table_str.contains("Active Workspaces:    3"));
    assert!(table_str.contains("Active Workspace Registry:"));
    assert!(table_str.contains("Repo"));
    assert!(table_str.contains("WkTr"));
    assert!(table_str.contains("SUBMIT"));
    assert!(table_str.contains("CG_DROP"));
    assert!(table_str.contains("AST_DROP"));
    assert!(table_str.contains("Hooks Served:         3 requests (3 submitted, 0 omitted)"));
    assert!(
        table_str.contains("Drop Telemetry:       0 call graphs pruned, 0 AST skeletons dropped")
    );

    // 9. Gracefully shutdown daemon via --stop
    let stop_out = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--stop",
            "--socket",
            socket_path.to_str().unwrap(),
        ])
        .current_dir(&base_path)
        .output()
        .await
        .expect("failed to execute cortx daemon --stop");
    assert!(stop_out.status.success(), "stop command must succeed");
    let stop_str = String::from_utf8_lossy(&stop_out.stdout);
    assert!(stop_str.contains("successfully terminated"));
    assert!(
        !socket_path.exists(),
        "socket file must be unlinked on shutdown"
    );

    let _ = server_handle.await;
}
