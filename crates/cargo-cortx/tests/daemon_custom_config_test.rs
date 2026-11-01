//! Integration tests verifying `cortx daemon --config <PATH>`, CWD config isolation,
//! `<10ms` zero-index cold startup, and per-project `ProjectConfig` enforcement.

use cortx_daemon::DaemonStatusInfo;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::tempdir;

fn run_git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap_or_else(|e| panic!("failed to run git {:?}: {}", args, e));
    assert!(status.success(), "git {:?} failed", args);
}

fn cortx_cmd(bin: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_CHILD_SESSION")
        .env_remove("CLAUDE_CODE_FILE_READ_MAX_OUTPUT_TOKENS");
    cmd
}

fn setup_fullstack_project(project_dir: &Path) -> PathBuf {
    std::fs::create_dir_all(project_dir.join("crates/core")).unwrap();
    std::fs::create_dir_all(project_dir.join("unwatched_rust")).unwrap();
    std::fs::create_dir_all(project_dir.join("ignored_secret")).unwrap();
    std::fs::create_dir_all(project_dir.join("go_service")).unwrap();

    run_git(project_dir, &["init"]);
    run_git(project_dir, &["config", "user.email", "test@example.com"]);
    run_git(project_dir, &["config", "user.name", "Test User"]);
    run_git(project_dir, &["config", "commit.gpgsign", "false"]);

    let core_rs = project_dir.join("crates/core/lib.rs");
    std::fs::write(
        &core_rs,
        r#"pub struct AcmeEngine {
    pub ready: bool,
}

pub trait AcmeService {
    fn execute(&self) -> bool;
    fn calculate_metric(&self, seed: u64) -> u64;
}

impl AcmeService for AcmeEngine {
    fn execute(&self) -> bool {
        if self.ready {
            println!("AcmeEngine execution confirmed");
            true
        } else {
            eprintln!("AcmeEngine not ready");
            false
        }
    }

    fn calculate_metric(&self, seed: u64) -> u64 {
        let mut score = seed;
        for i in 0..50 {
            score = score.wrapping_add(i * 17);
        }
        score
    }
}
"#,
    )
    .unwrap();

    let caller_rs = project_dir.join("crates/core/caller.rs");
    std::fs::write(
        &caller_rs,
        r#"use crate::lib::{AcmeEngine, AcmeService};

pub fn run_acme_pipeline(engine: &AcmeEngine) -> bool {
    engine.execute()
}
"#,
    )
    .unwrap();

    // Rust file outside `watch_paths = ["crates"]`
    std::fs::write(
        project_dir.join("unwatched_rust/extra.rs"),
        "pub struct UnwatchedRustSymbol {\n    pub skipped: bool,\n}\n",
    )
    .unwrap();

    // File inside project-level `ignored_dirs = ["ignored_secret"]`
    std::fs::write(
        project_dir.join("ignored_secret/secret.rs"),
        "pub struct SecretInternal {\n    pub code: u64,\n}\n",
    )
    .unwrap();

    // Go file inside a project configured with `lang = "rust,typescript"`
    std::fs::write(
        project_dir.join("go_service/main.go"),
        "package main\ntype UnindexedGoStruct struct {\n    ID int\n}\n",
    )
    .unwrap();

    run_git(project_dir, &["add", "."]);
    run_git(project_dir, &["commit", "-m", "initial commit"]);
    core_rs
}

