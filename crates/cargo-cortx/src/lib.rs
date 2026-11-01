//! # cargo-cortx CLI router
//!
//! Main CLI interface and cargo subcommand router for AST context distillation.

#![forbid(unsafe_code)]

use clap::builder::styling::{AnsiColor, Effects, Styles};
use clap::{Args, CommandFactory, Parser, Subcommand};
use cortx_core::config::CortxConfig;
use cortx_core::diagnostics::{error, info_banner, success};
use cortx_core::model::DistillMode;
use cortx_core::output::OutputFormat;
use cortx_core::span::{
    AgentVendor, ReadBudgetEnv, ReadCoverage, ReadKind, ReadSpan, detect_budget_env,
    parse_hook_stdin, resolve_read_budget,
};
use cortx_core::workspace::{
    resolve_active_socket, resolve_connected_socket, resolve_global_socket, resolve_workspace_root,
};
use cortx_daemon::{
    DaemonRequest, DaemonResponse, ServerOptions, run_server_with_options, send_daemon_request,
};
use cortx_distill::{DistillQuery, OverlayView, SymbolIndex, distill_with_query};

mod status_fmt;
pub(crate) use status_fmt::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

fn clap_styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::Green.on_default() | Effects::BOLD)
        .usage(AnsiColor::Green.on_default() | Effects::BOLD)
        .literal(AnsiColor::Cyan.on_default() | Effects::BOLD)
        .placeholder(AnsiColor::Cyan.on_default())
}

/// AI Ground-Truth & Context Distiller Engine
#[derive(Parser, Debug)]
#[command(
    name = "cargo-cortx",
    bin_name = "cargo-cortx",
    version = env!("CORTX_VERSION_STRING"),
    about = "Sub-millisecond AST context distiller daemon for AI coding agents",
    long_about = r#"Sub-millisecond AST context distiller daemon for AI coding agents

Transforms AI coding agents into reliable closed-loop engineers across large-scale
Rust, TypeScript, and Go codebases (>1.2M LOC) through compiler-aware context
distillation and non-destructive advisory context injection.

CORE SUBCOMMANDS & CAPABILITIES:
  distill        Context Distiller Engine
                 Sub-millisecond AST skeleton stripping, caller/callee graphs,
                 implicit interface satisfaction, and cross-boundary correlation.
  daemon         Agent Sidecar & Hook Daemon
                 Persistent background symbol indexing, operational probing (--ping,
                 --status, --stop), and non-destructive advisory post-read hooks.

QUICK START EXAMPLES:
  # Extract stripped AST skeleton for prompt injection
  cargo cortx distill src/engine.rs

  # Correlate backend route/model changes with frontend TypeScript fetch calls
  cargo cortx distill src/routes/users.rs --cross-boundary

  # Probe daemon liveness and round-trip latency
  cargo cortx daemon --ping

  # Query running daemon health, symbol count, and watcher metrics
  cargo cortx daemon --status

  # Gracefully stop the background daemon
  cargo cortx daemon --stop

  # Execute non-destructive advisory post-read hook
  cargo cortx daemon --hook-post-read --file src/engine.rs"#,
    styles = clap_styles()
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Verbosity level (-v, -vv)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Output JSON formatted logs
    #[arg(long, global = true)]
    pub json: bool,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Context Distiller Engine (sub-millisecond AST & semantic skeletons)
    Distill(DistillArgs),

    /// Agent Sidecar & Hook Daemon (non-destructive advisory tool hook)
    Daemon(DaemonArgs),
}

#[derive(Args, Debug, Clone)]
pub struct DistillArgs {
    /// Target file or symbol path to distill
    pub target: PathBuf,

    /// Distillation extraction mode
    #[arg(
        short,
        long,
        default_value = "skeleton",
        help_heading = "Distillation Options"
    )]
    pub mode: DistillMode,

    /// Output format
    #[arg(
        short,
        long,
        default_value = "markdown",
        help_heading = "Distillation Options"
    )]
    pub format: OutputFormat,

    /// Call hierarchy search depth
    #[arg(
        short,
        long,
        default_value_t = 2,
        help_heading = "Distillation Options"
    )]
    pub depth: usize,

    /// Query cross-boundary impact between Rust/Go backend and TypeScript frontend
    #[arg(long, help_heading = "Cross-Boundary Analysis")]
    pub cross_boundary: bool,

    /// 1-indexed inclusive start line
    #[arg(long, help_heading = "Distillation Options")]
    pub start_line: Option<usize>,

    /// 1-indexed inclusive end line
    #[arg(long, help_heading = "Distillation Options")]
    pub end_line: Option<usize>,

    /// Agent vendor for default read window bounds (claude, agy, opencode, generic)
    #[arg(long, value_enum, help_heading = "Distillation Options")]
    pub vendor: Option<AgentVendor>,
}

