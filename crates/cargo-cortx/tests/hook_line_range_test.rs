//! End-to-end integration tests for Claude Code, Antigravity (AGY), and OpenCode post-read hook
//! line spans, vendor defaults, and daemon token-saving telemetry.

use cargo_cortx::{DaemonArgs, compute_hook_read_metrics, resolve_hook_input};
use cortx_core::ReadKind;
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
    dense_service_file: PathBuf,
    original_content: String,
    service_content: String,
    dense_service_content: String,
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

        // Dense service and caller fixtures for PartialMacro ranked testing
        let dense_service_file = src_dir.join("dense_service.rs");
        let mut dense_service_content = String::new();
        for i in 1..=60 {
            let _ = writeln!(
                dense_service_content,
                "pub fn dense_op_{i:02}() -> u64 {{\n    {i}\n}}\n"
            );
        }
        std::fs::write(&dense_service_file, &dense_service_content)
            .expect("write dense_service.rs");

        let mut dense_caller_buf = String::from("use crate::dense_service::*;\n\n");
        for i in 1..=40 {
            let _ = writeln!(
                dense_caller_buf,
                "pub fn dense_caller_{i:02}() {{ let _ = dense_op_01(); }}"
            );
        }
        std::fs::write(src_dir.join("dense_caller.rs"), &dense_caller_buf)
            .expect("write dense_caller.rs");

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
            dense_service_file,
            original_content,
            service_content,
            dense_service_content,
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

fn assert_mtime_unchanged(path: &Path, before: std::time::SystemTime) {
    let after = std::fs::metadata(path)
        .expect("file metadata")
        .modified()
        .expect("modified time");
    assert_eq!(
        before, after,
        "advisory post-read hook must keep source file mtime strictly untouched"
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
    let mtime_before = std::fs::metadata(&harness.sample_file)
        .expect("metadata")
        .modified()
        .expect("mtime");

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
            read_kind: Some(ReadKind::Micro),
            total_lines: Some(TOTAL_LINES),
            vendor: Some(AgentVendor::Claude),
            is_full_read: Some(false),
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
    assert_eq!(status.delivered_bytes_served, 0);
    assert_eq!(status.submissions, 0);
    assert_eq!(status.micro_reads, 1);
    assert!(status.omitted_micro_reads >= 1);
    assert_eq!(status.workspaces.len(), 1);
    assert_eq!(status.workspaces[0].submissions, 0);
    assert!(status.workspaces[0].omitted_micro_reads >= 1);
    assert_file_contains_eventually(
        &status.main_log_path,
        "[MICRO] PostReadHook skipped (micro-read)",
    )
    .await;
    assert_file_unchanged(&harness.sample_file, &harness.original_content);
    assert_mtime_unchanged(&harness.sample_file, mtime_before);

    harness.shutdown().await;
}

#[tokio::test]
async fn claude_omitted_bounds_apply_eof_and_token_cap() {
    let harness = TestHarness::start().await;
    let mtime_before = std::fs::metadata(&harness.sample_file)
        .expect("metadata")
        .modified()
        .expect("mtime");

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
            read_kind: Some(ReadKind::Macro),
            total_lines: Some(TOTAL_LINES),
            vendor: Some(AgentVendor::Claude),
            is_full_read: Some(false),
        },
    )
    .await
    .expect("send PostReadHook");
    match resp {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            // Full-file read of a standalone leaf file omits self-skeleton and suppresses bloat
            assert!(
                advisory_block.is_empty(),
                "full-file read of leaf file should omit self-skeleton"
            );
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let status = harness.status().await;
    assert_eq!(status.hooks_served, 1);
    assert_eq!(status.macro_reads, 1);
    assert!(status.omitted_full_read_skeletons >= 1);
    assert_eq!(status.workspaces.len(), 1);
    assert_eq!(status.raw_bytes_served, 100_000);
    assert_eq!(status.delivered_bytes_served, 0);
    assert_file_unchanged(&harness.sample_file, &harness.original_content);
    assert_mtime_unchanged(&harness.sample_file, mtime_before);

    harness.shutdown().await;
}

