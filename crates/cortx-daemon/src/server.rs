//! Unix domain socket IPC server.

use crate::error::DaemonError;
use crate::hook::execute_post_read_query;
use crate::logging::DaemonLogManager;
use crate::protocol::{
    DaemonRequest, DaemonResponse, DaemonStatusInfo, PROTOCOL_VERSION, WorkspaceSummary,
    read_message, write_message,
};
use crate::registry::{WorkspaceEntry, WorkspaceRegistry};
use crate::watcher::DynamicWatcherManager;
use cortx_core::{AgentVendor, ReadCoverage, ReadKind, ReadSpan};
use cortx_distill::bundle::AdvisoryDisposition;
use cortx_distill::{DistillMode, DistillQuery, OverlayView, SymbolIndex};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::RwLock;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

/// Lock-free server telemetry counters.
#[derive(Debug, Default)]
pub struct ServerTelemetry {
    pub hooks_served: AtomicU64,
    pub submissions: AtomicU64,
    pub callgraph_drops: AtomicU64,
    pub ast_drops: AtomicU64,
    pub raw_bytes_served: AtomicU64,
    pub delivered_bytes_served: AtomicU64,
    pub macro_reads: AtomicU64,
    pub micro_reads: AtomicU64,
    pub omitted_micro_reads: AtomicU64,
    pub omitted_full_read_skeletons: AtomicU64,
    pub trimmed_advisories: AtomicU64,
}

impl ServerTelemetry {
    /// Records hook metrics (served count, raw bytes inspected, delivered advisory bytes).
    pub fn record_hook_metrics(&self, raw_bytes: u64, delivered_bytes: u64) {
        self.hooks_served.fetch_add(1, Ordering::Relaxed);
        self.raw_bytes_served
            .fetch_add(raw_bytes, Ordering::Relaxed);
        self.delivered_bytes_served
            .fetch_add(delivered_bytes, Ordering::Relaxed);
    }

    /// Increments the count of successful advisory context submissions.
    pub fn record_submission(&self) {
        self.submissions.fetch_add(1, Ordering::Relaxed);
    }

    /// Increments the count of call graph pruning events.
    pub fn record_callgraph_drop(&self) {
        self.callgraph_drops.fetch_add(1, Ordering::Relaxed);
    }

    /// Increments the count of total advisory context omissions.
    pub fn record_ast_drop(&self) {
        self.ast_drops.fetch_add(1, Ordering::Relaxed);
    }

