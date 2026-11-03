//! In-memory agent session state and deduplication tracking.

use cortx_core::{AgentVendor, ReadKind};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::RwLock;

/// Default idle session retention threshold (8 hours = 28,800 seconds).
pub const DEFAULT_SESSION_TTL: Duration =
    Duration::from_secs(cortx_core::config::DEFAULT_SESSION_TTL_SECS);

/// Cached session state tracking files served in a specific agent conversation session.
#[derive(Debug, Clone)]
pub struct SessionState {
    pub vendor: AgentVendor,
    pub files: HashMap<PathBuf, SystemTime>,
    pub macro_reads: u64,
    pub micro_reads: u64,
    pub repeat_reads: u64,
    pub last_accessed: Instant,
    pub access_seq: u64,
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
            access_seq: 0,
        }
    }
}

/// Dual-index session map providing $O(1)$ key lookup and $O(\log n)$ chronological expiration.
#[derive(Debug)]
pub struct SessionMap {
    ttl: Duration,
    sessions: HashMap<String, SessionState>,
    expiry_index: BTreeMap<(Instant, u64), String>,
    access_seq: u64,
}

impl Default for SessionMap {
    fn default() -> Self {
        Self::new(DEFAULT_SESSION_TTL)
    }
}

impl SessionMap {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            sessions: HashMap::new(),
            expiry_index: BTreeMap::new(),
            access_seq: 0,
        }
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Retrieves session by identifier in $O(1)$, returning `None` if absent or expired.
    pub fn get(&self, session_id: &str) -> Option<&SessionState> {
        let session = self.sessions.get(session_id)?;
        if session.last_accessed.elapsed() >= self.ttl {
            None
        } else {
            Some(session)
        }
    }

    /// Checks in $O(1)$ whether the target file has already been served in the session with matching mtime.
    pub fn is_repeat(
        &self,
        session_id: &str,
        canon_file: &Path,
        current_mtime: Option<SystemTime>,
    ) -> bool {
        let Some(mtime) = current_mtime else {
            return false;
        };
        self.get(session_id)
            .and_then(|s| s.files.get(canon_file))
            .is_some_and(|cached_mtime| *cached_mtime == mtime)
    }

    /// Evicts expired sessions from the chronological index in $O(k \log n)$, terminating at the first unexpired entry.
    pub fn prune_stale(&mut self) -> usize {
        let mut pruned = 0;
        let now = Instant::now();
        while let Some((&(instant, _), _)) = self.expiry_index.first_key_value() {
            if now.saturating_duration_since(instant) >= self.ttl {
                if let Some((_, sid)) = self.expiry_index.pop_first() {
                    self.sessions.remove(&sid);
                    pruned += 1;
                }
            } else {
                break;
            }
        }
        pruned
    }

    /// Touches or initializes a session in $O(\log n)$, refreshing its position in the chronological index.
    pub fn touch_or_create(&mut self, session_id: &str, vendor: AgentVendor) -> &mut SessionState {
        self.prune_stale();
        self.access_seq = self.access_seq.wrapping_add(1);
        let seq = self.access_seq;
        let now = Instant::now();

        if self.sessions.contains_key(session_id) {
            let session = self.sessions.get_mut(session_id).expect("checked above");
            let old_key = (session.last_accessed, session.access_seq);
            self.expiry_index.remove(&old_key);
            session.last_accessed = now;
            session.access_seq = seq;
            session.vendor = vendor;
            self.expiry_index.insert((now, seq), session_id.to_string());
            session
        } else {
            let mut session = SessionState::new(vendor);
            session.last_accessed = now;
            session.access_seq = seq;
            self.expiry_index.insert((now, seq), session_id.to_string());
            self.sessions.insert(session_id.to_string(), session);
            self.sessions
                .get_mut(session_id)
                .expect("session just inserted")
        }
    }

    pub fn record_activity(&mut self, session_id: &str, vendor: AgentVendor, read_kind: ReadKind) {
        let session = self.touch_or_create(session_id, vendor);
        match read_kind {
            ReadKind::Micro => session.micro_reads += 1,
            ReadKind::Macro => session.macro_reads += 1,
        }
    }

    pub fn record_repeat(&mut self, session_id: &str, vendor: AgentVendor) {
        let session = self.touch_or_create(session_id, vendor);
        session.repeat_reads += 1;
    }

    pub fn record_file(
        &mut self,
        session_id: &str,
        vendor: AgentVendor,
        canon_file: PathBuf,
        current_mtime: Option<SystemTime>,
    ) {
        let Some(mtime) = current_mtime else {
            return;
        };
        let session = self.touch_or_create(session_id, vendor);
        session.macro_reads += 1;
        session.files.insert(canon_file, mtime);
    }
}

