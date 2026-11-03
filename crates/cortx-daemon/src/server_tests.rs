use super::*;

#[test]
fn test_server_telemetry_metrics() {
    let telemetry = ServerTelemetry::default();
    telemetry.record_hook_metrics(1000, 200);
    telemetry.record_hook_metrics(50, 100);

    let (hooks, raw, delivered) = telemetry.snapshot();
    assert_eq!(hooks, 2);
    assert_eq!(raw, 1050);
    assert_eq!(delivered, 300);
}

#[tokio::test]
async fn test_server_telemetry_concurrent_updates() {
    let telemetry = Arc::new(ServerTelemetry::default());
    let mut handles = Vec::new();

    for _ in 0..16 {
        let t = Arc::clone(&telemetry);
        handles.push(tokio::spawn(async move {
            for _ in 0..100 {
                t.record_hook_metrics(100, 20);
            }
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    let (hooks, raw, delivered) = telemetry.snapshot();
    assert_eq!(hooks, 1600);
    assert_eq!(raw, 160_000);
    assert_eq!(delivered, 32_000);
}

#[tokio::test]
async fn test_server_telemetry_atomic_counters() {
    let telemetry = ServerTelemetry::default();
    assert_eq!(telemetry.submissions.load(Ordering::Relaxed), 0);
    assert_eq!(telemetry.callgraph_drops.load(Ordering::Relaxed), 0);
    assert_eq!(telemetry.ast_drops.load(Ordering::Relaxed), 0);
    assert_eq!(telemetry.macro_reads(), 0);
    assert_eq!(telemetry.micro_reads(), 0);
    assert_eq!(telemetry.omitted_micro_reads(), 0);
    assert_eq!(telemetry.omitted_full_read_skeletons(), 0);
    assert_eq!(telemetry.trimmed_advisories(), 0);

    telemetry.record_submission();
    telemetry.record_submission();
    telemetry.record_callgraph_drop();
    telemetry.record_ast_drop();
    telemetry.record_read_kind(ReadKind::Macro);
    telemetry.record_read_kind(ReadKind::Micro);
    telemetry.record_read_kind(ReadKind::Micro);
    telemetry
        .omitted_micro_reads
        .fetch_add(1, Ordering::Relaxed);
    telemetry
        .omitted_full_read_skeletons
        .fetch_add(1, Ordering::Relaxed);
    telemetry.trimmed_advisories.fetch_add(1, Ordering::Relaxed);

    assert_eq!(telemetry.submissions.load(Ordering::Relaxed), 2);
    assert_eq!(telemetry.callgraph_drops.load(Ordering::Relaxed), 1);
    assert_eq!(telemetry.ast_drops.load(Ordering::Relaxed), 1);
    assert_eq!(telemetry.macro_reads(), 1);
    assert_eq!(telemetry.micro_reads(), 2);
    assert_eq!(telemetry.omitted_micro_reads(), 1);
    assert_eq!(telemetry.omitted_full_read_skeletons(), 1);
    assert_eq!(telemetry.trimmed_advisories(), 1);
}

#[tokio::test]
async fn test_post_read_hook_macro_and_micro_telemetry() {
    let dir = tempfile::tempdir().unwrap();
    let log_dir = dir.path().join("logs");
    let opts = ServerOptions {
        socket_path: dir.path().join("test_telemetry.sock"),
        workspace_root: dir.path().to_path_buf(),
        log_dir: Some(log_dir),
        ..Default::default()
    };
    let index = Arc::new(RwLock::new(SymbolIndex::new()));
    let token = CancellationToken::new();
    let state = init_server_state(opts, index, token, std::time::Instant::now())
        .await
        .expect("init server state");

    let src_file = dir.path().join("main.rs");
    let mut file_content = String::new();
    for i in 0..200 {
        file_content.push_str(&format!("pub fn f{}() {{}}\n", i));
    }
    std::fs::write(&src_file, &file_content).unwrap();

    // 1. PartialMacro read (e.g. lines 1..160 of 200 lines, > 150 lines) -> PartialMacro read delivering unread outline
    let p_macro = PostReadParams {
        file: &src_file,
        start_line: Some(1),
        end_line: Some(160),
        raw_read_bytes: Some(file_content.len() as u64),
        read_kind: None,
        total_lines: Some(200),
        vendor: None,
        is_full_read: None,
        session_id: None,
    };
    let resp_macro = handle_post_read_request(p_macro, std::time::Instant::now(), &state).await;
    match resp_macro {
        DaemonResponse::AdvisoryContext {
            advisory_block,
            protocol_version,
            ..
        } => {
            assert_eq!(protocol_version, PROTOCOL_VERSION);
            assert!(
                !advisory_block.is_empty(),
                "PartialMacro should deliver unread outline"
            );
            assert!(
                advisory_block.contains("f160")
                    || advisory_block.contains("f161")
                    || advisory_block.contains("f199")
            );
        }
        other => panic!("expected AdvisoryContext, got {:?}", other),
    }
    assert_eq!(state.telemetry.macro_reads(), 1);
    assert_eq!(state.telemetry.micro_reads(), 0);
    let delivered_after_macro = state
        .telemetry
        .delivered_bytes_served
        .load(Ordering::Relaxed);
    assert!(delivered_after_macro > 0);

    // 2. Bounded read (lines 10..40, 31 lines <= 150) -> Micro read
    let p_micro = PostReadParams {
        file: &src_file,
        start_line: Some(10),
        end_line: Some(40),
        raw_read_bytes: Some(500),
        read_kind: None,
        total_lines: Some(200),
        vendor: None,
        is_full_read: None,
        session_id: None,
    };
    let resp_micro = handle_post_read_request(p_micro, std::time::Instant::now(), &state).await;
    match resp_micro {
        DaemonResponse::AdvisoryContext {
            advisory_block,
            protocol_version,
            ..
        } => {
            assert_eq!(protocol_version, PROTOCOL_VERSION);
            assert_eq!(advisory_block, "");
        }
        other => panic!("expected AdvisoryContext, got {:?}", other),
    }
    assert_eq!(state.telemetry.macro_reads(), 1);
    assert_eq!(state.telemetry.micro_reads(), 1);
    assert_eq!(state.telemetry.omitted_micro_reads(), 1);
    // Delivered bytes served must be strictly unchanged on omitted micro-reads!
    assert_eq!(
        state
            .telemetry
            .delivered_bytes_served
            .load(Ordering::Relaxed),
        delivered_after_macro
    );

    // 3. Explicit read_kind override
    let p_explicit = PostReadParams {
        file: &src_file,
        start_line: None,
        end_line: None,
        raw_read_bytes: Some(500),
        read_kind: Some(ReadKind::Micro),
        total_lines: None,
        vendor: None,
        is_full_read: None,
        session_id: None,
    };
    let resp_explicit =
        handle_post_read_request(p_explicit, std::time::Instant::now(), &state).await;
    match resp_explicit {
        DaemonResponse::AdvisoryContext {
            advisory_block,
            protocol_version,
            ..
        } => {
            assert_eq!(protocol_version, PROTOCOL_VERSION);
            assert_eq!(advisory_block, "");
        }
        other => panic!("expected AdvisoryContext, got {:?}", other),
    }
    assert_eq!(state.telemetry.macro_reads(), 1);
    assert_eq!(state.telemetry.micro_reads(), 2);
    assert_eq!(state.telemetry.omitted_micro_reads(), 2);
    assert_eq!(
        state
            .telemetry
            .delivered_bytes_served
            .load(Ordering::Relaxed),
        delivered_after_macro
    );

    // 4. Unbounded full-file read on standalone file without callers/contracts -> FullFile read with omitted skeleton
    let p_full = PostReadParams {
        file: &src_file,
        start_line: None,
        end_line: None,
        raw_read_bytes: Some(file_content.len() as u64),
        read_kind: None,
        total_lines: Some(200),
        vendor: None,
        is_full_read: Some(true),
        session_id: None,
    };
    let resp_full = handle_post_read_request(p_full, std::time::Instant::now(), &state).await;
    match resp_full {
        DaemonResponse::AdvisoryContext {
            advisory_block,
            protocol_version,
            ..
        } => {
            assert_eq!(protocol_version, PROTOCOL_VERSION);
            assert_eq!(advisory_block, "");
        }
        other => panic!("expected AdvisoryContext, got {:?}", other),
    }
    assert_eq!(state.telemetry.macro_reads(), 2);
    assert_eq!(state.telemetry.omitted_full_read_skeletons(), 1);

    // Check DaemonStatusInfo
    let status = build_status_response(&state).await;
    assert_eq!(status.protocol_version, PROTOCOL_VERSION);
    assert_eq!(status.macro_reads, 2);
    assert_eq!(status.micro_reads, 2);
    assert_eq!(status.omitted_micro_reads, 2);
    assert_eq!(status.omitted_full_read_skeletons, 1);
    assert!(status.supplemental_context_bytes > 0);
    assert_eq!(
        status.supplemental_context_tokens,
        status.supplemental_context_bytes / cortx_core::span::EST_BYTES_PER_TOKEN
    );
    assert_eq!(status.workspaces.len(), 1);
    assert_eq!(status.workspaces[0].macro_reads, 2);
    assert_eq!(status.workspaces[0].micro_reads, 2);
    assert_eq!(status.workspaces[0].omitted_micro_reads, 2);
    assert_eq!(status.workspaces[0].omitted_full_read_skeletons, 1);
}

#[tokio::test]
async fn test_toctou_safe_socket_bind() {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("test_bind.sock");

    // Stale dead socket file removal: create an inactive dummy socket file
    std::fs::write(&sock_path, b"dead").unwrap();

    let index = Arc::new(RwLock::new(SymbolIndex::new()));
    let token = CancellationToken::new();
    let token_clone = token.clone();
    let sock_path_clone = sock_path.clone();

    let server_opts = ServerOptions {
        socket_path: sock_path_clone.clone(),
        workspace_root: dir.path().to_path_buf(),
        ..Default::default()
    };

    // run_server_with_options should clean up dead socket and bind successfully
    let server_handle =
        tokio::spawn(async move { run_server_with_options(server_opts, index, token_clone).await });

    // Give it a brief moment to bind
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(sock_path.exists());

    // Probe running daemon: ping should succeed
    assert!(probe_active_socket(&sock_path).await);

    // Attempting a second bind on active socket fails with SocketAlreadyBound
    let index2 = Arc::new(RwLock::new(SymbolIndex::new()));
    let token2 = CancellationToken::new();
    let server_opts2 = ServerOptions {
        socket_path: sock_path_clone,
        workspace_root: dir.path().to_path_buf(),
        ..Default::default()
    };
    let err = run_server_with_options(server_opts2, index2, token2).await;
    assert!(matches!(err, Err(DaemonError::SocketAlreadyBound(_))));

    // Shutdown cleanly
    token.cancel();
    let _ = server_handle.await;
}

#[tokio::test]
async fn test_post_read_hook_session_repeat_deduplication() {
    let dir = tempfile::tempdir().unwrap();
    let log_dir = dir.path().join("logs");
    let opts = ServerOptions {
        socket_path: dir.path().join("test_session_dedup.sock"),
        workspace_root: dir.path().to_path_buf(),
        log_dir: Some(log_dir),
        ..Default::default()
    };
    let index = Arc::new(RwLock::new(SymbolIndex::new()));
    let token = CancellationToken::new();
    let state = init_server_state(opts, index, token, std::time::Instant::now())
        .await
        .expect("init server state");

    let src_file = dir.path().join("lib.rs");
    let mut file_content = String::new();
    for i in 0..200 {
        file_content.push_str(&format!("pub fn func_{}() {{}}\n", i));
    }
    std::fs::write(&src_file, &file_content).unwrap();

    let session_id = "test-session-123";

    // 1. First macro read in session -> delivers advisory context
    let p1 = PostReadParams {
        file: &src_file,
        start_line: Some(1),
        end_line: Some(160),
        raw_read_bytes: Some(file_content.len() as u64),
        read_kind: None,
        total_lines: Some(200),
        vendor: None,
        is_full_read: None,
        session_id: Some(session_id),
    };
    let resp1 = handle_post_read_request(p1, std::time::Instant::now(), &state).await;
    match resp1 {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(
                !advisory_block.is_empty(),
                "First read must deliver advisory"
            );
        }
        other => panic!("expected AdvisoryContext, got {:?}", other),
    }
    assert_eq!(state.telemetry.macro_reads(), 1);
    assert_eq!(state.telemetry.omitted_repeat_reads(), 0);

    // 2. Second macro read to same file in same session without edits -> omitted as repeat
    let p2 = PostReadParams {
        file: &src_file,
        start_line: Some(1),
        end_line: Some(160),
        raw_read_bytes: Some(file_content.len() as u64),
        read_kind: None,
        total_lines: Some(200),
        vendor: None,
        is_full_read: None,
        session_id: Some(session_id),
    };
    let resp2 = handle_post_read_request(p2, std::time::Instant::now(), &state).await;
    match resp2 {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert_eq!(advisory_block, "", "Repeated read must be omitted");
        }
        other => panic!("expected AdvisoryContext, got {:?}", other),
    }
    assert_eq!(state.telemetry.macro_reads(), 2);
    assert_eq!(state.telemetry.omitted_repeat_reads(), 1);

    // 3. Different session reading the same file -> delivers advisory
    let p3 = PostReadParams {
        file: &src_file,
        start_line: Some(1),
        end_line: Some(160),
        raw_read_bytes: Some(file_content.len() as u64),
        read_kind: None,
        total_lines: Some(200),
        vendor: None,
        is_full_read: None,
        session_id: Some("different-session-456"),
    };
    let resp3 = handle_post_read_request(p3, std::time::Instant::now(), &state).await;
    match resp3 {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(
                !advisory_block.is_empty(),
                "Different session must deliver advisory"
            );
        }
        other => panic!("expected AdvisoryContext, got {:?}", other),
    }
    assert_eq!(state.telemetry.macro_reads(), 3);
    assert_eq!(state.telemetry.omitted_repeat_reads(), 1);

    // 4. File modified on disk -> same session re-reads -> delivers fresh advisory
    file_content.push_str("pub fn newly_added() {}\n");
    // Sleep briefly to ensure mtime ticks forward on coarse filesystems
    tokio::time::sleep(Duration::from_millis(50)).await;
    std::fs::write(&src_file, &file_content).unwrap();

    let p4 = PostReadParams {
        file: &src_file,
        start_line: Some(1),
        end_line: Some(160),
        raw_read_bytes: Some(file_content.len() as u64),
        read_kind: None,
        total_lines: Some(201),
        vendor: None,
        is_full_read: None,
        session_id: Some(session_id),
    };
    let resp4 = handle_post_read_request(p4, std::time::Instant::now(), &state).await;
    match resp4 {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(
                !advisory_block.is_empty(),
                "Modified file must re-deliver advisory"
            );
        }
        other => panic!("expected AdvisoryContext, got {:?}", other),
    }
    assert_eq!(state.telemetry.macro_reads(), 4);
    assert_eq!(state.telemetry.omitted_repeat_reads(), 1);
}

