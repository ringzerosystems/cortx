//! # cortx-daemon
//!
//! Agent sidecar and background indexing daemon.
//!
//! Provides high-throughput Unix domain socket IPC for sub-millisecond
//! agent tool hook responses and non-destructive advisory context injection.

#![forbid(unsafe_code)]

pub mod error;
pub mod hook;
pub mod logging;
pub mod protocol;
pub mod registry;
pub mod server;
pub mod watcher;

pub use error::DaemonError;
pub use hook::{
    execute_post_read_bundle, execute_post_read_bundle_for_project, execute_post_read_hook,
    execute_post_read_hook_for_project, execute_post_read_hook_with_overlay,
};
pub use logging::{DaemonLogManager, WorkspaceLogger};
pub use protocol::{
    DaemonRequest, DaemonResponse, DaemonStatusInfo, WorkspaceSummary, read_message, write_message,
};
pub use registry::{WorkspaceEntry, WorkspaceRegistry};
pub use server::{
    ServerOptions, ServerTelemetry, SocketGuard, run_server, run_server_with_options,
};
pub use watcher::{DynamicWatcherManager, start_watcher};

use std::path::Path;
use tokio::net::UnixStream;

/// Sends a request to a running daemon over Unix domain socket.
pub async fn send_daemon_request(
    socket_path: &Path,
    request: DaemonRequest,
) -> Result<DaemonResponse, DaemonError> {
    let mut stream =
        UnixStream::connect(socket_path)
            .await
            .map_err(|source| DaemonError::Socket {
                path: socket_path.to_path_buf(),
                source,
            })?;

    write_message(&mut stream, &request).await?;
    let response = read_message(&mut stream).await?;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cortx_distill::SymbolIndex;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tempfile::NamedTempFile;
    use tokio::sync::RwLock;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn test_daemon_ping_pong() {
        let tmp = NamedTempFile::new().unwrap();
        let socket_path = tmp.path().to_path_buf();
        // Remove temp file so UnixListener can bind
        drop(tmp);

        let index = Arc::new(RwLock::new(SymbolIndex::new()));
        let token = CancellationToken::new();

        let s_path = socket_path.clone();
        let s_token = token.clone();
        let server_handle = tokio::spawn(async move {
            let _ = run_server(&s_path, index, s_token).await;
        });

        // Wait briefly for socket creation
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let response = send_daemon_request(&socket_path, DaemonRequest::Ping)
            .await
            .unwrap();
        assert!(matches!(response, DaemonResponse::Pong));

        let shutdown_resp = send_daemon_request(&socket_path, DaemonRequest::Shutdown)
            .await
            .unwrap();
        assert!(matches!(shutdown_resp, DaemonResponse::ShutdownAck));

        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn test_daemon_status() {
        let tmp = NamedTempFile::new().unwrap();
        let socket_path = tmp.path().to_path_buf();
        drop(tmp);

        let mut idx = SymbolIndex::new();
        idx.index_any_source_str("src/main.rs", "pub fn main() {}")
            .unwrap();
        let index = Arc::new(RwLock::new(idx));
        let token = CancellationToken::new();

        let s_path = socket_path.clone();
        let s_token = token.clone();
        let server_handle = tokio::spawn(async move {
            let opts = ServerOptions {
                socket_path: s_path,
                workspace_root: PathBuf::from("/test/root"),
                watcher_active: true,
                log_dir: None,
                idle_ttl_secs: 7200,
                ..ServerOptions::default()
            };
            let _ = run_server_with_options(opts, index, s_token).await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let response = send_daemon_request(&socket_path, DaemonRequest::Status)
            .await
            .unwrap();
        match response {
            DaemonResponse::Status(status) => {
                assert_eq!(status.pid, std::process::id());
                assert_eq!(status.daemon_mode, "Isolated Workspace");
                assert_eq!(status.workspace_root, PathBuf::from("/test/root"));
                assert_eq!(status.indexed_files, 1);
                assert!(status.total_symbols >= 1);
                assert!(status.watcher_active);
            }
            _ => panic!("Expected DaemonResponse::Status"),
        }

        let _ = send_daemon_request(&socket_path, DaemonRequest::Shutdown).await;
        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn test_daemon_socket_already_bound() {
        let tmp = NamedTempFile::new().unwrap();
        let socket_path = tmp.path().to_path_buf();
        drop(tmp);

        let index1 = Arc::new(RwLock::new(SymbolIndex::new()));
        let token1 = CancellationToken::new();

        let s_path = socket_path.clone();
        let s_token1 = token1.clone();
        let server1 = tokio::spawn(async move {
            let _ = run_server(&s_path, index1, s_token1).await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let index2 = Arc::new(RwLock::new(SymbolIndex::new()));
        let token2 = CancellationToken::new();
        let res = run_server(&socket_path, index2, token2).await;
        assert!(matches!(res, Err(DaemonError::SocketAlreadyBound(_))));

        let _ = send_daemon_request(&socket_path, DaemonRequest::Shutdown).await;
        let _ = server1.await;
    }

    #[tokio::test]
    async fn test_daemon_stale_socket_self_healing() {
        let tmp = NamedTempFile::new().unwrap();
        let socket_path = tmp.path().to_path_buf();
        std::fs::write(&socket_path, b"stale dead socket").unwrap();
        assert!(socket_path.exists());

        let index = Arc::new(RwLock::new(SymbolIndex::new()));
        let token = CancellationToken::new();

        let s_path = socket_path.clone();
        let s_token = token.clone();
        let server = tokio::spawn(async move {
            let _ = run_server(&s_path, index, s_token).await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        let response = send_daemon_request(&socket_path, DaemonRequest::Ping)
            .await
            .unwrap();
        assert!(matches!(response, DaemonResponse::Pong));

        let _ = send_daemon_request(&socket_path, DaemonRequest::Shutdown).await;
        let _ = server.await;
    }

    #[tokio::test]
    async fn test_multi_tenant_server_lazy_indexing_and_status() {
        let sock_tmp = NamedTempFile::new().unwrap();
        let socket_path = sock_tmp.path().to_path_buf();
        drop(sock_tmp);

        let log_dir = tempfile::tempdir().unwrap();
        let repo_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo_dir.path().join(".git")).unwrap();
        let src_dir = repo_dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let test_file = src_dir.join("lib.rs");
        std::fs::write(&test_file, "pub struct Engine; pub fn start() {}").unwrap();

        let index = Arc::new(RwLock::new(SymbolIndex::new()));
        let token = CancellationToken::new();

        let s_path = socket_path.clone();
        let s_token = token.clone();
        let log_path = log_dir.path().to_path_buf();
        let server_handle = tokio::spawn(async move {
            let opts = ServerOptions {
                socket_path: s_path,
                workspace_root: PathBuf::new(),
                watcher_active: true,
                log_dir: Some(log_path),
                idle_ttl_secs: 7200,
                ..ServerOptions::default()
            };
            let _ = run_server_with_options(opts, index, s_token).await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let hook_resp = send_daemon_request(
            &socket_path,
            DaemonRequest::PostReadHook {
                file: test_file.clone(),
                start_line: Some(1),
                end_line: Some(10),
                raw_read_bytes: Some(4200),
                read_kind: None,
                total_lines: None,
                vendor: None,
                is_full_read: None,
            },
        )
        .await
        .unwrap();

        match hook_resp {
            DaemonResponse::AdvisoryContext { advisory_block, .. } => {
                assert!(advisory_block.contains("pub struct Engine"));
            }
            _ => panic!("Expected AdvisoryContext response"),
        }

        let status_resp = send_daemon_request(&socket_path, DaemonRequest::Status)
            .await
            .unwrap();

        match status_resp {
            DaemonResponse::Status(status) => {
                assert_eq!(status.pid, std::process::id());
                assert_eq!(status.daemon_mode, "System-Wide Multi-Tenant");
                assert!(status.memory_rss_mb >= 0.0);
                assert_eq!(status.active_workspaces_count, 1);
                assert_eq!(status.workspaces.len(), 1);
                assert_eq!(
                    status.workspaces[0].root,
                    std::fs::canonicalize(repo_dir.path()).unwrap()
                );
                assert!(status.workspaces[0].memory_mb > 0.0);
                assert!(status.workspaces[0].log_path.exists());
                assert_eq!(status.hooks_served, 1);
                assert_eq!(status.raw_bytes_served, 4200);
            }
            _ => panic!("Expected DaemonResponse::Status"),
        }

        let _ = send_daemon_request(&socket_path, DaemonRequest::Shutdown).await;
        let _ = server_handle.await;
    }
}
