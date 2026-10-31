//! String interning pool and compact call graph edge representations.

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

/// Compact identifier for an interned string in `StringPool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SymbolId(pub u32);

/// Compact representation of a call graph edge using interned string identifiers (16 bytes, zero heap allocations).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CompactCallEdge {
    /// Upstream caller symbol identifier.
    pub caller: SymbolId,
    /// Downstream callee symbol identifier.
    pub callee: SymbolId,
    /// Source file path identifier.
    pub file: SymbolId,
    /// 1-indexed source line where the call occurs.
    pub call_line: u32,
}

impl CompactCallEdge {
    /// Resolves an interned CompactCallEdge back into an owned CallEdge using the StringPool.
    pub fn resolve(&self, pool: &StringPool) -> Option<CallEdge> {
        Some(CallEdge {
            caller: pool.resolve(self.caller)?.to_string(),
            callee: pool.resolve(self.callee)?.to_string(),
            file: pool.resolve(self.file)?.to_string(),
            call_line: self.call_line as usize,
        })
    }
}

/// Contiguous string pool for deduplicating symbols, functions, and file paths.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StringPool {
    /// Contiguous array of interned strings indexed by SymbolId.
    pub strings: Vec<String>,
    /// Fast deduplication lookup mapping string slice to SymbolId.
    pub lookup: FxHashMap<String, SymbolId>,
}

impl StringPool {
    /// Creates an empty string pool.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an empty string pool with pre-allocated capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            strings: Vec::with_capacity(capacity),
            lookup: FxHashMap::with_capacity_and_hasher(capacity, Default::default()),
        }
    }

    /// Interns a string slice, returning its unique `SymbolId`.
    pub fn intern(&mut self, s: &str) -> SymbolId {
        if let Some(&id) = self.lookup.get(s) {
            return id;
        }
        let id = SymbolId(self.strings.len() as u32);
        let owned = s.to_string();
        // ponytail: clone needed for dual Vec/FxHashMap index without unsafe self-referential borrows
        self.strings.push(owned.clone());
        self.lookup.insert(owned, id);
        id
    }

    /// Interns an owned string, avoiding redundant string allocations when already present or interned.
    pub fn intern_string(&mut self, s: String) -> SymbolId {
        if let Some(&id) = self.lookup.get(s.as_str()) {
            return id;
        }
        let id = SymbolId(self.strings.len() as u32);
        self.strings.push(s.clone());
        self.lookup.insert(s, id);
        id
    }

    /// Resolves a `SymbolId` back to its string slice.
    pub fn resolve(&self, id: SymbolId) -> Option<&str> {
        self.strings.get(id.0 as usize).map(|s| s.as_str())
    }

    /// Returns the `SymbolId` for an existing string without interning it.
    pub fn get_id(&self, s: &str) -> Option<SymbolId> {
        self.lookup.get(s).copied()
    }

    /// Returns the number of unique strings interned in the pool.
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// Returns true if the pool contains no interned strings.
    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}

/// Represents a caller-to-callee relation in the indexed call graph.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CallEdge {
    /// Upstream caller symbol or function.
    pub caller: String,
    /// Downstream callee symbol or function.
    pub callee: String,
    /// Source file path where invocation occurs.
    pub file: String,
    /// 1-indexed source line where invocation occurs (0 if unspecified).
    #[serde(default)]
    pub call_line: usize,
}
