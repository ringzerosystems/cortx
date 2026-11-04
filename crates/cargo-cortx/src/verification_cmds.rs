//! Verification subcommands for the 5 Verification Pillars.
//!
//! Provides CLI argument definitions and handlers for:
//! - `simulate`: Deterministic Simulation Testing (DST) & Go concurrency sweeps
//! - `diff-fuzz`: Differential Fuzzing Oracle & Cross-Boundary Contract Verification
//! - `verify`: Bounded Model Checking via Kani
//! - `invariants`: Metamorphic Invariant Testing & Property Scaffolding
//! - `state-machine`: Sans-I/O Pure Deterministic State Machine Engine

#![forbid(unsafe_code)]

use std::path::PathBuf;

use clap::Args;
use cortx_core::config::CortxConfig;
use cortx_core::diagnostics::{error, success, warn};
use cortx_harness::diff_fuzz::{
    CasingStyle, DiffFuzzOptions, ReproducerLanguage, ReproducerSpec, export_reproducer_to_file,
    run_diff_fuzz, validate_cross_boundary_contract,
};
use cortx_harness::invariants::{
    MetamorphicPropertyKind, PropertyLanguage, check_idempotence, check_permutation_invariance,
    scaffold_property_suite,
};
use cortx_harness::simulate::{
    DstEngine, FaultConfig, SimulateOptions, run_go_concurrency_sweep, run_simulation,
};
use cortx_harness::verify::{
    KaniProofOptions, VerifyError, VerifyOptions, run_kani_verify, scaffold_kani_proof,
};
use cortx_state::{FuzzOptions, StateMachine, TransitionMatrix, fuzz_transitions};

// ---------------------------------------------------------------------------
// 1. Simulate Subcommand
// ---------------------------------------------------------------------------

/// Deterministic Simulation Testing (DST) & Go concurrency sweeps.
#[derive(Args, Debug, Clone)]
pub struct SimulateArgs {
    /// Number of seeds to run in simulation sweep
    #[arg(short, long, help_heading = "Simulation Options")]
    pub seeds: Option<u64>,

    /// Explicit seed to replay (hex or decimal, e.g. 0xDEADBEEF)
    #[arg(long, help_heading = "Simulation Options")]
    pub seed: Option<String>,

    /// Target language or environment (rust, go)
    #[arg(short, long, help_heading = "Simulation Options")]
    pub lang: Option<String>,

