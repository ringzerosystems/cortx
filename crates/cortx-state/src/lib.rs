//! Sans-I/O pure deterministic state machine engine for Cortx.
//!
//! Provides the core `StateMachine` trait, high-speed deterministic transition
//! fuzzing (>10M transitions/sec), and `TransitionMatrix` reachability analysis
//! in a `#![no_std]` environment.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod fuzz;
pub mod matrix;
pub mod state_machine;

#[cfg(test)]
mod tests;

pub use fuzz::{FuzzError, FuzzOptions, FuzzReport, XorShift64, fuzz_transitions};
pub use matrix::{TransitionEdge, TransitionMatrix};
pub use state_machine::{StateMachine, drive_sequence};