/// Thread-safe registry of active agent sessions.
pub type SessionRegistry = Arc<RwLock<SessionMap>>;

/// Creates a new thread-safe session registry with the specified TTL.
pub fn create_session_registry(ttl: Duration) -> SessionRegistry {
    Arc::new(RwLock::new(SessionMap::new(ttl)))
}

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
    sessions
        .read()
        .await
        .is_repeat(session_id, canon_file, current_mtime)
}

/// Records a micro-read or macro-read activity in the session registry.
pub async fn record_session_activity(
    sessions: &SessionRegistry,
    session_id: &str,
    vendor: AgentVendor,
    read_kind: ReadKind,
) {
    sessions
        .write()
        .await
        .record_activity(session_id, vendor, read_kind);
}

/// Records an omitted repeat read for the session.
pub async fn record_session_repeat(
    sessions: &SessionRegistry,
    session_id: &str,
    vendor: AgentVendor,
) {
    sessions.write().await.record_repeat(session_id, vendor);
}

/// Records a file served in the session with its current disk mtime.
pub async fn record_session_file(
    sessions: &SessionRegistry,
    session_id: &str,
    vendor: AgentVendor,
    canon_file: PathBuf,
    current_mtime: Option<SystemTime>,
) {
    sessions
        .write()
        .await
        .record_file(session_id, vendor, canon_file, current_mtime);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_session_ttl() {
        assert_eq!(DEFAULT_SESSION_TTL, Duration::from_secs(28800));
        let map = SessionMap::default();
        assert_eq!(map.ttl(), Duration::from_secs(28800));
        assert!(map.is_empty());
    }

    #[tokio::test]
    async fn test_session_state_lifecycle_and_attribution() {
        let sessions: SessionRegistry = create_session_registry(DEFAULT_SESSION_TTL);
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

    #[tokio::test]
    async fn test_session_expiration_eviction() {
        let mut map = SessionMap::new(Duration::from_millis(20));
        let sid = "session-expire-1";
        let canon = PathBuf::from("/tmp/expire.rs");
        let mtime = SystemTime::now();

        map.record_file(sid, AgentVendor::Claude, canon.clone(), Some(mtime));
        assert!(map.is_repeat(sid, &canon, Some(mtime)));
        assert_eq!(map.len(), 1);

        tokio::time::sleep(Duration::from_millis(30)).await;

        assert!(!map.is_repeat(sid, &canon, Some(mtime)));
        assert!(map.get(sid).is_none());
        assert_eq!(map.prune_stale(), 1);
        assert!(map.is_empty());
    }

    #[test]
    fn test_monotonic_access_seq_collision_handling() {
        let mut map = SessionMap::new(Duration::from_secs(60));
        map.touch_or_create("sid-1", AgentVendor::Agy);
        map.touch_or_create("sid-2", AgentVendor::Claude);

        assert_eq!(map.len(), 2);
        assert_eq!(map.expiry_index.len(), 2);
        assert_eq!(map.sessions.get("sid-1").unwrap().access_seq, 1);
        assert_eq!(map.sessions.get("sid-2").unwrap().access_seq, 2);

        map.touch_or_create("sid-1", AgentVendor::Agy);
        assert_eq!(map.len(), 2);
        assert_eq!(map.expiry_index.len(), 2);
        assert_eq!(map.sessions.get("sid-1").unwrap().access_seq, 3);
    }

    #[test]
    fn test_metric_counters_across_multiple_accesses() {
        let mut map = SessionMap::new(Duration::from_secs(60));
        let sid = "metrics-sid";
        let canon = PathBuf::from("/tmp/metrics.rs");
        let mtime = SystemTime::now();

        map.record_activity(sid, AgentVendor::Agy, ReadKind::Micro);
        map.record_activity(sid, AgentVendor::Agy, ReadKind::Macro);
        map.record_repeat(sid, AgentVendor::Agy);
        map.record_file(sid, AgentVendor::Agy, canon, Some(mtime));

        let entry = map.get(sid).unwrap();
        assert_eq!(entry.micro_reads, 1);
        assert_eq!(entry.macro_reads, 2);
        assert_eq!(entry.repeat_reads, 1);
    }
}
