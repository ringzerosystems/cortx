//! Standardized terminal diagnostics and presentation helpers.

use colored::Colorize;

/// Prints an informational banner to stderr.
pub fn info_banner(tag: &str, message: &str) {
    eprintln!("{} {}", tag.bright_blue().bold(), message);
}

/// Prints a success message to stderr.
pub fn success(tag: &str, message: &str) {
    eprintln!("{} {}", tag.bright_green().bold(), message);
}

/// Prints a warning message to stderr.
pub fn warn(tag: &str, message: &str) {
    eprintln!("{} {}", tag.bright_yellow().bold(), message);
}

/// Prints an error message to stderr.
pub fn error(tag: &str, message: &str) {
    eprintln!("{} {}", tag.bright_red().bold(), message);
}

/// Formats a counterexample or diff header.
pub fn format_counterexample_header(oracle_name: &str, seed: Option<u64>) -> String {
    let mut header = format!(
        "=== Counterexample Discovered by [{}] ===\n",
        oracle_name.bright_magenta().bold()
    );
    if let Some(s) = seed {
        header.push_str(&format!("Seed: {:#018x}\n", s));
    }
    header
}
