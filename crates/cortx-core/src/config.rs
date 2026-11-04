//! Configuration models and discovery for cargo-cortx.

use crate::error::CoreError;
use crate::output::OutputFormat;
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Standard universal build artifacts, dependency directories, and VCS metadata.
/// NOTE: `.worktrees` and `.claude` are intentionally EXCLUDED here because they may
/// contain nested Git worktree roots; those are pruned relative to each workspace root.
pub const STANDARD_IGNORED_DIRS: &[&str] = &[
    "target",
    ".git",
    "node_modules",
    "vendor",
    "dist",
    "build",
    "out",
    ".next",
    ".nuxt",
    ".turbo",
    ".yarn",
    ".pnpm-store",
    ".output",
    ".svelte-kit",
    "coverage",
    ".idea",
    ".vscode",
    ".cache",
];

/// Primary configuration structure for cargo-cortx workspaces.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CortxConfig {
    /// General workspace settings.
    #[serde(default)]
    pub general: GeneralConfig,
    /// Configuration for context distillation engine.
    #[serde(default)]
    pub distill: DistillConfig,
    /// Configuration for daemon sidecar service.
    #[serde(default)]
    pub daemon: DaemonConfig,
    /// Configuration for TypeScript / TSX context distillation.
    #[serde(default)]
    pub typescript: TypeScriptConfig,
    /// Configuration for Go context distillation.
    #[serde(default)]
    pub go: GoConfig,
    /// Configuration for cross-boundary contract correlation.
    #[serde(default)]
    pub cross_boundary: CrossBoundaryConfig,
    /// Explicitly namespaced projects: `[projects.<name>]`.
    #[serde(default)]
    pub projects: HashMap<String, ProjectConfig>,
    /// Top-level project tables: `[<project-name>]`.
    #[serde(flatten)]
    pub dynamic_projects: HashMap<String, toml::Value>,
}

impl Eq for CortxConfig {}

/// General engine configuration options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneralConfig {
    /// Codebase scale hint: "small", "medium", "massive" (adjusts cache/pool allocations).
    #[serde(default = "default_codebase_scale")]
    pub codebase_scale: String,
    /// Default output format for reports and distillations.
    #[serde(default)]
    pub default_format: OutputFormat,
}

fn default_codebase_scale() -> String {
    "massive".to_string()
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            codebase_scale: default_codebase_scale(),
            default_format: OutputFormat::Markdown,
        }
    }
}

/// Context distillation engine configuration options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistillConfig {
    /// Maximum call-graph depth for caller/callee exploration.
    #[serde(default = "default_max_call_depth")]
    pub max_call_depth: usize,
    /// Whether to strip comments during skeleton unparsing.
    #[serde(default)]
    pub strip_comments: bool,
    /// Whether to include private/internal types in context exports.
    #[serde(default = "default_true")]
    pub include_private_types: bool,
    /// Whether to omit tests from distillation output (AST skeleton and call graph). Defaults to true.
    #[serde(default = "default_true")]
    pub omit_tests: bool,
    /// Custom directory names to ignore during file discovery.
    #[serde(default)]
    pub custom_ignored_dirs: Vec<String>,
}

fn default_max_call_depth() -> usize {
    2
}

fn default_true() -> bool {
    true
}

impl Default for DistillConfig {
    fn default() -> Self {
        Self {
            max_call_depth: default_max_call_depth(),
            strip_comments: false,
            include_private_types: true,
            omit_tests: true,
            custom_ignored_dirs: Vec::new(),
        }
    }
}

/// Default agent session inactivity TTL in seconds (8 hours = 28,800 seconds).
pub const DEFAULT_SESSION_TTL_SECS: u64 = 28800;

/// Background daemon configuration options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// Unix domain socket path override (None = auto-derived per workspace/worktree).
    #[serde(default)]
    pub socket_path: Option<String>,
    /// Whether this daemon operates in system-wide multi-tenant mode (default: true).
    #[serde(default = "default_true")]
    pub system_daemon: bool,
    /// Whether indexing is deferred lazily until first workspace touch (default: true).
    #[serde(default = "default_true")]
    pub lazy_indexing: bool,
    /// Global Unix domain socket path override (None = auto-derived in XDG runtime dir).
    #[serde(default)]
    pub global_socket: Option<String>,
    /// Inactivity threshold in seconds before unindexing an idle workspace (default: 7200 = 2 hours).
    #[serde(default = "default_idle_ttl_secs")]
    pub idle_ttl_secs: u64,
    /// Inactivity threshold in seconds before evicting an idle agent session (default: 28800 = 8 hours).
    #[serde(default = "default_session_ttl_secs")]
    pub session_ttl_secs: u64,
    /// Whether background filesystem watching is enabled across active workspaces.
    #[serde(default = "default_true")]
    pub enable_file_watcher: bool,
    /// Directory for file logging (None = default ~/.local/state/cortx/logs).
    #[serde(default)]
    pub log_dir: Option<String>,
    /// Enable per-workspace isolated log files under <log_dir>/workspaces/ (default: true).
    #[serde(default = "default_true")]
    pub workspace_logging: bool,
}