    /// Increments the count for the given read kind (macro vs micro).
    pub fn record_read_kind(&self, kind: ReadKind) {
        match kind {
            ReadKind::Macro => {
                self.macro_reads.fetch_add(1, Ordering::Relaxed);
            }
            ReadKind::Micro => {
                self.micro_reads.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Returns the total macro reads count.
    pub fn macro_reads(&self) -> u64 {
        self.macro_reads.load(Ordering::Relaxed)
    }

    /// Returns the total micro reads count.
    pub fn micro_reads(&self) -> u64 {
        self.micro_reads.load(Ordering::Relaxed)
    }

    /// Returns the total omitted micro reads count.
    pub fn omitted_micro_reads(&self) -> u64 {
        self.omitted_micro_reads.load(Ordering::Relaxed)
    }

    /// Returns the total omitted full-read skeletons count.
    pub fn omitted_full_read_skeletons(&self) -> u64 {
        self.omitted_full_read_skeletons.load(Ordering::Relaxed)
    }

    /// Returns the total trimmed advisories count.
    pub fn trimmed_advisories(&self) -> u64 {
        self.trimmed_advisories.load(Ordering::Relaxed)
    }

    /// Returns a snapshot of cumulative telemetry: (hooks_served, raw_bytes, delivered_bytes).
    pub fn snapshot(&self) -> (u64, u64, u64) {
        let hooks = self.hooks_served.load(Ordering::Relaxed);
        let raw = self.raw_bytes_served.load(Ordering::Relaxed);
        let delivered = self.delivered_bytes_served.load(Ordering::Relaxed);
        (hooks, raw, delivered)
    }
}

/// Configuration options for launching the daemon server.
#[derive(Debug, Clone)]
pub struct ServerOptions {
    /// Bound socket path (if empty, defaults to the global system socket path).
    pub socket_path: PathBuf,
    /// Monitored workspace root directory.
    pub workspace_root: PathBuf,
    /// Indicates whether the background file watcher is active.
    pub watcher_active: bool,
    /// Directory for file logging (default: system state dir).
    pub log_dir: Option<PathBuf>,
    /// Inactivity threshold in seconds before unindexing an idle workspace (default: 7200).
    pub idle_ttl_secs: u64,
    /// Loaded daemon configuration (projects, service settings).
    pub daemon_config: cortx_core::CortxConfig,
    /// Canonical provenance path of the loaded daemon configuration file, if any.
    pub config_path: Option<PathBuf>,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            socket_path: PathBuf::new(),
            workspace_root: PathBuf::new(),
            watcher_active: false,
            log_dir: None,
            idle_ttl_secs: 7200,
            daemon_config: cortx_core::CortxConfig::default(),
            config_path: None,
        }
    }
}

/// Internal shared state for connection handlers.
struct ServerState {
    options: Arc<ServerOptions>,
    start_time: std::time::Instant,
    legacy_index: Arc<RwLock<SymbolIndex>>,
    registry: Arc<WorkspaceRegistry>,
    log_manager: Arc<DaemonLogManager>,
    watcher_manager: Option<Arc<DynamicWatcherManager>>,
    token: CancellationToken,
    telemetry: Arc<ServerTelemetry>,
}

/// RAII Socket cleaner to remove socket file on drop.
pub struct SocketGuard {
    path: PathBuf,
}

impl SocketGuard {
    /// Creates a new guard tracking socket path.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Probes whether an active daemon is responsive on the given socket path.
async fn probe_active_socket(socket_path: &Path) -> bool {
    let connect_fut = UnixStream::connect(socket_path);
    let Ok(Ok(mut stream)) = timeout(Duration::from_millis(50), connect_fut).await else {
        return false;
    };

    let ping_fut = async {
        write_message(&mut stream, &DaemonRequest::Ping).await?;
        let resp: DaemonResponse = read_message(&mut stream).await?;
        Ok::<_, DaemonError>(resp)
    };

    matches!(
        timeout(Duration::from_millis(50), ping_fut).await,
        Ok(Ok(DaemonResponse::Pong))
    )
}

async fn init_server_state(
    options: ServerOptions,
    index: Arc<RwLock<SymbolIndex>>,
    token: CancellationToken,
    start_time: std::time::Instant,
) -> Result<Arc<ServerState>, DaemonError> {
    let pid = std::process::id();
    let log_dir = options
        .log_dir
        .clone()
        .unwrap_or_else(cortx_core::logging::resolve_default_log_dir);
    let log_manager = Arc::new(DaemonLogManager::init_with_options(
        &log_dir,
        options.daemon_config.daemon.workspace_logging,
    )?);
    let cfg_label = options
        .config_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "built-in defaults".to_string());
    log_manager.log_daemon(&format!(
        "Cortx daemon IPC server started: pid={}, socket='{}', config='{}', declared_projects={}",
        pid,
        options.socket_path.display(),
        cfg_label,
        options.daemon_config.projects.len()
    ));

    let registry = Arc::new(WorkspaceRegistry::with_config(
        Arc::clone(&log_manager),
        options.daemon_config.clone(),
        options.config_path.clone(),
    ));

    let watcher_manager = if options.watcher_active {
        Some(Arc::new(DynamicWatcherManager::new(token.clone())?))
    } else {
        None
    };