#[tokio::test]
async fn agy_micro_read_records_exact_distill_bytes() {
    let harness = TestHarness::start().await;
    let mtime_before = std::fs::metadata(&harness.sample_file)
        .expect("metadata")
        .modified()
        .expect("mtime");

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
            read_kind: Some(ReadKind::Micro),
            total_lines: Some(TOTAL_LINES),
            vendor: Some(AgentVendor::Agy),
            is_full_read: Some(false),
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
    assert_eq!(status.delivered_bytes_served, 0);
    assert_eq!(status.submissions, 0);
    assert_eq!(status.micro_reads, 1);
    assert!(status.omitted_micro_reads >= 1);
    assert_eq!(status.workspaces[0].omitted_micro_reads, 1);
    assert_file_unchanged(&harness.sample_file, &harness.original_content);
    assert_mtime_unchanged(&harness.sample_file, mtime_before);

    harness.shutdown().await;
}

#[tokio::test]
async fn agy_omitted_lines_apply_default_800_lines_and_46kb_cap() {
    let harness = TestHarness::start().await;
    let mtime_before = std::fs::metadata(&harness.sample_file)
        .expect("metadata")
        .modified()
        .expect("mtime");

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
            read_kind: Some(ReadKind::Macro),
            total_lines: Some(TOTAL_LINES),
            vendor: Some(AgentVendor::Agy),
            is_full_read: Some(false),
        },
    )
    .await
    .expect("send PostReadHook");
    match resp {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(
                !advisory_block.is_empty(),
                "800-line AGY read of 2500-line file should deliver non-empty unread outline"
            );
            assert!(
                advisory_block.chars().count() <= 9_200,
                "delivered advisory must be within 9,200 char wire limit"
            );
            assert!(
                advisory_block.contains("## Unread File Outline"),
                "advisory must include unread outline header"
            );
            assert_eq!(
                advisory_block.matches("```").count() % 2,
                0,
                "code fences must be balanced"
            );
            assert!(
                !advisory_block.contains("Token Reduction"),
                "advisory block must not contain 'Token Reduction'"
            );
            // Declarations outside 1..=800 should be present
            assert!(
                advisory_block.contains("item_0801"),
                "unread outline should contain declarations past line 800"
            );
            // Declarations inside 1..=800 must NOT be repeated
            assert!(
                !advisory_block.contains("item_0001"),
                "already-read declarations in lines 1..=800 must not be repeated"
            );
            assert!(
                !advisory_block.contains("item_0800"),
                "already-read declarations in lines 1..=800 must not be repeated"
            );
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let status = harness.status().await;
    assert_eq!(status.hooks_served, 1);
    assert_eq!(status.macro_reads, 1);
    assert!(status.submissions >= 1);
    assert_eq!(status.raw_bytes_served, DEFAULT_AGY_BYTE_LIMIT);
    assert!(status.delivered_bytes_served > 0);
    assert!(status.delivered_bytes_served <= 9_200);
    assert_file_unchanged(&harness.sample_file, &harness.original_content);
    assert_mtime_unchanged(&harness.sample_file, mtime_before);

    harness.shutdown().await;
}

#[tokio::test]
async fn legacy_post_read_hook_without_line_info_falls_back_to_file_size() {
    let harness = TestHarness::start().await;
    let mtime_before = std::fs::metadata(&harness.sample_file)
        .expect("metadata")
        .modified()
        .expect("mtime");

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
    assert_eq!(status.macro_reads, 1);
    assert_eq!(
        status.raw_bytes_served,
        (TOTAL_LINES * LINE_BYTE_LEN) as u64
    );
    assert_file_unchanged(&harness.sample_file, &harness.original_content);
    assert_mtime_unchanged(&harness.sample_file, mtime_before);

    harness.shutdown().await;
}