#[tokio::test]
async fn test_post_read_hook_session_vendor_activity_logging() {
    let dir = tempfile::tempdir().unwrap();
    let log_dir = dir.path().join("logs");
    let opts = ServerOptions {
        socket_path: dir.path().join("test_session_vendor_log.sock"),
        workspace_root: dir.path().to_path_buf(),
        log_dir: Some(log_dir),
        ..Default::default()
    };
    let index = Arc::new(RwLock::new(SymbolIndex::new()));
    let token = CancellationToken::new();
    let state = init_server_state(opts, index, token, std::time::Instant::now())
        .await
        .expect("init server state");

    let src_file = dir.path().join("service.rs");
    let mut file_content = String::new();
    for i in 0..200 {
        file_content.push_str(&format!("pub fn svc_{}() {{}}\n", i));
    }
    std::fs::write(&src_file, &file_content).unwrap();

    // 1. Partial macro read with session and vendor (Agy)
    let p_macro = PostReadParams {
        file: &src_file,
        start_line: Some(1),
        end_line: Some(160),
        raw_read_bytes: Some(file_content.len() as u64),
        read_kind: None,
        total_lines: Some(200),
        vendor: Some(AgentVendor::Agy),
        is_full_read: None,
        session_id: Some("agy-chat-001"),
    };
    let _ = handle_post_read_request(p_macro, std::time::Instant::now(), &state).await;

    // 2. Micro read with same session (vendor inferred from session state)
    let p_micro = PostReadParams {
        file: &src_file,
        start_line: Some(10),
        end_line: Some(20),
        raw_read_bytes: Some(300),
        read_kind: None,
        total_lines: Some(200),
        vendor: None,
        is_full_read: None,
        session_id: Some("agy-chat-001"),
    };
    let _ = handle_post_read_request(p_micro, std::time::Instant::now(), &state).await;

    // 3. Repeat macro read with same session
    let p_repeat = PostReadParams {
        file: &src_file,
        start_line: Some(1),
        end_line: Some(160),
        raw_read_bytes: Some(file_content.len() as u64),
        read_kind: None,
        total_lines: Some(200),
        vendor: None,
        is_full_read: None,
        session_id: Some("agy-chat-001"),
    };
    let _ = handle_post_read_request(p_repeat, std::time::Instant::now(), &state).await;

    // Allow logger channel task to flush
    tokio::time::sleep(Duration::from_millis(100)).await;

    let log_content =
        std::fs::read_to_string(state.log_manager.main_log_path()).expect("read main daemon log");

    // Verify macro served has session and vendor
    assert!(
        log_content.contains("PostReadHook served"),
        "log must contain PostReadHook served"
    );
    assert!(
        log_content.contains("session: 'agy-chat-001' (vendor: agy)"),
        "log must contain session with vendor name for macro read"
    );

    // Verify micro read has session and vendor
    assert!(
        log_content.contains("[MICRO] PostReadHook skipped (micro-read)"),
        "log must contain micro skipped"
    );
    assert!(
        log_content.contains("[MICRO] PostReadHook skipped (micro-read): '")
            && log_content.contains("| session: 'agy-chat-001' (vendor: agy) |"),
        "log must contain session with vendor name for micro read"
    );

    // Verify repeat read has session and vendor
    assert!(
        log_content.contains("[REPEAT] PostReadHook skipped (session repeat)"),
        "log must contain repeat skipped"
    );
    assert!(
        log_content.contains("[REPEAT] PostReadHook skipped (session repeat): '")
            && log_content.contains("| session: 'agy-chat-001' (vendor: agy) |"),
        "log must contain session with vendor name for repeat read"
    );
}

