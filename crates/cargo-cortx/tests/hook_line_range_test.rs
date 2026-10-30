//! End-to-end integration tests for Claude Code and Antigravity (AGY) post-read hook
//! line spans, vendor defaults, and daemon token-saving telemetry.

use cargo_cortx::{DaemonArgs, compute_hook_read_metrics, resolve_hook_input};
use cortx_core::span::{AgentVendor, DEFAULT_AGY_BYTE_LIMIT, ReadBudgetEnv};
use cortx_daemon::{
    DaemonRequest, DaemonResponse, DaemonStatusInfo, ServerOptions, run_server_with_options,
    send_daemon_request,
};
use cortx_distill::SymbolIndex;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const TOTAL_LINES: usize = 2500;
const LINE_BYTE_LEN: usize = 60;

struct TestHarness {
    _dir: TempDir,
    socket_path: PathBuf,
    sample_file: PathBuf,
    service_file: PathBuf,
    original_content: String,
    service_content: String,
    token: CancellationToken,
    server_handle: JoinHandle<()>,
}

impl TestHarness {
    async fn start() -> Self {
        let dir = TempDir::new().expect("create temp dir");
        std::fs::create_dir_all(dir.path().join(".git")).expect("create .git dir");
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).expect("create src dir");

        let sample_file = src_dir.join("sample.rs");
        let original_content = generate_rust_source(TOTAL_LINES, LINE_BYTE_LEN);
        std::fs::write(&sample_file, &original_content).expect("write sample.rs");

        let service_file = src_dir.join("service.rs");
        let service_content = "pub fn target_task(id: u64) -> u64 {\n    id * 2\n}\n".to_string();
        std::fs::write(&service_file, &service_content).expect("write service.rs");

        let mut caller_buf = String::from("use crate::service::target_task;\n\n");
        for i in 1..=12 {
            let _ = writeln!(
                caller_buf,
                "pub fn caller_{i:02}() {{ let _ = target_task({i}); }}"
            );
        }
        std::fs::write(src_dir.join("caller.rs"), &caller_buf).expect("write caller.rs");

        let socket_path = dir.path().join("daemon.sock");
        let log_dir = dir.path().join("logs");

        let mut index = SymbolIndex::new();
        let _ = index.index_directory(dir.path());
        let shared_index = Arc::new(RwLock::new(index));
        let token = CancellationToken::new();

        let opts = ServerOptions {
            socket_path: socket_path.clone(),
            workspace_root: dir.path().to_path_buf(),
            watcher_active: false,
            log_dir: Some(log_dir),
            idle_ttl_secs: 60,
            daemon_config: Default::default(),
            config_path: None,
        };

        let server_token = token.clone();
        let server_handle = tokio::spawn(async move {
            let _ = run_server_with_options(opts, shared_index, server_token).await;
        });

        for _ in 0..50 {
            if matches!(
                send_daemon_request(&socket_path, DaemonRequest::Ping).await,
                Ok(DaemonResponse::Pong)
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        Self {
            _dir: dir,
            socket_path,
            sample_file,
            service_file,
            original_content,
            service_content,
            token,
            server_handle,
        }
    }

    async fn status(&self) -> DaemonStatusInfo {
        match send_daemon_request(&self.socket_path, DaemonRequest::Status)
            .await
            .expect("status request")
        {
            DaemonResponse::Status(info) => info,
            other => panic!("unexpected response: {other:?}"),
        }
    }

    async fn shutdown(self) {
        let _ = send_daemon_request(&self.socket_path, DaemonRequest::Shutdown).await;
        self.token.cancel();
        let _ = self.server_handle.await;
    }
}

fn generate_rust_source(lines: usize, line_len: usize) -> String {
    let mut buf = String::with_capacity(lines * line_len);
    for i in 1..=lines {
        let prefix = format!("pub fn item_{i:04}() -> u64 {{ {i} }} // ");
        let pad = line_len.saturating_sub(prefix.len() + 1);
        let _ = writeln!(buf, "{prefix}{}", "x".repeat(pad));
    }
    assert_eq!(buf.len(), lines * line_len);
    buf
}

fn assert_file_unchanged(path: &Path, expected: &str) {
    let on_disk = std::fs::read_to_string(path).expect("read sample file");
    assert_eq!(
        on_disk, expected,
        "advisory post-read hook must keep source file byte-identical on disk"
    );
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

#[tokio::test]
async fn claude_micro_read_records_exact_distill_bytes() {
    let harness = TestHarness::start().await;
    let stdin_json = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Read",
        "tool_input": {
            "file_path": harness.sample_file,
            "offset": 100,
            "limit": 30
        }
    })
    .to_string();

