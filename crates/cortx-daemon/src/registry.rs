//! Multi-tenant workspace registry and single-flight lazy initialization coordinator.
//!
//! Tracks active repositories and linked Git worktrees, lazily indexing codebases
//! on demand on first touch, managing overlay indices for worktrees, and pruning
//! inactive workspaces after idle timeouts.

use crate::error::DaemonError;
use crate::logging::{DaemonLogManager, WorkspaceLogger};
use cortx_distill::SymbolIndex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::RwLock;

/// Active workspace or worktree entry in the daemon registry.
pub struct WorkspaceEntry {
    /// Canonical workspace root directory.
    pub root: PathBuf,
    /// Matched declarative project name from configuration, if any.
    pub project_name: Option<String>,
    /// Canonical path to the configuration file governing this workspace, if any.
    pub config_path: Option<PathBuf>,
    /// Resolved per-project configuration applied to this workspace.
    pub config: cortx_core::ProjectConfig,
    /// Whether this workspace is a linked Git worktree.
    pub is_worktree: bool,
    /// Parent repository root if this workspace is a worktree.
    pub parent_repo: Option<PathBuf>,
    /// In-memory symbol index (overlay index for worktrees, full index for standard repos).
    pub index: Arc<RwLock<SymbolIndex>>,
    /// Shared base repository symbol index if this entry is a worktree overlay.
    pub base_index: Option<Arc<RwLock<SymbolIndex>>>,
    /// Dedicated file logger for this workspace.
    pub logger: Arc<WorkspaceLogger>,
    /// Total successful advisory context submissions for this workspace.
    pub submissions: AtomicU64,
    /// Total times advisory call graph was pruned due to bloat.
    pub callgraph_drops: AtomicU64,
    /// Total times advisory context was completely omitted due to bloat.
    pub ast_drops: AtomicU64,
    /// Total macro reads (broad or unbounded file sweeps).
    pub macro_reads: AtomicU64,
    /// Total micro reads (focused inspections <= 150 lines).
    pub micro_reads: AtomicU64,
    /// Timestamp of last activity or hook evaluation.
    last_accessed: std::sync::Mutex<std::time::Instant>,
}

impl WorkspaceEntry {
    /// Creates a new workspace entry with initialized last accessed time.
    pub fn new(
        root: PathBuf,
        is_worktree: bool,
        parent_repo: Option<PathBuf>,
        index: Arc<RwLock<SymbolIndex>>,
        base_index: Option<Arc<RwLock<SymbolIndex>>>,
        logger: Arc<WorkspaceLogger>,
    ) -> Self {
        Self {
            root,
            project_name: None,
            config_path: None,
            config: cortx_core::ProjectConfig::default(),
            is_worktree,
            parent_repo,
            index,
            base_index,
            logger,
            submissions: AtomicU64::new(0),
            callgraph_drops: AtomicU64::new(0),
            ast_drops: AtomicU64::new(0),
            macro_reads: AtomicU64::new(0),
            micro_reads: AtomicU64::new(0),
            last_accessed: std::sync::Mutex::new(std::time::Instant::now()),
        }
    }

    /// Attaches resolved project metadata and provenance to this workspace entry.
    pub fn with_project_metadata(
        mut self,
        project_name: Option<String>,
        config_path: Option<PathBuf>,
        config: cortx_core::ProjectConfig,
    ) -> Self {
        self.project_name = project_name;
        self.config_path = config_path;
        self.config = config;
        self
    }

    /// Touches this workspace entry, resetting its idle duration timer.
    pub fn touch(&self) {
        if let Ok(mut lock) = self.last_accessed.lock() {
            *lock = std::time::Instant::now();
        }
    }

    /// Returns the elapsed time since this workspace was last accessed.
    pub fn idle_duration(&self) -> Duration {
        self.last_accessed
            .lock()
            .map(|t| t.elapsed())
            .unwrap_or_default()
    }