    let raw_root = &options.workspace_root;
    if !raw_root.as_os_str().is_empty() {
        let canonical_root = std::fs::canonicalize(raw_root).unwrap_or_else(|_| raw_root.clone());
        if let Ok(ws_logger) = log_manager.create_workspace_logger(&canonical_root) {
            let (project_name, project_config, config_path) =
                registry.resolve_project_for_root(&canonical_root, None);
            let ws_entry = Arc::new(
                WorkspaceEntry::new(
                    canonical_root.clone(),
                    false,
                    None,
                    Arc::clone(&index),
                    None,
                    Arc::new(ws_logger),
                )
                .with_project_metadata(
                    project_name,
                    config_path,
                    project_config.clone(),
                ),
            );
            registry.register(ws_entry.clone()).await;
            if let Some(ref w) = watcher_manager {
                let _ = w.watch_workspace_with_config(
                    canonical_root,
                    Arc::clone(&index),
                    Arc::clone(&ws_entry.logger),
                    project_config,
                );
            }
        }
    }

    Ok(Arc::new(ServerState {
        options: Arc::new(options),
        start_time,
        legacy_index: index,
        registry,
        log_manager,
        watcher_manager,
        token,
        telemetry: Arc::new(ServerTelemetry::default()),
    }))
}

/// Runs the IPC daemon server with explicit options until shutdown token is cancelled.
pub async fn run_server_with_options(
    mut options: ServerOptions,
    index: Arc<RwLock<SymbolIndex>>,
    token: CancellationToken,
) -> Result<(), DaemonError> {
    if options.socket_path.as_os_str().is_empty() {
        options.socket_path = cortx_core::workspace::resolve_global_socket();
    }
    let socket_path = &options.socket_path;
    if let Some(parent) = socket_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let listener = match UnixListener::bind(socket_path) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if probe_active_socket(socket_path).await {
                return Err(DaemonError::SocketAlreadyBound(socket_path.clone()));
            }
            let _ = std::fs::remove_file(socket_path);
            UnixListener::bind(socket_path).map_err(|source| DaemonError::Socket {
                path: socket_path.clone(),
                source,
            })?
        }
        Err(source) => {
            return Err(DaemonError::Socket {
                path: socket_path.clone(),
                source,
            });
        }
    };

    let _guard = SocketGuard::new(socket_path.clone());
    let start_time = std::time::Instant::now();
    let pid = std::process::id();
    tracing::info!(socket = %socket_path.display(), pid, "Starting cortx daemon IPC server");

    let state = init_server_state(options, index, token.clone(), start_time).await?;

    let mut prune_interval = tokio::time::interval(Duration::from_secs(60));
    prune_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    prune_interval.tick().await;

    loop {
        tokio::select! {
            _ = token.cancelled() => {
                break;
            }
            _ = prune_interval.tick() => {
                let idle_ttl = Duration::from_secs(state.options.idle_ttl_secs);
                let pruned = state.registry.prune_idle_workspaces(idle_ttl).await;
                if let Some(ref watcher) = state.watcher_manager {
                    for p in &pruned {
                        let _ = watcher.unwatch_workspace(p);
                    }
                }
            }
            res = listener.accept() => {
                match res {
                    Ok((stream, _addr)) => {
                        let st = Arc::clone(&state);
                        tracing::debug!(socket = %st.options.socket_path.display(), "Accepted new IPC connection");
                        tokio::spawn(async move {
                            let _ = handle_client(stream, st).await;
                        });
                    }
                    Err(e) => {
                        tracing::warn!("Failed accepting connection: {}", e);
                    }
                }
            }
        }
    }

    Ok(())
}

/// Runs the IPC daemon server loop until shutdown token is cancelled.
pub async fn run_server(
    socket_path: &Path,
    index: Arc<RwLock<SymbolIndex>>,
    token: CancellationToken,
) -> Result<(), DaemonError> {
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let opts = ServerOptions {
        socket_path: socket_path.to_path_buf(),
        workspace_root: root,
        ..ServerOptions::default()
    };
    run_server_with_options(opts, index, token).await
}

struct PostReadParams<'a> {
    file: &'a Path,
    start_line: Option<usize>,
    end_line: Option<usize>,
    raw_read_bytes: Option<u64>,
    read_kind: Option<ReadKind>,
    total_lines: Option<usize>,
    vendor: Option<AgentVendor>,
    is_full_read: Option<bool>,
}