fn default_idle_ttl_secs() -> u64 {
    7200
}

fn default_session_ttl_secs() -> u64 {
    DEFAULT_SESSION_TTL_SECS
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: None,
            system_daemon: true,
            lazy_indexing: true,
            global_socket: None,
            idle_ttl_secs: default_idle_ttl_secs(),
            session_ttl_secs: default_session_ttl_secs(),
            enable_file_watcher: true,
            log_dir: None,
            workspace_logging: true,
        }
    }
}

/// Configuration for TypeScript / TSX context distillation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeScriptConfig {
    /// Enable TypeScript parsing and distillation.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Root tsconfig path for symbol resolution.
    #[serde(default = "default_tsconfig_path")]
    pub tsconfig_path: String,
    /// Supported extensions for distillation and file watching.
    #[serde(default = "default_ts_extensions")]
    pub extensions: Vec<String>,
    /// Strips JSX blocks down to clean type signatures.
    #[serde(default = "default_true")]
    pub strip_jsx: bool,
    /// Strips React hook internals (useEffect, useState).
    #[serde(default = "default_true")]
    pub strip_hook_bodies: bool,
}

fn default_tsconfig_path() -> String {
    "frontend/tsconfig.json".to_string()
}

fn default_ts_extensions() -> Vec<String> {
    vec![
        "ts".to_string(),
        "tsx".to_string(),
        "js".to_string(),
        "jsx".to_string(),
    ]
}

impl Default for TypeScriptConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            tsconfig_path: default_tsconfig_path(),
            extensions: default_ts_extensions(),
            strip_jsx: true,
            strip_hook_bodies: true,
        }
    }
}

/// Configuration for cross-boundary contract correlation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossBoundaryConfig {
    /// Directory containing shared Serde models.
    #[serde(default = "default_rust_models")]
    pub rust_models: String,
    /// Directory containing TypeScript contract definitions.
    #[serde(default = "default_ts_types")]
    pub ts_types: String,
    /// Directory containing Go backend models.
    #[serde(default = "default_go_models")]
    pub go_models: String,
    /// Enforce camelCase vs snake_case consistency checks.
    #[serde(default = "default_true")]
    pub strict_casing_check: bool,
    /// Enforce omitempty zero-value discrepancy validation.
    #[serde(default = "default_true")]
    pub strict_omitempty_check: bool,
    /// Enforce nil slice vs empty slice serialization consistency.
    #[serde(default = "default_true")]
    pub strict_nil_slice_check: bool,
}

fn default_rust_models() -> String {
    "crates/shared-types/src".to_string()
}

fn default_ts_types() -> String {
    "frontend/src/types".to_string()
}

fn default_go_models() -> String {
    "backend/pkg/models".to_string()
}

impl Default for CrossBoundaryConfig {
    fn default() -> Self {
        Self {
            rust_models: default_rust_models(),
            ts_types: default_ts_types(),
            go_models: default_go_models(),
            strict_casing_check: true,
            strict_omitempty_check: true,
            strict_nil_slice_check: true,
        }
    }
}

/// Configuration for Go context distillation and verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoConfig {
    /// Enable Go parsing and distillation.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Root directory for Go modules or monorepo root.
    #[serde(default = "default_go_module_root")]
    pub module_root: String,
    /// Supported Go file extensions.
    #[serde(default = "default_go_extensions")]
    pub extensions: Vec<String>,
    /// Strip function and method bodies.
    #[serde(default = "default_true")]
    pub strip_function_bodies: bool,
    /// Compute implicit interface satisfaction matrix.
    #[serde(default = "default_true")]
    pub index_implicit_interfaces: bool,
    /// Include unexported (lowercase) types and helpers.
    #[serde(default)]
    pub include_private_types: bool,
}

