//! Length-prefixed IPC messaging protocol for agent sidecar communication.

use crate::error::DaemonError;
use cortx_core::AgentVendor;
use cortx_core::ReadKind;
use cortx_core::model::DistillMode;
use cortx_core::output::OutputFormat;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Wire protocol version for daemon IPC.
pub const PROTOCOL_VERSION: u32 = 2;

/// IPC request sent from agent tool hook or CLI client to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DaemonRequest {
    /// Heartbeat ping.
    Ping,
    /// Request runtime statistics and cache metrics.
    Status,
    /// Advisory post-read tool hook request.
    PostReadHook {
        /// Target file path inspected.
        file: PathBuf,
        /// Optional 1-indexed start line requested by the agent.
        #[serde(default)]
        start_line: Option<usize>,
        /// Optional 1-indexed inclusive end line requested by the agent.
        #[serde(default)]
        end_line: Option<usize>,
        /// Optional actual byte count read by the agent (after line range & vendor limits).
        #[serde(default)]
        raw_read_bytes: Option<u64>,
        /// Optional pre-classified read kind (Micro vs Macro read).
        #[serde(default)]
        read_kind: Option<ReadKind>,
        /// Optional total line count of target file.
        #[serde(default)]
        total_lines: Option<usize>,
        /// Optional agent vendor.
        #[serde(default)]
        vendor: Option<AgentVendor>,
        /// Optional flag indicating whether this is an unbounded or full-file read.
        #[serde(default)]
        is_full_read: Option<bool>,
    },
    /// Request AST context distillation for file.
    Distill {
        /// Target file path.
        file: PathBuf,
        /// Distillation mode.
        mode: DistillMode,
        /// Desired output format.
        format: OutputFormat,
        /// Optional 1-indexed start line.
        #[serde(default)]
        start_line: Option<usize>,
        /// Optional 1-indexed inclusive end line.
        #[serde(default)]
        end_line: Option<usize>,
        /// Optional agent vendor.
        #[serde(default)]
        vendor: Option<AgentVendor>,
        /// Enable cross-boundary HTTP route and model correlation.
        #[serde(default)]
        cross_boundary: bool,
    },
    /// Request graceful shutdown of daemon.
    Shutdown,
}

/// Summary telemetry for a single registered workspace in the multi-tenant daemon.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceSummary {
    /// Canonical workspace root directory.
    pub root: PathBuf,
    /// Matched declarative project name from configuration, if any.
    #[serde(default)]
    pub project_name: Option<String>,
    /// Canonical path to the configuration file governing this workspace, if any.
    #[serde(default)]
    pub config_path: Option<PathBuf>,
    /// Indicates whether this workspace is a Git worktree.
    pub is_worktree: bool,
    /// Parent repository root if this workspace is a worktree.
    pub parent_repo: Option<PathBuf>,
    /// Number of distinct source files indexed.
    pub indexed_files: usize,
    /// Total number of symbols declared in index.
    pub total_symbols: usize,
    /// Estimated in-memory index size in Megabytes.
    pub memory_mb: f64,
    /// Absolute path to this workspace's dedicated log file.
    pub log_path: PathBuf,
    /// Seconds elapsed since last activity or query.
    pub last_accessed_secs_ago: u64,
    /// Total successful advisory context submissions for this workspace.
    #[serde(default)]
    pub submissions: u64,
    /// Total times advisory call graph was pruned due to bloat.
    #[serde(default)]
    pub callgraph_drops: u64,
    /// Total times advisory context was completely omitted due to bloat.
    #[serde(default)]
    pub ast_drops: u64,
    /// Total macro reads (unbounded or >150 lines) served for this workspace.
    #[serde(default)]
    pub macro_reads: u64,
    /// Total micro reads (<= 150 lines bounded) served for this workspace.
    #[serde(default)]
    pub micro_reads: u64,
    /// Total micro reads with omitted advisory output for this workspace.
    #[serde(default)]
    pub omitted_micro_reads: u64,
    /// Total full-file reads with omitted self-skeletons for this workspace.
    #[serde(default)]
    pub omitted_full_read_skeletons: u64,
    /// Total advisories trimmed to wire character budget for this workspace.
    #[serde(default)]
    pub trimmed_advisories: u64,
    /// Total times parser fell back to cached/empty facts on syntax error for this workspace.
    #[serde(default)]
    pub parse_fallbacks: u64,
}