    /// Returns seconds since last access.
    pub fn last_accessed_secs_ago(&self) -> u64 {
        self.idle_duration().as_secs()
    }

    /// Estimates total in-memory heap footprint of the workspace's symbol index in Megabytes.
    pub async fn estimate_memory_mb(&self) -> f64 {
        self.index.read().await.estimate_memory_mb()
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
    pub fn record_read(&self, kind: cortx_core::ReadKind) {
        match kind {
            cortx_core::ReadKind::Macro => {
                self.macro_reads.fetch_add(1, Ordering::Relaxed);
            }
            cortx_core::ReadKind::Micro => {
                self.micro_reads.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Returns the total successful submissions count for this workspace.
    pub fn submissions(&self) -> u64 {
        self.submissions.load(Ordering::Relaxed)
    }

    /// Returns the total call graph pruning count for this workspace.
    pub fn callgraph_drops(&self) -> u64 {
        self.callgraph_drops.load(Ordering::Relaxed)
    }

    /// Returns the total AST context omission count for this workspace.
    pub fn ast_drops(&self) -> u64 {
        self.ast_drops.load(Ordering::Relaxed)
    }

    /// Returns the total macro reads count for this workspace.
    pub fn macro_reads(&self) -> u64 {
        self.macro_reads.load(Ordering::Relaxed)
    }

    /// Returns the total micro reads count for this workspace.
    pub fn micro_reads(&self) -> u64 {
        self.micro_reads.load(Ordering::Relaxed)
    }
}

/// Detects added or modified files in a worktree relative to its parent repository.
fn detect_worktree_modified_files(
    worktree_root: &Path,
    parent_repo: &Path,
    project: &cortx_core::ProjectConfig,
) -> Vec<PathBuf> {
    let dummy_idx = SymbolIndex::new();
    let wt_files = dummy_idx.collect_source_files_for_project(worktree_root, project);
    let mut modified = Vec::new();

    for file in wt_files {
        if let Ok(rel) = file.strip_prefix(worktree_root) {
            let parent_file = parent_repo.join(rel);
            if !parent_file.exists() {
                modified.push(file);
            } else {
                let wt_meta = std::fs::metadata(&file).ok();
                let p_meta = std::fs::metadata(&parent_file).ok();
                match (wt_meta, p_meta) {
                    (Some(wm), Some(pm)) => {
                        if wm.len() != pm.len() || wm.modified().ok() != pm.modified().ok() {
                            modified.push(file);
                        } else if let (Ok(c1), Ok(c2)) =
                            (std::fs::read(&file), std::fs::read(&parent_file))
                        {
                            if c1 != c2 {
                                modified.push(file);
                            }
                        }
                    }
                    _ => modified.push(file),
                }
            }
        }
    }
    modified
}

type InFlightMap =
    HashMap<PathBuf, tokio::sync::broadcast::Sender<Result<Arc<WorkspaceEntry>, String>>>;

/// Multi-tenant registry holding all loaded workspaces, coordinating single-flight lazy initialization.
#[derive(Clone)]
pub struct WorkspaceRegistry {
    entries: Arc<RwLock<HashMap<PathBuf, Arc<WorkspaceEntry>>>>,
    in_flight: Arc<tokio::sync::Mutex<InFlightMap>>,
    log_manager: Arc<DaemonLogManager>,
    daemon_config: cortx_core::CortxConfig,
    daemon_config_path: Option<PathBuf>,
}

impl WorkspaceRegistry {
    /// Creates a new workspace registry backed by the given log manager.
    pub fn new(log_manager: Arc<DaemonLogManager>) -> Self {
        Self::with_config(log_manager, cortx_core::CortxConfig::default(), None)
    }

    /// Creates a workspace registry configured with a loaded daemon configuration and its provenance path.
    pub fn with_config(
        log_manager: Arc<DaemonLogManager>,
        mut daemon_config: cortx_core::CortxConfig,
        daemon_config_path: Option<PathBuf>,
    ) -> Self {
        daemon_config.normalize_projects();
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
            in_flight: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            log_manager,
            daemon_config,
            daemon_config_path,
        }
    }

    /// Resolves `(project_name, project_config, config_path)` for a workspace root.
    /// Uses exclusively the daemon configuration loaded at startup (system path or `--config`),
    /// never reading or merging configuration files from workspace directories.
    pub fn resolve_project_for_root(
        &self,
        canonical_root: &Path,
        parent_repo: Option<&Path>,
    ) -> (Option<String>, cortx_core::ProjectConfig, Option<PathBuf>) {
        let direct = self.daemon_config.find_project_for_path(canonical_root);
        let via_parent = parent_repo.and_then(|p| self.daemon_config.find_project_for_path(p));
        let matched = match (direct, via_parent) {
            (Some(d), Some(p))
                if p.1.qualified_path.as_os_str().len() > d.1.qualified_path.as_os_str().len() =>
            {
                Some(p)
            }
            (Some(d), _) => Some(d),
            (None, Some(p)) => Some(p),
            (None, None) => None,
        };
        if let Some((name, proj)) = matched {
            return (
                Some(name.to_string()),
                proj.clone(),
                self.daemon_config_path.clone(),
            );
        }

        (
            None,
            cortx_core::ProjectConfig::from_cortx_config(
                canonical_root.to_path_buf(),
                &self.daemon_config,
            ),
            self.daemon_config_path.clone(),
        )
    }

    /// Retrieves an existing workspace entry or lazily indexes it on first touch (single-flight).
    pub async fn get_or_init(
        &self,
        target_path: &Path,
    ) -> Result<Arc<WorkspaceEntry>, DaemonError> {
        let raw_root = cortx_core::workspace::resolve_workspace_root(target_path);
        let canonical_root = std::fs::canonicalize(&raw_root).unwrap_or(raw_root);

        // 1. Fast path: check existing entries
        {
            let guard = self.entries.read().await;
            if let Some(entry) = guard.get(&canonical_root) {
                entry.touch();
                return Ok(entry.clone());
            }
        }

        // 2. Single-flight coordination
        let (rx, is_leader) = {
            let mut inflight = self.in_flight.lock().await;
            // Double check existing entries inside lock
            let guard = self.entries.read().await;
            if let Some(entry) = guard.get(&canonical_root) {
                entry.touch();
                return Ok(entry.clone());
            }

            if let Some(sender) = inflight.get(&canonical_root) {
                (Some(sender.subscribe()), false)
            } else {
                let (tx, _) = tokio::sync::broadcast::channel(1);
                inflight.insert(canonical_root.clone(), tx);
                (None, true)
            }
        };

        if !is_leader {
            if let Some(mut subscriber) = rx {
                match subscriber.recv().await {
                    Ok(Ok(entry)) => {
                        entry.touch();
                        return Ok(entry);
                    }
                    Ok(Err(err_msg)) => {
                        return Err(DaemonError::Protocol(format!(
                            "lazy init failed for '{}': {}",
                            canonical_root.display(),
                            err_msg
                        )));
                    }
                    Err(e) => {
                        return Err(DaemonError::Protocol(format!(
                            "single-flight coordination error: {}",
                            e
                        )));
                    }
                }
            }
        }

        // 3. Leader performs indexing
        let init_result = self.init_workspace(&canonical_root).await;

        // 4. Broadcast result and clean up in-flight map
        let mut inflight = self.in_flight.lock().await;
        let sender = inflight.remove(&canonical_root);

        match init_result {
            Ok(entry) => {
                self.entries
                    .write()
                    .await
                    .insert(canonical_root.clone(), entry.clone());
                if let Some(tx) = sender {
                    let _ = tx.send(Ok(entry.clone()));
                }
                Ok(entry)
            }
            Err(e) => {
                let err_str = e.to_string();
                if let Some(tx) = sender {
                    let _ = tx.send(Err(err_str));
                }
                Err(e)
            }
        }
    }

    /// Internal cold start initialization for a canonical workspace root.
    async fn init_workspace(
        &self,
        canonical_root: &Path,
    ) -> Result<Arc<WorkspaceEntry>, DaemonError> {
        let start_time = std::time::Instant::now();
        let parent_repo = cortx_core::workspace::detect_worktree_parent_repo(canonical_root);
        let is_worktree = parent_repo.is_some();
        let (project_name, project_config, config_path) =
            self.resolve_project_for_root(canonical_root, parent_repo.as_deref());

        let logger = Arc::new(self.log_manager.create_workspace_logger(canonical_root)?);
        let proj_label = project_name.as_deref().unwrap_or("<discovered>");
        let cfg_label = config_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "built-in defaults".to_string());
        self.log_manager.log_daemon(&format!(
            "Cold start indexing initiated for '{}' (is_worktree: {}, project: '{}', config: '{}')",
            canonical_root.display(),
            is_worktree,
            proj_label,
            cfg_label
        ));
        logger.log(&format!(
            "Initial indexing started for '{}' (project: '{}', config: '{}')",
            canonical_root.display(),
            proj_label,
            cfg_label
        ));

        let (index, base_index) = if let Some(ref parent) = parent_repo {
            // Ensure parent repo is initialized recursively
            let parent_entry = Box::pin(self.get_or_init(parent)).await?;
            let base = parent_entry.index.clone();

            let modified_files =
                detect_worktree_modified_files(canonical_root, parent, &project_config);
            let overlay = {
                let base_guard = base.read().await;
                base_guard.create_overlay(&modified_files)
            };

            logger.log(&format!(
                "Overlay indexed {} modified files against parent '{}' in {:?}",
                modified_files.len(),
                parent.display(),
                start_time.elapsed()
            ));

            (Arc::new(RwLock::new(overlay)), Some(base))
        } else {
            let mut idx = SymbolIndex::new();
            let indexed_files =
                idx.index_directory_with_project(canonical_root, &project_config)?;

            let metrics = idx.metrics();
            logger.log(&format!(
                "Indexed {} files ({} symbols) in {:?}",
                indexed_files,
                metrics.total_symbols_count,
                start_time.elapsed()
            ));

            (Arc::new(RwLock::new(idx)), None)
        };

        let entry = Arc::new(
            WorkspaceEntry::new(
                canonical_root.to_path_buf(),
                is_worktree,
                parent_repo,
                index,
                base_index,
                logger,
            )
            .with_project_metadata(project_name, config_path, project_config),
        );

        Ok(entry)
    }

    /// Fast lookup of an already indexed workspace without triggering cold start.
    pub async fn get(&self, target_path: &Path) -> Option<Arc<WorkspaceEntry>> {
        let raw_root = cortx_core::workspace::resolve_workspace_root(target_path);
        let canonical_root = std::fs::canonicalize(&raw_root).unwrap_or(raw_root);
        let guard = self.entries.read().await;
        guard.get(&canonical_root).cloned()
    }

    /// Returns snapshots of all currently registered workspaces.
    pub async fn entries(&self) -> Vec<Arc<WorkspaceEntry>> {
        let guard = self.entries.read().await;
        guard.values().cloned().collect()
    }

    /// Returns the number of currently active workspaces in the registry.
    pub async fn len(&self) -> usize {
        self.entries.read().await.len()
    }

    /// Returns true if no workspaces are currently registered.
    pub async fn is_empty(&self) -> bool {
        self.entries.read().await.is_empty()
    }

    /// Prunes workspaces that have been idle for longer than `max_idle` or no longer exist on disk.
    pub async fn prune_idle_workspaces(&self, max_idle: Duration) -> Vec<PathBuf> {
        let mut entries_guard = self.entries.write().await;
        let mut to_remove = Vec::new();

        for (path, entry) in entries_guard.iter() {
            if entry.idle_duration() > max_idle || !path.exists() {
                to_remove.push(path.clone());
            }
        }

        for path in &to_remove {
            if let Some(entry) = entries_guard.remove(path) {
                self.log_manager.log_daemon(&format!(
                    "Evicted idle workspace '{}' (idle for {:?})",
                    path.display(),
                    entry.idle_duration()
                ));
                entry.logger.log(&format!(
                    "Workspace index evicted due to inactivity (idle for {:?})",
                    entry.idle_duration()
                ));
            }
        }

        to_remove
    }

    /// Explicitly registers a pre-constructed workspace entry into the registry.
    pub async fn register(&self, entry: Arc<WorkspaceEntry>) {
        let canonical = std::fs::canonicalize(&entry.root).unwrap_or_else(|_| entry.root.clone());
        self.entries.write().await.insert(canonical, entry);
    }

    /// Removes an individual workspace from the registry explicitly.
    pub async fn remove(&self, root: &Path) -> Option<Arc<WorkspaceEntry>> {
        let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let mut guard = self.entries.write().await;
        guard.remove(&canonical)
    }

    /// Reference to the underlying log manager.
    pub fn log_manager(&self) -> &Arc<DaemonLogManager> {
        &self.log_manager
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_workspace_registry_cold_start() {
        let log_dir = tempdir().unwrap();
        let repo_dir = tempdir().unwrap();

        let src_dir = repo_dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(
            src_dir.join("lib.rs"),
            "pub struct Service; pub fn serve() {}",
        )
        .unwrap();

        let log_manager = Arc::new(DaemonLogManager::init(log_dir.path()).unwrap());
        let registry = WorkspaceRegistry::new(log_manager);

        let entry = registry
            .get_or_init(repo_dir.path())
            .await
            .expect("cold start should succeed");

        assert_eq!(entry.root, std::fs::canonicalize(repo_dir.path()).unwrap());
        assert!(!entry.is_worktree);
        assert!(entry.parent_repo.is_none());
        assert!(entry.base_index.is_none());

        let idx_guard = entry.index.read().await;
        assert!(idx_guard.symbols_map.contains_key("Service"));
        drop(idx_guard);

        assert!(entry.estimate_memory_mb().await > 0.0);
        assert_eq!(registry.len().await, 1);
    }

    #[tokio::test]
    async fn test_single_flight_deduplication() {
        let log_dir = tempdir().unwrap();
        let repo_dir = tempdir().unwrap();

        let src_dir = repo_dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("main.rs"), "pub fn main() {}").unwrap();

        let log_manager = Arc::new(DaemonLogManager::init(log_dir.path()).unwrap());
        let registry = WorkspaceRegistry::new(log_manager);

        let mut handles = Vec::new();
        for _ in 0..8 {
            let reg = registry.clone();
            let p = repo_dir.path().to_path_buf();
            handles.push(tokio::spawn(async move { reg.get_or_init(&p).await }));
        }

        for h in handles {
            let res = h.await.unwrap();
            assert!(res.is_ok());
            assert_eq!(
                res.unwrap().root,
                std::fs::canonicalize(repo_dir.path()).unwrap()
            );
        }

        assert_eq!(registry.len().await, 1);
    }

    #[tokio::test]
    async fn test_worktree_overlay_registration() {
        let log_dir = tempdir().unwrap();
        let parent_repo = tempdir().unwrap();

        let parent_git = parent_repo.path().join(".git");
        std::fs::create_dir_all(&parent_git).unwrap();
        let parent_src = parent_repo.path().join("src");
        std::fs::create_dir_all(&parent_src).unwrap();
        std::fs::write(
            parent_src.join("lib.rs"),
            "pub struct BaseEngine; pub fn run_base() {}",
        )
        .unwrap();

        // Create worktree directory structure
        let worktrees_git = parent_git.join("worktrees").join("feat1");
        std::fs::create_dir_all(&worktrees_git).unwrap();
        std::fs::write(worktrees_git.join("commondir"), "../..\n").unwrap();

        let worktree_dir = tempdir().unwrap();
        let wt_git_file = worktree_dir.path().join(".git");
        std::fs::write(
            &wt_git_file,
            format!("gitdir: {}\n", worktrees_git.display()),
        )
        .unwrap();

        let wt_src = worktree_dir.path().join("src");
        std::fs::create_dir_all(&wt_src).unwrap();
        std::fs::write(
            wt_src.join("lib.rs"),
            "pub struct BaseEngine; pub fn run_overlay() {}",
        )
        .unwrap();

        let log_manager = Arc::new(DaemonLogManager::init(log_dir.path()).unwrap());
        let registry = WorkspaceRegistry::new(log_manager);

        let wt_entry = registry
            .get_or_init(worktree_dir.path())
            .await
            .expect("worktree cold start should succeed");

        assert!(wt_entry.is_worktree);
        assert_eq!(
            wt_entry.parent_repo,
            Some(std::fs::canonicalize(parent_repo.path()).unwrap())
        );
        assert!(wt_entry.base_index.is_some());

        // Overlay should contain overlay function
        let wt_guard = wt_entry.index.read().await;
        assert!(wt_guard.symbols_map.contains_key("run_overlay"));
    }

    #[tokio::test]
    async fn test_prune_idle_workspaces() {
        let log_dir = tempdir().unwrap();
        let repo_dir = tempdir().unwrap();

        let src_dir = repo_dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("main.rs"), "pub fn main() {}").unwrap();

        let log_manager = Arc::new(DaemonLogManager::init(log_dir.path()).unwrap());
        let registry = WorkspaceRegistry::new(log_manager);

        let _ = registry.get_or_init(repo_dir.path()).await.unwrap();
        assert_eq!(registry.len().await, 1);

        tokio::time::sleep(Duration::from_millis(20)).await;

        let pruned = registry
            .prune_idle_workspaces(Duration::from_millis(10))
            .await;
        assert_eq!(pruned.len(), 1);
        assert_eq!(registry.len().await, 0);
    }

    #[tokio::test]
    async fn test_workspace_registry_with_declared_project_config() {
        let log_dir = tempdir().unwrap();
        let parent_repo = tempdir().unwrap();
        let canonical_parent = std::fs::canonicalize(parent_repo.path()).unwrap();

        let parent_git = canonical_parent.join(".git");
        std::fs::create_dir_all(&parent_git).unwrap();
        let parent_src = canonical_parent.join("src");
        std::fs::create_dir_all(&parent_src).unwrap();
        std::fs::write(parent_src.join("lib.rs"), "pub fn parent_fn() {}").unwrap();
        // Go file should be ignored when lang = ["rust"]
        std::fs::write(
            parent_src.join("main.go"),
            "package main\nfunc GoIgnored() {}",
        )
        .unwrap();

        let worktrees_git = parent_git.join("worktrees").join("agent-wt");
        std::fs::create_dir_all(&worktrees_git).unwrap();
        std::fs::write(worktrees_git.join("commondir"), "../..\n").unwrap();

        let nested_wt = canonical_parent
            .join(".claude")
            .join("worktrees")
            .join("agent-wt");
        std::fs::create_dir_all(nested_wt.join("src")).unwrap();
        std::fs::write(
            nested_wt.join(".git"),
            format!("gitdir: {}\n", worktrees_git.display()),
        )
        .unwrap();
        std::fs::write(
            nested_wt.join("src").join("lib.rs"),
            "pub fn nested_wt_fn() {}",
        )
        .unwrap();

        let mut daemon_cfg = cortx_core::CortxConfig::default();
        daemon_cfg.projects.insert(
            "acme-web".to_string(),
            cortx_core::ProjectConfig {
                qualified_path: canonical_parent.clone(),
                has_worktrees: true,
                custom_worktree_path: Some(PathBuf::from(".claude/worktrees")),
                lang: vec!["rust".to_string()],
                ignored_dirs: vec![".claude".to_string()],
                ..cortx_core::ProjectConfig::default()
            },
        );
        let cfg_path = PathBuf::from("/etc/cortx/cortx.toml");

        let log_manager = Arc::new(DaemonLogManager::init(log_dir.path()).unwrap());
        let registry =
            WorkspaceRegistry::with_config(log_manager, daemon_cfg, Some(cfg_path.clone()));

        let wt_entry = registry
            .get_or_init(&nested_wt)
            .await
            .expect("nested worktree should initialize");
        assert!(wt_entry.is_worktree);
        assert_eq!(wt_entry.project_name.as_deref(), Some("acme-web"));
        assert_eq!(wt_entry.config_path.as_ref(), Some(&cfg_path));

        let wt_guard = wt_entry.index.read().await;
        assert!(wt_guard.symbols_map.contains_key("nested_wt_fn"));
        drop(wt_guard);

        let parent_entry = registry.get(&canonical_parent).await.unwrap();
        assert_eq!(parent_entry.project_name.as_deref(), Some("acme-web"));
        let p_guard = parent_entry.index.read().await;
        assert!(p_guard.symbols_map.contains_key("parent_fn"));
        assert!(!p_guard.symbols_map.contains_key("GoIgnored"));
        assert!(!p_guard.symbols_map.contains_key("nested_wt_fn"));
        drop(p_guard);

        // Undeclared workspace with a local `.cortx.toml` must NOT load local config;
        // daemon uses strictly its own config (system path or --config).
        let undeclared_repo = tempdir().unwrap();
        std::fs::create_dir_all(undeclared_repo.path().join(".git")).unwrap();
        std::fs::write(
            undeclared_repo.path().join(".cortx.toml"),
            "[distill]\ncustom_ignored_dirs = [\"src\"]\n",
        )
        .unwrap();
        std::fs::create_dir_all(undeclared_repo.path().join("src")).unwrap();
        std::fs::write(
            undeclared_repo.path().join("src/lib.rs"),
            "pub fn undeclared_fn() {}",
        )
        .unwrap();

        let undeclared_entry = registry.get_or_init(undeclared_repo.path()).await.unwrap();
        assert!(undeclared_entry.project_name.is_none());
        assert_eq!(undeclared_entry.config_path.as_ref(), Some(&cfg_path));
        assert!(
            undeclared_entry
                .index
                .read()
                .await
                .symbols_map
                .contains_key("undeclared_fn")
        );
    }

    #[tokio::test]
    async fn test_workspace_entry_metrics_counters() {
        let log_dir = tempdir().unwrap();
        let log_manager = Arc::new(DaemonLogManager::init(log_dir.path()).unwrap());
        let logger = Arc::new(
            log_manager
                .create_workspace_logger(Path::new("/tmp/test-metrics"))
                .unwrap(),
        );
        let idx = Arc::new(RwLock::new(SymbolIndex::new()));

        let entry = WorkspaceEntry::new(
            PathBuf::from("/tmp/test-metrics"),
            false,
            None,
            idx,
            None,
            logger,
        );

        assert_eq!(entry.submissions(), 0);
        assert_eq!(entry.callgraph_drops(), 0);
        assert_eq!(entry.ast_drops(), 0);
        assert_eq!(entry.macro_reads(), 0);
        assert_eq!(entry.micro_reads(), 0);

        entry.record_submission();
        entry.record_submission();
        entry.record_callgraph_drop();
        entry.record_ast_drop();
        entry.record_read(cortx_core::ReadKind::Macro);
        entry.record_read(cortx_core::ReadKind::Micro);
        entry.record_read(cortx_core::ReadKind::Micro);

        assert_eq!(entry.submissions(), 2);
        assert_eq!(entry.callgraph_drops(), 1);
        assert_eq!(entry.ast_drops(), 1);
        assert_eq!(entry.macro_reads(), 1);
        assert_eq!(entry.micro_reads(), 2);
    }
}