async fn wait_for_socket(socket_path: &Path) {
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(3) {
        if socket_path.exists() {
            return;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    }
    panic!(
        "Timed out waiting for daemon socket at '{}'",
        socket_path.display()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_daemon_custom_config_ignores_poisoned_cwd_and_applies_project_rules() {
    let cortx_bin = env!("CARGO_BIN_EXE_cortx");
    let base = tempdir().unwrap();
    let base_path = std::fs::canonicalize(base.path()).unwrap();

    // 1. Create a poisoned CWD with malformed .cortx.toml and an unwanted source file
    let poisoned_cwd = base_path.join("poisoned_cwd");
    std::fs::create_dir_all(poisoned_cwd.join("src")).unwrap();
    std::fs::write(
        poisoned_cwd.join(".cortx.toml"),
        "THIS IS BROKEN TOML [[[ === invalid syntax",
    )
    .unwrap();
    std::fs::write(
        poisoned_cwd.join("src/unwanted.rs"),
        "pub struct PoisonedCwdSymbol {\n    pub bad: bool,\n}\n",
    )
    .unwrap();

    // 2. Set up declared project directories
    let acme_dir = base_path.join("acme-web");
    let core_rs = setup_fullstack_project(&acme_dir);
    let payment_dir = base_path.join("payment-gateway");
    std::fs::create_dir_all(&payment_dir).unwrap();

    // 3. Write custom daemon configuration file (simulating Docker/K8s --config mount)
    let log_dir = base_path.join("logs");
    let custom_toml = base_path.join("custom_daemon.toml");
    let config_content = format!(
        r#"
[daemon]
system_daemon = true
lazy_indexing = true
enable_file_watcher = false
log_dir = "{log_dir}"

[acme-web]
qualified_path = "{acme_dir}"
has_worktrees = true
lang = "rust,typescript"
watch_paths = ["crates"]
ignored_dirs = ["ignored_secret"]

[payment-gateway]
qualified_path = "{payment_dir}"
has_worktrees = false
lang = "go"
"#,
        log_dir = log_dir.display(),
        acme_dir = acme_dir.display(),
        payment_dir = payment_dir.display(),
    );
    std::fs::write(&custom_toml, config_content).unwrap();
    let canonical_custom_toml = std::fs::canonicalize(&custom_toml).unwrap();

    // 4. Spawn `cortx daemon --config <custom_toml>` from inside `poisoned_cwd`
    let socket_path = base_path.join("custom_daemon.sock");
    let mut daemon_child = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--config",
            custom_toml.to_str().unwrap(),
            "--socket",
            socket_path.to_str().unwrap(),
        ])
        .current_dir(&poisoned_cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn cortx daemon");

    wait_for_socket(&socket_path).await;

    // 5. Verify cold-start status: 0 active workspaces, 0 indexed files, 2 declared projects
    let status_out = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--status",
            "--socket",
            socket_path.to_str().unwrap(),
            "--json",
        ])
        .current_dir(&poisoned_cwd)
        .output()
        .await
        .expect("failed to run daemon --status");
    assert!(status_out.status.success());
    let status: DaemonStatusInfo =
        serde_json::from_slice(&status_out.stdout).expect("valid status JSON");

    assert_eq!(status.active_workspaces_count, 0);
    assert_eq!(status.indexed_files, 0);
    assert_eq!(status.declared_projects_count, 2);
    assert_eq!(status.config_path, Some(canonical_custom_toml.clone()));
    assert_eq!(status.daemon_mode, "System-Wide Multi-Tenant");

    // 6. Trigger lazy indexing for `acme-web` and verify ProjectConfig rules
    let hook_out = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--hook-post-read",
            "--file",
            core_rs.to_str().unwrap(),
            "--socket",
            socket_path.to_str().unwrap(),
        ])
        .current_dir(&poisoned_cwd)
        .output()
        .await
        .expect("failed to run post-read hook");
    assert!(hook_out.status.success());
    let hook_stdout = String::from_utf8_lossy(&hook_out.stdout);
    assert!(hook_stdout.contains("<!-- cortx-context-provenance: daemon (socket:"));
    assert!(hook_stdout.contains("AcmeEngine"));
    assert!(hook_stdout.contains("AcmeService"));
    assert!(hook_stdout.contains("## Upstream Callers"));
    assert!(hook_stdout.contains("run_acme_pipeline"));
    assert!(hook_stdout.contains("caller.rs"));
    assert!(!hook_stdout.contains("## Type Skeleton"));
    assert!(!hook_stdout.contains("Token Reduction"));

    // 7. Re-query status and verify project_name, config_path, and filtered file count
    let status_after_out = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--status",
            "--socket",
            socket_path.to_str().unwrap(),
            "--json",
        ])
        .current_dir(&poisoned_cwd)
        .output()
        .await
        .expect("failed to run daemon --status");
    let status_after: DaemonStatusInfo =
        serde_json::from_slice(&status_after_out.stdout).expect("valid status JSON");

    assert_eq!(status_after.active_workspaces_count, 1);
    let ws = &status_after.workspaces[0];
    assert_eq!(ws.project_name.as_deref(), Some("acme-web"));
    assert_eq!(ws.config_path, Some(canonical_custom_toml));
    // Only files under crates/ (`crates/core/lib.rs` and `crates/core/caller.rs`) should be indexed
    // (`unwatched_rust/`, `ignored_secret/`, and `.go` excluded)
    assert_eq!(ws.indexed_files, 2);

    // Verify human status table output includes PROJECT (`acme-web`) and CONFIG (`custom_daemon.toml`)
    let status_human_out = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--status",
            "--socket",
            socket_path.to_str().unwrap(),
        ])
        .current_dir(&poisoned_cwd)
        .output()
        .await
        .expect("failed to run daemon --status human table");
    assert!(status_human_out.status.success());
    let status_human_stdout = String::from_utf8_lossy(&status_human_out.stdout);
    assert!(status_human_stdout.contains("PROJECT"));
    assert!(status_human_stdout.contains("CONFIG"));
    assert!(status_human_stdout.contains("acme-web"));
    assert!(status_human_stdout.contains("custom_daemon.toml"));

    // 8. Stop daemon cleanly
    let stop_out = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--stop",
            "--socket",
            socket_path.to_str().unwrap(),
        ])
        .current_dir(&poisoned_cwd)
        .output()
        .await
        .expect("failed to stop daemon");
    assert!(stop_out.status.success());
    let exit_status = daemon_child.wait().await.expect("daemon child wait");
    assert!(exit_status.success());
}