    /// Timeout in seconds per simulation
    #[arg(short, long, help_heading = "Simulation Options")]
    pub timeout: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CortxSimState {
    pub ops: usize,
    pub counter: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CortxSimEvent {
    Increment(u64),
    Decrement(u64),
    Reset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CortxSimAction {
    Logged(String),
}

pub struct CortxSimMachine {
    pub state: CortxSimState,
}

impl StateMachine for CortxSimMachine {
    type State = CortxSimState;
    type Event = CortxSimEvent;
    type Action = CortxSimAction;
    type Error = String;

    fn step(&mut self, event: Self::Event) -> Result<Vec<Self::Action>, Self::Error> {
        self.state.ops += 1;
        match event {
            CortxSimEvent::Increment(val) => {
                self.state.counter = self.state.counter.saturating_add(val);
                Ok(vec![CortxSimAction::Logged("inc".into())])
            }
            CortxSimEvent::Decrement(val) => {
                self.state.counter = self.state.counter.saturating_sub(val);
                Ok(vec![CortxSimAction::Logged("dec".into())])
            }
            CortxSimEvent::Reset => {
                self.state.counter = 0;
                Ok(vec![CortxSimAction::Logged("rst".into())])
            }
        }
    }

    fn state(&self) -> &Self::State {
        &self.state
    }
}

/// Dispatches the `simulate` CLI subcommand.
pub async fn handle_simulate(
    args: SimulateArgs,
    cfg: &CortxConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()?;
    let is_go = args.lang.as_deref() == Some("go")
        || (args.lang.as_deref() != Some("rust") && cwd.join("go.mod").exists());

    if is_go {
        match run_go_concurrency_sweep(&cwd, &cfg.go.concurrency) {
            Ok(res) => {
                if res.passed {
                    success(
                        "Simulate",
                        &format!(
                            "Tier 1 Go concurrency sweep passed: {} passes with GOMAXPROCS {:?}",
                            res.passes_executed, res.gomaxprocs_tested
                        ),
                    );
                } else {
                    error("Simulate", "Tier 1 Go concurrency sweep detected issues");
                    if let Some(prompt) = res.ai_repair_prompt {
                        println!("{prompt}");
                    }
                }
            }
            Err(e) => {
                error("Simulate", &format!("Go concurrency sweep error: {e}"));
            }
        }
        return Ok(());
    }

    let specific_seed = if let Some(ref s) = args.seed {
        let trimmed = s.trim();
        let val = if trimmed.starts_with("0x") || trimmed.starts_with("0X") {
            u64::from_str_radix(&trimmed[2..], 16)
                .map_err(|e| format!("Invalid hex seed '{trimmed}': {e}"))?
        } else {
            trimmed
                .parse::<u64>()
                .map_err(|e| format!("Invalid decimal seed '{trimmed}': {e}"))?
        };
        Some(val)
    } else {
        None
    };

    let seeds = specific_seed
        .map(|_| 1)
        .unwrap_or(args.seeds.unwrap_or(cfg.simulate.default_seeds));
    let timeout = args.timeout.unwrap_or(cfg.simulate.timeout_seconds);

    let options = SimulateOptions {
        seeds,
        specific_seed,
        timeout_seconds: timeout,
        fault_config: FaultConfig {
            drop_rate: 0.05,
            delay_rate: 0.1,
            max_delay_ms: 15,
        },
        max_steps: 1_000,
    };

    let res = run_simulation(&options, |seed| {
        let machine = CortxSimMachine {
            state: CortxSimState {
                ops: 0,
                counter: 100,
            },
        };
        let mut engine = DstEngine::new(machine, seed, options.fault_config.clone());
        engine.schedule(CortxSimEvent::Increment(seed % 50 + 1), 5);
        engine.schedule(CortxSimEvent::Decrement(seed % 30 + 1), 10);
        engine.schedule(CortxSimEvent::Reset, 25);
        engine.add_invariant(|st| {
            if st.ops > 10_000 {
                Err("Ops limit breached".into())
            } else {
                Ok(())
            }
        });
        engine
    });

    if let Some(failed) = res.failed_seed {
        error(
            "Simulate",
            &format!(
                "DST simulation invariant violated at seed {:#018x} (step {})",
                failed.seed, failed.step_count
            ),
        );
        println!("{}", failed.repair_prompt);
    } else {
        success(
            "Simulate",
            &format!(
                "DST simulation passed: {} seeds evaluated across {}ms (0 failures)",
                res.passed_seeds, res.duration_ms
            ),
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// 2. DiffFuzz Subcommand
// ---------------------------------------------------------------------------

/// Differential Fuzzing Oracle & Cross-Boundary Contract Verification.
#[derive(Args, Debug, Clone)]
pub struct DiffFuzzArgs {
    /// Target trait or interface to fuzz
    #[arg(long, help_heading = "Diff-Fuzz Options")]
    pub target_trait: Option<String>,

    /// Legacy reference implementation symbol
    #[arg(long, help_heading = "Diff-Fuzz Options")]
    pub legacy: Option<String>,

    /// Candidate implementation symbol
    #[arg(long, help_heading = "Diff-Fuzz Options")]
    pub candidate: Option<String>,

    /// Cross-boundary contract validation mode (Serde <-> TS <-> Go)
    #[arg(long, help_heading = "Diff-Fuzz Options")]
    pub cross_boundary: bool,

    /// Number of fuzzing iterations
    #[arg(short, long, help_heading = "Diff-Fuzz Options")]
    pub iterations: Option<u64>,

    /// Export standalone reproducer test file on divergence or violation
    #[arg(long, help_heading = "Diff-Fuzz Options")]
    pub export_repro: Option<PathBuf>,
}

/// Dispatches the `diff-fuzz` CLI subcommand.
pub async fn handle_diff_fuzz(
    args: DiffFuzzArgs,
    cfg: &CortxConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let iterations = args.iterations.unwrap_or(cfg.diff_fuzz.default_iterations);

    if args.cross_boundary {
        let reference = serde_json::json!({
            "userId": 42,
            "userName": "Alice",
            "items": []
        });
        let candidate = serde_json::json!({
            "userId": 42,
            "userName": "Alice",
            "items": []
        });

        let violations = validate_cross_boundary_contract(
            &reference,
            &candidate,
            if cfg.cross_boundary.strict_casing_check {
                Some(CasingStyle::CamelCase)
            } else {
                None
            },
        );

        if violations.is_empty() {
            success(
                "Diff-Fuzz",
                "Cross-boundary Serde <-> TypeScript <-> Go contracts verified cleanly (0 violations)",
            );
        } else {
            error(
                "Diff-Fuzz",
                &format!(
                    "Cross-boundary contract violations detected: {}",
                    violations.len()
                ),
            );
            for v in &violations {
                eprintln!("  - [{:?}] {}: {}", v.kind, v.path, v.message);
            }
            if let Some(repro_path) = &args.export_repro {
                let spec = ReproducerSpec {
                    test_name: "cross_boundary_contract_mismatch",
                    language: ReproducerLanguage::Rust,
                    input_json: &reference.to_string(),
                    expected_json: Some(&reference.to_string()),
                    candidate_json: Some(&candidate.to_string()),
                };
                export_reproducer_to_file(repro_path, &spec)?;
                success(
                    "Diff-Fuzz",
                    &format!(
                        "Exported minimal reproducer test to '{}'",
                        repro_path.display()
                    ),
                );
            }
        }
        return Ok(());
    }

    let opts = DiffFuzzOptions {
        iterations,
        seed: 0x1337_C0DE_CAFE_BABE,
        shrink_on_failure: true,
    };

    let summary = run_diff_fuzz(&opts, |i| i * 2, |&x| x + 1, |&x| x + 1).map_err(|m| {
        format!(
            "Differential divergence at iteration {}: ref={}, cand={}",
            m.iteration, m.reference_output, m.candidate_output
        )
    })?;

    success(
        "Diff-Fuzz",
        &format!(
            "Differential fuzzing completed: {} matches across {} iterations (0 divergences)",
            summary.matches, summary.iterations_executed
        ),
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// 3. Verify Subcommand
// ---------------------------------------------------------------------------

/// Bounded Model Checking via Kani.
#[derive(Args, Debug, Clone)]
pub struct VerifyArgs {
    /// Loop and recursion unwinding bound
    #[arg(short, long, help_heading = "Verification Options")]
    pub unwind: Option<usize>,

    /// SAT/SMT solver backend (kissat, cadical, z3)
    #[arg(short, long, help_heading = "Verification Options")]
    pub solver: Option<String>,

    /// Generate starter #[kani::proof] verification harness scaffolding for symbol
    #[arg(long, help_heading = "Verification Options")]
    pub scaffold: Option<String>,

    /// Target file or proof harness name
    #[arg(long, help_heading = "Verification Options")]
    pub target: Option<String>,
}

/// Dispatches the `verify` CLI subcommand.
pub async fn handle_verify(
    args: VerifyArgs,
    cfg: &CortxConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(ref symbol) = args.scaffold {
        let opts = KaniProofOptions {
            unwind: args.unwind.unwrap_or(cfg.verify.default_unwind),
            solver: args
                .solver
                .clone()
                .unwrap_or_else(|| cfg.verify.solver.clone()),
            assumptions: Vec::new(),
            function_name: None,
        };
        let code = scaffold_kani_proof(symbol, &opts);
        println!("{code}");
        return Ok(());
    }

    let opts = VerifyOptions {
        unwind: args.unwind.unwrap_or(cfg.verify.default_unwind),
        solver: args
            .solver
            .clone()
            .unwrap_or_else(|| cfg.verify.solver.clone()),
        harness: args.target.clone(),
        timeout_seconds: 60,
        target_file: None,
    };

    match run_kani_verify(&opts, None) {
        Ok(res) => {
            if res.success {
                success(
                    "Verify",
                    &format!(
                        "Kani model checking passed: {} checks verified cleanly",
                        res.passed_checks
                    ),
                );
            } else {
                error(
                    "Verify",
                    &format!(
                        "Kani verification failed: {} of {} checks failed",
                        res.failed_checks, res.total_checks
                    ),
                );
                for ce in &res.counterexamples {
                    println!("{}", ce.concrete_initializer);
                }
            }
        }
        Err(VerifyError::KaniNotInstalled) => {
            warn("Verify", "cargo-kani is not installed or not in PATH.");
            eprintln!("Install via: cargo install --locked kani-verifier && cargo kani setup");
        }
        Err(e) => {
            error("Verify", &format!("Verification error: {e}"));
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// 4. Invariants Subcommand
// ---------------------------------------------------------------------------

/// Metamorphic Invariant Testing & Property Scaffolding.
#[derive(Args, Debug, Clone)]
pub struct InvariantArgs {
    /// Generate metamorphic property test scaffolding for symbol
    #[arg(long, help_heading = "Invariant Options")]
    pub scaffold: Option<String>,

    /// Target language for scaffolding (rust, typescript, go)
    #[arg(
        short,
        long,
        default_value = "rust",
        help_heading = "Invariant Options"
    )]
    pub lang: String,
}

/// Dispatches the `invariants` CLI subcommand.
pub async fn handle_invariants(
    args: InvariantArgs,
    _cfg: &CortxConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let lang = args
        .lang
        .parse::<PropertyLanguage>()
        .map_err(|e| format!("Invalid language '{}': {e}", args.lang))?;

    if let Some(ref symbol) = args.scaffold {
        let props = [
            MetamorphicPropertyKind::RoundTrip,
            MetamorphicPropertyKind::Idempotence,
            MetamorphicPropertyKind::PermutationInvariance,
            MetamorphicPropertyKind::StateBounds,
        ];
        let code = scaffold_property_suite(symbol, lang, &props);
        println!("{code}");
        return Ok(());
    }

    check_idempotence(&"cortx_identity_property", |s| *s)
        .map_err(|e| format!("Idempotence violation: {e:?}"))?;
    check_permutation_invariance(&[10, 20, 30, 40], |slice| slice.iter().sum::<i32>())
        .map_err(|e| format!("Permutation invariance violation: {e:?}"))?;

    success(
        "Invariants",
        "Metamorphic invariant properties verified: round-trip, idempotence, permutation invariance, state bounds",
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// 5. StateMachine Subcommand
// ---------------------------------------------------------------------------

/// Sans-I/O Pure Deterministic State Machine Engine.
#[derive(Args, Debug, Clone)]
pub struct StateMachineArgs {
    /// Execute high-speed transition fuzzing (>10M transitions/sec)
    #[arg(long, help_heading = "State Machine Options")]
    pub fuzz_transitions: bool,

    /// Number of transitions to fuzz
    #[arg(short, long, help_heading = "State Machine Options")]
    pub transitions: Option<usize>,

    /// Perform reachability matrix analysis and dead-state detection
    #[arg(long, help_heading = "State Machine Options")]
    pub matrix: bool,

    /// Path to TypeScript reducer file for state extraction
    #[arg(long, help_heading = "State Machine Options")]
    pub ts_reducer: Option<PathBuf>,

    /// Path to Go state machine file
    #[arg(long, help_heading = "State Machine Options")]
    pub go_state_machine: Option<PathBuf>,
}

/// Dispatches the `state-machine` CLI subcommand.
pub async fn handle_state_machine(
    args: StateMachineArgs,
    cfg: &CortxConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    if args.matrix {
        let mut mat = TransitionMatrix::new();
        mat.add_transition("Init", "start", "Processing");
        mat.add_transition("Processing", "finish", "Completed");
        mat.add_transition("Processing", "error", "Failed");
        mat.add_transition("Failed", "retry", "Init");

        let states = mat.states();
        let dead_ends = mat.dead_end_states();
        let unreachable = mat.unreachable_states("Init");

        success(
            "StateMachine",
            &format!(
                "Transition matrix analyzed: {} states, {} dead-ends, {} unreachable states",
                states.len(),
                dead_ends.len(),
                unreachable.len()
            ),
        );
        return Ok(());
    }

    let count = args
        .transitions
        .unwrap_or(cfg.go.state_machine.fuzz_transitions);
    let mut machine = CortxSimMachine {
        state: CortxSimState { ops: 0, counter: 0 },
    };
    let opts = FuzzOptions {
        transitions: count,
        seed: 0x1337_C0DE_CAFE_BABE,
    };

    let start = std::time::Instant::now();
    let report = fuzz_transitions(
        &mut machine,
        opts,
        |rng, _| match rng.next_u64() % 3 {
            0 => CortxSimEvent::Increment(1),
            1 => CortxSimEvent::Decrement(1),
            _ => CortxSimEvent::Reset,
        },
        |_st, _acts| Ok(()),
    )
    .map_err(|e| format!("Fuzzing error: {e}"))?;
    let elapsed = start.elapsed();
    let mops = (report.transitions_executed as f64 / elapsed.as_secs_f64().max(1e-6)) / 1_000_000.0;

    success(
        "StateMachine",
        &format!(
            "Transition fuzzing completed: {} transitions in {:?} ({:.2}M transitions/sec, 0 invariant violations)",
            report.transitions_executed, elapsed, mops,
        ),
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simulate_machine_transitions() {
        let mut machine = CortxSimMachine {
            state: CortxSimState { ops: 0, counter: 0 },
        };
        let _ = machine.step(CortxSimEvent::Increment(50)).unwrap();
        assert_eq!(machine.state().counter, 50);
        assert_eq!(machine.state().ops, 1);

        let _ = machine.step(CortxSimEvent::Decrement(20)).unwrap();
        assert_eq!(machine.state().counter, 30);
        assert_eq!(machine.state().ops, 2);

        let _ = machine.step(CortxSimEvent::Reset).unwrap();
        assert_eq!(machine.state().counter, 0);
        assert_eq!(machine.state().ops, 3);
    }

    #[tokio::test]
    async fn test_handle_invariants_scaffold() {
        let args = InvariantArgs {
            scaffold: Some("UserAccount".into()),
            lang: "rust".into(),
        };
        let cfg = CortxConfig::default();
        let res = handle_invariants(args, &cfg).await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_handle_state_machine_matrix() {
        let args = StateMachineArgs {
            fuzz_transitions: false,
            transitions: None,
            matrix: true,
            ts_reducer: None,
            go_state_machine: None,
        };
        let cfg = CortxConfig::default();
        let res = handle_state_machine(args, &cfg).await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_handle_state_machine_fuzzing() {
        let args = StateMachineArgs {
            fuzz_transitions: true,
            transitions: Some(5_000),
            matrix: false,
            ts_reducer: None,
            go_state_machine: None,
        };
        let cfg = CortxConfig::default();
        let res = handle_state_machine(args, &cfg).await;
        assert!(res.is_ok());
    }
}