fn default_go_module_root() -> String {
    "backend".to_string()
}

fn default_go_extensions() -> Vec<String> {
    vec!["go".to_string()]
}

impl Default for GoConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            module_root: default_go_module_root(),
            extensions: default_go_extensions(),
            strip_function_bodies: true,
            index_implicit_interfaces: true,
            include_private_types: false,
        }
    }
}

/// Declarative per-project configuration for multi-repo environments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// Canonical qualified path to the project root directory.
    #[serde(default)]
    pub qualified_path: PathBuf,
    /// Whether this project employs Git worktrees.
    #[serde(default)]
    pub has_worktrees: bool,
    /// Custom path or subdirectory where worktrees reside.
    #[serde(default, alias = "has_custom_worktree_path")]
    pub custom_worktree_path: Option<PathBuf>,
    /// Languages enabled for this project (e.g. rust, typescript, go).
    #[serde(default, deserialize_with = "deserialize_lang_list")]
    pub lang: Vec<String>,

    /// Codebase scale hint: "small", "medium", "massive".
    #[serde(default = "default_codebase_scale")]
    pub codebase_scale: String,
    /// Default output format ("markdown", "json", "text").
    #[serde(default)]
    pub default_format: OutputFormat,

    /// Subdirectories to monitor for source code modifications within this project.
    /// Empty means all non-ignored paths under the workspace root are monitored.
    #[serde(default)]
    pub watch_paths: Vec<String>,
    /// Additional directory names to ignore during indexing and watching.
    #[serde(default)]
    pub ignored_dirs: Vec<String>,

    /// Distillation call-graph exploration depth bound.
    #[serde(default = "default_max_call_depth")]
    pub max_call_depth: usize,
    /// Whether to strip comments during skeleton unparsing.
    #[serde(default)]
    pub strip_comments: bool,
    /// Whether to include private/internal types in context exports.
    #[serde(default = "default_true")]
    pub include_private_types: bool,
    /// Whether to omit tests from distillation output (AST skeleton and call graph). Defaults to true.
    #[serde(default = "default_true")]
    pub omit_tests: bool,
    /// Custom directory names to ignore during file discovery.
    #[serde(default)]
    pub custom_ignored_dirs: Vec<String>,

    /// Per-project distillation options.
    #[serde(default)]
    pub distill: DistillConfig,
    /// Per-project TypeScript options.
    #[serde(default)]
    pub typescript: TypeScriptConfig,
    /// Per-project Go options.
    #[serde(default)]
    pub go: GoConfig,
    /// Per-project cross-boundary contract options.
    #[serde(default)]
    pub cross_boundary: CrossBoundaryConfig,

    /// Precomputed normalized ignored path prefixes for zero-allocation checks.
    #[serde(skip)]
    pub precomputed_ignored_prefixes: Vec<PathBuf>,
    /// Precomputed ignored path component names for fast O(1) set lookup.
    #[serde(skip)]
    pub precomputed_ignored_components: HashSet<String>,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            qualified_path: PathBuf::new(),
            has_worktrees: false,
            custom_worktree_path: None,
            lang: Vec::new(),
            codebase_scale: default_codebase_scale(),
            default_format: OutputFormat::Markdown,
            watch_paths: Vec::new(),
            ignored_dirs: Vec::new(),
            max_call_depth: default_max_call_depth(),
            strip_comments: false,
            include_private_types: true,
            omit_tests: true,
            custom_ignored_dirs: Vec::new(),
            distill: DistillConfig::default(),
            typescript: TypeScriptConfig::default(),
            go: GoConfig::default(),
            cross_boundary: CrossBoundaryConfig::default(),
            precomputed_ignored_prefixes: Vec::new(),
            precomputed_ignored_components: HashSet::new(),
        }
    }
}

fn normalize_rel_pattern(pattern: &str) -> &Path {
    let p = Path::new(pattern.trim());
    p.strip_prefix(".").unwrap_or(p)
}

