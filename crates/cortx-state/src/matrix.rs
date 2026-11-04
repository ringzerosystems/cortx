//! State machine transition matrix and reachability graph analyzer.

use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;

/// A directed edge representing an event-driven state transition.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TransitionEdge {
    /// Source state.
    pub from: String,
    /// Triggering event.
    pub event: String,
    /// Destination state.
    pub to: String,
}

impl TransitionEdge {
    /// Creates a new transition edge.
    pub fn new(from: impl Into<String>, event: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            from: from.into(),
            event: event.into(),
            to: to.into(),
        }
    }
}

/// Transition matrix and reachability graph for verifying state machine completeness.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TransitionMatrix {
    states: BTreeSet<String>,
    edges: BTreeSet<TransitionEdge>,
}

impl TransitionMatrix {
    /// Creates an empty transition matrix.
    pub fn new() -> Self {
        Self::default()
    }

    /// Explicitly registers a state in the matrix.
    pub fn add_state(&mut self, state: impl Into<String>) {
        self.states.insert(state.into());
    }

    /// Registers a directed transition between two states.
    pub fn add_transition(
        &mut self,
        from: impl Into<String>,
        event: impl Into<String>,
        to: impl Into<String>,
    ) {
        let from_str = from.into();
        let to_str = to.into();
        self.states.insert(from_str.clone());
        self.states.insert(to_str.clone());
        self.edges.insert(TransitionEdge {
            from: from_str,
            event: event.into(),
            to: to_str,
        });
    }

    /// Returns the set of all registered states.
    pub fn states(&self) -> &BTreeSet<String> {
        &self.states
    }

    /// Returns the set of all transition edges.
    pub fn edges(&self) -> &BTreeSet<TransitionEdge> {
        &self.edges
    }

    /// Computes the set of states reachable from a given start state.
    pub fn reachable_from(&self, start: &str) -> BTreeSet<String> {
        let mut visited = BTreeSet::new();
        let mut stack = Vec::new();

        if self.states.contains(start) {
            visited.insert(String::from(start));
            stack.push(String::from(start));
        }

        while let Some(current) = stack.pop() {
            for edge in &self.edges {
                if edge.from == current && !visited.contains(&edge.to) {
                    visited.insert(edge.to.clone());
                    stack.push(edge.to.clone());
                }
            }
        }

        visited
    }

    /// Checks whether `target` is reachable from `start`.
    pub fn is_reachable(&self, start: &str, target: &str) -> bool {
        self.reachable_from(start).contains(target)
    }

    /// Returns all states that cannot be reached from `initial_state`.
    pub fn unreachable_states(&self, initial_state: &str) -> BTreeSet<String> {
        let reachable = self.reachable_from(initial_state);
        let mut unreachable = BTreeSet::new();
        for state in &self.states {
            if !reachable.contains(state) {
                unreachable.insert(state.clone());
            }
        }
        unreachable
    }

    /// Returns all states that have no outgoing transitions (terminal/sink states).
    pub fn dead_end_states(&self) -> BTreeSet<String> {
        let mut outgoing = BTreeSet::new();
        for edge in &self.edges {
            outgoing.insert(edge.from.as_str());
        }

        let mut dead_ends = BTreeSet::new();
        for state in &self.states {
            if !outgoing.contains(state.as_str()) {
                dead_ends.insert(state.clone());
            }
        }
        dead_ends
    }

    /// Calculates transition coverage percentage (0 to 100) against an observed set of transitions.
    pub fn coverage_percentage(&self, observed: &BTreeSet<TransitionEdge>) -> usize {
        if self.edges.is_empty() {
            return 100;
        }
        let covered = self.edges.intersection(observed).count();
        (covered * 100) / self.edges.len()
    }

    /// Exports the transition matrix into a Mermaid `stateDiagram-v2` markdown diagram.
    pub fn to_mermaid(&self) -> String {
        let mut out = String::from("stateDiagram-v2\n");
        for edge in &self.edges {
            out.push_str("    ");
            out.push_str(&edge.from);
            out.push_str(" --> ");
            out.push_str(&edge.to);
            out.push_str(": ");
            out.push_str(&edge.event);
            out.push('\n');
        }
        out
    }
}