#[derive(Args, Debug, Default, Clone)]
pub struct DaemonArgs {
    /// Start daemon IPC server in foreground
    #[arg(long, help_heading = "Daemon Lifecycle")]
    pub start: bool,

    /// Explicit path to daemon configuration file (Docker, K8s ConfigMap, or custom path).
    /// If omitted, configuration is loaded exclusively from the system path.
    #[arg(short = 'c', long, help_heading = "Configuration")]
    pub config: Option<PathBuf>,

    /// Run as legacy isolated per-workspace daemon instead of system-wide daemon
    #[arg(long, help_heading = "Daemon Lifecycle")]
    pub isolated: bool,

    /// Probe running daemon liveness and report round-trip latency
    #[arg(long, help_heading = "Daemon Lifecycle")]
    pub ping: bool,

    /// Query running daemon status, index metrics, and cache statistics
    #[arg(long, help_heading = "Daemon Lifecycle")]
    pub status: bool,

    /// Gracefully stop the running background daemon
    #[arg(long, help_heading = "Daemon Lifecycle")]
    pub stop: bool,

    /// Execute advisory post-read tool hook
    #[arg(long, help_heading = "Advisory Tool Hooks")]
    pub hook_post_read: bool,

    /// Target file for hook execution
    #[arg(long, help_heading = "Advisory Tool Hooks")]
    pub file: Option<PathBuf>,

    /// 1-indexed inclusive start line for post-read hook (AGY StartLine)
    #[arg(long, help_heading = "Advisory Tool Hooks")]
    pub start_line: Option<usize>,

    /// 1-indexed inclusive end line for post-read hook (AGY EndLine)
    #[arg(long, help_heading = "Advisory Tool Hooks")]
    pub end_line: Option<usize>,

    /// 1-indexed starting line offset for post-read hook (Claude offset)
    #[arg(long, help_heading = "Advisory Tool Hooks")]
    pub offset: Option<usize>,

    /// Maximum number of lines read for post-read hook (Claude limit)
    #[arg(long, help_heading = "Advisory Tool Hooks")]
    pub limit: Option<usize>,

    /// Agent vendor for default read window bounds (claude, agy, generic)
    #[arg(long, value_enum, help_heading = "Advisory Tool Hooks")]
    pub vendor: Option<AgentVendor>,

    /// Conversation or session identifier for post-read hook deduplication
    #[arg(long, help_heading = "Advisory Tool Hooks")]
    pub session_id: Option<String>,

    /// Socket path override
    #[arg(long, help_heading = "Configuration")]
    pub socket: Option<String>,
}

/// Dispatches CLI invocation to corresponding subcommands.
pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().collect();
    // Support `cargo cortx ...` where cargo passes subcommand as second argument
    if args.len() > 1 && args[1] == "cortx" {
        args.remove(1);
    }

    let cli = Cli::parse_from(args);
    let _ = cortx_core::init_logging(cli.verbose, cli.json);

    // If running daemon subcommand, isolate config loading strictly to custom path or system path!
    if let Some(Commands::Daemon(ref daemon_args)) = cli.command {
        let (daemon_cfg, qualified_path) =
            CortxConfig::load_daemon_config(daemon_args.config.as_deref())?;
        let is_lifecycle_start = !daemon_args.ping
            && !daemon_args.status
            && !daemon_args.stop
            && !daemon_args.hook_post_read;
        if is_lifecycle_start {
            if let Some(ref path) = qualified_path {
                info_banner(
                    "Config",
                    &format!("Daemon configuration loaded from '{}'", path.display()),
                );
                tracing::info!(
                    path = %path.display(),
                    projects = daemon_cfg.projects.len(),
                    "Daemon configuration loaded"
                );
            } else {
                info_banner(
                    "Config",
                    "Operating with default built-in daemon configuration (no system config found)",
                );
                tracing::info!("Operating with default built-in daemon configuration");
            }
        }
        return handle_daemon(daemon_args.clone(), &daemon_cfg, qualified_path, cli.json).await;
    }

    // For all other commands, load system configuration or fall back to defaults
    let (cfg, provenance) = CortxConfig::load_or_default(None)?;
    provenance.log_details();
    if let Some(p) = provenance.primary_path() {
        if cli.verbose > 0 && !cli.json {
            info_banner(
                "Config",
                &format!("Loaded system configuration from '{}'", p.display()),
            );
        }
    }

    match cli.command {
        Some(Commands::Distill(args)) => handle_distill(args, &cfg).await?,
        Some(Commands::Daemon(_)) => unreachable!(),
        None => {
            Cli::command().print_help()?;
            println!();
        }
    }

    Ok(())
}

