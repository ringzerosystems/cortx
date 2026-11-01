//! Formatting helpers for daemon status tables and telemetry output.

use cortx_daemon::{DaemonStatusInfo, WorkspaceSummary};

pub(crate) fn format_with_commas(n: usize) -> String {
    let s = n.to_string();
    let first_len = s.len() % 3;
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    if first_len > 0 {
        result.push_str(&s[..first_len]);
    }
    for (i, chunk) in s.as_bytes()[first_len..].chunks(3).enumerate() {
        if first_len > 0 || i > 0 {
            result.push(',');
        }
        result.push_str(std::str::from_utf8(chunk).unwrap_or(""));
    }
    result
}

pub(crate) fn format_uptime(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{}h {}m {}s", h, m, s)
    } else if m > 0 {
        format!("{}m {}s", m, s)
    } else {
        format!("{}s", s)
    }
}

pub(crate) fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

pub(crate) fn format_time_ago(secs: u64) -> String {
    if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else {
        format!("{}h ago", secs / 3600)
    }
}

pub(crate) fn truncate_tail(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count > max_chars {
        let keep = max_chars.saturating_sub(3);
        let tail: String = s.chars().skip(count - keep).collect();
        format!("...{}", tail)
    } else {
        s.to_string()
    }
}

pub(crate) fn format_range(start: Option<usize>, end: Option<usize>) -> String {
    match (start, end) {
        (Some(s), Some(e)) => format!(" (lines {}..{})", s, e),
        (Some(s), None) => format!(" (lines {}..)", s),
        (None, Some(e)) => format!(" (lines ..{})", e),
        (None, None) => String::new(),
    }
}

pub(crate) fn format_hooks_served(served: u64, submissions: u64) -> String {
    if served > 0 {
        let omitted = served.saturating_sub(submissions);
        format!(
            "{:<22}{} requests ({} submitted, {} omitted)",
            "Hooks Served:",
            format_with_commas(served as usize),
            format_with_commas(submissions as usize),
            format_with_commas(omitted as usize)
        )
    } else {
        format!("{:<22}0 requests", "Hooks Served:")
    }
}

pub(crate) fn format_drop_telemetry(cg_drops: u64, ast_drops: u64) -> String {
    format!(
        "{:<22}{} call graphs pruned, {} AST skeletons dropped",
        "Drop Telemetry:",
        format_with_commas(cg_drops as usize),
        format_with_commas(ast_drops as usize)
    )
}

pub(crate) fn format_read_operations(served: u64, macro_reads: u64, micro_reads: u64) -> String {
    let total = if served > 0 {
        served
    } else {
        macro_reads + micro_reads
    };
    let total_reads = macro_reads + micro_reads;
    let micro_pct = if total_reads > 0 {
        (micro_reads as f64 / total_reads as f64) * 100.0
    } else {
        0.0
    };
    format!(
        "{:<22}{} requests ({} macro, {} micro, {:.1}% micro)",
        "Read Operations:",
        format_with_commas(total as usize),
        format_with_commas(macro_reads as usize),
        format_with_commas(micro_reads as usize),
        micro_pct
    )
}

pub(crate) fn format_supplemental_context(bytes: u64, tokens: u64) -> String {
    if bytes > 0 || tokens > 0 {
        let est_tokens = if tokens > 0 { tokens } else { bytes / 4 };
        format!(
            "{:<22}{} delivered (~{} tokens, est. 4B/token)",
            "Supplemental Context:",
            format_bytes(bytes),
            format_with_commas(est_tokens as usize)
        )
    } else {
        format!(
            "{:<22}0 B (no requests served yet)",
            "Supplemental Context:"
        )
    }
}

pub(crate) fn format_raw_source_inspected(raw_bytes: u64, supp_bytes: u64) -> String {
    let ratio = if raw_bytes > 0 {
        (supp_bytes as f64 / raw_bytes as f64) * 100.0
    } else {
        0.0
    };
    format!(
        "{:<22}{} (advisory ratio: {:.1}%)",
        "Raw Source Inspected:",
        format_bytes(raw_bytes),
        ratio
    )
}