    let args = DaemonArgs {
        hook_post_read: true,
        ..Default::default()
    };
    let (file, span) =
        resolve_hook_input(&args, Some(&stdin_json)).expect("resolve Claude micro-read");
    assert_eq!(span.vendor, Some(AgentVendor::Claude));

    let (start_line, end_line, raw_read_bytes) =
        compute_hook_read_metrics(&file, &span, &ReadBudgetEnv::default());
    assert_eq!(start_line, Some(100));
    assert_eq!(end_line, Some(129));
    assert_eq!(raw_read_bytes, Some((30 * LINE_BYTE_LEN) as u64));

    let resp = send_daemon_request(
        &harness.socket_path,
        DaemonRequest::PostReadHook {
            file: file.clone(),
            start_line,
            end_line,
            raw_read_bytes,
            read_kind: None,
        },
    )
    .await
    .expect("send PostReadHook");
    match resp {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(
                advisory_block.is_empty(),
                "micro-read smaller than full-file skeleton should suppress advisory bloat"
            );
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let status = harness.status().await;
    assert_eq!(status.hooks_served, 1);
    assert_eq!(status.raw_bytes_served, (30 * LINE_BYTE_LEN) as u64);
    assert_eq!(status.delivered_bytes_served, status.raw_bytes_served);
    assert_eq!(status.bytes_saved, 0);
    assert_eq!(status.submissions, 0);
    assert_eq!(status.callgraph_drops, 0);
    assert_eq!(status.ast_drops, 1);
    assert_eq!(status.workspaces.len(), 1);
    assert_eq!(status.workspaces[0].submissions, 0);
    assert_eq!(status.workspaces[0].callgraph_drops, 0);
    assert_eq!(status.workspaces[0].ast_drops, 1);
    assert_file_contains_eventually(
        &status.main_log_path,
        "PostReadHook dropped all advisory context: skeleton",
    )
    .await;
    assert_file_unchanged(&harness.sample_file, &harness.original_content);

    harness.shutdown().await;
}

#[tokio::test]
async fn claude_omitted_bounds_apply_eof_and_token_cap() {
    let harness = TestHarness::start().await;
    let stdin_json = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Read",
        "tool_input": {
            "file_path": harness.sample_file
        }
    })
    .to_string();

    let args = DaemonArgs {
        hook_post_read: true,
        ..Default::default()
    };
    let (file, span) =
        resolve_hook_input(&args, Some(&stdin_json)).expect("resolve Claude default read");
    assert_eq!(span.vendor, Some(AgentVendor::Claude));

    let (start_line, end_line, raw_read_bytes) =
        compute_hook_read_metrics(&file, &span, &ReadBudgetEnv::default());
    assert_eq!(start_line, Some(1));
    assert_eq!(end_line, Some(TOTAL_LINES));
    assert_eq!(raw_read_bytes, Some(100_000));

    let resp = send_daemon_request(
        &harness.socket_path,
        DaemonRequest::PostReadHook {
            file,
            start_line,
            end_line,
            raw_read_bytes,
            read_kind: None,
        },
    )
    .await
    .expect("send PostReadHook");
    match resp {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(
                !advisory_block.is_empty(),
                "default 2000 lines should deliver non-empty skeleton bundle"
            );
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let status = harness.status().await;
    assert_eq!(status.hooks_served, 1);
    assert_eq!(status.submissions, 1);
    assert_eq!(status.callgraph_drops, 0);
    assert_eq!(status.ast_drops, 0);
    assert_eq!(status.workspaces.len(), 1);
    assert_eq!(status.workspaces[0].submissions, 1);
    assert_eq!(status.workspaces[0].callgraph_drops, 0);
    assert_eq!(status.workspaces[0].ast_drops, 0);
    assert_eq!(status.raw_bytes_served, 100_000);
    assert_file_unchanged(&harness.sample_file, &harness.original_content);

    harness.shutdown().await;
}