async fn handle_distill(
    args: DistillArgs,
    cfg: &CortxConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let raw_parent = args
        .target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = std::fs::canonicalize(raw_parent).unwrap_or_else(|_| raw_parent.to_path_buf());
    let socket_override = cfg.daemon.socket_path.as_deref();
    let connected_socket = resolve_connected_socket(socket_override, &parent);

    if let Some(ref hook_socket) = connected_socket {
        if let Ok(resp) = send_daemon_request(
            hook_socket,
            DaemonRequest::Distill {
                file: args.target.clone(),
                mode: args.mode,
                format: args.format,
                start_line: args.start_line,
                end_line: args.end_line,
                vendor: args.vendor,
                cross_boundary: args.cross_boundary,
            },
        )
        .await
        {
            match resp {
                DaemonResponse::DistillResult { content } => {
                    println!("{}", content);
                    return Ok(());
                }
                DaemonResponse::Error { message } => {
                    tracing::warn!("Daemon distill returned error: {}", message);
                }
                _ => {}
            }
        }
    }

    let ws_root = resolve_workspace_root(&parent);
    let mut proj = cfg
        .find_project_for_path(&args.target)
        .or_else(|| cfg.find_project_for_path(&ws_root))
        .map(|(_, p)| p.clone())
        .unwrap_or_else(|| cortx_core::ProjectConfig::from_cortx_config(ws_root.clone(), cfg));
    if args.depth != 2 {
        proj.distill.max_call_depth = args.depth;
    }
    let mut index = SymbolIndex::new();
    let _ = index.index_directory_with_project(&parent, &proj);
    if index.symbols_map.is_empty() && parent != ws_root {
        let _ = index.index_directory_with_project(&ws_root, &proj);
    }
    let span = if args.start_line.is_some() || args.end_line.is_some() || args.vendor.is_some() {
        Some(ReadSpan {
            start_line: args.start_line,
            end_line: args.end_line,
            vendor: args.vendor,
            ..Default::default()
        })
    } else {
        None
    };
    let view = OverlayView {
        base: Some(&index),
        overlay: None,
        shadowed_files: None,
    };
    let query = DistillQuery {
        file_path: &args.target,
        mode: args.mode,
        coverage: ReadCoverage::PartialMacro,
        span,
        preloaded_source: None,
        cross_boundary: args.cross_boundary,
        project_cfg: Some(&proj),
    };
    let bundle = distill_with_query(&query, view)?;
    let output = bundle.format_with(args.format)?;
    println!("{}", output);
    Ok(())
}

async fn handle_daemon(
    args: DaemonArgs,
    cfg: &CortxConfig,
    config_path: Option<PathBuf>,
    json_output: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let current_dir = std::env::current_dir()?;
    let socket_override = args.socket.as_deref().or(cfg.daemon.socket_path.as_deref());

    if args.ping {
        let socket_path = if args.isolated {
            let root = resolve_workspace_root(&current_dir);
            resolve_active_socket(socket_override, &root)
        } else {
            resolve_connected_socket(socket_override, &current_dir).unwrap_or_else(|| {
                socket_override
                    .map(PathBuf::from)
                    .unwrap_or_else(resolve_global_socket)
            })
        };
        return handle_daemon_ping(&socket_path).await;
    }

    if args.status {
        let socket_path = if args.isolated {
            let root = resolve_workspace_root(&current_dir);
            resolve_active_socket(socket_override, &root)
        } else {
            resolve_connected_socket(socket_override, &current_dir).unwrap_or_else(|| {
                socket_override
                    .map(PathBuf::from)
                    .unwrap_or_else(resolve_global_socket)
            })
        };
        return handle_daemon_status(&socket_path, json_output).await;
    }

    if args.stop {
        let socket_path = if args.isolated {
            let root = resolve_workspace_root(&current_dir);
            resolve_active_socket(socket_override, &root)
        } else {
            resolve_connected_socket(socket_override, &current_dir).unwrap_or_else(|| {
                socket_override
                    .map(PathBuf::from)
                    .unwrap_or_else(resolve_global_socket)
            })
        };
        return handle_daemon_stop(&socket_path).await;
    }

    if args.hook_post_read {
        return handle_daemon_hook_post_read(&args, socket_override, &current_dir).await;
    }

    let is_isolated = args.isolated || !cfg.daemon.system_daemon;
    let (socket_path, root) = if is_isolated {
        let root = resolve_workspace_root(&current_dir);
        let socket_path = resolve_active_socket(socket_override, &root);
        (socket_path, Some(root))
    } else {
        let socket_path = socket_override
            .map(PathBuf::from)
            .unwrap_or_else(resolve_global_socket);
        // System daemon: do not bind or pre-index CWD
        (socket_path, None)
    };

    handle_daemon_start(&socket_path, root.as_deref(), cfg, config_path).await
}

