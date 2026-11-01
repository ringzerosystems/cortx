//! End-to-end integration test verifying daemon observability, CLI control commands,
//! and out-of-band context provenance headers.

use cortx_daemon::{DaemonStatusInfo, ServerOptions, run_server_with_options};
use cortx_distill::SymbolIndex;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::process::Command;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

fn cortx_cmd(bin: &str) -> Command {
    let mut cmd = Command::new(bin);
    cmd.env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_CHILD_SESSION")
        .env_remove("CLAUDE_CODE_FILE_READ_MAX_OUTPUT_TOKENS");
    cmd
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_daemon_observability_lifecycle() {
    let cortx_bin = env!("CARGO_BIN_EXE_cortx");
    let base = tempdir().unwrap();
    let src_dir = base.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    let test_file = src_dir.join("lib.rs");
    let test_content = br#"pub struct Service {
    pub name: String,
    pub retries: u32,
    pub timeout_ms: u64,
}

pub trait Runner {
    fn run(&self) -> bool;
    fn validate(&self) -> Result<(), String>;
    fn execute_batch(&self, items: &[String]) -> usize;
}

impl Runner for Service {
    fn run(&self) -> bool {
        let is_valid = !self.name.is_empty();
        if is_valid {
            println!("Service {} running with timeout {}", self.name, self.timeout_ms);
            true
        } else {
            eprintln!("Invalid service name configuration");
            false
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.retries > 10 {
            return Err("Too many retries configured for service worker".to_string());
        }
        if self.timeout_ms < 100 {
            return Err("Timeout configured is too low for reliable execution".to_string());
        }
        Ok(())
    }

    fn execute_batch(&self, items: &[String]) -> usize {
        let mut count = 0;
        for item in items {
            if !item.is_empty() {
                count += 1;
                println!("Processing batch item {} on service {}", item, self.name);
            }
        }
        count
    }
}
"#;
    std::fs::write(&test_file, test_content).unwrap();

    let socket_path = base.path().join("cortx_obs.sock");
    let mut initial_index = SymbolIndex::new();
    initial_index.index_directory(base.path()).unwrap();
    let index = Arc::new(RwLock::new(initial_index));
    let token = CancellationToken::new();

    let s_path = socket_path.clone();
    let s_token = token.clone();
    let s_root = base.path().to_path_buf();
    let server_handle = tokio::spawn(async move {
        let opts = ServerOptions {
            socket_path: s_path,
            workspace_root: s_root,
            watcher_active: false,
            log_dir: None,
            idle_ttl_secs: 7200,
            daemon_config: Default::default(),
            config_path: None,
        };
        let _ = run_server_with_options(opts, index, s_token).await;
    });

    // Allow server socket to bind
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // 1. Probe liveness via --ping
    let ping_out = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--ping",
            "--socket",
            socket_path.to_str().unwrap(),
        ])
        .current_dir(base.path())
        .output()
        .await
        .expect("failed to execute cortx daemon --ping");
    assert!(ping_out.status.success(), "ping must succeed");
    let ping_str = String::from_utf8_lossy(&ping_out.stdout);
    assert!(ping_str.contains("[Daemon] Daemon is responsive on"));
    assert!(ping_str.contains("round-trip:"));

    // 2. Query initial status via --status (human table)
    let status_out = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--status",
            "--socket",
            socket_path.to_str().unwrap(),
        ])
        .current_dir(base.path())
        .output()
        .await
        .expect("failed to execute cortx daemon --status");
    assert!(status_out.status.success(), "status must succeed");
    let status_str = String::from_utf8_lossy(&status_out.stdout);
    assert!(status_str.contains("Cortx Daemon Status:  Active"));
    assert!(status_str.contains("Version / Protocol:   v"));
    assert!(status_str.contains("protocol v2"));
    assert!(status_str.contains("PID:"));
    assert!(status_str.contains("Uptime:"));
    assert!(status_str.contains("Indexed Files:        1 files"));
    assert!(status_str.contains("Total Symbols:"));
    assert!(status_str.contains("Hooks Served:         0 requests"));
    assert!(status_str.contains("Supplemental Context: 0 B (no requests served yet)"));

    // 3. Query initial status via --status --json: verify 0 hooks served initially
    let status_json_out = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--status",
            "--socket",
            socket_path.to_str().unwrap(),
            "--json",
        ])
        .current_dir(base.path())
        .output()
        .await
        .expect("failed to execute cortx daemon --status --json");
    assert!(status_json_out.status.success());
    let json_str = String::from_utf8_lossy(&status_json_out.stdout);
    let status_info: DaemonStatusInfo =
        serde_json::from_str(&json_str).expect("status must be valid JSON");
    assert_eq!(status_info.protocol_version, cortx_daemon::PROTOCOL_VERSION);
    assert_eq!(status_info.indexed_files, 1);
    assert!(status_info.total_symbols >= 2);
    assert_eq!(status_info.pid, std::process::id());
    assert_eq!(status_info.hooks_served, 0);
    assert_eq!(status_info.macro_reads, 0);
    assert_eq!(status_info.micro_reads, 0);
    assert_eq!(status_info.supplemental_context_bytes, 0);
    assert_eq!(status_info.supplemental_context_tokens, 0);

    // 4. Hook post-read with live daemon (provenance: daemon)
    let hook_daemon_out = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--hook-post-read",
            "--file",
            test_file.to_str().unwrap(),
            "--socket",
            socket_path.to_str().unwrap(),
        ])
        .current_dir(base.path())
        .output()
        .await
        .expect("failed to execute cortx daemon --hook-post-read");
    assert!(hook_daemon_out.status.success());
    let hook_daemon_str = String::from_utf8_lossy(&hook_daemon_out.stdout);
    assert!(
        hook_daemon_str.contains("<!-- cortx-context-provenance: daemon (socket:"),
        "expected daemon provenance header, got:\n{}",
        hook_daemon_str
    );
    assert!(hook_daemon_str.contains("# Cortx Context Distillation:"));

    // 5. Query status post-hook via --status --json: verify telemetry incremented
    let status_post_json_out = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--status",
            "--socket",
            socket_path.to_str().unwrap(),
            "--json",
        ])
        .current_dir(base.path())
        .output()
        .await
        .expect("failed to execute cortx daemon --status --json post-hook");
    assert!(status_post_json_out.status.success());
    let post_json_str = String::from_utf8_lossy(&status_post_json_out.stdout);
    let post_status_info: DaemonStatusInfo =
        serde_json::from_str(&post_json_str).expect("post-hook status must be valid JSON");
    assert_eq!(post_status_info.hooks_served, 1);
    assert!(post_status_info.raw_bytes_served > 0);
    assert!(post_status_info.delivered_bytes_served > 0);
    assert_eq!(post_status_info.macro_reads, 1);
    assert_eq!(post_status_info.micro_reads, 0);
    assert!(post_status_info.supplemental_context_bytes > 0);
    assert!(post_status_info.supplemental_context_tokens > 0);

    // 6. Query status post-hook via human table: verify formatted telemetry lines
    let status_post_table_out = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--status",
            "--socket",
            socket_path.to_str().unwrap(),
        ])
        .current_dir(base.path())
        .output()
        .await
        .expect("failed to execute cortx daemon --status post-hook");
    assert!(status_post_table_out.status.success());
    let post_table_str = String::from_utf8_lossy(&status_post_table_out.stdout);
    assert!(post_table_str.contains("Hooks Served:         1 requests"));
    assert!(post_table_str.contains("Read Operations:"));
    assert!(post_table_str.contains("1 macro, 0 micro"));
    assert!(post_table_str.contains("Supplemental Context:"));

    // 6b. Issue a micro-read hook call (offset: 10, limit: 5) and verify micro_reads connects to daemon
    let hook_micro_out = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--hook-post-read",
            "--file",
            test_file.to_str().unwrap(),
            "--offset",
            "10",
            "--limit",
            "5",
            "--socket",
            socket_path.to_str().unwrap(),
        ])
        .current_dir(base.path())
        .output()
        .await
        .expect("failed to execute cortx daemon --hook-post-read micro");
    assert!(hook_micro_out.status.success());
    assert!(
        hook_micro_out.stdout.is_empty(),
        "micro-read must return empty advisory output"
    );

    let status_post_micro_json_out = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--status",
            "--socket",
            socket_path.to_str().unwrap(),
            "--json",
        ])
        .current_dir(base.path())
        .output()
        .await
        .expect("failed to execute cortx daemon --status --json post-micro");
    assert!(status_post_micro_json_out.status.success());
    let post_micro_json_str = String::from_utf8_lossy(&status_post_micro_json_out.stdout);
    let post_micro_status: DaemonStatusInfo =
        serde_json::from_str(&post_micro_json_str).expect("post-micro status must be valid JSON");
    assert_eq!(post_micro_status.hooks_served, 2);
    assert_eq!(post_micro_status.macro_reads, 1);
    assert_eq!(post_micro_status.micro_reads, 1);
    assert_eq!(post_micro_status.omitted_micro_reads, 1);
    assert!(
        post_micro_status
            .workspaces
            .iter()
            .any(|ws| ws.macro_reads == 1 && ws.micro_reads == 1 && ws.omitted_micro_reads == 1),
        "expected at least one workspace to record 1 macro and 1 micro read"
    );

    // 6c. Verify session deduplication for repeated macro reads
    let hook_macro_sess1 = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--hook-post-read",
            "--file",
            test_file.to_str().unwrap(),
            "--session-id",
            "test-obs-session",
            "--socket",
            socket_path.to_str().unwrap(),
        ])
        .current_dir(base.path())
        .output()
        .await
        .expect("macro read with session");
    assert!(hook_macro_sess1.status.success());
    let hook_sess1_str = String::from_utf8_lossy(&hook_macro_sess1.stdout);
    assert!(
        hook_sess1_str.contains("<!-- cortx-context-provenance: daemon"),
        "initial session macro read must deliver advisory context"
    );

    // Repeated macro read to same file with same session-id -> omitted!
    let hook_macro_sess2 = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--hook-post-read",
            "--file",
            test_file.to_str().unwrap(),
            "--session-id",
            "test-obs-session",
            "--socket",
            socket_path.to_str().unwrap(),
        ])
        .current_dir(base.path())
        .output()
        .await
        .expect("repeat macro read with session");
    assert!(hook_macro_sess2.status.success());
    assert!(
        hook_macro_sess2.stdout.is_empty(),
        "repeated session macro read must be omitted to prevent context flooding"
    );

    let status_post_repeat_json = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--status",
            "--socket",
            socket_path.to_str().unwrap(),
            "--json",
        ])
        .current_dir(base.path())
        .output()
        .await
        .expect("status post repeat");
    assert!(status_post_repeat_json.status.success());
    let post_repeat_status: DaemonStatusInfo =
        serde_json::from_str(&String::from_utf8_lossy(&status_post_repeat_json.stdout))
            .expect("valid JSON status post repeat");
    assert_eq!(post_repeat_status.omitted_repeat_reads, 1);

    let status_post_repeat_table = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--status",
            "--socket",
            socket_path.to_str().unwrap(),
        ])
        .current_dir(base.path())
        .output()
        .await
        .expect("status table post repeat");
    assert!(status_post_repeat_table.status.success());
    let post_repeat_table_str = String::from_utf8_lossy(&status_post_repeat_table.stdout);
    assert!(post_repeat_table_str.contains("1 repeats omitted"));

    // 7. Graceful shutdown via --stop
    let stop_out = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--stop",
            "--socket",
            socket_path.to_str().unwrap(),
        ])
        .current_dir(base.path())
        .output()
        .await
        .expect("failed to execute cortx daemon --stop");
    assert!(stop_out.status.success(), "stop must succeed");
    let stop_str = String::from_utf8_lossy(&stop_out.stdout);
    assert!(stop_str.contains("[Daemon] Daemon on"));
    assert!(stop_str.contains("successfully terminated."));
    assert!(!socket_path.exists(), "socket must be unlinked");
    let _ = server_handle.await;

    // 6. Hook post-read post-shutdown (provenance: in-process-fallback)
    let file_bytes_before = std::fs::read(&test_file).unwrap();
    let hook_fallback_out = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--hook-post-read",
            "--file",
            test_file.to_str().unwrap(),
            "--socket",
            socket_path.to_str().unwrap(),
        ])
        .current_dir(base.path())
        .output()
        .await
        .expect("failed to execute cortx daemon --hook-post-read after shutdown");
    assert!(hook_fallback_out.status.success());
    let hook_fallback_str = String::from_utf8_lossy(&hook_fallback_out.stdout);
    assert!(
        hook_fallback_str.contains(
            "<!-- cortx-context-provenance: in-process-fallback (daemon unreachable) -->"
        ),
        "expected in-process fallback header, got:\n{}",
        hook_fallback_str
    );
    assert!(hook_fallback_str.contains("# Cortx Context Distillation:"));

    // Critical invariant: source file on disk is strictly byte-identical
    let file_bytes_after = std::fs::read(&test_file).unwrap();
    assert_eq!(
        file_bytes_before, file_bytes_after,
        "CRITICAL INVARIANT: target file on disk modified during hook execution"
    );

    // 7. Ping failure when daemon is offline
    let ping_offline_out = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--ping",
            "--socket",
            socket_path.to_str().unwrap(),
        ])
        .current_dir(base.path())
        .output()
        .await
        .expect("failed to execute cortx daemon --ping offline");
    assert!(
        !ping_offline_out.status.success(),
        "ping should fail when offline"
    );
}

