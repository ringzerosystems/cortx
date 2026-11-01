//! In-memory agent session state and deduplication tracking.

use cortx_core::{AgentVendor, ReadKind};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::RwLock;

/// Maximum duration an idle session is retained before eviction.
const SESSION_TTL: Duration = Duration::from_secs(4 * 3600);
/// Maximum active sessions before pruning idle entries.
const SESSION_CAPACITY_THRESHOLD: usize = 100;

/// Cached session state tracking files served in a specific agent conversation session.
#[derive(Debug, Clone)]
pub struct SessionState {
    pub vendor: AgentVendor,
    pub files: HashMap<PathBuf, SystemTime>,
    pub macro_reads: u64,
    pub micro_reads: u64,
    pub repeat_reads: u64,
    pub last_accessed: Instant,
}

impl SessionState {
    pub fn new(vendor: AgentVendor) -> Self {
        Self {
            vendor,
            files: HashMap::new(),
            macro_reads: 0,
            micro_reads: 0,
            repeat_reads: 0,
            last_accessed: Instant::now(),
        }
    }
}

/// Thread-safe registry of active agent sessions.
pub type SessionRegistry = Arc<RwLock<HashMap<String, SessionState>>>;

/// Formats the session identifier with its associated vendor for structured logging.
pub fn format_session_attribution(session_id: Option<&str>, vendor: AgentVendor) -> String {
    match session_id {
        Some(sid) => format!("session: '{}' (vendor: {})", sid, vendor),
        None => format!("session: none (vendor: {})", vendor),
    }
}

/// Checks whether the target file has already been served in the given session with matching mtime.
pub async fn is_session_repeat(
    sessions: &SessionRegistry,
    session_id: &str,
    canon_file: &Path,
    current_mtime: Option<SystemTime>,
) -> bool {
    let Some(mtime) = current_mtime else {
        return false;
    };
    let history = sessions.read().await;
    history
        .get(session_id)
        .and_then(|s| s.files.get(canon_file))
        .is_some_and(|cached_mtime| *cached_mtime == mtime)
}

/// Records a micro-read or macro-read activity in the session registry.
pub async fn record_session_activity(
    sessions: &SessionRegistry,
    session_id: &str,
    vendor: AgentVendor,
    read_kind: ReadKind,
) {
    let mut history = sessions.write().await;
    prune_stale_sessions(&mut history);
    let session = history
        .entry(session_id.to_string())
        .or_insert_with(|| SessionState::new(vendor));
    session.last_accessed = Instant::now();
    session.vendor = vendor;
    match read_kind {
        ReadKind::Micro => session.micro_reads += 1,
        ReadKind::Macro => session.macro_reads += 1,
    }
}

/// Records an omitted repeat read for the session.
pub async fn record_session_repeat(
    sessions: &SessionRegistry,
    session_id: &str,
    vendor: AgentVendor,
) {
    let mut history = sessions.write().await;
    let session = history
        .entry(session_id.to_string())
        .or_insert_with(|| SessionState::new(vendor));
    session.last_accessed = Instant::now();
    session.vendor = vendor;
    session.repeat_reads += 1;
}

/// Records a file served in the session with its current disk mtime.
pub async fn record_session_file(
    sessions: &SessionRegistry,
    session_id: &str,
    vendor: AgentVendor,
    canon_file: PathBuf,
    current_mtime: Option<SystemTime>,
) {
    let Some(mtime) = current_mtime else {
        return;
    };
    let mut history = sessions.write().await;
    prune_stale_sessions(&mut history);
    let session = history
        .entry(session_id.to_string())
        .or_insert_with(|| SessionState::new(vendor));
    session.last_accessed = Instant::now();
    session.vendor = vendor;
    session.macro_reads += 1;
    session.files.insert(canon_file, mtime);
}

fn prune_stale_sessions(history: &mut HashMap<String, SessionState>) {
    if history.len() > SESSION_CAPACITY_THRESHOLD {
        let now = Instant::now();
        history.retain(|_, s| now.duration_since(s.last_accessed) < SESSION_TTL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_session_state_lifecycle_and_attribution() {
        let sessions: SessionRegistry = Arc::new(RwLock::new(HashMap::new()));
        let sid = "session-test-abc";
        let canon = PathBuf::from("/tmp/test.rs");
        let now = SystemTime::now();

        assert_eq!(
            format_session_attribution(Some(sid), AgentVendor::Agy),
            "session: 'session-test-abc' (vendor: agy)"
        );
        assert_eq!(
            format_session_attribution(None, AgentVendor::Claude),
            "session: none (vendor: claude)"
        );

        assert!(!is_session_repeat(&sessions, sid, &canon, Some(now)).await);

        record_session_file(&sessions, sid, AgentVendor::Agy, canon.clone(), Some(now)).await;
        assert!(is_session_repeat(&sessions, sid, &canon, Some(now)).await);

        let later = now + Duration::from_secs(5);
        assert!(!is_session_repeat(&sessions, sid, &canon, Some(later)).await);

        record_session_repeat(&sessions, sid, AgentVendor::Agy).await;
        record_session_activity(&sessions, sid, AgentVendor::Agy, ReadKind::Micro).await;

        let history = sessions.read().await;
        let entry = history.get(sid).unwrap();
        assert_eq!(entry.vendor, AgentVendor::Agy);
        assert_eq!(entry.macro_reads, 1);
        assert_eq!(entry.micro_reads, 1);
        assert_eq!(entry.repeat_reads, 1);
    }
}