/// Detailed runtime statistics and index telemetry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DaemonStatusInfo {
    /// Wire protocol version.
    #[serde(default)]
    pub protocol_version: u32,
    /// Daemon package version.
    #[serde(default)]
    pub daemon_version: String,
    /// Process ID of the running daemon.
    pub pid: u32,
    /// Daemon process uptime in seconds.
    pub uptime_secs: u64,
    /// Bound socket path.
    pub socket_path: PathBuf,
    /// Operational mode of the daemon ("System-Wide Multi-Tenant" or "Isolated Workspace").
    #[serde(default)]
    pub daemon_mode: String,
    /// Canonical path of the loaded daemon configuration file, if any.
    #[serde(default)]
    pub config_path: Option<PathBuf>,
    /// Number of declarative project sections loaded in the daemon configuration.
    #[serde(default)]
    pub declared_projects_count: usize,
    /// Absolute path to the active timestamped daemon log file.
    #[serde(default)]
    pub main_log_path: PathBuf,
    /// Total process resident memory consumption in Megabytes.
    #[serde(default)]
    pub memory_rss_mb: f64,
    /// Number of active registered workspaces.
    #[serde(default)]
    pub active_workspaces_count: usize,
    /// Summary breakdown of each registered workspace.
    #[serde(default)]
    pub workspaces: Vec<WorkspaceSummary>,

    /// Monitored workspace root directory (legacy/fallback).
    #[serde(default)]
    pub workspace_root: PathBuf,
    /// Number of distinct source files currently indexed in memory.
    #[serde(default)]
    pub indexed_files: usize,
    /// Total number of symbols declared in index.
    #[serde(default)]
    pub total_symbols: usize,
    /// Total number of call-graph edges indexed.
    #[serde(default)]
    pub total_call_edges: usize,
    /// Total number of trait/interface contract implementations recorded.
    #[serde(default)]
    pub total_contracts: usize,
    /// Total number of HTTP routes and client bindings recorded.
    #[serde(default)]
    pub total_routes: usize,
    /// Indicates whether the background file watcher is active.
    #[serde(default)]
    pub watcher_active: bool,

    // --- Context Bloat Reduction Telemetry ---
    /// Total number of advisory tool hook requests served since daemon startup.
    #[serde(default)]
    pub hooks_served: u64,
    /// Total successful advisory context submissions across all workspaces.
    #[serde(default)]
    pub submissions: u64,
    /// Total times advisory call graph was pruned due to bloat across all workspaces.
    #[serde(default)]
    pub callgraph_drops: u64,
    /// Total times advisory context was completely omitted due to bloat across all workspaces.
    #[serde(default)]
    pub ast_drops: u64,
    /// Total raw source bytes processed prior to distillation.
    #[serde(default)]
    pub raw_bytes_served: u64,
    /// Total advisory context bytes emitted (skeletons + contracts).
    #[serde(default)]
    pub delivered_bytes_served: u64,
    /// Cumulative raw bytes saved (raw - delivered, clamped >= 0).
    #[serde(default)]
    pub bytes_saved: u64,
    /// Overall context reduction percentage across all served requests.
    #[serde(default)]
    pub token_reduction_pct: f64,
    /// Estimated LLM tokens saved across all requests (~4 bytes per token).
    #[serde(default)]
    pub est_tokens_saved: u64,
    /// Total macro reads served across all workspaces.
    #[serde(default)]
    pub macro_reads: u64,
    /// Total micro reads served across all workspaces.
    #[serde(default)]
    pub micro_reads: u64,
    /// Total micro reads with omitted advisory output across all workspaces.
    #[serde(default)]
    pub omitted_micro_reads: u64,
    /// Total full-file reads with omitted self-skeletons across all workspaces.
    #[serde(default)]
    pub omitted_full_read_skeletons: u64,
    /// Total advisories trimmed to wire character budget across all workspaces.
    #[serde(default)]
    pub trimmed_advisories: u64,
    /// Total times parser fell back to cached/empty facts on syntax error across all workspaces.
    #[serde(default)]
    pub parse_fallbacks: u64,
    /// Total supplemental context delivered in bytes across all requests.
    #[serde(default)]
    pub supplemental_context_bytes: u64,
    /// Estimated tokens of supplemental context delivered (~4 bytes per token).
    #[serde(default)]
    pub supplemental_context_tokens: u64,
}