#[tokio::test]
async fn three_tier_coverage_micro_drops_advisory() {
    let harness = TestHarness::start().await;
    let mtime_before = std::fs::metadata(&harness.service_file)
        .expect("metadata")
        .modified()
        .expect("mtime");

    // Micro-read (<= 150 lines bounded): lines 1..=50
    let resp = send_daemon_request(
        &harness.socket_path,
        DaemonRequest::PostReadHook {
            file: harness.service_file.clone(),
            start_line: Some(1),
            end_line: Some(50),
            raw_read_bytes: Some(500),
            read_kind: Some(ReadKind::Micro),
            total_lines: Some(1000),
            vendor: Some(AgentVendor::Generic),
            is_full_read: Some(false),
        },
    )
    .await
    .expect("send PostReadHook for Micro");

    match resp {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(
                advisory_block.is_empty(),
                "micro-read tier must return empty string"
            );
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let status = harness.status().await;
    assert_eq!(status.hooks_served, 1);
    assert_eq!(status.micro_reads, 1);
    assert_eq!(status.delivered_bytes_served, 0);
    assert!(status.omitted_micro_reads >= 1);
    assert_file_unchanged(&harness.service_file, &harness.service_content);
    assert_mtime_unchanged(&harness.service_file, mtime_before);

    harness.shutdown().await;
}

#[tokio::test]
async fn three_tier_coverage_full_file_delivers_cross_file_callers_without_skeleton() {
    let harness = TestHarness::start().await;
    let mtime_before = std::fs::metadata(&harness.service_file)
        .expect("metadata")
        .modified()
        .expect("mtime");

    // Full-file read of service.rs (3 lines) which has 12 callers in caller.rs
    let resp = send_daemon_request(
        &harness.socket_path,
        DaemonRequest::PostReadHook {
            file: harness.service_file.clone(),
            start_line: Some(1),
            end_line: Some(3),
            raw_read_bytes: Some(harness.service_content.len() as u64),
            read_kind: Some(ReadKind::Macro),
            total_lines: Some(3),
            vendor: Some(AgentVendor::Generic),
            is_full_read: Some(true),
        },
    )
    .await
    .expect("send PostReadHook for FullFile");

    match resp {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(
                !advisory_block.is_empty(),
                "full-file read with callers must deliver non-empty advisory"
            );
            assert!(
                advisory_block.contains("## Upstream Callers"),
                "full-file advisory must deliver upstream callers"
            );
            assert!(
                advisory_block.contains("caller.rs:"),
                "upstream callers must reference file and line location"
            );
            assert!(
                advisory_block.contains("caller_01"),
                "upstream callers must include caller functions"
            );
            assert!(
                !advisory_block.contains("## Type Skeleton"),
                "full-file advisory must omit target file skeleton"
            );
            assert!(
                !advisory_block.contains("pub fn target_task"),
                "full-file advisory must omit target function definition"
            );
            assert!(
                !advisory_block.contains("Token Reduction"),
                "full-file advisory must omit 'Token Reduction'"
            );
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let status = harness.status().await;
    assert_eq!(status.hooks_served, 1);
    assert_eq!(status.macro_reads, 1);
    assert!(status.submissions >= 1);
    assert!(status.omitted_full_read_skeletons >= 1);
    assert!(status.delivered_bytes_served > 0);
    assert_file_unchanged(&harness.service_file, &harness.service_content);
    assert_mtime_unchanged(&harness.service_file, mtime_before);

    harness.shutdown().await;
}

#[tokio::test]
async fn three_tier_coverage_partial_macro_delivers_ranked_callers_and_unread_outline() {
    let harness = TestHarness::start().await;
    let mtime_before = std::fs::metadata(&harness.dense_service_file)
        .expect("metadata")
        .modified()
        .expect("mtime");

    // Partial macro read (> 150 lines on a 240-line file with 40 callers)
    // Reading lines 1..=160
    let resp = send_daemon_request(
        &harness.socket_path,
        DaemonRequest::PostReadHook {
            file: harness.dense_service_file.clone(),
            start_line: Some(1),
            end_line: Some(160),
            raw_read_bytes: Some(160 * 30),
            read_kind: Some(ReadKind::Macro),
            total_lines: Some(240),
            vendor: Some(AgentVendor::Generic),
            is_full_read: Some(false),
        },
    )
    .await
    .expect("send PostReadHook for PartialMacro");

    match resp {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(
                !advisory_block.is_empty(),
                "partial macro read must deliver non-empty advisory"
            );
            assert!(
                advisory_block.chars().count() <= 9_200,
                "advisory block must be bounded within 9,200 char wire limit"
            );
            assert_eq!(
                advisory_block.matches("```").count() % 2,
                0,
                "code fences must be balanced"
            );
            assert!(
                advisory_block.contains("## Upstream Callers"),
                "advisory must deliver proximity-ranked upstream callers"
            );
            assert!(
                advisory_block.contains("callers omitted"),
                "dense callers (>32) must emit omitted caller suffix"
            );
            assert!(
                advisory_block.contains("## Unread File Outline"),
                "partial macro must deliver unread file outline"
            );
            let outline_part = advisory_block
                .split("## Unread File Outline")
                .nth(1)
                .expect("unread outline section");
            // Declarations past line 160 should be in unread outline
            assert!(
                outline_part.contains("dense_op_45"),
                "outline should include declarations outside read range"
            );
            // Declarations within lines 1..=160 must not be in outline
            assert!(
                !outline_part.contains("dense_op_01"),
                "outline should not repeat declarations from read range"
            );
            assert!(
                !outline_part.contains("dense_op_40"),
                "outline should not repeat declarations from read range"
            );
            assert!(
                !advisory_block.contains("Token Reduction"),
                "advisory block must omit 'Token Reduction'"
            );
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let status = harness.status().await;
    assert_eq!(status.hooks_served, 1);
    assert_eq!(status.macro_reads, 1);
    assert!(status.submissions >= 1);
    assert!(status.delivered_bytes_served > 0);
    assert!(status.delivered_bytes_served <= 9_200);
    assert_file_unchanged(&harness.dense_service_file, &harness.dense_service_content);
    assert_mtime_unchanged(&harness.dense_service_file, mtime_before);

    harness.shutdown().await;
}

#[tokio::test]
async fn opencode_stdin_micro_read_omits_advisory_and_preserves_mtime() {
    let harness = TestHarness::start().await;
    let mtime_before = std::fs::metadata(&harness.sample_file)
        .expect("metadata")
        .modified()
        .expect("mtime");

    let stdin_json = serde_json::json!({
        "tool_name": "read",
        "tool_input": {
            "filePath": harness.sample_file,
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
        resolve_hook_input(&args, Some(&stdin_json)).expect("resolve OpenCode micro-read");
    assert_eq!(span.vendor, Some(AgentVendor::OpenCode));

    let (start_line, end_line, raw_read_bytes) =
        compute_hook_read_metrics(&file, &span, &ReadBudgetEnv::default());
    assert_eq!(start_line, Some(100));
    assert_eq!(end_line, Some(129));
    assert_eq!(raw_read_bytes, Some((30 * LINE_BYTE_LEN) as u64));

    let resp = send_daemon_request(
        &harness.socket_path,
        DaemonRequest::PostReadHook {
            file,
            start_line,
            end_line,
            raw_read_bytes,
            read_kind: Some(ReadKind::Micro),
            total_lines: Some(TOTAL_LINES),
            vendor: Some(AgentVendor::OpenCode),
            is_full_read: Some(false),
        },
    )
    .await
    .expect("send PostReadHook for OpenCode micro");

    match resp {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(
                advisory_block.is_empty(),
                "OpenCode micro-read must omit advisory context"
            );
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let status = harness.status().await;
    assert_eq!(status.hooks_served, 1);
    assert_eq!(status.micro_reads, 1);
    assert_eq!(status.delivered_bytes_served, 0);
    assert!(status.omitted_micro_reads >= 1);
    assert_file_unchanged(&harness.sample_file, &harness.original_content);
    assert_mtime_unchanged(&harness.sample_file, mtime_before);

    harness.shutdown().await;
}

#[tokio::test]
async fn opencode_stdin_full_file_under_2000_lines_delivers_callers_without_skeleton() {
    let harness = TestHarness::start().await;
    let mtime_before = std::fs::metadata(&harness.service_file)
        .expect("metadata")
        .modified()
        .expect("mtime");

    // OpenCode reading service.rs (3 lines <= 2000 lines default limit)
    let stdin_json = serde_json::json!({
        "tool_name": "read",
        "tool_input": {
            "filePath": harness.service_file,
            "offset": 1,
            "limit": 2000
        }
    })
    .to_string();

    let args = DaemonArgs {
        hook_post_read: true,
        ..Default::default()
    };
    let (file, span) =
        resolve_hook_input(&args, Some(&stdin_json)).expect("resolve OpenCode full-file read");
    assert_eq!(span.vendor, Some(AgentVendor::OpenCode));

    let (start_line, end_line, raw_read_bytes) =
        compute_hook_read_metrics(&file, &span, &ReadBudgetEnv::default());
    assert_eq!(start_line, Some(1));
    assert_eq!(end_line, Some(3));

    let resp = send_daemon_request(
        &harness.socket_path,
        DaemonRequest::PostReadHook {
            file,
            start_line,
            end_line,
            raw_read_bytes,
            read_kind: Some(ReadKind::Macro),
            total_lines: Some(3),
            vendor: Some(AgentVendor::OpenCode),
            is_full_read: Some(true),
        },
    )
    .await
    .expect("send PostReadHook for OpenCode full-file");

    match resp {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(
                !advisory_block.is_empty(),
                "OpenCode full-file read with callers must deliver non-empty advisory"
            );
            assert!(
                advisory_block.contains("## Upstream Callers"),
                "advisory must include upstream callers"
            );
            assert!(
                advisory_block.contains("caller.rs:"),
                "callers must reference caller.rs"
            );
            assert!(
                !advisory_block.contains("## Type Skeleton"),
                "advisory must omit self-skeleton"
            );
            assert!(
                !advisory_block.contains("Token Reduction"),
                "advisory must omit 'Token Reduction'"
            );
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let status = harness.status().await;
    assert_eq!(status.hooks_served, 1);
    assert_eq!(status.macro_reads, 1);
    assert!(status.submissions >= 1);
    assert!(status.omitted_full_read_skeletons >= 1);
    assert!(status.delivered_bytes_served > 0);
    assert_file_unchanged(&harness.service_file, &harness.service_content);
    assert_mtime_unchanged(&harness.service_file, mtime_before);

    harness.shutdown().await;
}

#[tokio::test]
async fn opencode_stdin_partial_macro_over_2000_lines_delivers_outline_and_preserves_mtime() {
    let harness = TestHarness::start().await;
    let mtime_before = std::fs::metadata(&harness.sample_file)
        .expect("metadata")
        .modified()
        .expect("mtime");

    // OpenCode reading sample.rs (2500 lines > 2000 lines default limit)
    let stdin_json = serde_json::json!({
        "tool_name": "read",
        "tool_input": {
            "filePath": harness.sample_file,
            "offset": 1,
            "limit": 2000
        }
    })
    .to_string();

    let args = DaemonArgs {
        hook_post_read: true,
        ..Default::default()
    };
    let (file, span) =
        resolve_hook_input(&args, Some(&stdin_json)).expect("resolve OpenCode partial read");
    assert_eq!(span.vendor, Some(AgentVendor::OpenCode));

    let (start_line, end_line, raw_read_bytes) =
        compute_hook_read_metrics(&file, &span, &ReadBudgetEnv::default());
    assert_eq!(start_line, Some(1));
    assert_eq!(end_line, Some(2000));

    let resp = send_daemon_request(
        &harness.socket_path,
        DaemonRequest::PostReadHook {
            file,
            start_line,
            end_line,
            raw_read_bytes,
            read_kind: Some(ReadKind::Macro),
            total_lines: Some(TOTAL_LINES),
            vendor: Some(AgentVendor::OpenCode),
            is_full_read: Some(false),
        },
    )
    .await
    .expect("send PostReadHook for OpenCode partial read");

    match resp {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(
                !advisory_block.is_empty(),
                "OpenCode >2000 line read must deliver unread outline"
            );
            assert!(
                advisory_block.chars().count() <= 9_200,
                "advisory block must be bounded within 9,200 char wire limit"
            );
            assert!(
                advisory_block.contains("## Unread File Outline"),
                "advisory must contain unread outline"
            );
            assert_eq!(
                advisory_block.matches("```").count() % 2,
                0,
                "code fences must remain balanced"
            );
            assert!(
                !advisory_block.contains("Token Reduction"),
                "advisory must omit 'Token Reduction'"
            );
            // Declarations past line 2000 should be in outline
            assert!(
                advisory_block.contains("item_2001"),
                "outline must contain unread declarations past line 2000"
            );
            // Declarations within 1..=2000 must NOT be in outline
            assert!(
                !advisory_block.contains("item_0001"),
                "outline must not contain already-read declarations from lines 1..=2000"
            );
            assert!(
                !advisory_block.contains("item_2000"),
                "outline must not contain already-read declarations from lines 1..=2000"
            );
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let status = harness.status().await;
    assert_eq!(status.hooks_served, 1);
    assert_eq!(status.macro_reads, 1);
    assert!(status.submissions >= 1);
    assert!(status.delivered_bytes_served > 0);
    assert!(status.delivered_bytes_served <= 9_200);
    assert_file_unchanged(&harness.sample_file, &harness.original_content);
    assert_mtime_unchanged(&harness.sample_file, mtime_before);

    harness.shutdown().await;
}