#[tokio::test]
async fn agy_micro_read_records_exact_distill_bytes() {
    let harness = TestHarness::start().await;
    let stdin_json = serde_json::json!({
        "toolCall": {
            "name": "read_file",
            "args": {
                "Path": harness.sample_file,
                "StartLine": 50,
                "EndLine": 80
            }
        }
    })
    .to_string();

    let args = DaemonArgs {
        hook_post_read: true,
        ..Default::default()
    };
    let (file, span) =
        resolve_hook_input(&args, Some(&stdin_json)).expect("resolve AGY micro-read");
    assert_eq!(span.vendor, Some(AgentVendor::Agy));

    // Lines 50..=80 inclusive = 31 lines
    let expected_bytes = (31 * LINE_BYTE_LEN) as u64;
    let (start_line, end_line, raw_read_bytes) =
        compute_hook_read_metrics(&file, &span, &ReadBudgetEnv::default());
    assert_eq!(start_line, Some(50));
    assert_eq!(end_line, Some(80));
    assert_eq!(raw_read_bytes, Some(expected_bytes));

    let resp = send_daemon_request(
        &harness.socket_path,
        DaemonRequest::PostReadHook {
            file,
            start_line,
            end_line,
            raw_read_bytes,
            read_kind: None,
        },
    )
    .await
    .expect("send PostReadHook");
    match resp {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(
                advisory_block.is_empty(),
                "micro-read smaller than full-file skeleton should suppress advisory bloat"
            );
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let status = harness.status().await;
    assert_eq!(status.hooks_served, 1);
    assert_eq!(status.raw_bytes_served, expected_bytes);
    assert_eq!(status.delivered_bytes_served, expected_bytes);
    assert_eq!(status.bytes_saved, 0);
    assert_eq!(status.submissions, 0);
    assert_eq!(status.callgraph_drops, 0);
    assert_eq!(status.ast_drops, 1);
    assert_file_unchanged(&harness.sample_file, &harness.original_content);

    harness.shutdown().await;
}

#[tokio::test]
async fn agy_omitted_lines_apply_default_800_lines_and_46kb_cap() {
    let harness = TestHarness::start().await;
    let stdin_json = serde_json::json!({
        "toolCall": {
            "name": "read_file",
            "args": {
                "Path": harness.sample_file
            }
        }
    })
    .to_string();

    let args = DaemonArgs {
        hook_post_read: true,
        ..Default::default()
    };
    let (file, span) =
        resolve_hook_input(&args, Some(&stdin_json)).expect("resolve AGY default read");
    assert_eq!(span.vendor, Some(AgentVendor::Agy));

    // 800 lines * 60 bytes = 48,000 bytes, capped at DEFAULT_AGY_BYTE_LIMIT (46,080)
    let (start_line, end_line, raw_read_bytes) =
        compute_hook_read_metrics(&file, &span, &ReadBudgetEnv::default());
    assert_eq!(start_line, Some(1));
    assert_eq!(end_line, Some(800));
    assert_eq!(raw_read_bytes, Some(DEFAULT_AGY_BYTE_LIMIT));

    let resp = send_daemon_request(
        &harness.socket_path,
        DaemonRequest::PostReadHook {
            file,
            start_line,
            end_line,
            raw_read_bytes,
            read_kind: None,
        },
    )
    .await
    .expect("send PostReadHook");
    match resp {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(
                advisory_block.is_empty(),
                "2500-fn skeleton (~67.5KB) exceeds 46KB cap and must be omitted"
            );
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let status = harness.status().await;
    assert_eq!(status.hooks_served, 1);
    assert_eq!(status.submissions, 0);
    assert_eq!(status.callgraph_drops, 0);
    assert_eq!(status.ast_drops, 1);
    assert_eq!(status.raw_bytes_served, DEFAULT_AGY_BYTE_LIMIT);
    assert_file_unchanged(&harness.sample_file, &harness.original_content);

    harness.shutdown().await;
}

#[tokio::test]
async fn legacy_post_read_hook_without_line_info_falls_back_to_file_size() {
    let harness = TestHarness::start().await;

    let legacy_json = serde_json::json!({
        "PostReadHook": {
            "file": harness.sample_file
        }
    })
    .to_string();
    let req: DaemonRequest =
        serde_json::from_str(&legacy_json).expect("deserialize legacy PostReadHook");

    let resp = send_daemon_request(&harness.socket_path, req)
        .await
        .expect("send legacy PostReadHook");
    assert!(matches!(resp, DaemonResponse::AdvisoryContext { .. }));

    let status = harness.status().await;
    assert_eq!(status.hooks_served, 1);
    assert_eq!(status.submissions, 1);
    assert_eq!(status.callgraph_drops, 0);
    assert_eq!(status.ast_drops, 0);
    assert_eq!(
        status.raw_bytes_served,
        (TOTAL_LINES * LINE_BYTE_LEN) as u64
    );
    assert_file_unchanged(&harness.sample_file, &harness.original_content);

    harness.shutdown().await;
}

#[tokio::test]
async fn two_stage_adaptive_bloat_guard_stages() {
    let harness = TestHarness::start().await;

    // --- Stage 1: Call graph pruning when bundle exceeds raw read but skeleton fits ---
    // Full bundle has 12 callers (~950 bytes), skeleton alone is ~175 bytes.
    // Reading 350 bytes triggers Stage 1 (pruning callers, keeping skeleton).
    let resp1 = send_daemon_request(
        &harness.socket_path,
        DaemonRequest::PostReadHook {
            file: harness.service_file.clone(),
            start_line: Some(1),
            end_line: Some(5),
            raw_read_bytes: Some(350),
            read_kind: None,
        },
    )
    .await
    .expect("send PostReadHook for Stage 1");

    match resp1 {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(
                !advisory_block.is_empty(),
                "Stage 1 should deliver skeleton"
            );
            assert!(
                advisory_block.contains("target_task"),
                "skeleton should contain target_task signature"
            );
            assert!(
                !advisory_block.contains("## Upstream Callers"),
                "call graph should be stripped in Stage 1"
            );
            assert!(
                !advisory_block.contains("caller_01"),
                "individual caller edges should not appear in Stage 1"
            );
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let status1 = harness.status().await;
    assert_eq!(status1.hooks_served, 1);
    assert_eq!(status1.submissions, 1);
    assert_eq!(status1.callgraph_drops, 1);
    assert_eq!(status1.ast_drops, 0);
    assert_eq!(status1.workspaces.len(), 1);
    assert_eq!(status1.workspaces[0].submissions, 1);
    assert_eq!(status1.workspaces[0].callgraph_drops, 1);
    assert_eq!(status1.workspaces[0].ast_drops, 0);
    assert_file_contains_eventually(
        &status1.main_log_path,
        "PostReadHook pruned call graph: bundle",
    )
    .await;

    // --- Stage 2: Complete AST omission when micro-read is smaller than skeleton ---
    // Reading 30 bytes (< skeleton ~175 bytes) triggers Stage 2 (omission).
    let resp2 = send_daemon_request(
        &harness.socket_path,
        DaemonRequest::PostReadHook {
            file: harness.service_file.clone(),
            start_line: Some(1),
            end_line: Some(2),
            raw_read_bytes: Some(30),
            read_kind: None,
        },
    )
    .await
    .expect("send PostReadHook for Stage 2");

    match resp2 {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(
                advisory_block.is_empty(),
                "Stage 2 micro-read should omit all advisory context"
            );
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let status2 = harness.status().await;
    assert_eq!(status2.hooks_served, 2);
    assert_eq!(status2.submissions, 1);
    assert_eq!(status2.callgraph_drops, 1);
    assert_eq!(status2.ast_drops, 1);
    assert_eq!(status2.workspaces[0].submissions, 1);
    assert_eq!(status2.workspaces[0].callgraph_drops, 1);
    assert_eq!(status2.workspaces[0].ast_drops, 1);
    assert_file_contains_eventually(
        &status2.main_log_path,
        "PostReadHook dropped all advisory context: skeleton",
    )
    .await;

    // --- Stage 0: Full bundle delivery when raw read accommodates complete bundle ---
    // Reading 3000 bytes (> full bundle ~950 bytes) triggers Stage 0 (full delivery).
    let resp3 = send_daemon_request(
        &harness.socket_path,
        DaemonRequest::PostReadHook {
            file: harness.service_file.clone(),
            start_line: Some(1),
            end_line: Some(50),
            raw_read_bytes: Some(3000),
            read_kind: None,
        },
    )
    .await
    .expect("send PostReadHook for Stage 0");

    match resp3 {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(
                !advisory_block.is_empty(),
                "Stage 0 should deliver full bundle"
            );
            assert!(
                advisory_block.contains("target_task"),
                "should contain target_task signature"
            );
            assert!(
                advisory_block.contains("## Upstream Callers"),
                "full bundle in Stage 0 should include callers"
            );
            assert!(
                advisory_block.contains("caller_01"),
                "should include individual caller edges"
            );
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let status3 = harness.status().await;
    assert_eq!(status3.hooks_served, 3);
    assert_eq!(status3.submissions, 2);
    assert_eq!(status3.callgraph_drops, 1);
    assert_eq!(status3.ast_drops, 1);
    assert_eq!(status3.workspaces[0].submissions, 2);
    assert_eq!(status3.workspaces[0].callgraph_drops, 1);
    assert_eq!(status3.workspaces[0].ast_drops, 1);

    // Target files on disk must remain 100% byte-identical
    assert_file_unchanged(&harness.service_file, &harness.service_content);

    harness.shutdown().await;
}

#[tokio::test]
async fn test_explicit_zero_byte_budget_drops_advisory() {
    let harness = TestHarness::start().await;

    let resp = send_daemon_request(
        &harness.socket_path,
        DaemonRequest::PostReadHook {
            file: harness.service_file.clone(),
            start_line: None,
            end_line: None,
            raw_read_bytes: Some(0),
            read_kind: None,
        },
    )
    .await
    .expect("send PostReadHook with 0 budget");

    match resp {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(
                advisory_block.is_empty(),
                "0-byte budget must drop advisory context completely"
            );
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let status = harness.status().await;
    assert_eq!(status.hooks_served, 1);
    assert_eq!(status.submissions, 0);
    assert_eq!(status.callgraph_drops, 0);
    assert_eq!(status.ast_drops, 1);

    harness.shutdown().await;
}

#[tokio::test]
async fn test_legacy_none_budget_on_tiny_file_drops_advisory() {
    let harness = TestHarness::start().await;
    let tiny_file = harness._dir.path().join("src/tiny.rs");
    std::fs::write(&tiny_file, "pub fn t() {}\n").expect("write tiny.rs");

    let resp = send_daemon_request(
        &harness.socket_path,
        DaemonRequest::PostReadHook {
            file: tiny_file,
            start_line: None,
            end_line: None,
            raw_read_bytes: None,
            read_kind: None,
        },
    )
    .await
    .expect("send PostReadHook with legacy None budget");

    match resp {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(
                advisory_block.is_empty(),
                "legacy None budget on tiny file must fall back to file size and drop advisory"
            );
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let status = harness.status().await;
    assert_eq!(status.hooks_served, 1);
    assert_eq!(status.submissions, 0);
    assert_eq!(status.callgraph_drops, 0);
    assert_eq!(status.ast_drops, 1);

    harness.shutdown().await;
}

#[tokio::test]
async fn test_no_call_graph_oversized_bundle_drops_not_prunes() {
    let harness = TestHarness::start().await;
    let standalone_file = harness._dir.path().join("src/standalone.rs");
    std::fs::write(
        &standalone_file,
        "pub struct Standalone;\nimpl Standalone { pub fn op(&self) {} }\n",
    )
    .expect("write standalone.rs");

    // Reading only 10 bytes: smaller than skeleton, and file has no call graph
    let resp = send_daemon_request(
        &harness.socket_path,
        DaemonRequest::PostReadHook {
            file: standalone_file,
            start_line: Some(1),
            end_line: Some(2),
            raw_read_bytes: Some(10),
            read_kind: None,
        },
    )
    .await
    .expect("send PostReadHook for no-callgraph oversized bundle");

    match resp {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(
                advisory_block.is_empty(),
                "oversized bundle with no call graph must drop without pruning"
            );
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let status = harness.status().await;
    assert_eq!(status.hooks_served, 1);
    assert_eq!(status.submissions, 0);
    assert_eq!(status.callgraph_drops, 0);
    assert_eq!(status.ast_drops, 1);

    harness.shutdown().await;
}