fn classify_coverage(
    span: &ReadSpan,
    total_lines: Option<usize>,
    raw_bytes: Option<u64>,
    is_full_read: Option<bool>,
    read_kind: Option<ReadKind>,
    env: &cortx_core::span::ReadBudgetEnv,
) -> ReadCoverage {
    if read_kind == Some(ReadKind::Micro) {
        return ReadCoverage::Micro;
    }
    if is_full_read == Some(true) && total_lines.is_none() {
        return ReadCoverage::FullFile;
    }
    cortx_core::classify_read_coverage(span, total_lines, raw_bytes, env)
}

fn format_range(start: Option<usize>, end: Option<usize>) -> String {
    match (start, end) {
        (Some(s), Some(e)) => format!(" (lines {}..{})", s, e),
        (Some(s), None) => format!(" (lines {}..)", s),
        (None, Some(e)) => format!(" (lines ..{})", e),
        (None, None) => String::new(),
    }
}

async fn handle_post_read_request(
    params: PostReadParams<'_>,
    req_start: std::time::Instant,
    state: &ServerState,
) -> DaemonResponse {
    let file = params.file;
    let env = cortx_core::span::detect_budget_env(|k| std::env::var(k).ok());
    let span = ReadSpan {
        start_line: params.start_line,
        end_line: params.end_line,
        vendor: params.vendor,
        ..Default::default()
    };

    let initial_coverage = classify_coverage(
        &span,
        params.total_lines,
        params.raw_read_bytes,
        params.is_full_read,
        params.read_kind,
        &env,
    );

    if initial_coverage == ReadCoverage::Micro {
        let entry = state.registry.get(file).await;
        if let Some(ref ws) = entry {
            ws.record_read(ReadKind::Micro);
            ws.record_omitted_micro_read();
        }
        state.telemetry.record_read_kind(ReadKind::Micro);
        state
            .telemetry
            .omitted_micro_reads
            .fetch_add(1, Ordering::Relaxed);

        let raw_len = params.raw_read_bytes.unwrap_or(0);
        state.telemetry.record_hook_metrics(raw_len, 0);
        let elapsed_us = req_start.elapsed().as_micros();
        let range_str = format_range(params.start_line, params.end_line);
        let log_msg = format!(
            "[MICRO] PostReadHook skipped (micro-read): '{}'{} | raw: {} B | delivered: 0 B | latency: {:.1}ms",
            file.display(),
            range_str,
            raw_len,
            elapsed_us as f64 / 1000.0,
        );
        state.log_manager.log_daemon(&log_msg);
        if let Some(ref ws) = entry {
            ws.logger.log(&log_msg);
        }

        return DaemonResponse::AdvisoryContext {
            file: file.display().to_string(),
            advisory_block: String::new(),
            protocol_version: PROTOCOL_VERSION,
        };
    }

    execute_macro_post_read(params, span, env, req_start, state).await
}

fn record_packed_disposition_telemetry(
    packed: &cortx_distill::bundle::PackedAdvisory,
    state: &ServerState,
    entry: Option<&WorkspaceEntry>,
) {
    match packed.disposition {
        AdvisoryDisposition::OmittedMicro => {
            state
                .telemetry
                .omitted_micro_reads
                .fetch_add(1, Ordering::Relaxed);
            if let Some(ws) = entry {
                ws.record_omitted_micro_read();
            }
        }
        AdvisoryDisposition::FullReadNoSkeleton => {
            state
                .telemetry
                .omitted_full_read_skeletons
                .fetch_add(1, Ordering::Relaxed);
            if let Some(ws) = entry {
                ws.record_omitted_full_read_skeleton();
            }
        }
        AdvisoryDisposition::Trimmed => {
            state
                .telemetry
                .trimmed_advisories
                .fetch_add(1, Ordering::Relaxed);
            if let Some(ws) = entry {
                ws.record_trimmed_advisory();
            }
        }
        AdvisoryDisposition::PartialMacroDelivered => {}
    }

    if !packed.markdown.is_empty() {
        state.telemetry.record_submission();
        if let Some(ws) = entry {
            ws.record_submission();
        }
    }
}