async fn handle_daemon_ping(socket_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let start = std::time::Instant::now();
    match send_daemon_request(socket_path, DaemonRequest::Ping).await {
        Ok(DaemonResponse::Pong) => {
            let latency_ms = start.elapsed().as_secs_f64() * 1000.0;
            println!(
                "[Daemon] Daemon is responsive on '{}' (round-trip: {:.2}ms)",
                socket_path.display(),
                latency_ms
            );
            Ok(())
        }
        Ok(resp) => {
            error(
                "Daemon",
                &format!("Unexpected response to ping: {:?}", resp),
            );
            std::process::exit(1);
        }
        Err(e) => {
            error(
                "Daemon",
                &format!(
                    "Failed connecting to daemon on '{}': {}",
                    socket_path.display(),
                    e
                ),
            );
            std::process::exit(1);
        }
    }
}

async fn handle_daemon_status(
    socket_path: &Path,
    json_output: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    match send_daemon_request(socket_path, DaemonRequest::Status).await {
        Ok(DaemonResponse::Status(status)) => {
            if status.protocol_version != cortx_daemon::PROTOCOL_VERSION {
                eprintln!(
                    "[Daemon] Warning: daemon on '{}' is running protocol version {} (CLI expects protocol version {}). Restart daemon with 'cargo cortx daemon --stop'.",
                    socket_path.display(),
                    status.protocol_version,
                    cortx_daemon::PROTOCOL_VERSION
                );
            }
            if json_output {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                print_daemon_status_table(&status);
            }
            Ok(())
        }
        Ok(resp) => {
            error(
                "Daemon",
                &format!("Unexpected response to status: {:?}", resp),
            );
            std::process::exit(1);
        }
        Err(e) => {
            error(
                "Daemon",
                &format!(
                    "Failed connecting to daemon on '{}': {}",
                    socket_path.display(),
                    e
                ),
            );
            std::process::exit(1);
        }
    }
}