impl ProjectConfig {
    /// Constructs a `ProjectConfig` synthesized from a `CortxConfig`.
    pub fn from_cortx_config(root: PathBuf, cfg: &CortxConfig) -> Self {
        let mut proj = Self {
            qualified_path: root,
            has_worktrees: false,
            custom_worktree_path: None,
            lang: Vec::new(),
            codebase_scale: cfg.general.codebase_scale.clone(),
            default_format: cfg.general.default_format,
            watch_paths: Vec::new(),
            ignored_dirs: Vec::new(),
            max_call_depth: cfg.distill.max_call_depth,
            strip_comments: cfg.distill.strip_comments,
            include_private_types: cfg.distill.include_private_types,
            omit_tests: cfg.distill.omit_tests,
            custom_ignored_dirs: cfg.distill.custom_ignored_dirs.clone(),
            distill: cfg.distill.clone(),
            typescript: cfg.typescript.clone(),
            go: cfg.go.clone(),
            cross_boundary: cfg.cross_boundary.clone(),
            ..Default::default()
        };
        proj.normalize();
        proj
    }

    /// Normalizes inline scalar fields with nested sub-table configurations.
    pub fn normalize(&mut self) {
        if self.max_call_depth != default_max_call_depth()
            && self.distill.max_call_depth == default_max_call_depth()
        {
            self.distill.max_call_depth = self.max_call_depth;
        }
        if self.strip_comments && !self.distill.strip_comments {
            self.distill.strip_comments = self.strip_comments;
        }
        if !self.include_private_types && self.distill.include_private_types {
            self.distill.include_private_types = self.include_private_types;
        }
        if !self.omit_tests && self.distill.omit_tests {
            self.distill.omit_tests = self.omit_tests;
        } else if !self.distill.omit_tests && self.omit_tests {
            self.omit_tests = self.distill.omit_tests;
        }
        if !self.custom_ignored_dirs.is_empty() {
            self.distill
                .custom_ignored_dirs
                .extend(self.custom_ignored_dirs.clone());
            self.distill.custom_ignored_dirs.sort();
            self.distill.custom_ignored_dirs.dedup();
        }
        self.recompute_ignored_caches();
    }

    /// Precomputes normalized ignored path prefixes and single-component names
    /// to eliminate allocations during recursive directory discovery.
    pub fn recompute_ignored_caches(&mut self) {
        let mut prefixes = Vec::new();
        let mut components = HashSet::new();

        for d in self
            .ignored_dirs
            .iter()
            .chain(&self.custom_ignored_dirs)
            .chain(&self.distill.custom_ignored_dirs)
        {
            let pat = normalize_rel_pattern(d);
            if !pat.as_os_str().is_empty() {
                prefixes.push(pat.to_path_buf());
                if pat.parent().is_none_or(|p| p.as_os_str().is_empty()) {
                    if let Some(s) = pat.to_str() {
                        components.insert(s.to_string());
                    }
                }
            }
        }
        prefixes.sort();
        prefixes.dedup();
        self.precomputed_ignored_prefixes = prefixes;
        self.precomputed_ignored_components = components;
    }