/// IPC response sent from daemon back to agent tool or CLI client.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DaemonResponse {
    /// Heartbeat response.
    Pong,
    /// Status telemetry response.
    Status(DaemonStatusInfo),
    /// Advisory context block returned out-of-band for post-read hooks.
    AdvisoryContext {
        /// Target file.
        file: String,
        /// Advisory markdown block.
        advisory_block: String,
        /// Wire protocol version.
        #[serde(default)]
        protocol_version: u32,
    },
    /// Distillation output content.
    DistillResult {
        /// Formatted context content.
        content: String,
    },
    /// Operation error response.
    Error {
        /// Error message.
        message: String,
    },
    /// Acknowledgment of shutdown signal.
    ShutdownAck,
}

/// Writes a length-prefixed JSON message onto an async stream.
pub async fn write_message<W, T>(stream: &mut W, message: &T) -> Result<(), DaemonError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let payload = serde_json::to_vec(message).map_err(|e| DaemonError::Protocol(e.to_string()))?;
    let len = payload.len() as u32;

    stream
        .write_all(&len.to_be_bytes())
        .await
        .map_err(DaemonError::Io)?;
    stream.write_all(&payload).await.map_err(DaemonError::Io)?;
    stream.flush().await.map_err(DaemonError::Io)?;
    Ok(())
}

