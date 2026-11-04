//! Pure Sans-I/O deterministic state machine traits and execution primitives.

use alloc::vec::Vec;

/// A pure deterministic, side-effect free state machine.
///
/// In a Sans-I/O architecture, the state machine produces explicit `Action` commands
/// instead of performing I/O directly. An external runtime executes actions
/// and feeds events back into `step`.
pub trait StateMachine {
    /// State representation.
    type State: Clone + Send + 'static;
    /// Input event triggering state transitions.
    type Event: Clone + Send + 'static;
    /// Output side-effect command to be executed by the outer runtime.
    type Action: Clone + Send + 'static;
    /// Domain error returned if an invalid transition or guard fails.
    type Error: core::fmt::Debug + Send + 'static;

    /// Advances the state machine with an incoming event.
    ///
    /// Returns a list of actions to execute, or an error if the transition is invalid.
    fn step(&mut self, event: Self::Event) -> Result<Vec<Self::Action>, Self::Error>;

    /// Returns a reference to the current state.
    fn state(&self) -> &Self::State;
}

/// Executes a sequential stream of events against a state machine, accumulating actions.
pub fn drive_sequence<M, I>(machine: &mut M, events: I) -> Result<Vec<M::Action>, M::Error>
where
    M: StateMachine,
    I: IntoIterator<Item = M::Event>,
{
    let mut all_actions = Vec::new();
    for event in events {
        let actions = machine.step(event)?;
        all_actions.extend(actions);
    }
    Ok(all_actions)
}
