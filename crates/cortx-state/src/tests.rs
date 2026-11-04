use crate::fuzz::{FuzzError, FuzzOptions, XorShift64, fuzz_transitions};
use crate::matrix::{TransitionEdge, TransitionMatrix};
use crate::state_machine::{StateMachine, drive_sequence};
use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Terminated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionEvent {
    Connect,
    ConnectedAck,
    Disconnect,
    Terminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionAction {
    SendSyn,
    NotifyReady,
    SendFin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionError {
    InvalidTransition,
}

struct ConnectionMachine {
    state: ConnectionState,
    counter: usize,
}

impl ConnectionMachine {
    fn new() -> Self {
        Self {
            state: ConnectionState::Disconnected,
            counter: 0,
        }
    }
}

impl StateMachine for ConnectionMachine {
    type State = ConnectionState;
    type Event = ConnectionEvent;
    type Action = ConnectionAction;
    type Error = ConnectionError;

    fn step(&mut self, event: Self::Event) -> Result<Vec<Self::Action>, Self::Error> {
        self.counter += 1;
        match (self.state, event) {
            (ConnectionState::Disconnected, ConnectionEvent::Connect) => {
                self.state = ConnectionState::Connecting;
                Ok(vec![ConnectionAction::SendSyn])
            }
            (ConnectionState::Connecting, ConnectionEvent::ConnectedAck) => {
                self.state = ConnectionState::Connected;
                Ok(vec![ConnectionAction::NotifyReady])
            }
            (ConnectionState::Connected, ConnectionEvent::Disconnect) => {
                self.state = ConnectionState::Disconnected;
                Ok(vec![ConnectionAction::SendFin])
            }
            (ConnectionState::Connecting, ConnectionEvent::Disconnect) => {
                self.state = ConnectionState::Disconnected;
                Ok(vec![])
            }
            (_, ConnectionEvent::Terminate) => {
                self.state = ConnectionState::Terminated;
                Ok(vec![])
            }
            _ => Err(ConnectionError::InvalidTransition),
        }
    }

    fn state(&self) -> &Self::State {
        &self.state
    }
}

#[test]
fn test_drive_sequence() {
    let mut sm = ConnectionMachine::new();
    let events = [
        ConnectionEvent::Connect,
        ConnectionEvent::ConnectedAck,
        ConnectionEvent::Disconnect,
    ];

    let actions = drive_sequence(&mut sm, events).expect("sequence should succeed");
    assert_eq!(*sm.state(), ConnectionState::Disconnected);
    assert_eq!(
        actions,
        vec![
            ConnectionAction::SendSyn,
            ConnectionAction::NotifyReady,
            ConnectionAction::SendFin
        ]
    );
}

#[test]
fn test_xorshift64_deterministic() {
    let mut rng1 = XorShift64::new(42);
    let mut rng2 = XorShift64::new(42);

    for _ in 0..100 {
        assert_eq!(rng1.next_u64(), rng2.next_u64());
        assert_eq!(rng1.next_usize(10), rng2.next_usize(10));
        assert_eq!(rng1.next_bool(), rng2.next_bool());
    }
}

#[test]
fn test_fuzz_transitions_throughput() {
    let mut sm = ConnectionMachine::new();
    let options = FuzzOptions::new(12345, 1_000_000);

    let report = fuzz_transitions(
        &mut sm,
        options,
        |rng, state| match state {
            ConnectionState::Disconnected => ConnectionEvent::Connect,
            ConnectionState::Connecting => {
                if rng.next_bool() {
                    ConnectionEvent::ConnectedAck
                } else {
                    ConnectionEvent::Disconnect
                }
            }
            ConnectionState::Connected => ConnectionEvent::Disconnect,
            ConnectionState::Terminated => ConnectionEvent::Terminate,
        },
        |_state, _actions| Ok(()),
    )
    .expect("fuzzing 1M transitions should succeed");

    assert_eq!(report.transitions_executed, 1_000_000);
    assert!(report.success);
}

#[test]
fn test_fuzz_transitions_detects_invariant_violation() {
    let mut sm = ConnectionMachine::new();
    let options = FuzzOptions::new(999, 100);

    let err = fuzz_transitions(
        &mut sm,
        options,
        |_rng, _state| ConnectionEvent::Connect,
        |state, _actions| {
            if *state == ConnectionState::Connecting {
                Err(String::from("Connecting state forbidden by invariant"))
            } else {
                Ok(())
            }
        },
    )
    .expect_err("should catch invariant violation");

    match err {
        FuzzError::InvariantViolation { step, message } => {
            assert_eq!(step, 0);
            assert_eq!(message, "Connecting state forbidden by invariant");
        }
        _ => panic!("unexpected error variant"),
    }
}

#[test]
fn test_fuzz_transitions_detects_machine_error() {
    let mut sm = ConnectionMachine::new();
    let options = FuzzOptions::new(42, 10);

    let err = fuzz_transitions(
        &mut sm,
        options,
        |_rng, _state| ConnectionEvent::ConnectedAck,
        |_state, _actions| Ok(()),
    )
    .expect_err("should catch invalid transition error");

    match err {
        FuzzError::MachineError { step, error } => {
            assert_eq!(step, 0);
            assert_eq!(error, ConnectionError::InvalidTransition);
        }
        _ => panic!("unexpected error variant"),
    }
}

#[test]
fn test_transition_matrix_reachability_and_dead_ends() {
    let mut matrix = TransitionMatrix::new();
    matrix.add_transition("Disconnected", "Connect", "Connecting");
    matrix.add_transition("Connecting", "ConnectedAck", "Connected");
    matrix.add_transition("Connected", "Disconnect", "Disconnected");
    matrix.add_transition("Connecting", "Timeout", "Terminated");
    matrix.add_state("OrphanedState");

    assert!(matrix.is_reachable("Disconnected", "Connected"));
    assert!(matrix.is_reachable("Disconnected", "Terminated"));
    assert!(!matrix.is_reachable("Disconnected", "OrphanedState"));

    let unreachable = matrix.unreachable_states("Disconnected");
    assert!(unreachable.contains("OrphanedState"));
    assert_eq!(unreachable.len(), 1);

    let dead_ends = matrix.dead_end_states();
    assert!(dead_ends.contains("Terminated"));
    assert!(dead_ends.contains("OrphanedState"));
    assert_eq!(dead_ends.len(), 2);

    let mut observed = BTreeSet::new();
    observed.insert(TransitionEdge::new("Disconnected", "Connect", "Connecting"));
    observed.insert(TransitionEdge::new(
        "Connecting",
        "ConnectedAck",
        "Connected",
    ));

    assert_eq!(matrix.coverage_percentage(&observed), 50);

    let mermaid = matrix.to_mermaid();
    assert!(mermaid.starts_with("stateDiagram-v2\n"));
    assert!(mermaid.contains("Disconnected --> Connecting: Connect\n"));
}