/// Reads a length-prefixed JSON message from an async stream.
pub async fn read_message<R, T>(stream: &mut R) -> Result<T, DaemonError>
where
    R: AsyncRead + Unpin,
    T: for<'de> Deserialize<'de>,
{
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .map_err(DaemonError::Io)?;
    let len = u32::from_be_bytes(len_buf) as usize;

    if len > 64 * 1024 * 1024 {
        return Err(DaemonError::Protocol(format!(
            "frame size {} exceeds 64MB limit",
            len
        )));
    }

    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await.map_err(DaemonError::Io)?;

    let msg = serde_json::from_slice(&buf).map_err(|e| DaemonError::Protocol(e.to_string()))?;
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[tokio::test]
    async fn test_protocol_status_request_framing() {
        let mut buf = Vec::new();
        let req = DaemonRequest::Status;
        write_message(&mut buf, &req).await.unwrap();

        let mut cursor = Cursor::new(buf);
        let decoded: DaemonRequest = read_message(&mut cursor).await.unwrap();
        assert!(matches!(decoded, DaemonRequest::Status));
    }

    #[tokio::test]
    async fn test_protocol_status_response_framing() {
        let mut buf = Vec::new();
        let status = DaemonStatusInfo {
            protocol_version: PROTOCOL_VERSION,
            daemon_version: "0.1.0".to_string(),
            pid: 12345,
            uptime_secs: 42,
            socket_path: PathBuf::from("/tmp/cortx-test.sock"),
            daemon_mode: "System-Wide Multi-Tenant".to_string(),
            config_path: Some(PathBuf::from("/etc/cortx/cortx.toml")),
            declared_projects_count: 2,
            main_log_path: PathBuf::from("/tmp/logs/daemon-20261020-120000.log"),
            memory_rss_mb: 48.5,
            active_workspaces_count: 1,
            workspaces: vec![WorkspaceSummary {
                root: PathBuf::from("/home/user/project"),
                project_name: Some("acme-web".to_string()),
                config_path: Some(PathBuf::from("/etc/cortx/cortx.toml")),
                is_worktree: false,
                parent_repo: None,
                indexed_files: 10,
                total_symbols: 100,
                memory_mb: 12.3,
                log_path: PathBuf::from("/tmp/logs/workspaces/project-abc123.log"),
                last_accessed_secs_ago: 5,
                submissions: 3,
                callgraph_drops: 1,
                ast_drops: 0,
                macro_reads: 1,
                micro_reads: 2,
                omitted_micro_reads: 1,
                omitted_full_read_skeletons: 2,
                trimmed_advisories: 0,
                parse_fallbacks: 0,
            }],
            workspace_root: PathBuf::from("/home/user/project"),
            indexed_files: 10,
            total_symbols: 100,
            total_call_edges: 50,
            total_contracts: 5,
            total_routes: 2,
            watcher_active: true,
            hooks_served: 4,
            submissions: 3,
            callgraph_drops: 1,
            ast_drops: 0,
            raw_bytes_served: 10000,
            delivered_bytes_served: 2000,
            bytes_saved: 8000,
            token_reduction_pct: 80.0,
            est_tokens_saved: 2000,
            macro_reads: 1,
            micro_reads: 2,
            omitted_micro_reads: 1,
            omitted_full_read_skeletons: 2,
            trimmed_advisories: 0,
            parse_fallbacks: 0,
            supplemental_context_bytes: 2000,
            supplemental_context_tokens: 500,
        };
        let resp = DaemonResponse::Status(status.clone());
        write_message(&mut buf, &resp).await.unwrap();

        let mut cursor = Cursor::new(buf);
        let decoded: DaemonResponse = read_message(&mut cursor).await.unwrap();
        match decoded {
            DaemonResponse::Status(decoded_status) => {
                assert_eq!(decoded_status, status);
            }
            _ => panic!("Expected DaemonResponse::Status"),
        }
    }

    #[test]
    fn test_protocol_status_backward_compatibility() {
        let legacy_json = r#"{
            "pid": 12345,
            "uptime_secs": 42,
            "socket_path": "/tmp/cortx-test.sock",
            "workspace_root": "/home/user/project",
            "indexed_files": 10,
            "total_symbols": 100,
            "total_call_edges": 50,
            "total_contracts": 5,
            "total_routes": 2,
            "watcher_active": true
        }"#;

        let decoded: DaemonStatusInfo = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(decoded.protocol_version, 0);
        assert_eq!(decoded.daemon_version, "");
        assert_eq!(decoded.pid, 12345);
        assert_eq!(decoded.daemon_mode, "");
        assert_eq!(decoded.config_path, None);
        assert_eq!(decoded.declared_projects_count, 0);
        assert_eq!(decoded.main_log_path, PathBuf::new());
        assert_eq!(decoded.memory_rss_mb, 0.0);
        assert_eq!(decoded.active_workspaces_count, 0);
        assert!(decoded.workspaces.is_empty());
        assert_eq!(decoded.hooks_served, 0);
        assert_eq!(decoded.submissions, 0);
        assert_eq!(decoded.callgraph_drops, 0);
        assert_eq!(decoded.ast_drops, 0);
        assert_eq!(decoded.raw_bytes_served, 0);
        assert_eq!(decoded.delivered_bytes_served, 0);
        assert_eq!(decoded.bytes_saved, 0);
        assert_eq!(decoded.token_reduction_pct, 0.0);
        assert_eq!(decoded.est_tokens_saved, 0);
        assert_eq!(decoded.macro_reads, 0);
        assert_eq!(decoded.micro_reads, 0);
        assert_eq!(decoded.omitted_micro_reads, 0);
        assert_eq!(decoded.omitted_full_read_skeletons, 0);
        assert_eq!(decoded.trimmed_advisories, 0);
        assert_eq!(decoded.parse_fallbacks, 0);
        assert_eq!(decoded.supplemental_context_bytes, 0);
        assert_eq!(decoded.supplemental_context_tokens, 0);
    }

    #[test]
    fn test_protocol_workspace_summary_backward_compatibility() {
        let legacy_json = r#"{
            "root": "/home/user/project",
            "is_worktree": false,
            "indexed_files": 10,
            "total_symbols": 100,
            "memory_mb": 12.3,
            "log_path": "/tmp/logs/project.log",
            "last_accessed_secs_ago": 5
        }"#;

        let decoded: WorkspaceSummary = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(decoded.root, PathBuf::from("/home/user/project"));
        assert_eq!(decoded.submissions, 0);
        assert_eq!(decoded.callgraph_drops, 0);
        assert_eq!(decoded.ast_drops, 0);
        assert_eq!(decoded.macro_reads, 0);
        assert_eq!(decoded.micro_reads, 0);
        assert_eq!(decoded.omitted_micro_reads, 0);
        assert_eq!(decoded.omitted_full_read_skeletons, 0);
        assert_eq!(decoded.trimmed_advisories, 0);
        assert_eq!(decoded.parse_fallbacks, 0);
    }

    #[test]
    fn test_protocol_advisory_context_backward_compatibility() {
        let legacy_json =
            r##"{"AdvisoryContext":{"file":"/tmp/test.rs","advisory_block":"# context"}}"##;
        let decoded: DaemonResponse = serde_json::from_str(legacy_json).unwrap();
        match decoded {
            DaemonResponse::AdvisoryContext {
                file,
                advisory_block,
                protocol_version,
            } => {
                assert_eq!(file, "/tmp/test.rs");
                assert_eq!(advisory_block, "# context");
                assert_eq!(protocol_version, 0);
            }
            _ => panic!("Expected AdvisoryContext"),
        }

        let current = DaemonResponse::AdvisoryContext {
            file: "/tmp/test.rs".to_string(),
            advisory_block: "# context".to_string(),
            protocol_version: PROTOCOL_VERSION,
        };
        let serialized = serde_json::to_string(&current).unwrap();
        let decoded: DaemonResponse = serde_json::from_str(&serialized).unwrap();
        match decoded {
            DaemonResponse::AdvisoryContext {
                protocol_version, ..
            } => {
                assert_eq!(protocol_version, PROTOCOL_VERSION);
            }
            _ => panic!("Expected AdvisoryContext"),
        }
    }

    #[test]
    fn test_protocol_pong_wire_compatibility() {
        let json = r#""Pong""#;
        let decoded: DaemonResponse = serde_json::from_str(json).unwrap();
        assert!(matches!(decoded, DaemonResponse::Pong));
        let serialized = serde_json::to_string(&DaemonResponse::Pong).unwrap();
        assert_eq!(serialized, r#""Pong""#);
    }

    #[test]
    fn test_protocol_post_read_hook_extended_fields() {
        let json = r#"{"PostReadHook":{"file":"/tmp/test.rs","total_lines":500,"vendor":"claude","is_full_read":true}}"#;
        let decoded: DaemonRequest = serde_json::from_str(json).unwrap();
        match decoded {
            DaemonRequest::PostReadHook {
                file,
                total_lines,
                vendor,
                is_full_read,
                ..
            } => {
                assert_eq!(file, PathBuf::from("/tmp/test.rs"));
                assert_eq!(total_lines, Some(500));
                assert_eq!(vendor, Some(AgentVendor::Claude));
                assert_eq!(is_full_read, Some(true));
            }
            _ => panic!("Expected PostReadHook"),
        }
    }

    #[test]
    fn test_protocol_distill_backward_compatibility_and_extended_fields() {
        let legacy_json =
            r#"{"Distill":{"file":"/tmp/test.rs","mode":"skeleton","format":"markdown"}}"#;
        let decoded: DaemonRequest = serde_json::from_str(legacy_json).unwrap();
        match decoded {
            DaemonRequest::Distill {
                file,
                mode,
                format,
                start_line,
                end_line,
                vendor,
                cross_boundary,
            } => {
                assert_eq!(file, PathBuf::from("/tmp/test.rs"));
                assert!(matches!(mode, DistillMode::Skeleton));
                assert!(matches!(format, OutputFormat::Markdown));
                assert_eq!(start_line, None);
                assert_eq!(end_line, None);
                assert_eq!(vendor, None);
                assert!(!cross_boundary);
            }
            _ => panic!("Expected Distill"),
        }

        let extended_json = r#"{"Distill":{"file":"/tmp/test.rs","mode":"bundle","format":"json","start_line":10,"end_line":25,"vendor":"agy","cross_boundary":true}}"#;
        let decoded_ext: DaemonRequest = serde_json::from_str(extended_json).unwrap();
        match decoded_ext {
            DaemonRequest::Distill {
                start_line,
                end_line,
                vendor,
                cross_boundary,
                ..
            } => {
                assert_eq!(start_line, Some(10));
                assert_eq!(end_line, Some(25));
                assert_eq!(vendor, Some(AgentVendor::Agy));
                assert!(cross_boundary);
            }
            _ => panic!("Expected Distill"),
        }
    }

    #[test]
    fn test_protocol_post_read_hook_backward_compatibility_and_fields() {
        let legacy_json = r#"{"PostReadHook":{"file":"/tmp/test.rs"}}"#;
        let decoded: DaemonRequest = serde_json::from_str(legacy_json).unwrap();
        match decoded {
            DaemonRequest::PostReadHook {
                file,
                start_line,
                end_line,
                raw_read_bytes,
                read_kind,
                total_lines,
                vendor,
                is_full_read,
            } => {
                assert_eq!(file, PathBuf::from("/tmp/test.rs"));
                assert_eq!(start_line, None);
                assert_eq!(end_line, None);
                assert_eq!(raw_read_bytes, None);
                assert_eq!(read_kind, None);
                assert_eq!(total_lines, None);
                assert_eq!(vendor, None);
                assert_eq!(is_full_read, None);
            }
            _ => panic!("Expected PostReadHook"),
        }

        let spanned_json = r#"{"PostReadHook":{"file":"/tmp/test.rs","start_line":10,"end_line":40,"raw_read_bytes":850}}"#;
        let decoded_spanned: DaemonRequest = serde_json::from_str(spanned_json).unwrap();
        match decoded_spanned {
            DaemonRequest::PostReadHook {
                file,
                start_line,
                end_line,
                raw_read_bytes,
                read_kind,
                total_lines,
                vendor,
                is_full_read,
            } => {
                assert_eq!(file, PathBuf::from("/tmp/test.rs"));
                assert_eq!(start_line, Some(10));
                assert_eq!(end_line, Some(40));
                assert_eq!(raw_read_bytes, Some(850));
                assert_eq!(read_kind, None);
                assert_eq!(total_lines, None);
                assert_eq!(vendor, None);
                assert_eq!(is_full_read, None);
            }
            _ => panic!("Expected PostReadHook"),
        }

        let explicit_kind_json = r#"{"PostReadHook":{"file":"/tmp/test.rs","start_line":10,"end_line":40,"raw_read_bytes":850,"read_kind":"Micro"}}"#;
        let decoded_kind: DaemonRequest = serde_json::from_str(explicit_kind_json).unwrap();
        match decoded_kind {
            DaemonRequest::PostReadHook { read_kind, .. } => {
                assert_eq!(read_kind, Some(ReadKind::Micro));
            }
            _ => panic!("Expected PostReadHook"),
        }

        let macro_kind_json = r#"{"PostReadHook":{"file":"/tmp/test.rs","read_kind":"Macro"}}"#;
        let decoded_macro: DaemonRequest = serde_json::from_str(macro_kind_json).unwrap();
        match decoded_macro {
            DaemonRequest::PostReadHook { read_kind, .. } => {
                assert_eq!(read_kind, Some(ReadKind::Macro));
            }
            _ => panic!("Expected PostReadHook"),
        }
    }
}