async fn execute_macro_post_read(
    params: PostReadParams<'_>,
    span: ReadSpan,
    env: cortx_core::span::ReadBudgetEnv,
    req_start: std::time::Instant,
    state: &ServerState,
) -> DaemonResponse {
    let file = params.file;
    let entry = match state.registry.get_or_init(file).await {
        Ok(e) => {
            e.record_read(ReadKind::Macro);
            if let Some(ref watcher) = state.watcher_manager {
                if !watcher.is_watching(&e.root) {
                    let _ = watcher.watch_workspace_with_config(
                        e.root.clone(),
                        Arc::clone(&e.index),
                        Arc::clone(&e.logger),
                        e.config.clone(),
                    );
                }
            }
            Some(e)
        }
        Err(e) => {
            tracing::debug!(
                file = %file.display(),
                error = %e,
                "Workspace could not be auto-initialized; falling back to default index"
            );
            None
        }
    };
    state.telemetry.record_read_kind(ReadKind::Macro);

    let target_file = file.to_path_buf();
    let ws_cfg = entry.as_ref().map(|e| e.config.clone());
    let index_handle = entry
        .as_ref()
        .map(|e| Arc::clone(&e.index))
        .unwrap_or_else(|| Arc::clone(&state.legacy_index));
    let base_handle = entry
        .as_ref()
        .and_then(|e| e.base_index.as_ref().map(Arc::clone));
    let shadowed_handle = entry.as_ref().map(|e| Arc::clone(&e.shadowed_files));
    let params_raw_bytes = params.raw_read_bytes;
    let params_total_lines = params.total_lines;
    let params_is_full_read = params.is_full_read;

    let block_res = tokio::task::spawn_blocking(move || {
        let source = std::fs::read_to_string(&target_file)
            .map_err(|e| DaemonError::Watcher(e.to_string()))?;
        let src_bytes = source.len() as u64;
        let tot_lines = params_total_lines.unwrap_or_else(|| source.split_inclusive('\n').count());
        let raw_bytes_for_calc = params_raw_bytes.unwrap_or(src_bytes);

        let coverage = classify_coverage(
            &span,
            Some(tot_lines),
            Some(raw_bytes_for_calc),
            params_is_full_read,
            None,
            &env,
        );

        let base_guard = base_handle.as_ref().map(|b| b.blocking_read());
        let idx_guard = index_handle.blocking_read();
        let shadowed_guard = shadowed_handle.as_ref().map(|s| s.blocking_read());

        let (base_ref, overlay_ref, shadow_ref) = if base_handle.is_some() {
            (
                base_guard.as_deref(),
                Some(&*idx_guard),
                shadowed_guard.as_deref(),
            )
        } else {
            (Some(&*idx_guard), None, None)
        };

        let view = OverlayView {
            base: base_ref,
            overlay: overlay_ref,
            shadowed_files: shadow_ref,
        };

        let query = DistillQuery {
            file_path: &target_file,
            mode: DistillMode::Bundle,
            coverage,
            span: Some(span),
            preloaded_source: Some(&source),
            cross_boundary: true,
            project_cfg: ws_cfg.as_ref(),
        };

        let bundle = execute_post_read_query(&query, view)?;
        let packed = bundle.pack_for_hook(coverage);
        Ok::<_, DaemonError>((packed, raw_bytes_for_calc, coverage))
    })
    .await;

    match block_res {
        Ok(Ok((packed, raw_len, coverage))) => {
            let delivered_len = packed.markdown.len() as u64;
            let elapsed_us = req_start.elapsed().as_micros();
            let range_str = format_range(params.start_line, params.end_line);

            record_packed_disposition_telemetry(&packed, state, entry.as_deref());

            state.telemetry.record_hook_metrics(raw_len, delivered_len);
            let kind_tag = match coverage {
                ReadCoverage::Micro => "MICRO",
                ReadCoverage::FullFile => "FULL_FILE",
                ReadCoverage::PartialMacro => "PARTIAL_MACRO",
            };

            let log_msg = format!(
                "[{kind_tag}] PostReadHook served ({:?}): '{}'{} | raw: {} B | delivered: {} B | latency: {:.1}ms",
                packed.disposition,
                file.display(),
                range_str,
                raw_len,
                delivered_len,
                elapsed_us as f64 / 1000.0,
            );
            state.log_manager.log_daemon(&log_msg);
            if let Some(ref ws) = entry {
                ws.logger.log(&log_msg);
            }

            DaemonResponse::AdvisoryContext {
                file: file.display().to_string(),
                advisory_block: packed.markdown,
                protocol_version: PROTOCOL_VERSION,
            }
        }
        Ok(Err(e)) => {
            let err_msg = format!(
                "[MACRO] PostReadHook failed for '{}': {}",
                file.display(),
                e
            );
            state.log_manager.log_daemon(&err_msg);
            if let Some(ref ws) = entry {
                ws.logger.log(&err_msg);
            }
            DaemonResponse::Error {
                message: e.to_string(),
            }
        }
        Err(join_err) => {
            let err_msg = format!(
                "[MACRO] PostReadHook task panicked for '{}': {}",
                file.display(),
                join_err
            );
            state.log_manager.log_daemon(&err_msg);
            DaemonResponse::Error { message: err_msg }
        }
    }
}