#[tokio::test]
async fn test_post_read_hook_session_ttl_expiration() {
    let dir = tempfile::tempdir().unwrap();
    let log_dir = dir.path().join("logs");
    let opts = ServerOptions {
        socket_path: dir.path().join("test_session_ttl.sock"),
        workspace_root: dir.path().to_path_buf(),
        log_dir: Some(log_dir),
        session_ttl_secs: 1,
        ..Default::default()
    };
    let index = Arc::new(RwLock::new(SymbolIndex::new()));
    let token = CancellationToken::new();
    let state = init_server_state(opts, index, token, std::time::Instant::now())
        .await
        .expect("init server state");

    let src_file = dir.path().join("lib.rs");
    let mut file_content = String::new();
    for i in 0..200 {
        file_content.push_str(&format!("pub fn func_{}() {{}}\n", i));
    }
    std::fs::write(&src_file, &file_content).unwrap();

    let session_id = "test-session-ttl";

    // 1. First macro read -> delivers advisory context
    let p1 = PostReadParams {
        file: &src_file,
        start_line: Some(1),
        end_line: Some(160),
        raw_read_bytes: Some(file_content.len() as u64),
        read_kind: None,
        total_lines: Some(200),
        vendor: None,
        is_full_read: None,
        session_id: Some(session_id),
    };
    let resp1 = handle_post_read_request(p1, std::time::Instant::now(), &state).await;
    match resp1 {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(
                !advisory_block.is_empty(),
                "First read must deliver advisory"
            );
        }
        other => panic!("expected AdvisoryContext, got {:?}", other),
    }
    assert_eq!(state.telemetry.macro_reads(), 1);
    assert_eq!(state.telemetry.omitted_repeat_reads(), 0);

    // Sleep past the 1-second TTL threshold
    tokio::time::sleep(Duration::from_millis(1050)).await;

    // 2. Second read after TTL expiration -> treated as fresh macro read, delivering advisory again
    let p2 = PostReadParams {
        file: &src_file,
        start_line: Some(1),
        end_line: Some(160),
        raw_read_bytes: Some(file_content.len() as u64),
        read_kind: None,
        total_lines: Some(200),
        vendor: None,
        is_full_read: None,
        session_id: Some(session_id),
    };
    let resp2 = handle_post_read_request(p2, std::time::Instant::now(), &state).await;
    match resp2 {
        DaemonResponse::AdvisoryContext { advisory_block, .. } => {
            assert!(
                !advisory_block.is_empty(),
                "Expired session read must deliver fresh advisory"
            );
        }
        other => panic!("expected AdvisoryContext, got {:?}", other),
    }
    assert_eq!(state.telemetry.macro_reads(), 2);
    assert_eq!(state.telemetry.omitted_repeat_reads(), 0);
}