    /// Determines whether a path relative to the workspace root should be ignored,
    /// directly evaluating universal build artifacts, project-specific `ignored_dirs`,
    /// and distillation `custom_ignored_dirs`.
    pub fn is_relative_path_ignored(&self, rel: &Path) -> bool {
        let rel = rel.strip_prefix(".").unwrap_or(rel);
        let has_cached = !self.precomputed_ignored_prefixes.is_empty()
            || !self.precomputed_ignored_components.is_empty();

        if has_cached {
            if self
                .precomputed_ignored_prefixes
                .iter()
                .any(|p| rel.starts_with(p))
            {
                return true;
            }
        } else if self
            .ignored_dirs
            .iter()
            .chain(&self.custom_ignored_dirs)
            .chain(&self.distill.custom_ignored_dirs)
            .any(|d| {
                let pat = normalize_rel_pattern(d);
                !pat.as_os_str().is_empty() && rel.starts_with(pat)
            })
        {
            return true;
        }

        for comp in rel.components() {
            if let std::path::Component::Normal(c) = comp {
                if let Some(s) = c.to_str() {
                    if STANDARD_IGNORED_DIRS.contains(&s) {
                        return true;
                    }
                    if has_cached {
                        if self.precomputed_ignored_components.contains(s) {
                            return true;
                        }
                    } else if self
                        .ignored_dirs
                        .iter()
                        .chain(&self.custom_ignored_dirs)
                        .chain(&self.distill.custom_ignored_dirs)
                        .any(|d| normalize_rel_pattern(d).as_os_str() == s)
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Returns `true` if a workspace-relative directory `rel_dir` is within or is an ancestor
    /// of any configured `watch_paths` entry (or if `watch_paths` is empty/unrestricted).
    pub fn is_dir_in_watch_paths(&self, rel_dir: &Path) -> bool {
        let rel_dir = rel_dir.strip_prefix(".").unwrap_or(rel_dir);
        if self.watch_paths.is_empty() || rel_dir.as_os_str().is_empty() {
            return true;
        }
        self.watch_paths.iter().any(|wp| {
            if wp.trim().is_empty() {
                return false;
            }
            let wp_path = normalize_rel_pattern(wp);
            wp_path.as_os_str().is_empty()
                || rel_dir.starts_with(wp_path)
                || wp_path.starts_with(rel_dir)
        })
    }

    /// Returns `true` if a workspace-relative file `rel_file` resides inside any configured
    /// `watch_paths` entry (or if `watch_paths` is empty/unrestricted).
    pub fn is_file_in_watch_paths(&self, rel_file: &Path) -> bool {
        let rel_file = rel_file.strip_prefix(".").unwrap_or(rel_file);
        if self.watch_paths.is_empty() {
            return true;
        }
        self.watch_paths.iter().any(|wp| {
            if wp.trim().is_empty() {
                return false;
            }
            let wp_path = normalize_rel_pattern(wp);
            wp_path.as_os_str().is_empty() || rel_file.starts_with(wp_path)
        })
    }

    /// Resolves `custom_worktree_path` as a normalized workspace-relative path against `root`
    /// (stripping `root`, `qualified_path`, and any leading `./` prefix).
    pub fn relative_custom_worktree_path<'a>(&'a self, root: &Path) -> Option<&'a Path> {
        let wt = self.custom_worktree_path.as_deref()?;
        let stripped = wt
            .strip_prefix(root)
            .or_else(|_| wt.strip_prefix(&self.qualified_path))
            .unwrap_or(wt);
        let normalized = stripped.strip_prefix(".").unwrap_or(stripped);
        (!normalized.as_os_str().is_empty() && normalized.is_relative()).then_some(normalized)
    }

    /// Checks whether a file extension is enabled by this project's `lang` setting.
    pub fn is_extension_enabled(&self, ext: &str) -> bool {
        if self.lang.is_empty() {
            return matches!(ext, "rs" | "ts" | "tsx" | "js" | "jsx" | "go");
        }
        match ext {
            "rs" => self.lang.iter().any(|l| l == "rust" || l == "rs"),
            "ts" | "tsx" | "js" | "jsx" => {
                self.typescript.enabled
                    && self.lang.iter().any(|l| {
                        matches!(
                            l.as_str(),
                            "typescript" | "ts" | "tsx" | "javascript" | "js"
                        )
                    })
            }
            "go" => self.go.enabled && self.lang.iter().any(|l| l == "go" || l == "golang"),
            _ => false,
        }
    }
}

/// Flexible deserializer accepting either a comma-separated string ("rust,typescript")
/// or an array of strings (["rust", "typescript"]).
fn deserialize_lang_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum LangInput {
        List(Vec<String>),
        CommaString(String),
    }

    match LangInput::deserialize(deserializer)? {
        LangInput::List(list) => Ok(list
            .into_iter()
            .map(|part| part.trim().to_ascii_lowercase())
            .filter(|part| !part.is_empty())
            .collect()),
        LangInput::CommaString(s) => Ok(s
            .split(',')
            .map(|part| part.trim().to_ascii_lowercase())
            .filter(|part| !part.is_empty())
            .collect()),
    }
}

/// Tracks the configuration source loaded: either an explicit `--config` path or the platform default system path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigProvenance {
    /// Explicit `--config` override path, if supplied.
    pub custom_path: Option<PathBuf>,
    /// Platform default system configuration file path (`cortx.toml`), if loaded.
    pub system_path: Option<PathBuf>,
}

impl ConfigProvenance {
    /// Returns the active configuration path (custom -> system).
    pub fn primary_path(&self) -> Option<&Path> {
        self.custom_path.as_deref().or(self.system_path.as_deref())
    }

    /// Emits structured tracing logs detailing the resolved configuration source.
    pub fn log_details(&self) {
        if let Some(ref p) = self.custom_path {
            tracing::info!(path = %p.display(), layer = "custom", "Loaded custom configuration");
        } else if let Some(ref p) = self.system_path {
            tracing::info!(path = %p.display(), layer = "system", "Loaded system configuration");
        } else {
            tracing::info!("Operating with default built-in configuration");
        }
    }
}