pub(crate) fn format_daemon_version_protocol(version: &str, protocol: u32) -> String {
    let ver = if version.is_empty() {
        "0.1.0"
    } else {
        version.strip_prefix('v').unwrap_or(version)
    };
    format!(
        "{:<22}v{} (protocol v{})",
        "Version / Protocol:", ver, protocol
    )
}

pub(crate) fn format_section_dispositions(
    omitted_micro: u64,
    omitted_repeat: u64,
    omitted_skeletons: u64,
    trimmed: u64,
    fallbacks: u64,
) -> String {
    format!(
        "{:<22}{} micro omitted, {} repeats omitted, {} full skeletons omitted, {} trimmed, {} parse fallbacks",
        "Section Dispositions:",
        format_with_commas(omitted_micro as usize),
        format_with_commas(omitted_repeat as usize),
        format_with_commas(omitted_skeletons as usize),
        format_with_commas(trimmed as usize),
        format_with_commas(fallbacks as usize)
    )
}

pub(crate) fn format_workspace_registry_row(ws: &WorkspaceSummary) -> String {
    let ws_type = if ws.is_worktree { "WkTr" } else { "Repo" };
    let root_trunc = truncate_tail(&ws.root.display().to_string(), 28);
    let proj_trunc = truncate_tail(ws.project_name.as_deref().unwrap_or("-"), 16);
    let cfg_str = ws
        .config_path
        .as_ref()
        .and_then(|p| p.file_name())
        .and_then(|f| f.to_str())
        .unwrap_or("-");
    let cfg_trunc = truncate_tail(cfg_str, 24);
    let log_str = ws
        .log_path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("");
    let log_trunc = truncate_tail(log_str, 28);
    let accessed_str = format_time_ago(ws.last_accessed_secs_ago);
    let total_reads = ws.macro_reads + ws.micro_reads;
    let micro_pct_str = if total_reads > 0 {
        let pct = (ws.micro_reads as f64 / total_reads as f64) * 100.0;
        format!("{:.1}%", pct)
    } else {
        "-".to_string()
    };
    format!(
        "  {:<28} {:<16} {:<6} {:>8} {:>10} {:>7.1} MB {:>7} {:>8} {:>9} {:>7} {:>7} {:>7}  {:<24} {:<28} {:>8}",
        root_trunc,
        proj_trunc,
        ws_type,
        format_with_commas(ws.indexed_files),
        format_with_commas(ws.total_symbols),
        ws.memory_mb,
        format_with_commas(ws.submissions as usize),
        format_with_commas(ws.callgraph_drops as usize),
        format_with_commas(ws.ast_drops as usize),
        format_with_commas(ws.macro_reads as usize),
        format_with_commas(ws.micro_reads as usize),
        micro_pct_str,
        cfg_trunc,
        log_trunc,
        accessed_str
    )
}

pub(crate) fn print_daemon_status_table(status: &DaemonStatusInfo) {
    println!("Cortx Daemon Status:  Active");
    println!(
        "{}",
        format_daemon_version_protocol(&status.daemon_version, status.protocol_version)
    );
    println!("{:<22}{}", "PID:", status.pid);
    println!("{:<22}{}", "Uptime:", format_uptime(status.uptime_secs));
    if !status.daemon_mode.is_empty() {
        println!("{:<22}{}", "Daemon Mode:", status.daemon_mode);
    }
    let cfg_display = status
        .config_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "built-in defaults".to_string());
    println!("{:<22}{}", "Config Path:", cfg_display);
    println!(
        "{:<22}{}",
        "Declared Projects:", status.declared_projects_count
    );
    println!("{:<22}{}", "Socket Path:", status.socket_path.display());
    if !status.main_log_path.as_os_str().is_empty() {
        println!(
            "{:<22}{}",
            "Main Daemon Log:",
            status.main_log_path.display()
        );
    }
    println!("{:<22}{:.1} MB", "Memory RSS:", status.memory_rss_mb);
    println!(
        "{:<22}{}",
        "Active Workspaces:", status.active_workspaces_count
    );
    if !status.workspace_root.as_os_str().is_empty() {
        println!(
            "{:<22}{}",
            "Workspace Root:",
            status.workspace_root.display()
        );
    }
    println!(
        "{:<22}{} files",
        "Indexed Files:",
        format_with_commas(status.indexed_files)
    );
    println!(
        "{:<22}{} declarations",
        "Total Symbols:",
        format_with_commas(status.total_symbols)
    );
    println!(
        "{:<22}{} edges",
        "Call Edges:",
        format_with_commas(status.total_call_edges)
    );
    println!(
        "{:<22}{} trait implementations",
        "Contracts:",
        format_with_commas(status.total_contracts)
    );
    println!(
        "{:<22}{} API endpoints",
        "Routes:",
        format_with_commas(status.total_routes)
    );
    let watcher_str = if status.watcher_active {
        "Active (.rs, .ts, .tsx, .js, .jsx, .go)"
    } else {
        "Inactive"
    };
    println!("{:<22}{}", "File Watcher:", watcher_str);

    if !status.workspaces.is_empty() {
        print_workspace_registry_table(&status.workspaces);
    }

    print_context_telemetry(status);
}