struct DistillParams<'a> {
    file: &'a Path,
    mode: DistillMode,
    format: cortx_core::output::OutputFormat,
    start_line: Option<usize>,
    end_line: Option<usize>,
    vendor: Option<AgentVendor>,
    cross_boundary: bool,
}

async fn handle_distill_request(params: DistillParams<'_>, state: &ServerState) -> DaemonResponse {
    let req_start = std::time::Instant::now();
    let file = params.file;
    let entry = match state.registry.get_or_init(file).await {
        Ok(e) => {
            if let Some(ref watcher) = state.watcher_manager {
                if !watcher.is_watching(&e.root) {
                    let _ = watcher.watch_workspace_with_config(
                        e.root.clone(),
                        Arc::clone(&e.index),
                        Arc::clone(&e.logger),
                        e.config.clone(),
                    );
                }
            }
            Some(e)
        }
        Err(e) => {
            tracing::debug!(
                file = %file.display(),
                error = %e,
                "Workspace could not be auto-initialized; falling back to default index"
            );
            None
        }
    };

    let raw_len = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
    let span =
        if params.start_line.is_some() || params.end_line.is_some() || params.vendor.is_some() {
            Some(ReadSpan {
                start_line: params.start_line,
                end_line: params.end_line,
                vendor: params.vendor,
                ..Default::default()
            })
        } else {
            None
        };

    let project_cfg = entry.as_ref().map(|e| &e.config);
    let query = DistillQuery {
        file_path: file,
        mode: params.mode,
        coverage: ReadCoverage::PartialMacro,
        span,
        preloaded_source: None,
        cross_boundary: params.cross_boundary,
        project_cfg,
    };

    let distill_res = if let Some(ref ws) = entry {
        if let Some(ref base) = ws.base_index {
            let base_guard = base.read().await;
            let ov_guard = ws.index.read().await;
            let shadowed_guard = ws.shadowed_files.read().await;
            let view = OverlayView {
                base: Some(&*base_guard),
                overlay: Some(&*ov_guard),
                shadowed_files: Some(&*shadowed_guard),
            };
            cortx_distill::distill_with_query(&query, view)
        } else {
            let guard = ws.index.read().await;
            let view = OverlayView {
                base: Some(&*guard),
                overlay: None,
                shadowed_files: None,
            };
            cortx_distill::distill_with_query(&query, view)
        }
    } else {
        let guard = state.legacy_index.read().await;
        let view = OverlayView {
            base: Some(&*guard),
            overlay: None,
            shadowed_files: None,
        };
        cortx_distill::distill_with_query(&query, view)
    };

    match distill_res {
        Ok(bundle) => {
            let elapsed_us = req_start.elapsed().as_micros();
            let content = bundle.format_with(params.format).unwrap_or_default();
            let delivered_len = content.len() as u64;
            state.telemetry.record_hook_metrics(raw_len, delivered_len);
            let log_msg = format!(
                "Distill served: '{}' (mode: {:?}) | raw: {} B | delivered: {} B | latency: {:.1}ms",
                file.display(),
                params.mode,
                raw_len,
                delivered_len,
                elapsed_us as f64 / 1000.0,
            );
            state.log_manager.log_daemon(&log_msg);
            if let Some(ref ws) = entry {
                ws.logger.log(&log_msg);
            }
            DaemonResponse::DistillResult { content }
        }
        Err(e) => {
            let err_msg = format!("Distill failed for '{}': {}", file.display(), e);
            state.log_manager.log_daemon(&err_msg);
            if let Some(ref ws) = entry {
                ws.logger.log(&err_msg);
            }
            DaemonResponse::Error {
                message: e.to_string(),
            }
        }
    }
}