/// Resolves the candidate system-level configuration path (`cortx.toml`):
/// - Windows: `%APPDATA%\cortx\cortx.toml` (or `%PROGRAMDATA%\cortx\cortx.toml`)
/// - macOS: `~/Library/Application Support/cortx/cortx.toml`
/// - Linux: `$XDG_CONFIG_HOME/cortx/cortx.toml` (default `~/.config/cortx/cortx.toml`) or `/etc/cortx/cortx.toml`
pub fn resolve_system_config_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let p = PathBuf::from(appdata).join("cortx").join("cortx.toml");
            if p.is_file() {
                return std::fs::canonicalize(&p).ok().or(Some(p));
            }
        }
        if let Some(progdata) = std::env::var_os("ProgramData") {
            let p = PathBuf::from(progdata).join("cortx").join("cortx.toml");
            if p.is_file() {
                return std::fs::canonicalize(&p).ok().or(Some(p));
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            let mac_path = PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("cortx")
                .join("cortx.toml");
            if mac_path.is_file() {
                return std::fs::canonicalize(&mac_path).ok().or(Some(mac_path));
            }
        }
    }

    // Linux & Unix platforms (also fallback for macOS / container environments)
    if let Some(xdg_config) = std::env::var_os("XDG_CONFIG_HOME") {
        if !xdg_config.is_empty() {
            let p = PathBuf::from(&xdg_config).join("cortx").join("cortx.toml");
            if p.is_file() {
                return std::fs::canonicalize(&p).ok().or(Some(p));
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let user_path = PathBuf::from(home)
            .join(".config")
            .join("cortx")
            .join("cortx.toml");
        if user_path.is_file() {
            return std::fs::canonicalize(&user_path).ok().or(Some(user_path));
        }
    }
    let etc_cortx = PathBuf::from("/etc/cortx/cortx.toml");
    if etc_cortx.is_file() {
        return std::fs::canonicalize(&etc_cortx).ok().or(Some(etc_cortx));
    }

    None
}

/// Resolves the default system configuration file path for the active platform:
/// - Windows: `%APPDATA%\cortx\cortx.toml`
/// - macOS: `~/Library/Application Support/cortx/cortx.toml`
/// - Linux: `$XDG_CONFIG_HOME/cortx/cortx.toml` (default `~/.config/cortx/cortx.toml`)
pub fn resolve_default_config_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("cortx").join("cortx.toml");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("cortx")
                .join("cortx.toml");
        }
    }
    if let Some(xdg_config) = std::env::var_os("XDG_CONFIG_HOME") {
        if !xdg_config.is_empty() {
            return PathBuf::from(&xdg_config).join("cortx").join("cortx.toml");
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("cortx")
            .join("cortx.toml");
    }
    std::env::temp_dir().join("cortx").join("cortx.toml")
}

/// Alias for `resolve_default_config_path`.
pub fn resolve_global_config_path() -> PathBuf {
    resolve_default_config_path()
}

pub fn canonicalize_best_effort(path: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(path) {
        return c;
    }
    for ancestor in path.ancestors() {
        if let Ok(c_anc) = std::fs::canonicalize(ancestor) {
            if let Ok(rel) = path.strip_prefix(ancestor) {
                return c_anc.join(rel);
            }
        }
    }
    path.to_path_buf()
}

impl CortxConfig {
    /// Loads configuration from a specific file path.
    pub fn load(path: &Path) -> Result<Self, CoreError> {
        let content = std::fs::read_to_string(path).map_err(|source| CoreError::ConfigRead {
            path: path.to_path_buf(),
            source,
        })?;
        let mut config: Self =
            toml::from_str(&content).map_err(|source| CoreError::ConfigParse {
                path: path.to_path_buf(),
                source,
            })?;
        config.normalize_projects();
        Ok(config)
    }

    /// Normalizes top-level custom project sections into `self.projects`.
    pub fn normalize_projects(&mut self) {
        let reserved = [
            "general",
            "distill",
            "daemon",
            "typescript",
            "go",
            "cross_boundary",
            "projects",
        ];
        let dynamic = std::mem::take(&mut self.dynamic_projects);
        for (key, val) in dynamic {
            if !reserved.contains(&key.as_str()) {
                match val.try_into::<ProjectConfig>() {
                    Ok(mut proj_cfg) => {
                        proj_cfg.normalize();
                        self.projects.entry(key).or_insert(proj_cfg);
                    }
                    Err(err) => {
                        tracing::warn!(
                            project = %key,
                            error = %err,
                            "Ignoring malformed top-level project configuration section"
                        );
                    }
                }
            }
        }
        for proj in self.projects.values_mut() {
            proj.normalize();
        }
    }

