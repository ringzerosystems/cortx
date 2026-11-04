//! High-speed deterministic transition fuzzer (>10M transitions/sec).

use crate::state_machine::StateMachine;
use alloc::string::String;

/// Fast, deterministic 64-bit XorShift pseudorandom number generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    /// Creates a new generator from a seed. Seed `0` is replaced with a default non-zero constant.
    #[inline]
    pub const fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x5465_7374_5365_6564
            } else {
                seed
            },
        }
    }

    /// Generates the next pseudorandom `u64`.
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    /// Generates a pseudorandom integer in `[0, upper_bound)`.
    #[inline]
    pub fn next_usize(&mut self, upper_bound: usize) -> usize {
        if upper_bound == 0 {
            return 0;
        }
        (self.next_u64() % (upper_bound as u64)) as usize
    }

    /// Generates a pseudorandom boolean.
    #[inline]
    pub fn next_bool(&mut self) -> bool {
        (self.next_u64() & 1) == 1
    }
}

/// Configuration options for state machine transition fuzzing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FuzzOptions {
    /// Initial seed for the deterministic PRNG.
    pub seed: u64,
    /// Number of transitions to execute.
    pub transitions: usize,
}

impl Default for FuzzOptions {
    fn default() -> Self {
        Self {
            seed: 0x5465_7374_5365_6564,
            transitions: 100_000,
        }
    }
}

impl FuzzOptions {
    /// Constructs a new `FuzzOptions` with the given seed and transition count.
    #[inline]
    pub const fn new(seed: u64, transitions: usize) -> Self {
        Self { seed, transitions }
    }
}

/// Errors detected during transition fuzzing.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum FuzzError<E> {
    /// State machine returned an error on a valid transition step.
    MachineError { step: usize, error: E },
    /// An invariant check failed after a transition.
    InvariantViolation { step: usize, message: String },
}

impl<E: core::fmt::Debug> core::fmt::Display for FuzzError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MachineError { step, error } => {
                write!(f, "state machine error at step {step}: {error:?}")
            }
            Self::InvariantViolation { step, message } => {
                write!(f, "invariant violation at step {step}: {message}")
            }
        }
    }
}

/// Summary report of a completed fuzzing run.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FuzzReport {
    /// PRNG seed used for this run.
    pub seed: u64,
    /// Total number of transitions successfully executed.
    pub transitions_executed: usize,
    /// Whether all transitions and invariant checks passed.
    pub success: bool,
}

/// Fuzzes a state machine with pseudo-random events generated from `generator`
/// while verifying system invariants after each transition.
pub fn fuzz_transitions<M, G, Inv>(
    machine: &mut M,
    options: FuzzOptions,
    mut generator: G,
    mut invariant: Inv,
) -> Result<FuzzReport, FuzzError<M::Error>>
where
    M: StateMachine,
    G: FnMut(&mut XorShift64, &M::State) -> M::Event,
    Inv: FnMut(&M::State, &[M::Action]) -> Result<(), String>,
{
    let mut rng = XorShift64::new(options.seed);

    for step in 0..options.transitions {
        let event = generator(&mut rng, machine.state());
        let actions = machine
            .step(event)
            .map_err(|error| FuzzError::MachineError { step, error })?;

        if let Err(message) = invariant(machine.state(), &actions) {
            return Err(FuzzError::InvariantViolation { step, message });
        }
    }

    Ok(FuzzReport {
        seed: options.seed,
        transitions_executed: options.transitions,
        success: true,
    })
}