#[tokio::test]
async fn test_in_process_fallback_bloat_guard() {
    let cortx_bin = env!("CARGO_BIN_EXE_cortx");
    let base = tempdir().unwrap();
    let src_dir = base.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    let nonexistent_socket = base.path().join("nonexistent.sock");

    // Case 1: Micro-read on tiny file drops context completely (Stage 2)
    let tiny_file = src_dir.join("tiny.rs");
    let tiny_content = b"pub fn a() {}\npub fn b() {}\n";
    std::fs::write(&tiny_file, tiny_content).unwrap();

    let file_bytes_before = std::fs::read(&tiny_file).unwrap();
    let out_tiny = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--hook-post-read",
            "--file",
            tiny_file.to_str().unwrap(),
            "--start-line",
            "1",
            "--end-line",
            "2",
            "--socket",
            nonexistent_socket.to_str().unwrap(),
        ])
        .current_dir(base.path())
        .output()
        .await
        .expect("failed to execute cortx daemon --hook-post-read");

    assert!(out_tiny.status.success());
    let stdout_tiny = String::from_utf8_lossy(&out_tiny.stdout);
    assert!(
        stdout_tiny.trim().is_empty(),
        "expected empty stdout when micro-read < skeleton in fallback, got:\n{}",
        stdout_tiny
    );

    let file_bytes_after = std::fs::read(&tiny_file).unwrap();
    assert_eq!(
        file_bytes_before, file_bytes_after,
        "CRITICAL INVARIANT: target file on disk modified during hook execution"
    );

    // Case 2: Realistic file with trait contracts delivers contracts on FullFile fallback
    let realistic_file = src_dir.join("service.rs");
    let mut realistic_content = String::from(
        "pub trait WorkerTask {\n    fn run_task(&self) -> u64;\n}\n\npub struct Worker {\n    pub id: u64,\n}\n\nimpl WorkerTask for Worker {\n    fn run_task(&self) -> u64 { self.task_0() }\n}\n\nimpl Worker {\n",
    );
    for i in 0..30 {
        use std::fmt::Write;
        let _ = writeln!(
            realistic_content,
            "    pub fn task_{i}(&self) -> u64 {{\n        let x = {i} * 2;\n        println!(\"task {i}\");\n        x\n    }}"
        );
    }
    realistic_content.push_str("}\n");
    std::fs::write(&realistic_file, realistic_content.as_bytes()).unwrap();

    let real_bytes_before = std::fs::read(&realistic_file).unwrap();
    let out_real = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--hook-post-read",
            "--file",
            realistic_file.to_str().unwrap(),
            "--socket",
            nonexistent_socket.to_str().unwrap(),
        ])
        .current_dir(base.path())
        .output()
        .await
        .expect("failed to execute cortx daemon --hook-post-read");

    assert!(out_real.status.success());
    let stdout_real = String::from_utf8_lossy(&out_real.stdout);
    assert!(
        stdout_real.contains(
            "<!-- cortx-context-provenance: in-process-fallback (daemon unreachable) -->"
        ),
        "expected in-process fallback header, got:\n{}",
        stdout_real
    );
    assert!(
        stdout_real.contains("impl WorkerTask for Worker"),
        "expected trait contract in output, got:\n{}",
        stdout_real
    );
    assert!(
        !stdout_real.contains("pub fn task_00"),
        "expected self-skeleton omitted on FullFile read, got:\n{}",
        stdout_real
    );

    // Verify standalone offline fallback does NOT spawn a daemon
    assert!(
        !nonexistent_socket.exists(),
        "standalone offline fallback with explicit socket must not spawn daemon"
    );

    let real_bytes_after = std::fs::read(&realistic_file).unwrap();
    assert_eq!(
        real_bytes_before, real_bytes_after,
        "CRITICAL INVARIANT: target file on disk modified during hook execution"
    );
}