async fn build_status_response(state: &ServerState) -> DaemonStatusInfo {
    let memory_rss = cortx_core::logging::get_process_rss_mb();
    let entries = state.registry.entries().await;
    let mut ws_summaries = Vec::with_capacity(entries.len());
    let mut agg_files = 0;
    let mut agg_symbols = 0;
    let mut agg_call_edges = 0;
    let mut agg_contracts = 0;
    let mut agg_routes = 0;
    let mut agg_parse_fallbacks = 0;

    for entry in &entries {
        let (metrics, mem_mb, parse_fallbacks) = {
            let guard = entry.index.read().await;
            (
                guard.metrics(),
                guard.estimate_memory_mb(),
                guard.parse_fallbacks,
            )
        };
        agg_files += metrics.distinct_files_count;
        agg_symbols += metrics.total_symbols_count;
        agg_call_edges += metrics.total_call_edges_count;
        agg_contracts += metrics.total_contracts_count;
        agg_routes += metrics.total_routes_count;
        agg_parse_fallbacks += parse_fallbacks as u64;

        ws_summaries.push(WorkspaceSummary {
            root: entry.root.clone(),
            project_name: entry.project_name.clone(),
            config_path: entry.config_path.clone(),
            is_worktree: entry.is_worktree,
            parent_repo: entry.parent_repo.clone(),
            indexed_files: metrics.distinct_files_count,
            total_symbols: metrics.total_symbols_count,
            memory_mb: mem_mb,
            log_path: entry.logger.log_path().to_path_buf(),
            last_accessed_secs_ago: entry.last_accessed_secs_ago(),
            submissions: entry.submissions(),
            callgraph_drops: entry.callgraph_drops(),
            ast_drops: entry.ast_drops(),
            macro_reads: entry.macro_reads(),
            micro_reads: entry.micro_reads(),
            omitted_micro_reads: entry.omitted_micro_reads(),
            omitted_full_read_skeletons: entry.omitted_full_read_skeletons(),
            trimmed_advisories: entry.trimmed_advisories(),
            parse_fallbacks: parse_fallbacks as u64,
        });
    }

    if entries.is_empty() {
        let guard = state.legacy_index.read().await;
        let m = guard.metrics();
        agg_files = m.distinct_files_count;
        agg_symbols = m.total_symbols_count;
        agg_call_edges = m.total_call_edges_count;
        agg_contracts = m.total_contracts_count;
        agg_routes = m.total_routes_count;
        agg_parse_fallbacks = guard.parse_fallbacks as u64;
    }

    let daemon_mode = if state.options.workspace_root.as_os_str().is_empty() {
        "System-Wide Multi-Tenant".to_string()
    } else {
        "Isolated Workspace".to_string()
    };

    let (hooks, raw, delivered) = state.telemetry.snapshot();
    let supp_tokens = delivered / cortx_core::span::EST_BYTES_PER_TOKEN;
    DaemonStatusInfo {
        protocol_version: PROTOCOL_VERSION,
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        pid: std::process::id(),
        uptime_secs: state.start_time.elapsed().as_secs(),
        socket_path: state.options.socket_path.clone(),
        daemon_mode,
        config_path: state.options.config_path.clone(),
        declared_projects_count: state.options.daemon_config.projects.len(),
        workspace_root: state.options.workspace_root.clone(),
        main_log_path: state.log_manager.main_log_path().to_path_buf(),
        memory_rss_mb: memory_rss,
        active_workspaces_count: entries.len(),
        workspaces: ws_summaries,
        indexed_files: agg_files,
        total_symbols: agg_symbols,
        total_call_edges: agg_call_edges,
        total_contracts: agg_contracts,
        total_routes: agg_routes,
        watcher_active: state.options.watcher_active,
        hooks_served: hooks,
        submissions: state.telemetry.submissions.load(Ordering::Relaxed),
        callgraph_drops: state.telemetry.callgraph_drops.load(Ordering::Relaxed),
        ast_drops: state.telemetry.ast_drops.load(Ordering::Relaxed),
        raw_bytes_served: raw,
        delivered_bytes_served: delivered,
        macro_reads: state.telemetry.macro_reads.load(Ordering::Relaxed),
        micro_reads: state.telemetry.micro_reads.load(Ordering::Relaxed),
        omitted_micro_reads: state.telemetry.omitted_micro_reads.load(Ordering::Relaxed),
        omitted_full_read_skeletons: state
            .telemetry
            .omitted_full_read_skeletons
            .load(Ordering::Relaxed),
        trimmed_advisories: state.telemetry.trimmed_advisories.load(Ordering::Relaxed),
        parse_fallbacks: agg_parse_fallbacks,
        supplemental_context_bytes: delivered,
        supplemental_context_tokens: supp_tokens,
    }
}

