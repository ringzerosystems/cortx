//! Incremental file watcher for hot cache updates.
//!
//! Provides a thread-safe [`DynamicWatcherManager`] supporting dynamic registration
//! and unregistration of multiple workspace roots, selective event dispatch,
//! and per-workspace mutation logging.

use crate::error::DaemonError;
use crate::logging::WorkspaceLogger;
use cortx_core::canonicalize_best_effort;
use cortx_distill::indexer::WatchBatchUpdate;
use cortx_distill::{SymbolIndex, WorkspaceManifests};
use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::RwLock as TokioRwLock;
use tokio_util::sync::CancellationToken;

/// Raw filesystem mutation or eviction event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawWatchEvent {
    /// File created or modified.
    Upsert(PathBuf),
    /// File or directory deleted or renamed away.
    Remove(PathBuf),
}

/// Workspace entry registered with the dynamic file watcher.
#[derive(Clone)]
struct WatchedWorkspace {
    index: Arc<TokioRwLock<SymbolIndex>>,
    shadowed_files: Arc<TokioRwLock<HashSet<PathBuf>>>,
    logger: Arc<WorkspaceLogger>,
    config: cortx_core::ProjectConfig,
}

/// Shared internal state for the dynamic file watcher.
pub struct DynamicWatcherInner {
    watcher: Mutex<notify::RecommendedWatcher>,
    workspaces: RwLock<HashMap<PathBuf, WatchedWorkspace>>,
}

/// Thread-safe manager that coordinates filesystem watches across multiple active workspaces.
#[derive(Clone)]
pub struct DynamicWatcherManager {
    inner: Arc<DynamicWatcherInner>,
    token: CancellationToken,
}