    /// Finds the most specific declared project configuration that encompasses the given path
    /// using deterministic longest-prefix matching (with lexicographical project-name tie-breaking).
    pub fn find_project_for_path(&self, target_path: &Path) -> Option<(&str, &ProjectConfig)> {
        let canonical_target = canonicalize_best_effort(target_path);
        let mut best: Option<(usize, &str, &ProjectConfig)> = None;

        for (name, proj) in &self.projects {
            if proj.qualified_path.as_os_str().is_empty() {
                continue;
            }
            let canonical_proj = canonicalize_best_effort(&proj.qualified_path);
            let mut matched_len = if canonical_target.starts_with(&canonical_proj) {
                Some(canonical_proj.as_os_str().len())
            } else {
                None
            };

            if let Some(ref wt_path) = proj.custom_worktree_path {
                let full_wt_path = if wt_path.is_relative() {
                    canonical_proj.join(wt_path)
                } else {
                    wt_path.clone()
                };
                let canonical_wt = canonicalize_best_effort(&full_wt_path);
                if canonical_target.starts_with(&canonical_wt) {
                    let wt_len = canonical_wt.as_os_str().len();
                    matched_len = Some(matched_len.map_or(wt_len, |l| l.max(wt_len)));
                }
            }

            if let Some(len) = matched_len {
                let is_better = match best {
                    None => true,
                    Some((best_len, best_name, _)) => {
                        len > best_len || (len == best_len && name.as_str() < best_name)
                    }
                };
                if is_better {
                    best = Some((len, name.as_str(), proj));
                }
            }
        }
        best.map(|(_, name, proj)| (name, proj))
    }

    /// Loads configuration from an explicit custom path (`--config`), or falls back to
    /// the platform default system path (`cortx.toml`). If neither exists, returns default settings.
    pub fn load_or_default(
        custom_path: Option<&Path>,
    ) -> Result<(Self, ConfigProvenance), CoreError> {
        let (cfg, path) =
            Self::load_daemon_config_with_system(custom_path, resolve_system_config_path())?;
        let prov = match (custom_path, path) {
            (Some(_), Some(p)) => ConfigProvenance {
                custom_path: Some(p),
                system_path: None,
            },
            (None, Some(p)) => ConfigProvenance {
                custom_path: None,
                system_path: Some(p),
            },
            _ => ConfigProvenance::default(),
        };
        Ok((cfg, prov))
    }

    /// Loads configuration specifically for the daemon runtime.
    ///
    /// If `custom_path` is provided (via `--config`), loads directly from that file.
    /// Otherwise, loads strictly from the system path. Never loads or merges CWD configuration.
    pub fn load_daemon_config(
        custom_path: Option<&Path>,
    ) -> Result<(Self, Option<PathBuf>), CoreError> {
        Self::load_daemon_config_with_system(custom_path, resolve_system_config_path())
    }

    /// Loads configuration with an explicit custom path or explicit system path override.
    pub fn load_daemon_config_with_system(
        custom_path: Option<&Path>,
        system_path: Option<PathBuf>,
    ) -> Result<(Self, Option<PathBuf>), CoreError> {
        let (mut cfg, path) = if let Some(path) = custom_path {
            let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            let cfg = Self::load(&canonical)?;
            (cfg, Some(canonical))
        } else if let Some(sys_path) = system_path {
            let cfg = Self::load(&sys_path)?;
            (cfg, Some(sys_path))
        } else {
            (Self::default(), None)
        };

        cfg.normalize_projects();
        Ok((cfg, path))
    }

    /// Merges an overlay configuration into `self`.
    pub fn merge(&mut self, overlay: CortxConfig) {
        let def = CortxConfig::default();
        self.merge_general_and_distill(&overlay, &def);
        self.merge_daemon(&overlay, &def);
        self.merge_polyglot(&overlay, &def);
        for (name, mut proj) in overlay.projects {
            proj.normalize();
            self.projects.insert(name, proj);
        }
        for (name, val) in overlay.dynamic_projects {
            self.dynamic_projects.insert(name, val);
        }
        self.normalize_projects();
    }