async fn handle_client(mut stream: UnixStream, state: Arc<ServerState>) -> Result<(), DaemonError> {
    loop {
        let req: DaemonRequest = match read_message(&mut stream).await {
            Ok(r) => {
                tracing::debug!(req = ?r, "Received daemon request");
                r
            }
            Err(_) => {
                tracing::debug!("Client disconnected");
                break;
            }
        };

        let req_start = std::time::Instant::now();
        match req {
            DaemonRequest::Ping => {
                write_message(&mut stream, &DaemonResponse::Pong).await?;
            }
            DaemonRequest::Status => {
                let status_info = build_status_response(&state).await;
                write_message(&mut stream, &DaemonResponse::Status(status_info)).await?;
            }
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
                let params = PostReadParams {
                    file: &file,
                    start_line,
                    end_line,
                    raw_read_bytes,
                    read_kind,
                    total_lines,
                    vendor,
                    is_full_read,
                };
                let resp = handle_post_read_request(params, req_start, &state).await;
                write_message(&mut stream, &resp).await?;
            }
            DaemonRequest::Distill {
                file,
                mode,
                format,
                start_line,
                end_line,
                vendor,
                cross_boundary,
            } => {
                let params = DistillParams {
                    file: &file,
                    mode,
                    format,
                    start_line,
                    end_line,
                    vendor,
                    cross_boundary,
                };
                let resp = handle_distill_request(params, &state).await;
                write_message(&mut stream, &resp).await?;
            }
            DaemonRequest::Shutdown => {
                let (hooks, raw, delivered) = state.telemetry.snapshot();
                state.log_manager.log_daemon(&format!(
                    "Daemon stopping: hooks_served={}, raw_bytes={}, delivered_bytes={}",
                    hooks, raw, delivered
                ));
                write_message(&mut stream, &DaemonResponse::ShutdownAck).await?;
                state.token.cancel();
                break;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
            tokio::spawn(
                async move { run_server_with_options(server_opts, index, token_clone).await },
            );

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
}