async fn handle_daemon_stop(socket_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    match send_daemon_request(socket_path, DaemonRequest::Shutdown).await {
        Ok(DaemonResponse::ShutdownAck) => {
            let start = std::time::Instant::now();
            while socket_path.exists() && start.elapsed() < std::time::Duration::from_millis(500) {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            println!(
                "[Daemon] Daemon on '{}' successfully terminated.",
                socket_path.display()
            );
            Ok(())
        }
        Ok(resp) => {
            error(
                "Daemon",
                &format!("Unexpected response to shutdown: {:?}", resp),
            );
            std::process::exit(1);
        }
        Err(e) => {
            error(
                "Daemon",
                &format!(
                    "Failed connecting to daemon on '{}': {}",
                    socket_path.display(),
                    e
                ),
            );
            std::process::exit(1);
        }
    }
}

/// Resolves the target file path and merged [`ReadSpan`] from optional `stdin` JSON and CLI flags.
#[must_use]
pub fn resolve_hook_input(
    args: &DaemonArgs,
    stdin_raw: Option<&str>,
) -> Option<(PathBuf, ReadSpan)> {
    let parsed = stdin_raw.and_then(parse_hook_stdin);
    let cli_span = ReadSpan {
        start_line: args.start_line,
        end_line: args.end_line,
        offset: args.offset,
        limit: args.limit,
        byte_offset: None,
        allow_large: None,
        vendor: args.vendor,
    };
    let base_span = parsed.as_ref().map(|p| p.span.clone()).unwrap_or_default();
    let merged_span = base_span.with_overrides(&cli_span);
    let file = args
        .file
        .clone()
        .or_else(|| parsed.and_then(|p| p.file_path))?;
    Some((file, merged_span))
}

/// Computes effective `(start_line, end_line, raw_read_bytes)` for a post-read hook invocation.
#[must_use]
pub fn compute_hook_read_metrics(
    file: &Path,
    span: &ReadSpan,
    env: &ReadBudgetEnv,
) -> (Option<usize>, Option<usize>, Option<u64>) {
    let Ok(content) = std::fs::read_to_string(file) else {
        return (span.start_line.or(span.offset), span.end_line, None);
    };
    let total_lines = content.split_inclusive('\n').count();
    let effective_vendor = span.vendor.or(env.env_vendor);
    let effective_span = if span.vendor.is_none() && effective_vendor.is_some() {
        let mut s = span.clone();
        s.vendor = effective_vendor;
        s
    } else {
        span.clone()
    };
    let (start, end) = effective_span.effective_range(total_lines);
    let raw_bytes = resolve_read_budget(span, &content, env);
    (
        (start > 0).then_some(start),
        (end > 0).then_some(end),
        Some(raw_bytes),
    )
}

fn read_json_from_reader<R: std::io::Read>(reader: R) -> Option<String> {
    use std::io::Read;
    let mut limited = reader.take(256 * 1024);
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match limited.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if serde_json::from_slice::<serde_json::Value>(&buf).is_ok() {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    String::from_utf8(buf).ok().filter(|s| !s.trim().is_empty())
}

async fn read_piped_stdin(has_cli_file: bool) -> Option<String> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        return None;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    // Detached OS thread so an idle inherited pipe never blocks Tokio runtime shutdown.
    std::thread::spawn(move || {
        let _ = tx.send(read_json_from_reader(std::io::stdin().lock()));
    });
    let timeout_ms = if has_cli_file { 10 } else { 50 };
    tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx)
        .await
        .ok()
        .and_then(Result::ok)
        .flatten()
}