    fn merge_general_and_distill(&mut self, overlay: &CortxConfig, def: &CortxConfig) {
        if overlay.general.codebase_scale != def.general.codebase_scale {
            self.general.codebase_scale = overlay.general.codebase_scale.clone();
        }
        if overlay.general.default_format != def.general.default_format {
            self.general.default_format = overlay.general.default_format;
        }
        if overlay.distill.max_call_depth != def.distill.max_call_depth {
            self.distill.max_call_depth = overlay.distill.max_call_depth;
        }
        if overlay.distill.strip_comments != def.distill.strip_comments {
            self.distill.strip_comments = overlay.distill.strip_comments;
        }
        if overlay.distill.include_private_types != def.distill.include_private_types {
            self.distill.include_private_types = overlay.distill.include_private_types;
        }
        if overlay.distill.omit_tests != def.distill.omit_tests {
            self.distill.omit_tests = overlay.distill.omit_tests;
        }
        if !overlay.distill.custom_ignored_dirs.is_empty() {
            self.distill
                .custom_ignored_dirs
                .extend(overlay.distill.custom_ignored_dirs.clone());
            self.distill.custom_ignored_dirs.sort();
            self.distill.custom_ignored_dirs.dedup();
        }
    }

    fn merge_daemon(&mut self, overlay: &CortxConfig, def: &CortxConfig) {
        if overlay.daemon.socket_path.is_some() {
            self.daemon.socket_path = overlay.daemon.socket_path.clone();
        }
        if overlay.daemon.system_daemon != def.daemon.system_daemon {
            self.daemon.system_daemon = overlay.daemon.system_daemon;
        }
        if overlay.daemon.lazy_indexing != def.daemon.lazy_indexing {
            self.daemon.lazy_indexing = overlay.daemon.lazy_indexing;
        }
        if overlay.daemon.global_socket.is_some() {
            self.daemon.global_socket = overlay.daemon.global_socket.clone();
        }
        if overlay.daemon.idle_ttl_secs != def.daemon.idle_ttl_secs {
            self.daemon.idle_ttl_secs = overlay.daemon.idle_ttl_secs;
        }
        if overlay.daemon.session_ttl_secs != def.daemon.session_ttl_secs {
            self.daemon.session_ttl_secs = overlay.daemon.session_ttl_secs;
        }
        if overlay.daemon.enable_file_watcher != def.daemon.enable_file_watcher {
            self.daemon.enable_file_watcher = overlay.daemon.enable_file_watcher;
        }
        if overlay.daemon.log_dir.is_some() {
            self.daemon.log_dir = overlay.daemon.log_dir.clone();
        }
        if overlay.daemon.workspace_logging != def.daemon.workspace_logging {
            self.daemon.workspace_logging = overlay.daemon.workspace_logging;
        }
    }

    fn merge_polyglot(&mut self, overlay: &CortxConfig, def: &CortxConfig) {
        if overlay.typescript.enabled != def.typescript.enabled {
            self.typescript.enabled = overlay.typescript.enabled;
        }
        if overlay.typescript.tsconfig_path != def.typescript.tsconfig_path {
            self.typescript.tsconfig_path = overlay.typescript.tsconfig_path.clone();
        }
        if overlay.typescript.extensions != def.typescript.extensions {
            self.typescript.extensions = overlay.typescript.extensions.clone();
        }
        if overlay.typescript.strip_jsx != def.typescript.strip_jsx {
            self.typescript.strip_jsx = overlay.typescript.strip_jsx;
        }
        if overlay.typescript.strip_hook_bodies != def.typescript.strip_hook_bodies {
            self.typescript.strip_hook_bodies = overlay.typescript.strip_hook_bodies;
        }
        if overlay.go.enabled != def.go.enabled {
            self.go.enabled = overlay.go.enabled;
        }
        if overlay.go.module_root != def.go.module_root {
            self.go.module_root = overlay.go.module_root.clone();
        }
        if overlay.go.extensions != def.go.extensions {
            self.go.extensions = overlay.go.extensions.clone();
        }
        if overlay.go.strip_function_bodies != def.go.strip_function_bodies {
            self.go.strip_function_bodies = overlay.go.strip_function_bodies;
        }
        if overlay.go.index_implicit_interfaces != def.go.index_implicit_interfaces {
            self.go.index_implicit_interfaces = overlay.go.index_implicit_interfaces;
        }
        if overlay.go.include_private_types != def.go.include_private_types {
            self.go.include_private_types = overlay.go.include_private_types;
        }
        if overlay.cross_boundary != def.cross_boundary {
            self.cross_boundary = overlay.cross_boundary.clone();
        }
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