#[tokio::test]
async fn test_protocol_mismatch_fail_open_warning() {
    let cortx_bin = env!("CARGO_BIN_EXE_cortx");
    let base = tempdir().unwrap();
    let src_dir = base.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    let test_file = src_dir.join("calc.rs");
    let test_content = br#"pub trait Calculator {
    fn compute(&self, val: u64) -> u64;
}

pub struct SimpleCalc;

impl Calculator for SimpleCalc {
    fn compute(&self, val: u64) -> u64 {
        val * 42
    }
}
"#;
    std::fs::write(&test_file, test_content).unwrap();

    let mock_socket = base.path().join("mock_v1.sock");
    let listener = tokio::net::UnixListener::bind(&mock_socket).expect("bind mock v1 socket");

    // Spawn mock server returning protocol_version: 0 (simulating v0 or v1 daemon)
    let server_task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let Ok(req) =
                cortx_daemon::read_message::<_, cortx_daemon::DaemonRequest>(&mut stream).await
            else {
                // Ignore probe connections that immediately close (e.g. is_socket_alive)
                continue;
            };
            match req {
                cortx_daemon::DaemonRequest::PostReadHook { file, .. } => {
                    let resp = cortx_daemon::DaemonResponse::AdvisoryContext {
                        file: file.display().to_string(),
                        advisory_block: "# Stale Mock Context".to_string(),
                        protocol_version: 0, // Mock protocol v0/v1 mismatch!
                    };
                    let _ = cortx_daemon::write_message(&mut stream, &resp).await;
                }
                cortx_daemon::DaemonRequest::Status => {
                    let resp = cortx_daemon::DaemonResponse::Status(DaemonStatusInfo {
                        protocol_version: 0,
                        daemon_version: "0.0.1".to_string(),
                        pid: std::process::id(),
                        uptime_secs: 120,
                        socket_path: PathBuf::from("/mock/socket.sock"),
                        daemon_mode: "System-Wide Multi-Tenant".to_string(),
                        config_path: None,
                        declared_projects_count: 0,
                        main_log_path: PathBuf::from("/tmp/mock.log"),
                        memory_rss_mb: 10.0,
                        active_workspaces_count: 1,
                        workspaces: Vec::new(),
                        workspace_root: PathBuf::new(),
                        indexed_files: 1,
                        total_symbols: 2,
                        total_call_edges: 0,
                        total_contracts: 1,
                        total_routes: 0,
                        watcher_active: false,
                        hooks_served: 1,
                        submissions: 0,
                        callgraph_drops: 0,
                        ast_drops: 0,
                        raw_bytes_served: 0,
                        delivered_bytes_served: 0,
                        macro_reads: 0,
                        micro_reads: 0,
                        omitted_micro_reads: 0,
                        omitted_repeat_reads: 0,
                        omitted_full_read_skeletons: 0,
                        trimmed_advisories: 0,
                        parse_fallbacks: 0,
                        supplemental_context_bytes: 0,
                        supplemental_context_tokens: 0,
                    });
                    let _ = cortx_daemon::write_message(&mut stream, &resp).await;
                }
                cortx_daemon::DaemonRequest::Shutdown => {
                    let _ = cortx_daemon::write_message(
                        &mut stream,
                        &cortx_daemon::DaemonResponse::ShutdownAck,
                    )
                    .await;
                    break;
                }
                _ => {}
            }
        }
    });

    // 1. Invoke --hook-post-read: CLI detects protocol_version: 0 != 2,
    // logs warning to stderr, and fails open to in-process distillation.
    let hook_out = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--hook-post-read",
            "--file",
            test_file.to_str().unwrap(),
            "--socket",
            mock_socket.to_str().unwrap(),
        ])
        .current_dir(base.path())
        .output()
        .await
        .expect("execute hook with mock v1 socket");

    assert!(
        hook_out.status.success(),
        "hook must succeed via fail-open fallback"
    );
    let stderr_str = String::from_utf8_lossy(&hook_out.stderr);
    assert!(
        stderr_str.contains("Warning: daemon on"),
        "expected protocol mismatch warning in stderr, got:\n{}",
        stderr_str
    );
    assert!(
        stderr_str.contains("is running protocol version 0 (CLI expects protocol version 2); falling back to in-process distillation"),
        "expected specific mismatch text in stderr, got:\n{}",
        stderr_str
    );

    let stdout_str = String::from_utf8_lossy(&hook_out.stdout);
    assert!(
        stdout_str
            .contains("<!-- cortx-context-provenance: in-process-fallback (protocol mismatch) -->"),
        "expected protocol mismatch fallback provenance header, got:\n{}",
        stdout_str
    );
    assert!(
        stdout_str.contains("impl Calculator for SimpleCalc"),
        "expected in-process distillation context, got:\n{}",
        stdout_str
    );
    assert!(
        !stdout_str.contains("# Stale Mock Context"),
        "must not use stale mock daemon context"
    );

    // 2. Query status: CLI detects protocol mismatch, warns on stderr, prints status table without failure
    let status_out = cortx_cmd(cortx_bin)
        .args([
            "daemon",
            "--status",
            "--socket",
            mock_socket.to_str().unwrap(),
        ])
        .current_dir(base.path())
        .output()
        .await
        .expect("execute status with mock v1 socket");

    assert!(status_out.status.success(), "status query must succeed");
    let status_stderr = String::from_utf8_lossy(&status_out.stderr);
    assert!(
        status_stderr.contains("Warning: daemon on"),
        "expected status protocol mismatch warning in stderr, got:\n{}",
        status_stderr
    );
    assert!(
        status_stderr.contains("is running protocol version 0 (CLI expects protocol version 2). Restart daemon with 'cargo cortx daemon --stop'."),
        "expected restart instruction in stderr, got:\n{}",
        status_stderr
    );

    // 3. Verify server was NOT killed by CLI
    assert!(
        mock_socket.exists(),
        "mock socket must not be unlinked or terminated by CLI"
    );

    server_task.abort();
}
