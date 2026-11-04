//! Ground-truth verification harness and testing oracles for Cortx.
//!
//! Provides oracles for the 5 Verification Pillars:
//! - Deterministic Simulation Testing (DST) & Go concurrency sweeps (`simulate`)
//! - Differential Fuzzing Oracle & Cross-Boundary Contracts (`diff_fuzz`)
//! - Bounded Model Checking via Kani (`verify`)
//! - Metamorphic Invariant Testing (`invariants`)

#![forbid(unsafe_code)]

pub mod diff_fuzz;
pub mod invariants;
pub mod simulate;
pub mod verify;

pub use diff_fuzz::{
    CasingStyle, ContractViolation, ContractViolationKind, CrossBoundaryFailure,
    CrossBoundaryOptions, DiffFuzzError, DiffFuzzOptions, DiffFuzzSummary, DiffMismatch,
    JS_MAX_SAFE_INT, JS_MIN_SAFE_INT, ReproducerLanguage, ReproducerSpec, ShadowTarget,
    check_casing, check_int53_bounds, check_null_slice_divergence, export_reproducer_test,
    export_reproducer_to_file, run_cross_boundary_diff_fuzz, run_diff_fuzz, run_diff_fuzz_sequence,
    run_shadow_diff_fuzz, shrink_sequence, validate_cross_boundary_contract,
};
pub use invariants::{
    InvariantError, InvariantReport, InvariantViolation, MetamorphicPropertyKind, PropertyLanguage,
    check_idempotence, check_permutation_invariance, check_round_trip, check_state_bounds,
    scaffold_property_suite,
};
pub use simulate::{
    DstEngine, FailedSeedReport, FaultConfig, GoSweepResult, ScheduledEvent, SimulateError,
    SimulateOptions, SimulationResult, SimulationSummary, TraceEntry, VirtualClock,
    generate_ai_repair_prompt, generate_go_repair_prompt, run_go_concurrency_sweep, run_simulation,
    run_single_seed,
};
pub use verify::{
    CounterexampleVariable, KaniCounterexample, KaniProofOptions, KaniVerificationResult,
    VerifyError, VerifyOptions, parse_kani_json, run_kani_verify, scaffold_kani_proof,
    synthesize_counterexample_initializer,
};
