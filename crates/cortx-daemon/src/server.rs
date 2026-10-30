//! Unix domain socket IPC server.

use crate::error::DaemonError;
use crate::hook::{execute_post_read_bundle, execute_post_read_bundle_for_project};
use crate::logging::DaemonLogManager;
use crate::protocol::{
    DaemonRequest, DaemonResponse, DaemonStatusInfo, WorkspaceSummary, read_message, write_message,
};
use crate::registry::{WorkspaceEntry, WorkspaceRegistry};
use crate::watcher::DynamicWatcherManager;
use cortx_core::ReadKind;
use cortx_distill::{SymbolIndex, distill_target, distill_target_for_project};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::RwLock;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

/// Lock-free context bloat reduction telemetry counters.
#[derive(Debug, Default)]
pub struct ServerTelemetry {
    pub hooks_served: AtomicU64,
    pub submissions: AtomicU64,
    pub callgraph_drops: AtomicU64,
    pub ast_drops: AtomicU64,
    pub raw_bytes_served: AtomicU64,
    pub delivered_bytes_served: AtomicU64,
    pub bytes_saved: AtomicU64,
    pub macro_reads: AtomicU64,
    pub micro_reads: AtomicU64,
}

impl ServerTelemetry {
    /// Records reduction metrics for a single request and returns the per-request reduction percentage.
    pub fn record_reduction(&self, raw_bytes: u64, delivered_bytes: u64) -> f64 {
        self.hooks_served.fetch_add(1, Ordering::Relaxed);
        self.raw_bytes_served
            .fetch_add(raw_bytes, Ordering::Relaxed);
        self.delivered_bytes_served
            .fetch_add(delivered_bytes, Ordering::Relaxed);
        let saved = raw_bytes.saturating_sub(delivered_bytes);
        self.bytes_saved.fetch_add(saved, Ordering::Relaxed);
        if raw_bytes > 0 {
            ((raw_bytes as f64 - delivered_bytes as f64) / raw_bytes as f64) * 100.0
        } else {
            0.0
        }
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

    /// Returns a snapshot of cumulative telemetry computed directly from `raw` vs `delivered`:
    /// (hooks_served, raw_bytes, delivered_bytes, bytes_saved, reduction_pct, est_tokens_saved).
    pub fn snapshot(&self) -> (u64, u64, u64, u64, f64, u64) {
        let hooks = self.hooks_served.load(Ordering::Relaxed);
        let raw = self.raw_bytes_served.load(Ordering::Relaxed);
        let delivered = self.delivered_bytes_served.load(Ordering::Relaxed);
        let saved = raw.saturating_sub(delivered);
        let pct = if raw > 0 {
            ((raw as f64 - delivered as f64) / raw as f64) * 100.0
        } else {
            0.0
        };
        let est_tokens = saved / cortx_core::span::EST_BYTES_PER_TOKEN;
        (hooks, raw, delivered, saved, pct, est_tokens)
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

    if socket_path.exists() {
        if probe_active_socket(socket_path).await {
            return Err(DaemonError::SocketAlreadyBound(socket_path.clone()));
        }
        let _ = std::fs::remove_file(socket_path);
    }

    let listener = UnixListener::bind(socket_path).map_err(|source| DaemonError::Socket {
        path: socket_path.clone(),
        source,
    })?;

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
}

fn classify_read(params: &PostReadParams<'_>, file: &Path) -> ReadKind {
    if let Some(kind) = params.read_kind {
        return kind;
    }
    let span = cortx_core::span::ReadSpan {
        start_line: params.start_line,
        end_line: params.end_line,
        ..Default::default()
    };
    let total_lines = std::fs::read_to_string(file)
        .map(|c| c.split_inclusive('\n').count())
        .unwrap_or(0);
    span.classify(total_lines)
}

/// Per-request context for applying a bloat-guard outcome.
struct GuardContext<'a> {
    state: &'a ServerState,
    entry: Option<&'a WorkspaceEntry>,
    file: &'a Path,
    range_str: &'a str,
    raw_len: u64,
}

fn log_guard_warning(ctx: &GuardContext<'_>, msg: &str) {
    tracing::warn!("{}", msg);
    let file_log = format!("[WARN] {}", msg);
    ctx.state.log_manager.log_daemon(&file_log);
    if let Some(ws) = ctx.entry {
        ws.logger.log(&file_log);
    }
}

/// Applies bloat-guard telemetry and logging based on the degradation outcome.
///
/// Returns `(delivered_len, action_label)`.
///
/// # Delivered Length Semantics
/// For `Full` and `CallGraphPruned`, `delivered_len` is the exact byte length of the
/// generated markdown payload. For `Dropped`, `delivered_len` is set equal to `raw_len`
/// (representing that only the agent's raw read was consumed, with zero advisory bloat),
/// ensuring token reduction calculations yield 0% rather than negative bloat.
fn apply_guard_outcome(
    ctx: &GuardContext<'_>,
    outcome: &cortx_distill::bundle::BudgetedAdvisory,
) -> (u64, &'static str) {
    match outcome.stage {
        cortx_distill::bundle::BudgetStage::Full => {
            ctx.state.telemetry.record_submission();
            if let Some(ws) = ctx.entry {
                ws.record_submission();
            }
            (outcome.markdown.len() as u64, "PostReadHook served (full)")
        }
        cortx_distill::bundle::BudgetStage::CallGraphPruned => {
            ctx.state.telemetry.record_submission();
            ctx.state.telemetry.record_callgraph_drop();
            if let Some(ws) = ctx.entry {
                ws.record_submission();
                ws.record_callgraph_drop();
            }
            let full_len = outcome.full_len.unwrap_or(outcome.pruned_len);
            let reduction = if ctx.raw_len > 0 {
                ((ctx.raw_len as f64 - outcome.pruned_len as f64) / ctx.raw_len as f64) * 100.0
            } else {
                0.0
            };
            let warn_msg = format!(
                "PostReadHook pruned call graph: bundle ({} B) >= read request ({} B), retained skeleton ({} B, {:.1}% reduction) for '{}'{}",
                full_len,
                ctx.raw_len,
                outcome.pruned_len,
                reduction,
                ctx.file.display(),
                ctx.range_str
            );
            log_guard_warning(ctx, &warn_msg);
            (
                outcome.markdown.len() as u64,
                "PostReadHook served (pruned call graph)",
            )
        }
        cortx_distill::bundle::BudgetStage::Dropped => {
            ctx.state.telemetry.record_ast_drop();
            if let Some(ws) = ctx.entry {
                ws.record_ast_drop();
            }
            let warn_msg = format!(
                "PostReadHook dropped all advisory context: skeleton ({} B) >= read request ({} B) for '{}'{}",
                outcome.pruned_len,
                ctx.raw_len,
                ctx.file.display(),
                ctx.range_str
            );
            log_guard_warning(ctx, &warn_msg);
            (ctx.raw_len, "PostReadHook skipped (micro-read < skeleton)")
        }
    }
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
    let read_kind = classify_read(&params, file);
    state.telemetry.record_read_kind(read_kind);

    let entry = match state.registry.get_or_init(file).await {
        Ok(e) => {
            e.record_read(read_kind);
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

    let kind_tag = match read_kind {
        ReadKind::Macro => "MACRO",
        ReadKind::Micro => "MICRO",
    };

    let raw_len = params
        .raw_read_bytes
        .unwrap_or_else(|| std::fs::metadata(file).map(|m| m.len()).unwrap_or(0));
    let advisory_res = if let Some(ref ws) = entry {
        if let Some(ref base) = ws.base_index {
            let base_guard = base.read().await;
            let ov_guard = ws.index.read().await;
            execute_post_read_bundle_for_project(
                file,
                Some(&*base_guard),
                Some(&*ov_guard),
                Some(&ws.config),
            )
        } else {
            let guard = ws.index.read().await;
            execute_post_read_bundle_for_project(file, Some(&*guard), None, Some(&ws.config))
        }
    } else {
        let guard = state.legacy_index.read().await;
        execute_post_read_bundle(file, Some(&*guard))
    };

    match advisory_res {
        Ok(bundle) => {
            let elapsed_us = req_start.elapsed().as_micros();
            let range_str = format_range(params.start_line, params.end_line);

            let outcome = bundle.fit_to_budget(raw_len);
            let ctx = GuardContext {
                state,
                entry: entry.as_deref(),
                file,
                range_str: &range_str,
                raw_len,
            };
            let (delivered_len, action) = apply_guard_outcome(&ctx, &outcome);

            let pct = state.telemetry.record_reduction(raw_len, delivered_len);
            let log_msg = format!(
                "[{kind_tag}] {action}: '{}'{range_str} | raw: {} B | delivered: {} B | reduction: {:.1}% | latency: {:.1}ms",
                file.display(),
                raw_len,
                delivered_len,
                pct,
                elapsed_us as f64 / 1000.0,
            );
            state.log_manager.log_daemon(&log_msg);
            if let Some(ref ws) = entry {
                ws.logger.log(&log_msg);
            }
            DaemonResponse::AdvisoryContext {
                file: file.display().to_string(),
                advisory_block: outcome.markdown,
            }
        }
        Err(e) => {
            let err_msg = format!(
                "[{kind_tag}] PostReadHook failed for '{}': {}",
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
    }
}

async fn handle_distill_request(
    file: &Path,
    mode: cortx_core::model::DistillMode,
    format: cortx_core::output::OutputFormat,
    state: &ServerState,
) -> DaemonResponse {
    let req_start = std::time::Instant::now();
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
    let distill_res = if let Some(ref ws) = entry {
        if let Some(ref base) = ws.base_index {
            let base_guard = base.read().await;
            let ov_guard = ws.index.read().await;
            distill_target_for_project(
                file,
                mode,
                Some(&*base_guard),
                Some(&*ov_guard),
                Some(&ws.config),
            )
        } else {
            let guard = ws.index.read().await;
            distill_target_for_project(file, mode, Some(&*guard), None, Some(&ws.config))
        }
    } else {
        let guard = state.legacy_index.read().await;
        distill_target(file, mode, Some(&*guard))
    };

    match distill_res {
        Ok(bundle) => {
            let elapsed_us = req_start.elapsed().as_micros();
            let content = bundle.format_with(format).unwrap_or_default();
            let delivered_len = content.len() as u64;
            let pct = state.telemetry.record_reduction(raw_len, delivered_len);
            let log_msg = format!(
                "Distill served: '{}' (mode: {:?}) | raw: {} B | delivered: {} B | reduction: {:.1}% | latency: {:.1}ms",
                file.display(),
                mode,
                raw_len,
                delivered_len,
                pct,
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

    for entry in &entries {
        let (metrics, mem_mb) = {
            let guard = entry.index.read().await;
            (guard.metrics(), guard.estimate_memory_mb())
        };
        agg_files += metrics.distinct_files_count;
        agg_symbols += metrics.total_symbols_count;
        agg_call_edges += metrics.total_call_edges_count;
        agg_contracts += metrics.total_contracts_count;
        agg_routes += metrics.total_routes_count;

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
    }

    let daemon_mode = if state.options.workspace_root.as_os_str().is_empty() {
        "System-Wide Multi-Tenant".to_string()
    } else {
        "Isolated Workspace".to_string()
    };

    let (hooks, raw, delivered, saved, pct, est_tokens) = state.telemetry.snapshot();
    let supp_tokens = delivered / cortx_core::span::EST_BYTES_PER_TOKEN;
    DaemonStatusInfo {
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
        bytes_saved: saved,
        token_reduction_pct: pct,
        est_tokens_saved: est_tokens,
        macro_reads: state.telemetry.macro_reads.load(Ordering::Relaxed),
        micro_reads: state.telemetry.micro_reads.load(Ordering::Relaxed),
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
            } => {
                let params = PostReadParams {
                    file: &file,
                    start_line,
                    end_line,
                    raw_read_bytes,
                    read_kind,
                };
                let resp = handle_post_read_request(params, req_start, &state).await;
                write_message(&mut stream, &resp).await?;
            }
            DaemonRequest::Distill { file, mode, format } => {
                let resp = handle_distill_request(&file, mode, format, &state).await;
                write_message(&mut stream, &resp).await?;
            }
            DaemonRequest::Shutdown => {
                let (hooks, raw, delivered, saved, pct, est_tokens) = state.telemetry.snapshot();
                state.log_manager.log_daemon(&format!(
                    "Daemon stopping: hooks_served={}, raw_bytes={}, delivered_bytes={}, bytes_saved={} ({:.1}%), est_tokens_saved={}",
                    hooks, raw, delivered, saved, pct, est_tokens
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
    fn test_server_telemetry_reduction_and_bounds() {
        let telemetry = ServerTelemetry::default();
        let pct = telemetry.record_reduction(1000, 200);
        assert!((pct - 80.0).abs() < 0.001);

        // Edge case: delivered > raw reflects negative reduction and deducts from net savings
        let pct2 = telemetry.record_reduction(50, 100);
        assert!((pct2 - (-100.0)).abs() < 0.001);

        let (hooks, raw, delivered, saved, cumulative_pct, tokens) = telemetry.snapshot();
        assert_eq!(hooks, 2);
        assert_eq!(raw, 1050);
        assert_eq!(delivered, 300);
        assert_eq!(saved, 750);
        assert!((cumulative_pct - (750.0 / 1050.0 * 100.0)).abs() < 0.001);
        assert_eq!(tokens, 187);
    }

    #[tokio::test]
    async fn test_server_telemetry_concurrent_updates() {
        let telemetry = Arc::new(ServerTelemetry::default());
        let mut handles = Vec::new();

        for _ in 0..16 {
            let t = Arc::clone(&telemetry);
            handles.push(tokio::spawn(async move {
                for _ in 0..100 {
                    t.record_reduction(100, 20);
                }
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        let (hooks, raw, delivered, saved, pct, tokens) = telemetry.snapshot();
        assert_eq!(hooks, 1600);
        assert_eq!(raw, 160_000);
        assert_eq!(delivered, 32_000);
        assert_eq!(saved, 128_000);
        assert!((pct - 80.0).abs() < 0.001);
        assert_eq!(tokens, 32_000);
    }

    #[tokio::test]
    async fn test_server_telemetry_atomic_counters() {
        let telemetry = ServerTelemetry::default();
        assert_eq!(telemetry.submissions.load(Ordering::Relaxed), 0);
        assert_eq!(telemetry.callgraph_drops.load(Ordering::Relaxed), 0);
        assert_eq!(telemetry.ast_drops.load(Ordering::Relaxed), 0);
        assert_eq!(telemetry.macro_reads(), 0);
        assert_eq!(telemetry.micro_reads(), 0);

        telemetry.record_submission();
        telemetry.record_submission();
        telemetry.record_callgraph_drop();
        telemetry.record_ast_drop();
        telemetry.record_read_kind(ReadKind::Macro);
        telemetry.record_read_kind(ReadKind::Micro);
        telemetry.record_read_kind(ReadKind::Micro);

        assert_eq!(telemetry.submissions.load(Ordering::Relaxed), 2);
        assert_eq!(telemetry.callgraph_drops.load(Ordering::Relaxed), 1);
        assert_eq!(telemetry.ast_drops.load(Ordering::Relaxed), 1);
        assert_eq!(telemetry.macro_reads(), 1);
        assert_eq!(telemetry.micro_reads(), 2);
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
            file_content.push_str(&format!("fn f{}() {{}}\n", i));
        }
        std::fs::write(&src_file, &file_content).unwrap();

        // 1. Unbounded read -> Macro read
        let p_macro = PostReadParams {
            file: &src_file,
            start_line: None,
            end_line: None,
            raw_read_bytes: Some(file_content.len() as u64),
            read_kind: None,
        };
        let _ = handle_post_read_request(p_macro, std::time::Instant::now(), &state).await;
        assert_eq!(state.telemetry.macro_reads(), 1);
        assert_eq!(state.telemetry.micro_reads(), 0);

        // 2. Bounded read (lines 10..40, 31 lines <= 150) -> Micro read
        let p_micro = PostReadParams {
            file: &src_file,
            start_line: Some(10),
            end_line: Some(40),
            raw_read_bytes: Some(500),
            read_kind: None,
        };
        let _ = handle_post_read_request(p_micro, std::time::Instant::now(), &state).await;
        assert_eq!(state.telemetry.macro_reads(), 1);
        assert_eq!(state.telemetry.micro_reads(), 1);

        // 3. Explicit read_kind override
        let p_explicit = PostReadParams {
            file: &src_file,
            start_line: None,
            end_line: None,
            raw_read_bytes: Some(500),
            read_kind: Some(ReadKind::Micro),
        };
        let _ = handle_post_read_request(p_explicit, std::time::Instant::now(), &state).await;
        assert_eq!(state.telemetry.macro_reads(), 1);
        assert_eq!(state.telemetry.micro_reads(), 2);

        // Check DaemonStatusInfo
        let status = build_status_response(&state).await;
        assert_eq!(status.macro_reads, 1);
        assert_eq!(status.micro_reads, 2);
        assert!(status.supplemental_context_bytes > 0);
        assert_eq!(
            status.supplemental_context_tokens,
            status.supplemental_context_bytes / cortx_core::span::EST_BYTES_PER_TOKEN
        );
        assert_eq!(status.workspaces.len(), 1);
        assert_eq!(status.workspaces[0].macro_reads, 1);
        assert_eq!(status.workspaces[0].micro_reads, 2);
    }

    #[tokio::test]
    async fn test_apply_guard_outcome_stages() {
        let dir = tempfile::tempdir().unwrap();
        let log_dir = dir.path().join("logs");
        let opts = ServerOptions {
            socket_path: dir.path().join("test.sock"),
            workspace_root: dir.path().to_path_buf(),
            log_dir: Some(log_dir),
            ..Default::default()
        };
        let index = Arc::new(RwLock::new(SymbolIndex::new()));
        let token = CancellationToken::new();
        let state = init_server_state(opts, index, token, std::time::Instant::now())
            .await
            .expect("init server state");

        let bundle = cortx_distill::DistillBundle {
            target: "src/test.rs".to_string(),
            skeleton: "pub fn small() {}".to_string(),
            contracts: vec![],
            callers: vec![cortx_distill::CallEdge {
                caller: "caller_fn".into(),
                callee: "small".into(),
                file: "src/caller.rs".into(),
                call_line: 0,
            }],
            callees: vec![],
            token_reduction_pct: 50.0,
            cross_boundary: None,
            unread_outline: None,
            omitted_callers: 0,
            omitted_callees: 0,
        };

        let full_md = bundle.format_markdown();
        let pruned_md = bundle.without_call_graph().format_markdown();
        let full_len = full_md.len() as u64;
        let pruned_len = pruned_md.len() as u64;
        assert!(pruned_len < full_len);

        let test_file = Path::new("src/test.rs");

        // Stage 0: raw_len > full_len -> Deliver full bundle
        let raw0 = full_len + 50;
        let outcome0 = bundle.fit_to_budget(raw0);
        let ctx0 = GuardContext {
            state: &state,
            entry: None,
            file: test_file,
            range_str: "",
            raw_len: raw0,
        };
        let (out_len, action) = apply_guard_outcome(&ctx0, &outcome0);
        assert_eq!(action, "PostReadHook served (full)");
        assert_eq!(out_len, full_len);
        assert_eq!(outcome0.markdown, full_md);
        assert_eq!(state.telemetry.submissions.load(Ordering::Relaxed), 1);
        assert_eq!(state.telemetry.callgraph_drops.load(Ordering::Relaxed), 0);
        assert_eq!(state.telemetry.ast_drops.load(Ordering::Relaxed), 0);

        // Stage 1: pruned_len < raw_len <= full_len -> Deliver pruned skeleton
        let target_raw = pruned_len + 1;
        let outcome1 = bundle.fit_to_budget(target_raw);
        let ctx1 = GuardContext {
            state: &state,
            entry: None,
            file: test_file,
            range_str: "",
            raw_len: target_raw,
        };
        let (out_len, action) = apply_guard_outcome(&ctx1, &outcome1);
        assert_eq!(action, "PostReadHook served (pruned call graph)");
        assert_eq!(out_len, pruned_len);
        assert_eq!(outcome1.markdown, pruned_md);
        assert_eq!(state.telemetry.submissions.load(Ordering::Relaxed), 2);
        assert_eq!(state.telemetry.callgraph_drops.load(Ordering::Relaxed), 1);
        assert_eq!(state.telemetry.ast_drops.load(Ordering::Relaxed), 0);

        // Stage 2: raw_len <= pruned_len on bounded micro-read -> Total drop (empty string)
        let raw2 = pruned_len - 1;
        let outcome2 = bundle.fit_to_budget(raw2);
        let ctx2 = GuardContext {
            state: &state,
            entry: None,
            file: test_file,
            range_str: " (lines 1..5)",
            raw_len: raw2,
        };
        let (out_len, action) = apply_guard_outcome(&ctx2, &outcome2);
        assert_eq!(action, "PostReadHook skipped (micro-read < skeleton)");
        assert_eq!(out_len, raw2);
        assert!(outcome2.markdown.is_empty());
        assert_eq!(state.telemetry.submissions.load(Ordering::Relaxed), 2);
        assert_eq!(state.telemetry.callgraph_drops.load(Ordering::Relaxed), 1);
        assert_eq!(state.telemetry.ast_drops.load(Ordering::Relaxed), 1);

        // Explicit 0-byte budget -> Total drop
        let outcome_zero = bundle.fit_to_budget(0);
        let ctx_zero = GuardContext {
            state: &state,
            entry: None,
            file: test_file,
            range_str: "",
            raw_len: 0,
        };
        let (out_len, action) = apply_guard_outcome(&ctx_zero, &outcome_zero);
        assert_eq!(action, "PostReadHook skipped (micro-read < skeleton)");
        assert_eq!(out_len, 0);
        assert!(outcome_zero.markdown.is_empty());
        assert_eq!(state.telemetry.ast_drops.load(Ordering::Relaxed), 2);
    }
}