async fn handle_daemon_hook_post_read(
    args: &DaemonArgs,
    socket_override: Option<&str>,
    current_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let stdin_payload = read_piped_stdin(args.file.is_some()).await;
    let parsed_stdin = stdin_payload.as_deref().and_then(parse_hook_stdin);
    if let Some(ref p) = parsed_stdin {
        if p.tool_error {
            return Ok(());
        }
    }
    let Some((file, span)) = resolve_hook_input(args, stdin_payload.as_deref()) else {
        error("Daemon", "--file is required when running post-read hook");
        return Ok(());
    };

    let budget_env = detect_budget_env(|k| std::env::var(k).ok());
    // Perform at most 1 client-side file read on macro reads: compute metrics once
    let preloaded_source = std::fs::read_to_string(&file).ok();
    let (total_lines, raw_read_bytes, start_line, end_line, effective_vendor, coverage) =
        if let Some(ref content) = preloaded_source {
            let tot_lines = content.split_inclusive('\n').count();
            let eff_vendor = span.vendor.or(budget_env.env_vendor);
            let mut eff_span = span.clone();
            if span.vendor.is_none() && eff_vendor.is_some() {
                eff_span.vendor = eff_vendor;
            }
            let (start, end) = eff_span.effective_range(tot_lines);
            let raw_bytes = resolve_read_budget(&span, content, &budget_env);
            let cov = cortx_core::classify_read_coverage(
                &span,
                Some(tot_lines),
                Some(raw_bytes),
                &budget_env,
            );
            (
                tot_lines,
                Some(raw_bytes),
                (start > 0).then_some(start),
                (end > 0).then_some(end),
                eff_vendor,
                cov,
            )
        } else {
            let (st, en, raw) = compute_hook_read_metrics(&file, &span, &budget_env);
            let cov = cortx_core::classify_read_coverage(&span, None, raw, &budget_env);
            (0, raw, st, en, span.vendor.or(budget_env.env_vendor), cov)
        };

    let is_full_read = coverage == ReadCoverage::FullFile;
    let read_kind = span.classify(total_lines);
    let session_id = args
        .session_id
        .clone()
        .or_else(|| parsed_stdin.as_ref().and_then(|p| p.session_id.clone()))
        .or(budget_env.env_session_id.clone());

    let target_dir = if let Some(parent) = file.parent() {
        if parent.as_os_str().is_empty() {
            current_dir
        } else {
            parent
        }
    } else {
        current_dir
    };
    let connected_socket = resolve_connected_socket(socket_override, target_dir);

    let mut advisory: Option<String> = None;
    let mut provenance: Option<String> = None;
    let mut fallback_reason: Option<&'static str> = None;
    let start_time = std::time::Instant::now();

    if let Some(ref hook_socket) = connected_socket {
        match send_daemon_request(
            hook_socket,
            DaemonRequest::PostReadHook {
                file: file.clone(),
                start_line,
                end_line,
                raw_read_bytes,
                read_kind: Some(read_kind),
                total_lines: Some(total_lines),
                vendor: effective_vendor,
                is_full_read: Some(is_full_read),
                session_id: session_id.clone(),
            },
        )
        .await
        {
            Ok(resp) => {
                let latency_ms = start_time.elapsed().as_secs_f64() * 1000.0;
                match resp {
                    DaemonResponse::AdvisoryContext {
                        advisory_block,
                        protocol_version,
                        ..
                    } => {
                        if protocol_version == cortx_daemon::PROTOCOL_VERSION {
                            advisory = Some(advisory_block);
                            provenance = Some(format!(
                                "<!-- cortx-context-provenance: daemon (socket: {}, latency: {:.1}ms) -->",
                                hook_socket.display(),
                                latency_ms
                            ));
                        } else {
                            eprintln!(
                                "[Daemon] Warning: daemon on '{}' is running protocol version {} (CLI expects protocol version {}); falling back to in-process distillation",
                                hook_socket.display(),
                                protocol_version,
                                cortx_daemon::PROTOCOL_VERSION
                            );
                            fallback_reason = Some("protocol mismatch");
                        }
                    }
                    DaemonResponse::Error { message } => {
                        tracing::warn!("Daemon hook returned error: {}", message);
                    }
                    _ => {}
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Failed communicating with daemon on '{}': {}",
                    hook_socket.display(),
                    e
                );
            }
        }
    }

    let (output, prov) = match advisory {
        Some(adv) => (adv, provenance.unwrap_or_default()),
        None => {
            let reason = fallback_reason.unwrap_or("daemon unreachable");
            let fallback_prov = format!(
                "<!-- cortx-context-provenance: in-process-fallback ({}) -->",
                reason
            );
            let params = FallbackParams {
                file: &file,
                start_line,
                end_line,
                read_kind,
                preloaded_source: preloaded_source.as_deref(),
                coverage,
                vendor: effective_vendor,
                session_id: session_id.as_deref(),
            };
            let adv = execute_in_process_fallback(params)?;
            (adv, fallback_prov)
        }
    };

    if !output.is_empty() {
        if !prov.is_empty() {
            println!("{}", prov);
        }
        println!("{}", output);
    }
    Ok(())
}

struct FallbackParams<'a> {
    file: &'a Path,
    start_line: Option<usize>,
    end_line: Option<usize>,
    read_kind: ReadKind,
    preloaded_source: Option<&'a str>,
    coverage: ReadCoverage,
    vendor: Option<AgentVendor>,
    session_id: Option<&'a str>,
}

fn execute_in_process_fallback(
    params: FallbackParams<'_>,
) -> Result<String, Box<dyn std::error::Error>> {
    let kind_tag = match params.read_kind {
        ReadKind::Macro => "MACRO",
        ReadKind::Micro => "MICRO",
    };
    tracing::warn!(
        "[{kind_tag}] Daemon offline or unreachable, falling back to in-process distillation"
    );
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

    let owned_source;
    let source = match params.preloaded_source {
        Some(s) => s,
        None => {
            owned_source = std::fs::read_to_string(params.file).unwrap_or_default();
            &owned_source
        }
    };

    let mut index = SymbolIndex::new();
    let _ = index.index_any_source_str(&params.file.to_string_lossy(), source);

    let query = DistillQuery {
        file_path: params.file,
        mode: DistillMode::Bundle,
        coverage: params.coverage,
        span,
        preloaded_source: Some(source),
        cross_boundary: true,
        project_cfg: None,
    };
    let view = OverlayView {
        base: Some(&index),
        overlay: None,
        shadowed_files: None,
    };
    let bundle = cortx_daemon::execute_post_read_query(&query, view)?;
    let packed = bundle.pack_for_hook(params.coverage);
    let range_str = format_range(params.start_line, params.end_line);
    let vendor = params.vendor.unwrap_or(AgentVendor::Generic);
    let session_attr = match params.session_id {
        Some(sid) => format!("session: '{}' (vendor: {})", sid, vendor),
        None => format!("session: none (vendor: {})", vendor),
    };
    tracing::info!(
        "[{kind_tag}] PostReadHook served fallback advisory context ({} B) for '{}'{} | {}",
        packed.markdown.len(),
        params.file.display(),
        range_str,
        session_attr
    );
    Ok(packed.markdown)
}