fn print_context_telemetry(status: &DaemonStatusInfo) {
    println!(
        "{}",
        format_hooks_served(status.hooks_served, status.submissions)
    );
    if status.hooks_served > 0 {
        println!(
            "{}",
            format_drop_telemetry(status.callgraph_drops, status.ast_drops)
        );
        let supp_bytes = if status.supplemental_context_bytes > 0 {
            status.supplemental_context_bytes
        } else {
            status.delivered_bytes_served
        };
        let supp_tokens = if status.supplemental_context_tokens > 0 {
            status.supplemental_context_tokens
        } else {
            supp_bytes / 4
        };
        println!(
            "{}",
            format_read_operations(status.hooks_served, status.macro_reads, status.micro_reads)
        );
        println!(
            "{}",
            format_section_dispositions(
                status.omitted_micro_reads,
                status.omitted_repeat_reads,
                status.omitted_full_read_skeletons,
                status.trimmed_advisories,
                status.parse_fallbacks
            )
        );
        println!("{}", format_supplemental_context(supp_bytes, supp_tokens));
        println!(
            "{}",
            format_raw_source_inspected(status.raw_bytes_served, supp_bytes)
        );
    } else {
        println!("{}", format_supplemental_context(0, 0));
    }
}

pub(crate) fn print_workspace_registry_table(workspaces: &[WorkspaceSummary]) {
    println!("\nActive Workspace Registry:");
    println!(
        "  {:<28} {:<16} {:<6} {:>8} {:>10} {:>10} {:>7} {:>8} {:>9} {:>7} {:>7} {:>7}  {:<24} {:<28} {:>8}",
        "WORKSPACE",
        "PROJECT",
        "TYPE",
        "FILES",
        "SYMBOLS",
        "MEMORY",
        "SUBMIT",
        "CG_DROP",
        "AST_DROP",
        "MACRO",
        "MICRO",
        "MICRO%",
        "CONFIG",
        "LOG FILE",
        "ACCESSED"
    );
    println!("  {}", "-".repeat(200));
    for ws in workspaces {
        println!("{}", format_workspace_registry_row(ws));
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.00 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.00 GB");
    }

    #[test]
    fn test_format_uptime() {
        assert_eq!(format_uptime(30), "30s");
        assert_eq!(format_uptime(90), "1m 30s");
        assert_eq!(format_uptime(3665), "1h 1m 5s");
    }

    #[test]
    fn test_format_time_ago() {
        assert_eq!(format_time_ago(5), "5s ago");
        assert_eq!(format_time_ago(120), "2m ago");
        assert_eq!(format_time_ago(7200), "2h ago");
    }

    #[test]
    fn test_truncate_tail_utf8_safety() {
        let path = "🦀_crab_🦀_crab_🦀_crab_🦀";
        let truncated = truncate_tail(path, 12);
        assert!(truncated.starts_with("..."));
        assert!(truncated.chars().count() <= 12);
    }

    #[test]
    fn test_format_hooks_served_and_drop_telemetry() {
        assert_eq!(
            format_hooks_served(0, 0),
            "Hooks Served:         0 requests"
        );
        assert_eq!(
            format_hooks_served(23, 18),
            "Hooks Served:         23 requests (18 submitted, 5 omitted)"
        );
        assert_eq!(
            format_drop_telemetry(8, 5),
            "Drop Telemetry:       8 call graphs pruned, 5 AST skeletons dropped"
        );
    }

    #[test]
    fn test_format_read_operations() {
        assert_eq!(
            format_read_operations(0, 0, 0),
            "Read Operations:      0 requests (0 macro, 0 micro, 0.0% micro)"
        );
        assert_eq!(
            format_read_operations(8, 1, 7),
            "Read Operations:      8 requests (1 macro, 7 micro, 87.5% micro)"
        );
    }

    #[test]
    fn test_format_supplemental_context() {
        assert_eq!(
            format_supplemental_context(0, 0),
            "Supplemental Context: 0 B (no requests served yet)"
        );
        assert_eq!(
            format_supplemental_context(2048, 512),
            "Supplemental Context: 2.0 KB delivered (~512 tokens, est. 4B/token)"
        );
    }

    #[test]
    fn test_format_raw_source_inspected() {
        assert_eq!(
            format_raw_source_inspected(0, 0),
            "Raw Source Inspected: 0 B (advisory ratio: 0.0%)"
        );
        assert_eq!(
            format_raw_source_inspected(10240, 2048),
            "Raw Source Inspected: 10.0 KB (advisory ratio: 20.0%)"
        );
    }

    #[test]
    fn test_format_workspace_registry_row_includes_drop_and_read_columns() {
        let mut ws = WorkspaceSummary {
            root: PathBuf::from("/home/mark/projects/pgrust"),
            project_name: Some("pgrust".to_string()),
            is_worktree: false,
            parent_repo: None,
            indexed_files: 3319,
            total_symbols: 65541,
            memory_mb: 292.0,
            config_path: Some(PathBuf::from("/home/mark/.config/cortx/cortx.toml")),
            log_path: PathBuf::from("/tmp/pgrust-a907563c9c2c.log"),
            last_accessed_secs_ago: 10,
            submissions: 7,
            callgraph_drops: 8,
            ast_drops: 5,
            macro_reads: 0,
            micro_reads: 0,
            omitted_micro_reads: 0,
            omitted_repeat_reads: 0,
            omitted_full_read_skeletons: 0,
            trimmed_advisories: 0,
            parse_fallbacks: 0,
        };
        let row_zero = format_workspace_registry_row(&ws);
        assert!(row_zero.contains("7"));
        assert!(row_zero.contains("8"));
        assert!(row_zero.contains("5"));
        assert!(row_zero.contains("-"));

        ws.macro_reads = 1;
        ws.micro_reads = 7;
        let row_active = format_workspace_registry_row(&ws);
        assert!(row_active.contains("1"));
        assert!(row_active.contains("7"));
        assert!(row_active.contains("87.5%"));
    }

    #[test]
    fn test_format_daemon_version_protocol() {
        assert_eq!(
            format_daemon_version_protocol("0.1.0", 2),
            "Version / Protocol:   v0.1.0 (protocol v2)"
        );
        assert_eq!(
            format_daemon_version_protocol("v0.2.1", 3),
            "Version / Protocol:   v0.2.1 (protocol v3)"
        );
        assert_eq!(
            format_daemon_version_protocol("", 2),
            "Version / Protocol:   v0.1.0 (protocol v2)"
        );
    }

    #[test]
    fn test_format_section_dispositions() {
        assert_eq!(
            format_section_dispositions(0, 0, 0, 0, 0),
            "Section Dispositions: 0 micro omitted, 0 repeats omitted, 0 full skeletons omitted, 0 trimmed, 0 parse fallbacks"
        );
        assert_eq!(
            format_section_dispositions(12, 5, 3, 4, 1),
            "Section Dispositions: 12 micro omitted, 5 repeats omitted, 3 full skeletons omitted, 4 trimmed, 1 parse fallbacks"
        );
    }
}
