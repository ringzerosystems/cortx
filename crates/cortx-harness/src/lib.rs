//! Ground-truth verification harness and testing oracles for Cortx.
//!
//! Provides oracles for the 5 Verification Pillars:
//! - Deterministic Simulation Testing (DST) & Go concurrency sweeps (`simulate`)

#![forbid(unsafe_code)]

pub mod simulate;

pub use simulate::{
    DstEngine, FailedSeedReport, FaultConfig, GoSweepResult, ScheduledEvent, SimulateError,
    SimulateOptions, SimulationResult, SimulationSummary, TraceEntry, VirtualClock,
    generate_ai_repair_prompt, generate_go_repair_prompt, run_go_concurrency_sweep, run_simulation,
    run_single_seed,
};