impl DynamicWatcherManager {
    /// Initializes a new dynamic watcher manager supervised by the provided cancellation token.
    pub fn new(token: CancellationToken) -> Result<Self, DaemonError> {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<RawWatchEvent>(2048);

        let watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                match event.kind {
                    EventKind::Remove(_) => {
                        for path in event.paths {
                            let _ = tx.blocking_send(RawWatchEvent::Remove(path));
                        }
                    }
                    EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                        for path in event.paths {
                            let _ = tx.blocking_send(RawWatchEvent::Remove(path));
                        }
                    }
                    EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
                        for path in event.paths {
                            if Self::is_path_relevant_for_upsert(&path) {
                                let _ = tx.blocking_send(RawWatchEvent::Upsert(path));
                            }
                        }
                    }
                    EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
                        if event.paths.len() >= 2 {
                            let _ = tx.blocking_send(RawWatchEvent::Remove(event.paths[0].clone()));
                            if Self::is_path_relevant_for_upsert(&event.paths[1]) {
                                let _ =
                                    tx.blocking_send(RawWatchEvent::Upsert(event.paths[1].clone()));
                            }
                        } else {
                            for path in event.paths {
                                let _ = tx.blocking_send(RawWatchEvent::Remove(path));
                            }
                        }
                    }
                    EventKind::Create(_) | EventKind::Modify(_) => {
                        for path in event.paths {
                            if Self::is_path_relevant_for_upsert(&path) {
                                let _ = tx.blocking_send(RawWatchEvent::Upsert(path));
                            }
                        }
                    }
                    _ => {}
                }
            }
        })
        .map_err(|e| DaemonError::Watcher(e.to_string()))?;

        let inner = Arc::new(DynamicWatcherInner {
            watcher: Mutex::new(watcher),
            workspaces: RwLock::new(HashMap::new()),
        });

        let weak_inner = Arc::downgrade(&inner);
        let cancel_token = token.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel_token.cancelled() => {
                        break;
                    }
                    first_event = rx.recv() => {
                        let Some(first) = first_event else {
                            break;
                        };
                        let mut batch = vec![first];
                        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(60);

                        loop {
                            tokio::select! {
                                _ = cancel_token.cancelled() => {
                                    break;
                                }
                                _ = tokio::time::sleep_until(deadline) => {
                                    break;
                                }
                                next_event = rx.recv() => {
                                    if let Some(event) = next_event {
                                        batch.push(event);
                                        if batch.len() >= 2048 {
                                            break;
                                        }
                                    } else {
                                        break;
                                    }
                                }
                            }
                        }

                        let inner = match weak_inner.upgrade() {
                            Some(i) => i,
                            None => break,
                        };
                        Self::dispatch_batch(&inner, batch).await;
                    }
                }
            }
        });

        Ok(Self { inner, token })
    }

    fn is_path_relevant_for_upsert(path: &Path) -> bool {
        WorkspaceManifests::is_manifest_path(path)
            || path.is_dir()
            || path.extension().is_some_and(|ext| {
                matches!(
                    ext.to_str().unwrap_or(""),
                    "rs" | "ts" | "tsx" | "js" | "jsx" | "go"
                )
            })
    }

    fn find_best_workspace(
        inner: &DynamicWatcherInner,
        canonical_path: &Path,
    ) -> Option<(PathBuf, WatchedWorkspace)> {
        let ws_guard = inner.workspaces.read().ok()?;
        let mut best_match: Option<(PathBuf, WatchedWorkspace)> = None;
        for (root, ws) in ws_guard.iter() {
            if canonical_path.starts_with(root) {
                match &best_match {
                    None => best_match = Some((root.clone(), ws.clone())),
                    Some((best_root, _)) => {
                        if root.as_os_str().len() > best_root.as_os_str().len() {
                            best_match = Some((root.clone(), ws.clone()));
                        }
                    }
                }
            }
        }
        best_match
    }

    fn is_nested_worktree_mutation(
        target_root: &Path,
        canonical_path: &Path,
        config: &cortx_core::ProjectConfig,
    ) -> bool {
        if let Some(parent) = canonical_path.parent() {
            for anc in parent.ancestors() {
                if anc == target_root || !anc.starts_with(target_root) {
                    break;
                }
                if anc.join(".git").exists() {
                    return true;
                }
            }
        }
        if !target_root.join(".git").is_file() {
            if let Ok(rel) = canonical_path.strip_prefix(target_root) {
                if rel
                    .components()
                    .any(|c| c.as_os_str() == "worktrees" || c.as_os_str() == ".worktrees")
                {
                    return true;
                }
                if let Some(wt_rel) = config.relative_custom_worktree_path(target_root) {
                    if rel.starts_with(wt_rel) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Dispatches an observed file modification event to the appropriate matching workspace index.
    pub async fn dispatch_mutation(inner: &DynamicWatcherInner, path: PathBuf) {
        let event = if path.exists() {
            RawWatchEvent::Upsert(path)
        } else {
            RawWatchEvent::Remove(path)
        };
        Self::dispatch_batch(inner, vec![event]).await;
    }

    /// Dispatches a single raw watch event.
    pub async fn dispatch_event(inner: &DynamicWatcherInner, event: RawWatchEvent) {
        Self::dispatch_batch(inner, vec![event]).await;
    }

    /// Dispatches a coalesced batch of raw watch events across matching registered workspaces.
    pub async fn dispatch_batch(inner: &DynamicWatcherInner, batch: Vec<RawWatchEvent>) {
        if batch.is_empty() {
            return;
        }

        let grouped = Self::group_events_by_workspace(inner, batch);
        for (target_root, (target_ws, upserted, removed)) in grouped {
            Self::apply_workspace_batch(target_root, target_ws, upserted, removed).await;
        }
    }

    fn group_events_by_workspace(
        inner: &DynamicWatcherInner,
        batch: Vec<RawWatchEvent>,
    ) -> HashMap<PathBuf, (WatchedWorkspace, Vec<PathBuf>, Vec<PathBuf>)> {
        let mut grouped: HashMap<PathBuf, (WatchedWorkspace, Vec<PathBuf>, Vec<PathBuf>)> =
            HashMap::new();

        for event in batch {
            let raw_path = match &event {
                RawWatchEvent::Upsert(p) | RawWatchEvent::Remove(p) => p,
            };
            let canonical_path = canonicalize_best_effort(raw_path);
            let Some((target_root, target_ws)) = Self::find_best_workspace(inner, &canonical_path)
            else {
                continue;
            };

            if Self::is_nested_worktree_mutation(&target_root, &canonical_path, &target_ws.config) {
                tracing::debug!(
                    path = %canonical_path.display(),
                    "Dropped nested worktree edit from parent workspace index"
                );
                continue;
            }

            if let Ok(rel) = canonical_path.strip_prefix(&target_root) {
                if target_ws.config.is_relative_path_ignored(rel) {
                    continue;
                }
            }

            let entry = grouped
                .entry(target_root.clone())
                .or_insert_with(|| (target_ws.clone(), Vec::new(), Vec::new()));

            match event {
                RawWatchEvent::Remove(_) => {
                    entry.2.push(canonical_path);
                }
                RawWatchEvent::Upsert(_) => {
                    if !canonical_path.exists() {
                        entry.2.push(canonical_path);
                    } else {
                        Self::route_upsert_path(
                            &target_root,
                            &canonical_path,
                            &target_ws,
                            &mut entry.1,
                        );
                    }
                }
            }
        }

        grouped
    }

    fn route_upsert_path(
        target_root: &Path,
        canonical_path: &Path,
        target_ws: &WatchedWorkspace,
        upserted: &mut Vec<PathBuf>,
    ) {
        if let Ok(rel) = canonical_path.strip_prefix(target_root) {
            if !WorkspaceManifests::is_manifest_path(canonical_path)
                && !target_ws.config.is_file_in_watch_paths(rel)
            {
                return;
            }
        }

        if canonical_path.is_dir() {
            let files = cortx_distill::SymbolIndex::new()
                .collect_source_files_for_project(canonical_path, &target_ws.config);
            upserted.extend(files);
        } else {
            let is_manifest = WorkspaceManifests::is_manifest_path(canonical_path);
            let ext_enabled = canonical_path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| target_ws.config.is_extension_enabled(ext));
            if is_manifest || ext_enabled {
                upserted.push(canonical_path.to_path_buf());
            }
        }
    }

    async fn apply_workspace_batch(
        target_root: PathBuf,
        target_ws: WatchedWorkspace,
        mut upserted: Vec<PathBuf>,
        mut removed: Vec<PathBuf>,
    ) {
        upserted.sort_unstable();
        upserted.dedup();
        removed.sort_unstable();
        removed.dedup();

        if upserted.is_empty() && removed.is_empty() {
            return;
        }

        let (mut snapshot, shadowed_files_lock, logger, config) = {
            let guard = target_ws.index.read().await;
            (
                guard.clone(),
                Arc::clone(&target_ws.shadowed_files),
                Arc::clone(&target_ws.logger),
                target_ws.config.clone(),
            )
        };

        if snapshot.manifests.workspace_root == Path::new(".") && target_root != Path::new(".") {
            snapshot.manifests = Arc::new(WorkspaceManifests::discover(
                &target_root,
                &config.ignored_dirs,
            ));
        }

        let start = std::time::Instant::now();
        let upserted_cloned = upserted.clone();
        let removed_cloned = removed.clone();
        let config_cloned = config.clone();

        let outcome = tokio::task::spawn_blocking(move || {
            snapshot.apply_watch_batch(WatchBatchUpdate {
                upserted: &upserted_cloned,
                removed: &removed_cloned,
                project_cfg: Some(&config_cloned),
            })
        })
        .await;

        if let Ok(outcome) = outcome {
            {
                let mut sf = shadowed_files_lock.write().await;
                for rem in &removed {
                    if let Ok(rel) = rem.strip_prefix(&target_root) {
                        sf.insert(rel.to_path_buf());
                    }
                }
                for ups in &upserted {
                    if let Ok(rel) = ups.strip_prefix(&target_root) {
                        sf.insert(rel.to_path_buf());
                    }
                }
            }

            {
                let mut guard = target_ws.index.write().await;
                *guard = outcome.next_index;
            }

            let elapsed_ms = start.elapsed().as_millis();
            tracing::debug!(
                root = %target_root.display(),
                upserted = upserted.len(),
                removed = removed.len(),
                elapsed_ms,
                "Symbol index updated via coalesced watcher batch"
            );

            for ups in &upserted {
                let rel_display = ups.strip_prefix(&target_root).unwrap_or(ups).display();
                logger.log(&format!(
                    "[DEBUG] File modified: '{}' -> updated symbol index in {}ms",
                    rel_display, elapsed_ms
                ));
            }
            for rem in &removed {
                let rel_display = rem.strip_prefix(&target_root).unwrap_or(rem).display();
                logger.log(&format!(
                    "[DEBUG] File removed: '{}' -> updated symbol index in {}ms",
                    rel_display, elapsed_ms
                ));
            }
        }
    }

    /// Dynamically registers a workspace root for recursive file monitoring using default project rules.
    pub fn watch_workspace(
        &self,
        root: PathBuf,
        index: Arc<TokioRwLock<SymbolIndex>>,
        logger: Arc<WorkspaceLogger>,
    ) -> Result<(), DaemonError> {
        let config = cortx_core::ProjectConfig {
            qualified_path: root.clone(),
            ..cortx_core::ProjectConfig::default()
        };
        self.watch_workspace_with_config(root, index, logger, config)
    }

    /// Dynamically registers a workspace root for recursive file monitoring with explicit `ProjectConfig`.
    pub fn watch_workspace_with_config(
        &self,
        root: PathBuf,
        index: Arc<TokioRwLock<SymbolIndex>>,
        logger: Arc<WorkspaceLogger>,
        config: cortx_core::ProjectConfig,
    ) -> Result<(), DaemonError> {
        self.watch_workspace_with_shadowed_files(
            root,
            index,
            Arc::new(TokioRwLock::new(HashSet::new())),
            logger,
            config,
        )
    }

    /// Dynamically registers a workspace root with an explicit `shadowed_files` tombstone set.
    pub fn watch_workspace_with_shadowed_files(
        &self,
        root: PathBuf,
        index: Arc<TokioRwLock<SymbolIndex>>,
        shadowed_files: Arc<TokioRwLock<HashSet<PathBuf>>>,
        logger: Arc<WorkspaceLogger>,
        config: cortx_core::ProjectConfig,
    ) -> Result<(), DaemonError> {
        let canonical_root = std::fs::canonicalize(&root).unwrap_or(root);

        let already_watched = {
            let ws_guard = self
                .inner
                .workspaces
                .read()
                .map_err(|e| DaemonError::Watcher(e.to_string()))?;
            ws_guard.contains_key(&canonical_root)
        };

        if !already_watched {
            let mut watcher_guard = self
                .inner
                .watcher
                .lock()
                .map_err(|e| DaemonError::Watcher(e.to_string()))?;
            watcher_guard
                .watch(&canonical_root, RecursiveMode::Recursive)
                .map_err(|e| DaemonError::Watcher(e.to_string()))?;
        }

        {
            let mut ws_guard = self
                .inner
                .workspaces
                .write()
                .map_err(|e| DaemonError::Watcher(e.to_string()))?;
            ws_guard.insert(
                canonical_root.clone(),
                WatchedWorkspace {
                    index,
                    shadowed_files,
                    logger,
                    config,
                },
            );
        }

        tracing::info!(root = %canonical_root.display(), "Workspace registered for file watching");
        Ok(())
    }

    /// Returns the shadowed files set for a given workspace root, if registered.
    pub fn shadowed_files(&self, root: &Path) -> Option<Arc<TokioRwLock<HashSet<PathBuf>>>> {
        let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        self.inner
            .workspaces
            .read()
            .ok()?
            .get(&canonical_root)
            .map(|w| Arc::clone(&w.shadowed_files))
    }

    /// Dynamically unregisters a workspace root and detaches its filesystem watch.
    pub fn unwatch_workspace(&self, root: &Path) -> Result<(), DaemonError> {
        let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        {
            let mut watcher_guard = self
                .inner
                .watcher
                .lock()
                .map_err(|e| DaemonError::Watcher(e.to_string()))?;
            if let Err(e) = watcher_guard.unwatch(&canonical_root) {
                tracing::debug!(root = %canonical_root.display(), error = %e, "OS unwatch notice");
            }
        }
        {
            let mut ws_guard = self
                .inner
                .workspaces
                .write()
                .map_err(|e| DaemonError::Watcher(e.to_string()))?;
            ws_guard.remove(&canonical_root);
        }
        tracing::info!(root = %canonical_root.display(), "Workspace unwatched");
        Ok(())
    }

    /// Checks whether a given directory is currently being actively watched.
    pub fn is_watching(&self, root: &Path) -> bool {
        let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        self.inner
            .workspaces
            .read()
            .map(|w| w.contains_key(&canonical_root))
            .unwrap_or(false)
    }

    /// Returns the count of currently watched workspaces.
    pub fn watched_count(&self) -> usize {
        self.inner.workspaces.read().map(|w| w.len()).unwrap_or(0)
    }

    /// Returns the list of currently registered workspace roots.
    pub fn watched_roots(&self) -> Vec<PathBuf> {
        self.inner
            .workspaces
            .read()
            .map(|w| w.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Returns a reference to the cancellation token supervising this watcher.
    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.token
    }

    /// Signals cancellation to the watcher background task.
    pub fn cancel(&self) {
        self.token.cancel();
    }
}

/// Watches target directory for source file changes and updates index incrementally (legacy entrypoint).
pub fn start_watcher(
    watch_dir: PathBuf,
    index: Arc<TokioRwLock<SymbolIndex>>,
    token: CancellationToken,
) -> Result<(), DaemonError> {
    let manager = DynamicWatcherManager::new(token.clone())?;
    let logger = Arc::new(WorkspaceLogger::noop(watch_dir.clone()));
    manager.watch_workspace(watch_dir, index, logger)?;

    tokio::spawn(async move {
        token.cancelled().await;
        drop(manager);
    });

    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::logging::DaemonLogManager;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_watcher_ignores_dependency_dirs() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let src_dir = root.join("src");
        let nm_dir = root.join("node_modules").join("pkg");
        let target_dir = root.join("target").join("debug");

        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&nm_dir).unwrap();
        std::fs::create_dir_all(&target_dir).unwrap();

        let index = Arc::new(TokioRwLock::new(SymbolIndex::new()));
        let token = CancellationToken::new();

        start_watcher(root.to_path_buf(), Arc::clone(&index), token.clone()).unwrap();

        // Brief delay for OS filesystem watcher registration
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Write files to ignored directories
        std::fs::write(nm_dir.join("index.js"), "function ignoredJs() {}").unwrap();
        std::fs::write(target_dir.join("gen.rs"), "pub fn ignoredRust() {}").unwrap();

        // Write file to valid source directory
        std::fs::write(src_dir.join("main.rs"), "pub fn validMain() {}").unwrap();

        // Poll for valid symbol update with timeout
        let start = std::time::Instant::now();
        let mut found = false;
        while start.elapsed() < std::time::Duration::from_secs(3) {
            {
                let guard = index.read().await;
                if guard.symbols_map.contains_key("validMain") {
                    found = true;
                    assert!(!guard.symbols_map.contains_key("ignoredJs"));
                    assert!(!guard.symbols_map.contains_key("ignoredRust"));
                    break;
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }

        token.cancel();
        assert!(
            found,
            "watcher should have indexed validMain while ignoring node_modules and target"
        );
    }

    #[tokio::test]
    async fn test_dynamic_watcher_registration_and_logging() {
        let log_tmp = tempdir().unwrap();
        let log_manager = Arc::new(DaemonLogManager::init(log_tmp.path()).unwrap());

        let dir_a = tempdir().unwrap();
        let src_a = dir_a.path().join("src");
        std::fs::create_dir_all(&src_a).unwrap();
        std::fs::write(src_a.join("lib.rs"), "pub fn alpha_init() {}").unwrap();

        let dir_b = tempdir().unwrap();
        let src_b = dir_b.path().join("src");
        std::fs::create_dir_all(&src_b).unwrap();
        std::fs::write(src_b.join("lib.rs"), "pub fn beta_init() {}").unwrap();

        let token = CancellationToken::new();
        let manager = DynamicWatcherManager::new(token.clone()).unwrap();

        let idx_a = Arc::new(TokioRwLock::new(SymbolIndex::new()));
        let logger_a = Arc::new(log_manager.create_workspace_logger(dir_a.path()).unwrap());
        manager
            .watch_workspace(
                dir_a.path().to_path_buf(),
                Arc::clone(&idx_a),
                logger_a.clone(),
            )
            .unwrap();

        let idx_b = Arc::new(TokioRwLock::new(SymbolIndex::new()));
        let logger_b = Arc::new(log_manager.create_workspace_logger(dir_b.path()).unwrap());
        manager
            .watch_workspace(dir_b.path().to_path_buf(), Arc::clone(&idx_b), logger_b)
            .unwrap();

        assert_eq!(manager.watched_count(), 2);
        assert!(manager.is_watching(dir_a.path()));
        assert!(manager.is_watching(dir_b.path()));

        // Settle watcher
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Mutate alpha
        std::fs::write(src_a.join("lib.rs"), "pub fn alpha_modified() {}").unwrap();

        let start = std::time::Instant::now();
        let mut updated = false;
        while start.elapsed() < std::time::Duration::from_secs(3) {
            {
                let guard = idx_a.read().await;
                if guard.symbols_map.contains_key("alpha_modified") {
                    updated = true;
                    break;
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }

        assert!(updated, "alpha index should receive updated symbol");

        // Verify isolated log file in logger_a
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        let log_content = std::fs::read_to_string(logger_a.log_path()).unwrap_or_default();
        assert!(
            log_content.contains("File modified:"),
            "logger_a log file must contain mutation event, got: {}",
            log_content
        );

        token.cancel();
    }

    #[tokio::test]
    async fn test_dynamic_watcher_unregistration() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "pub fn original() {}").unwrap();

        let token = CancellationToken::new();
        let manager = DynamicWatcherManager::new(token.clone()).unwrap();

        let idx = Arc::new(TokioRwLock::new(SymbolIndex::new()));
        let logger = Arc::new(WorkspaceLogger::noop(dir.path().to_path_buf()));

        manager
            .watch_workspace(dir.path().to_path_buf(), Arc::clone(&idx), logger)
            .unwrap();
        assert!(manager.is_watching(dir.path()));
        assert_eq!(manager.watched_count(), 1);

        manager.unwatch_workspace(dir.path()).unwrap();
        assert!(!manager.is_watching(dir.path()));
        assert_eq!(manager.watched_count(), 0);

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Write new file after unwatch
        std::fs::write(src.join("lib.rs"), "pub fn unwatched_symbol() {}").unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        let guard = idx.read().await;
        assert!(
            !guard.symbols_map.contains_key("unwatched_symbol"),
            "unwatched workspace index should not update"
        );

        token.cancel();
    }

    #[tokio::test]
    async fn test_watcher_worktree_carve_out_and_relative_ignores() {
        let dir = tempdir().unwrap();
        let parent_root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(parent_root.join(".git")).unwrap();
        std::fs::create_dir_all(parent_root.join("src")).unwrap();

        let registered_wt = parent_root
            .join(".claude")
            .join("worktrees")
            .join("agent-auth");
        std::fs::create_dir_all(registered_wt.join("src")).unwrap();
        std::fs::create_dir_all(registered_wt.join(".claude")).unwrap();
        std::fs::write(registered_wt.join(".git"), "gitdir: ../../../.git\n").unwrap();

        let unregistered_wt = parent_root
            .join(".claude")
            .join("worktrees")
            .join("unregistered-wt");
        std::fs::create_dir_all(unregistered_wt.join("src")).unwrap();
        std::fs::write(unregistered_wt.join(".git"), "gitdir: ../../../.git\n").unwrap();

        let proj_cfg = cortx_core::ProjectConfig {
            qualified_path: parent_root.clone(),
            has_worktrees: true,
            custom_worktree_path: Some(PathBuf::from(".claude/worktrees")),
            lang: vec!["rust".to_string()],
            watch_paths: vec!["src".to_string()],
            ignored_dirs: vec![".claude".to_string()],
            ..cortx_core::ProjectConfig::default()
        };

        let token = CancellationToken::new();
        let manager = DynamicWatcherManager::new(token.clone()).unwrap();

        let parent_idx = Arc::new(TokioRwLock::new(SymbolIndex::new()));
        let parent_logger = Arc::new(WorkspaceLogger::noop(parent_root.clone()));
        manager
            .watch_workspace_with_config(
                parent_root.clone(),
                Arc::clone(&parent_idx),
                parent_logger,
                proj_cfg.clone(),
            )
            .unwrap();

        let wt_idx = Arc::new(TokioRwLock::new(SymbolIndex::new()));
        let wt_logger = Arc::new(WorkspaceLogger::noop(registered_wt.clone()));
        manager
            .watch_workspace_with_config(
                registered_wt.clone(),
                Arc::clone(&wt_idx),
                wt_logger,
                proj_cfg,
            )
            .unwrap();

        // 1. Edit in unregistered worktree must be carved out from parent index
        let unreg_file = unregistered_wt.join("src").join("lib.rs");
        std::fs::write(&unreg_file, "pub fn unreg_leak() {}").unwrap();
        DynamicWatcherManager::dispatch_mutation(&manager.inner, unreg_file).await;
        assert!(
            !parent_idx
                .read()
                .await
                .symbols_map
                .contains_key("unreg_leak")
        );

        // 2. Edit inside registered worktree's internal .claude/ dir must be ignored
        let wt_internal_ignored = registered_wt.join(".claude").join("cache.rs");
        std::fs::write(&wt_internal_ignored, "pub fn internal_claude_ignored() {}").unwrap();
        DynamicWatcherManager::dispatch_mutation(&manager.inner, wt_internal_ignored).await;
        assert!(
            !wt_idx
                .read()
                .await
                .symbols_map
                .contains_key("internal_claude_ignored")
        );

        // 3. Valid edit inside registered worktree (.claude/worktrees/agent-auth/src/lib.rs)
        // must update wt_idx and NOT parent_idx
        let wt_valid_file = registered_wt.join("src").join("lib.rs");
        std::fs::write(&wt_valid_file, "pub fn wt_live_updated() {}").unwrap();
        DynamicWatcherManager::dispatch_mutation(&manager.inner, wt_valid_file).await;
        assert!(
            wt_idx
                .read()
                .await
                .symbols_map
                .contains_key("wt_live_updated")
        );
        assert!(
            !parent_idx
                .read()
                .await
                .symbols_map
                .contains_key("wt_live_updated")
        );

        // 4. Edit outside configured watch_paths = ["src"] must be ignored
        let unwatched_dir = parent_root.join("unwatched");
        std::fs::create_dir_all(&unwatched_dir).unwrap();
        let unwatched_file = unwatched_dir.join("extra.rs");
        std::fs::write(&unwatched_file, "pub fn unwatched_outside_src() {}").unwrap();
        DynamicWatcherManager::dispatch_mutation(&manager.inner, unwatched_file).await;
        assert!(
            !parent_idx
                .read()
                .await
                .symbols_map
                .contains_key("unwatched_outside_src")
        );

        token.cancel();
    }

    #[tokio::test]
    async fn test_watcher_unlink_eviction() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();

        let main_file = src.join("main.rs");
        let helper_file = src.join("helper.rs");
        std::fs::write(&main_file, "pub fn main_runner() {}").unwrap();
        std::fs::write(&helper_file, "pub fn helper_tool() {}").unwrap();

        let mut index = SymbolIndex::new();
        index.index_directory(&root).unwrap();
        assert!(index.symbols_map.contains_key("helper_tool"));

        let idx = Arc::new(TokioRwLock::new(index));
        let token = CancellationToken::new();
        let manager = DynamicWatcherManager::new(token.clone()).unwrap();
        let logger = Arc::new(WorkspaceLogger::noop(root.clone()));

        manager
            .watch_workspace(root.clone(), Arc::clone(&idx), logger)
            .unwrap();

        // Unlink helper.rs
        std::fs::remove_file(&helper_file).unwrap();
        DynamicWatcherManager::dispatch_event(&manager.inner, RawWatchEvent::Remove(helper_file))
            .await;

        let guard = idx.read().await;
        assert!(
            !guard.symbols_map.contains_key("helper_tool"),
            "helper_tool should be evicted after unlink"
        );
        assert!(
            guard.symbols_map.contains_key("main_runner"),
            "main_runner should remain"
        );
        token.cancel();
    }

    #[tokio::test]
    async fn test_watcher_rm_rf_directory_prefix_eviction() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let src = root.join("src");
        let sub = src.join("features");
        std::fs::create_dir_all(&sub).unwrap();

        let main_file = src.join("main.rs");
        let feat_file = sub.join("feat.rs");
        std::fs::write(&main_file, "pub fn main_app() {}").unwrap();
        std::fs::write(&feat_file, "pub fn feature_sub() {}").unwrap();

        let mut index = SymbolIndex::new();
        index.index_directory(&root).unwrap();
        assert!(index.symbols_map.contains_key("feature_sub"));

        let idx = Arc::new(TokioRwLock::new(index));
        let token = CancellationToken::new();
        let manager = DynamicWatcherManager::new(token.clone()).unwrap();
        let logger = Arc::new(WorkspaceLogger::noop(root.clone()));

        manager
            .watch_workspace(root.clone(), Arc::clone(&idx), logger)
            .unwrap();

        // rm -rf features
        std::fs::remove_dir_all(&sub).unwrap();
        DynamicWatcherManager::dispatch_event(&manager.inner, RawWatchEvent::Remove(sub)).await;

        let guard = idx.read().await;
        assert!(
            !guard.symbols_map.contains_key("feature_sub"),
            "feature_sub should be evicted after rm -rf directory"
        );
        assert!(
            guard.symbols_map.contains_key("main_app"),
            "main_app should remain"
        );
        token.cancel();
    }

    #[tokio::test]
    async fn test_watcher_worktree_deletion_tombstones() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();

        let lib_file = src.join("lib.rs");
        let deleted_file = src.join("deleted.rs");
        std::fs::write(&lib_file, "pub fn live_lib() {}").unwrap();
        std::fs::write(&deleted_file, "pub fn to_delete() {}").unwrap();

        let mut index = SymbolIndex::new();
        index.index_directory(&root).unwrap();

        let idx = Arc::new(TokioRwLock::new(index));
        let shadowed_files = Arc::new(TokioRwLock::new(HashSet::new()));
        let token = CancellationToken::new();
        let manager = DynamicWatcherManager::new(token.clone()).unwrap();
        let logger = Arc::new(WorkspaceLogger::noop(root.clone()));

        manager
            .watch_workspace_with_shadowed_files(
                root.clone(),
                Arc::clone(&idx),
                Arc::clone(&shadowed_files),
                logger,
                cortx_core::ProjectConfig::default(),
            )
            .unwrap();

        // Unlink deleted.rs
        std::fs::remove_file(&deleted_file).unwrap();
        DynamicWatcherManager::dispatch_event(&manager.inner, RawWatchEvent::Remove(deleted_file))
            .await;

        let sf = shadowed_files.read().await;
        assert!(
            sf.contains(Path::new("src/deleted.rs")),
            "shadowed_files must contain src/deleted.rs tombstone"
        );

        token.cancel();
    }
}