async fn handle_daemon_start(
    socket_path: &Path,
    root: Option<&Path>,
    cfg: &CortxConfig,
    config_path: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (initial_index, workspace_root) = if let Some(r) = root {
        info_banner(
            "Daemon",
            &format!("Starting isolated daemon on '{}'...", socket_path.display()),
        );
        let index_start = std::time::Instant::now();
        info_banner(
            "Index",
            &format!("Starting initial indexing for '{}'...", r.display()),
        );
        let mut idx = SymbolIndex::new();
        let proj = cfg
            .find_project_for_path(r)
            .map(|(_, p)| p.clone())
            .unwrap_or_else(|| cortx_core::ProjectConfig::from_cortx_config(r.to_path_buf(), cfg));
        let indexed = idx.index_directory_with_project(r, &proj).unwrap_or(0);
        let elapsed = index_start.elapsed();
        let metrics = idx.metrics();
        let throughput = if elapsed.as_secs_f64() > 0.0 {
            (indexed as f64) / elapsed.as_secs_f64()
        } else {
            0.0
        };
        info_banner(
            "Index",
            &format!(
                "Initial indexing complete: {} files ({} symbols, {} call edges, {} contracts, {} routes) in {:.2?} ({:.0} files/sec)",
                indexed,
                metrics.total_symbols_count,
                metrics.total_call_edges_count,
                metrics.total_contracts_count,
                metrics.total_routes_count,
                elapsed,
                throughput
            ),
        );
        (idx, r.to_path_buf())
    } else {
        info_banner(
            "Daemon",
            &format!(
                "Starting system-wide daemon on '{}' (multi-tenant lazy indexing, {} declared projects)...",
                socket_path.display(),
                cfg.projects.len()
            ),
        );
        (SymbolIndex::new(), PathBuf::new())
    };

    let index = Arc::new(RwLock::new(initial_index));
    let token = CancellationToken::new();
    let watcher_active = cfg.daemon.enable_file_watcher;

    let tok_ctrl_c = token.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        info_banner(
            "Daemon",
            "Shutdown signal received, shutting down gracefully...",
        );
        tok_ctrl_c.cancel();
    });

    let opts = ServerOptions {
        socket_path: socket_path.to_path_buf(),
        workspace_root,
        watcher_active,
        log_dir: cfg.daemon.log_dir.as_ref().map(PathBuf::from),
        idle_ttl_secs: cfg.daemon.idle_ttl_secs,
        daemon_config: cfg.clone(),
        config_path,
    };
    run_server_with_options(opts, index, token).await?;
    success("Daemon", "Daemon exited cleanly.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_hook_input_merges_stdin_and_cli() {
        let stdin_json = r#"{
            "tool_name": "Read",
            "tool_input": {
                "file_path": "/workspace/src/lib.rs",
                "offset": 100,
                "limit": 30
            }
        }"#;
        let args = DaemonArgs {
            hook_post_read: true,
            limit: Some(50),
            ..Default::default()
        };
        let (file, span) =
            resolve_hook_input(&args, Some(stdin_json)).expect("should resolve hook input");
        assert_eq!(file, PathBuf::from("/workspace/src/lib.rs"));
        assert_eq!(span.vendor, Some(AgentVendor::Claude));
        assert_eq!(span.offset, Some(100));
        assert_eq!(span.limit, Some(50));
    }

    #[test]
    fn test_daemon_cli_parses_hook_span_flags() {
        let cli = Cli::try_parse_from([
            "cargo-cortx",
            "daemon",
            "--hook-post-read",
            "--file",
            "src/lib.rs",
            "--start-line",
            "10",
            "--end-line",
            "40",
            "--vendor",
            "agy",
        ])
        .expect("should parse CLI flags");

        let Some(Commands::Daemon(d)) = cli.command else {
            panic!("expected Daemon command");
        };
        assert!(d.hook_post_read);
        assert_eq!(d.start_line, Some(10));
        assert_eq!(d.end_line, Some(40));
        assert_eq!(d.vendor, Some(AgentVendor::Agy));

        let cli_cfg = Cli::try_parse_from(["cargo-cortx", "daemon", "-c", "/etc/cortx/cortx.toml"])
            .expect("should parse -c config flag");
        let Some(Commands::Daemon(d2)) = cli_cfg.command else {
            panic!("expected Daemon command");
        };
        assert_eq!(d2.config, Some(PathBuf::from("/etc/cortx/cortx.toml")));
    }

    #[test]
    fn test_read_json_from_reader_stops_without_eof() {
        struct NonClosingPipe {
            delivered: bool,
        }
        impl std::io::Read for NonClosingPipe {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                assert!(
                    !self.delivered,
                    "reader should stop once JSON is complete without waiting for EOF"
                );
                self.delivered = true;
                let payload = br#"{"tool_name":"Read","tool_input":{"file_path":"src/lib.rs"}}"#;
                buf[..payload.len()].copy_from_slice(payload);
                Ok(payload.len())
            }
        }

        let out = read_json_from_reader(NonClosingPipe { delivered: false })
            .expect("should return JSON payload without waiting for EOF");
        assert!(out.contains("src/lib.rs"));
    }

    #[test]
    fn test_compute_hook_read_metrics_bare_span_returns_file_bytes() {
        let tmp = tempfile::NamedTempFile::new().expect("create temp file");
        std::fs::write(tmp.path(), "line 1\nline 2\nline 3\n").expect("write file");
        let (start, end, raw_bytes) =
            compute_hook_read_metrics(tmp.path(), &ReadSpan::default(), &ReadBudgetEnv::default());
        assert_eq!(start, Some(1));
        assert_eq!(end, Some(3));
        assert_eq!(raw_bytes, Some(21));
    }

    #[test]
    fn test_execute_in_process_fallback_runs_cleanly() {
        let tmp = tempfile::Builder::new()
            .suffix(".rs")
            .tempfile()
            .expect("create temp file");
        let content = r#"pub fn head() {
    println!("1");
}

pub fn mid() {
    println!("2");
}

pub fn tail() {
    println!("3");
}
"#;
        std::fs::write(tmp.path(), content).expect("write file");
        let params = FallbackParams {
            file: tmp.path(),
            start_line: Some(5),
            end_line: Some(8),
            read_kind: ReadKind::Macro,
            preloaded_source: Some(content),
            coverage: ReadCoverage::PartialMacro,
            vendor: None,
            session_id: None,
        };
        let result = execute_in_process_fallback(params);
        assert!(result.is_ok());
        let markdown = result.unwrap();
        assert!(markdown.contains("head"));
        assert!(markdown.contains("tail"));
    }

    #[test]
    fn test_distill_cli_parses_span_and_cross_boundary_flags() {
        let cli = Cli::try_parse_from([
            "cargo-cortx",
            "distill",
            "src/lib.rs",
            "--start-line",
            "15",
            "--end-line",
            "50",
            "--vendor",
            "claude",
            "--cross-boundary",
        ])
        .expect("should parse CLI flags");

        let Some(Commands::Distill(d)) = cli.command else {
            panic!("expected Distill command");
        };
        assert_eq!(d.target, PathBuf::from("src/lib.rs"));
        assert_eq!(d.start_line, Some(15));
        assert_eq!(d.end_line, Some(50));
        assert_eq!(d.vendor, Some(AgentVendor::Claude));
        assert!(d.cross_boundary);
    }

    #[test]
    fn test_micro_read_bounded_span_detection() {
        let span = ReadSpan {
            start_line: Some(10),
            end_line: Some(50),
            ..Default::default()
        };
        assert_eq!(span.effective_line_count(None), Some(41));
        assert!(span.effective_line_count(None).unwrap() <= cortx_core::MICRO_READ_MAX_LINES);

        let span_large = ReadSpan {
            start_line: Some(1),
            end_line: Some(200),
            ..Default::default()
        };
        assert_eq!(span_large.effective_line_count(None), Some(200));
        assert!(span_large.effective_line_count(None).unwrap() > cortx_core::MICRO_READ_MAX_LINES);
    }
}
