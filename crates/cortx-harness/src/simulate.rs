//! Pillar 1: Deterministic Simulation Testing (DST) & Go Concurrency Sweep.
//!
//! Provides discrete-event simulation with virtual clocks, seeded event reordering,
//! fault injection (event drops and delays), deterministic seed replay, Tier 1 Go
//! concurrency sweeps (`go test -race` + `uber-go/goleak`), and AI repair prompt generation.

use std::path::Path;
use std::process::Command;

use cortx_core::config::GoConcurrencyConfig;
use cortx_state::StateMachine;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};

/// Strongly typed errors produced during simulation runs.
#[derive(thiserror::Error, Debug)]
pub enum SimulateError {
    /// Invariant failure during simulation execution.
    #[error("simulation invariant violated at step {step} (virtual time {time_ms}ms): {details}")]
    InvariantViolation {
        /// Failing step number.
        step: usize,
        /// Virtual clock time when violation occurred.
        time_ms: u64,
        /// Description of the invariant violation.
        details: String,
    },
    /// Underlying state machine rejected an event.
    #[error("state machine execution error at step {step}: {details}")]
    MachineError {
        /// Step number where error occurred.
        step: usize,
        /// Debug details of the domain error.
        details: String,
    },
    /// Step budget exceeded without reaching queue completion.
    #[error("step limit of {0} exceeded")]
    StepLimitExceeded(usize),
    /// Simulation run timeout reached.
    #[error("timeout reached after {0} seconds")]
    Timeout(u64),
    /// Go toolchain or command execution failure.
    #[error("Go execution error: {0}")]
    GoExecutionError(String),
    /// Underlying I/O error.
    #[error("I/O error during simulation: {0}")]
    Io(#[from] std::io::Error),
}

/// Simulated monotonic virtual clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct VirtualClock {
    current_time_ms: u64,
}

impl VirtualClock {
    /// Creates a new virtual clock initialized at 0 ms.
    pub const fn new() -> Self {
        Self { current_time_ms: 0 }
    }

    /// Creates a new virtual clock initialized at a specific millisecond offset.
    pub const fn from_ms(ms: u64) -> Self {
        Self {
            current_time_ms: ms,
        }
    }

    /// Returns the current virtual clock timestamp in milliseconds.
    pub fn now_ms(&self) -> u64 {
        self.current_time_ms
    }

    /// Advances the clock forward by the given milliseconds.
    pub fn advance(&mut self, ms: u64) {
        self.current_time_ms = self.current_time_ms.saturating_add(ms);
    }

    /// Jumps the clock directly to `target_ms` if it lies in the future.
    pub fn jump_to(&mut self, target_ms: u64) {
        if target_ms > self.current_time_ms {
            self.current_time_ms = target_ms;
        }
    }
}

impl Default for VirtualClock {
    fn default() -> Self {
        Self::new()
    }
}

/// Fault injection configuration for discrete event simulations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaultConfig {
    /// Probability of dropping an event (0.0 to 1.0).
    pub drop_rate: f64,
    /// Probability of delaying an event past its scheduled time (0.0 to 1.0).
    pub delay_rate: f64,
    /// Maximum additional virtual delay injected into delayed events in milliseconds.
    pub max_delay_ms: u64,
}

impl Default for FaultConfig {
    fn default() -> Self {
        Self {
            drop_rate: 0.0,
            delay_rate: 0.0,
            max_delay_ms: 0,
        }
    }
}

/// A scheduled event pending in the simulation queue.
#[derive(Debug, Clone)]
pub struct ScheduledEvent<E> {
    /// Unique monotonically increasing event identifier.
    pub id: u64,
    /// Scheduled virtual delivery time in milliseconds.
    pub deliver_at_ms: u64,
    /// The event payload.
    pub event: E,
}

/// A recorded step in the deterministic simulation trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceEntry {
    /// Step number in the execution.
    pub step: usize,
    /// Virtual timestamp at execution.
    pub virtual_time_ms: u64,
    /// String summary of the delivered event.
    pub event_description: String,
    /// String summary of emitted actions.
    pub actions_description: String,
    /// Optional description of injected faults.
    pub fault_injected: Option<String>,
    /// State summary after step execution.
    pub state_summary: String,
}

/// Invariant validation function alias.
type InvariantFn<S> = Box<dyn Fn(&S) -> Result<(), String> + Send>;

/// Deterministic Simulation Testing (DST) engine.
pub struct DstEngine<M: StateMachine> {
    machine: M,
    seed: u64,
    clock: VirtualClock,
    fault_config: FaultConfig,
    events: Vec<ScheduledEvent<M::Event>>,
    next_event_id: u64,
    step_count: usize,
    trace: Vec<TraceEntry>,
    prng: StdRng,
    invariants: Vec<InvariantFn<M::State>>,
}

impl<M> DstEngine<M>
where
    M: StateMachine,
    M::State: core::fmt::Debug,
    M::Event: core::fmt::Debug,
    M::Action: core::fmt::Debug,
{
    /// Creates a new `DstEngine` with the given state machine, seed, and fault parameters.
    pub fn new(machine: M, seed: u64, fault_config: FaultConfig) -> Self {
        Self {
            machine,
            seed,
            clock: VirtualClock::new(),
            fault_config,
            events: Vec::new(),
            next_event_id: 1,
            step_count: 0,
            trace: Vec::new(),
            prng: StdRng::seed_from_u64(seed),
            invariants: Vec::new(),
        }
    }

    /// Registers a state invariant that must hold before and after every step.
    pub fn add_invariant<I>(&mut self, invariant: I)
    where
        I: Fn(&M::State) -> Result<(), String> + Send + 'static,
    {
        self.invariants.push(Box::new(invariant));
    }

    /// Schedules an event to be delivered after `delay_ms` virtual milliseconds.
    pub fn schedule(&mut self, event: M::Event, delay_ms: u64) {
        let deliver_at_ms = self.clock.now_ms().saturating_add(delay_ms);
        let id = self.next_event_id;
        self.next_event_id = self.next_event_id.saturating_add(1);
        self.events.push(ScheduledEvent {
            id,
            deliver_at_ms,
            event,
        });
    }

    /// Returns a reference to the virtual clock.
    pub fn clock(&self) -> &VirtualClock {
        &self.clock
    }

    /// Returns the current state machine state.
    pub fn state(&self) -> &M::State {
        self.machine.state()
    }

    /// Returns total executed steps so far.
    pub fn step_count(&self) -> usize {
        self.step_count
    }

    /// Returns the simulation seed.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Returns the recorded execution trace.
    pub fn trace(&self) -> &[TraceEntry] {
        &self.trace
    }

    /// Selects the next eligible event deterministically from the queue.
    fn pop_next_event(&mut self) -> Option<ScheduledEvent<M::Event>> {
        if self.events.is_empty() {
            return None;
        }
        let earliest_time = self.events.iter().map(|e| e.deliver_at_ms).min()?;
        self.clock.jump_to(earliest_time);

        let candidate_indices: Vec<usize> = self
            .events
            .iter()
            .enumerate()
            .filter(|(_, e)| e.deliver_at_ms == earliest_time)
            .map(|(i, _)| i)
            .collect();

        let chosen_slot = self.prng.gen_range(0..candidate_indices.len());
        let chosen_index = candidate_indices[chosen_slot];
        Some(self.events.swap_remove(chosen_index))
    }

    /// Advances the simulation by a single discrete event step.
    ///
    /// Returns `Ok(Some(actions))` on execution, `Ok(None)` if queue is empty,
    /// or `Err(SimulateError)` on machine failure or invariant violation.
    pub fn step(&mut self) -> Result<Option<Vec<M::Action>>, SimulateError> {
        let scheduled = match self.pop_next_event() {
            Some(e) => e,
            None => return Ok(None),
        };

        if self.fault_config.drop_rate > 0.0
            && self
                .prng
                .gen_bool(self.fault_config.drop_rate.clamp(0.0, 1.0))
        {
            self.trace.push(TraceEntry {
                step: self.step_count,
                virtual_time_ms: self.clock.now_ms(),
                event_description: format!("{:?}", scheduled.event),
                actions_description: "[]".into(),
                fault_injected: Some("DROPPED".into()),
                state_summary: format!("{:?}", self.machine.state()),
            });
            return Ok(Some(Vec::new()));
        }

        if self.fault_config.delay_rate > 0.0
            && self.fault_config.max_delay_ms > 0
            && self
                .prng
                .gen_bool(self.fault_config.delay_rate.clamp(0.0, 1.0))
        {
            let added_delay = self.prng.gen_range(1..=self.fault_config.max_delay_ms);
            let new_time = self.clock.now_ms().saturating_add(added_delay);
            self.trace.push(TraceEntry {
                step: self.step_count,
                virtual_time_ms: self.clock.now_ms(),
                event_description: format!("{:?}", scheduled.event),
                actions_description: "[]".into(),
                fault_injected: Some(format!("DELAYED by {added_delay}ms")),
                state_summary: format!("{:?}", self.machine.state()),
            });
            self.events.push(ScheduledEvent {
                id: scheduled.id,
                deliver_at_ms: new_time,
                event: scheduled.event,
            });
            return Ok(Some(Vec::new()));
        }

        self.step_count += 1;
        let event_desc = format!("{:?}", scheduled.event);
        let actions =
            self.machine
                .step(scheduled.event)
                .map_err(|e| SimulateError::MachineError {
                    step: self.step_count,
                    details: format!("{e:?}"),
                })?;

        for inv in &self.invariants {
            if let Err(details) = inv(self.machine.state()) {
                return Err(SimulateError::InvariantViolation {
                    step: self.step_count,
                    time_ms: self.clock.now_ms(),
                    details,
                });
            }
        }

        self.trace.push(TraceEntry {
            step: self.step_count,
            virtual_time_ms: self.clock.now_ms(),
            event_description: event_desc,
            actions_description: format!("{actions:?}"),
            fault_injected: None,
            state_summary: format!("{:?}", self.machine.state()),
        });

        Ok(Some(actions))
    }

    /// Runs the simulation until all events are exhausted or `max_steps` is reached.
    pub fn run_until_empty(&mut self, max_steps: usize) -> Result<usize, SimulateError> {
        let mut steps = 0;
        while steps < max_steps {
            match self.step()? {
                Some(_) => steps += 1,
                None => return Ok(steps),
            }
        }
        if !self.events.is_empty() {
            Err(SimulateError::StepLimitExceeded(max_steps))
        } else {
            Ok(steps)
        }
    }
}

/// Simulation run options.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimulateOptions {
    /// Total number of seeds to execute in sweep mode.
    pub seeds: u64,
    /// Explicit specific seed to replay (e.g. `0xDEADBEEF`).
    pub specific_seed: Option<u64>,
    /// Execution timeout in seconds.
    pub timeout_seconds: u64,
    /// Fault injection parameters.
    pub fault_config: FaultConfig,
    /// Maximum step budget per seed.
    pub max_steps: usize,
}

impl Default for SimulateOptions {
    fn default() -> Self {
        Self {
            seeds: 100,
            specific_seed: None,
            timeout_seconds: 30,
            fault_config: FaultConfig::default(),
            max_steps: 10_000,
        }
    }
}

/// Successful summary for a single seed simulation run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimulationSummary {
    /// Seed used for execution.
    pub seed: u64,
    /// Total steps processed.
    pub steps_executed: usize,
    /// Final virtual clock time in milliseconds.
    pub virtual_time_ms: u64,
}

/// Detailed report generated when a simulation seed fails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedSeedReport {
    /// The exact seed reproducing the failure.
    pub seed: u64,
    /// Step number where failure occurred.
    pub step_count: usize,
    /// Virtual timestamp at failure.
    pub virtual_time_ms: u64,
    /// Error message.
    pub error_message: String,
    /// Execution trace leading to the failure.
    pub trace: Vec<TraceEntry>,
    /// Actionable markdown repair prompt for AI coding agents.
    pub repair_prompt: String,
}

/// Aggregate results from a multi-seed simulation sweep.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationResult {
    /// Total seeds evaluated.
    pub total_seeds: u64,
    /// Count of passing seeds.
    pub passed_seeds: u64,
    /// Report of the first failing seed, if any.
    pub failed_seed: Option<FailedSeedReport>,
    /// Total real-world execution duration in milliseconds.
    pub duration_ms: u128,
}

/// Generates a structured AI agent prompt for diagnosing and repairing simulation bugs.
pub fn generate_ai_repair_prompt(
    seed: u64,
    error_message: &str,
    step_count: usize,
    virtual_time_ms: u64,
    trace: &[TraceEntry],
) -> String {
    let window_start = trace.len().saturating_sub(10);
    let trace_window = &trace[window_start..];

    let mut rows = String::new();
    for entry in trace_window {
        let fault = entry.fault_injected.as_deref().unwrap_or("-");
        rows.push_str(&format!(
            "| {} | {}ms | `{}` | `{}` | {} | `{}` |\n",
            entry.step,
            entry.virtual_time_ms,
            entry.event_description,
            entry.actions_description,
            fault,
            entry.state_summary
        ));
    }

    format!(
        "### 🚨 Deterministic Simulation Testing (DST) Invariant Violation\n\n\
        **Seed:** `0x{seed:016X}` (Replay with `cortx simulate --seed 0x{seed:016X}`)\n\
        **Failing Step:** {step_count}\n\
        **Virtual Time:** {virtual_time_ms} ms\n\
        **Failure Reason:** {error_message}\n\n\
        #### Execution Trace (Last {} Steps)\n\
        | Step | Time | Event | Actions | Fault | State After |\n\
        | :--- | :--- | :--- | :--- | :--- | :--- |\n\
        {rows}\n\
        #### Agent Action Required\n\
        1. Deterministically reproduce this failure: `cortx simulate --seed 0x{seed:016X}`\n\
        2. Inspect the sequence of events and state transitions in the trace above.\n\
        3. Identify the missing guard, illegal transition, or race condition in the state machine.\n\
        4. Apply the fix and verify that seed `0x{seed:016X}` passes.\n",
        trace_window.len()
    )
}

/// Runs a single deterministic simulation seed.
pub fn run_single_seed<M, F>(
    seed: u64,
    builder: F,
    max_steps: usize,
) -> Result<SimulationSummary, FailedSeedReport>
where
    M: StateMachine,
    M::State: core::fmt::Debug,
    M::Event: core::fmt::Debug,
    M::Action: core::fmt::Debug,
    F: FnOnce(u64) -> DstEngine<M>,
{
    let mut engine = builder(seed);
    match engine.run_until_empty(max_steps) {
        Ok(steps) => Ok(SimulationSummary {
            seed,
            steps_executed: steps,
            virtual_time_ms: engine.clock().now_ms(),
        }),
        Err(err) => {
            let err_msg = err.to_string();
            let repair_prompt = generate_ai_repair_prompt(
                seed,
                &err_msg,
                engine.step_count(),
                engine.clock().now_ms(),
                engine.trace(),
            );
            Err(FailedSeedReport {
                seed,
                step_count: engine.step_count(),
                virtual_time_ms: engine.clock().now_ms(),
                error_message: err_msg,
                trace: engine.trace().to_vec(),
                repair_prompt,
            })
        }
    }
}

/// Runs a multi-seed simulation sweep, stopping on the first failing seed.
pub fn run_simulation<M, F>(options: &SimulateOptions, mut builder: F) -> SimulationResult
where
    M: StateMachine,
    M::State: core::fmt::Debug,
    M::Event: core::fmt::Debug,
    M::Action: core::fmt::Debug,
    F: FnMut(u64) -> DstEngine<M>,
{
    let start = std::time::Instant::now();
    let seeds_to_run: Vec<u64> = if let Some(specific) = options.specific_seed {
        vec![specific]
    } else {
        (0..options.seeds).collect()
    };

    let total_seeds = seeds_to_run.len() as u64;
    let mut passed_seeds = 0;
    let mut failed_seed = None;

    for seed in seeds_to_run {
        match run_single_seed(seed, &mut builder, options.max_steps) {
            Ok(_) => passed_seeds += 1,
            Err(failure) => {
                failed_seed = Some(failure);
                break;
            }
        }
    }

    SimulationResult {
        total_seeds,
        passed_seeds,
        failed_seed,
        duration_ms: start.elapsed().as_millis(),
    }
}

/// Tier 1 Go concurrency sweep result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoSweepResult {
    /// Number of passes executed across GOMAXPROCS configurations.
    pub passes_executed: u64,
    /// List of distinct GOMAXPROCS configurations tested.
    pub gomaxprocs_tested: Vec<usize>,
    /// Whether all executed test passes succeeded without race or leak detections.
    pub passed: bool,
    /// True if data race was detected by Go's race detector.
    pub race_detected: bool,
    /// True if goroutine leak was detected by goleak or test output.
    pub goroutine_leak_detected: bool,
    /// Captured details from the failing test run.
    pub failure_details: Option<String>,
    /// Actionable repair prompt for AI coding agents.
    pub ai_repair_prompt: Option<String>,
}

/// Checks whether `go` executable is available in PATH.
fn check_go_installed() -> Result<(), SimulateError> {
    match Command::new("go").arg("version").output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(_) => Err(SimulateError::GoExecutionError(
            "`go version` returned non-zero status".into(),
        )),
        Err(err) => Err(SimulateError::GoExecutionError(format!(
            "Go binary not found: {err}"
        ))),
    }
}

/// Checks whether a directory contains Go files.
fn has_go_files(dir: &Path) -> bool {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "go")
                || path.file_name().is_some_and(|n| n == "go.mod")
            {
                return true;
            }
        }
    }
    false
}

/// Generates an actionable AI repair prompt for Go concurrency and race detector failures.
pub fn generate_go_repair_prompt(
    gomaxprocs: usize,
    race_detected: bool,
    leak_detected: bool,
    output: &str,
) -> String {
    let truncated = if output.len() > 3000 {
        &output[..3000]
    } else {
        output
    };

    format!(
        "### 🚨 Go Concurrency / Race Detector Failure Detected\n\n\
        **Environment:** `GOMAXPROCS={gomaxprocs}`\n\
        **Data Race Detected:** {race_detected}\n\
        **Goroutine Leak Detected:** {leak_detected}\n\n\
        #### Output Trace\n\
        ```text\n\
        {truncated}\n\
        ```\n\n\
        #### Agent Action Required\n\
        1. Reproduce locally: `GOMAXPROCS={gomaxprocs} go test -race -count=1 ./...`\n\
        2. Identify unsynchronized concurrent memory access or unclosed goroutines from the trace.\n\
        3. Protect shared variables using `sync.Mutex` or `sync/atomic`.\n\
        4. Re-run tests to verify the race detector passes cleanly.\n"
    )
}

/// Runs Tier 1 Go concurrency sweep across randomized or cycled GOMAXPROCS settings.
pub fn run_go_concurrency_sweep(
    dir: &Path,
    config: &GoConcurrencyConfig,
) -> Result<GoSweepResult, SimulateError> {
    if !has_go_files(dir) {
        return Ok(GoSweepResult {
            passes_executed: 0,
            gomaxprocs_tested: Vec::new(),
            passed: true,
            race_detected: false,
            goroutine_leak_detected: false,
            failure_details: Some("No Go files found in target directory".into()),
            ai_repair_prompt: None,
        });
    }

    check_go_installed()?;

    let sweep = if config.gomaxprocs_sweep.is_empty() {
        vec![1, 2, 4, 8]
    } else {
        config.gomaxprocs_sweep.clone()
    };

    let mut tested = Vec::new();
    let mut passes_done = 0;

    for pass in 0..config.default_passes {
        let gomaxprocs = sweep[pass as usize % sweep.len()];
        if !tested.contains(&gomaxprocs) {
            tested.push(gomaxprocs);
        }

        let mut cmd = Command::new("go");
        cmd.current_dir(dir);
        cmd.arg("test");
        if config.race_detector {
            cmd.arg("-race");
        }
        cmd.args(["-count=1", "./..."]);
        cmd.env("GOMAXPROCS", gomaxprocs.to_string());

        let output = cmd.output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{stdout}\n{stderr}");

        let race_detected = combined.contains("WARNING: DATA RACE");
        let leak_detected = config.goleak_check
            && (combined.contains("goleak") || combined.contains("unexpected goroutines"));

        if race_detected || leak_detected || !output.status.success() {
            let prompt =
                generate_go_repair_prompt(gomaxprocs, race_detected, leak_detected, &combined);
            return Ok(GoSweepResult {
                passes_executed: passes_done + 1,
                gomaxprocs_tested: tested,
                passed: false,
                race_detected,
                goroutine_leak_detected: leak_detected,
                failure_details: Some(combined),
                ai_repair_prompt: Some(prompt),
            });
        }

        passes_done += 1;
    }

    Ok(GoSweepResult {
        passes_executed: passes_done,
        gomaxprocs_tested: tested,
        passed: true,
        race_detected: false,
        goroutine_leak_detected: false,
        failure_details: None,
        ai_repair_prompt: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct BankState {
        balance: i64,
        transfers: usize,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum BankEvent {
        Deposit(i64),
        Withdraw(i64),
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum BankAction {
        Log(String),
    }

    struct BankMachine {
        state: BankState,
    }

    impl StateMachine for BankMachine {
        type State = BankState;
        type Event = BankEvent;
        type Action = BankAction;
        type Error = String;

        fn step(&mut self, event: Self::Event) -> Result<Vec<Self::Action>, Self::Error> {
            match event {
                BankEvent::Deposit(amt) => {
                    self.state.balance += amt;
                    self.state.transfers += 1;
                    Ok(vec![BankAction::Log(format!("Deposited {amt}"))])
                }
                BankEvent::Withdraw(amt) => {
                    self.state.balance -= amt;
                    self.state.transfers += 1;
                    Ok(vec![BankAction::Log(format!("Withdrew {amt}"))])
                }
            }
        }

        fn state(&self) -> &Self::State {
            &self.state
        }
    }

    #[test]
    fn test_virtual_clock() {
        let mut clock = VirtualClock::new();
        assert_eq!(clock.now_ms(), 0);
        clock.advance(50);
        assert_eq!(clock.now_ms(), 50);
        clock.jump_to(200);
        assert_eq!(clock.now_ms(), 200);
        clock.jump_to(100);
        assert_eq!(clock.now_ms(), 200);
    }

    #[test]
    fn test_simulation_determinism_same_seed() {
        let setup = |seed| {
            let machine = BankMachine {
                state: BankState {
                    balance: 100,
                    transfers: 0,
                },
            };
            let mut engine = DstEngine::new(machine, seed, FaultConfig::default());
            engine.schedule(BankEvent::Deposit(50), 10);
            engine.schedule(BankEvent::Withdraw(20), 10);
            engine.schedule(BankEvent::Deposit(100), 20);
            engine.schedule(BankEvent::Withdraw(30), 10);
            engine
        };

        let mut engine1 = setup(0x1337);
        let mut engine2 = setup(0x1337);

        engine1.run_until_empty(100).expect("engine1 failed");
        engine2.run_until_empty(100).expect("engine2 failed");

        assert_eq!(engine1.trace(), engine2.trace());
        assert_eq!(engine1.state(), engine2.state());
    }

    #[test]
    fn test_fault_injection_drop_and_delay() {
        let machine = BankMachine {
            state: BankState {
                balance: 100,
                transfers: 0,
            },
        };
        let fault_config = FaultConfig {
            drop_rate: 0.3,
            delay_rate: 0.3,
            max_delay_ms: 50,
        };
        let mut engine = DstEngine::new(machine, 42, fault_config);
        for i in 0..20 {
            engine.schedule(BankEvent::Deposit(i * 10), i as u64 * 5);
        }

        let steps = engine.run_until_empty(200).expect("run failed");
        assert!(steps > 0);

        let has_fault = engine.trace().iter().any(|t| t.fault_injected.is_some());
        assert!(has_fault, "fault config should inject drop or delay");
    }

    #[test]
    fn test_invariant_violation_and_repair_prompt() {
        let builder = |seed| {
            let machine = BankMachine {
                state: BankState {
                    balance: 50,
                    transfers: 0,
                },
            };
            let mut engine = DstEngine::new(machine, seed, FaultConfig::default());
            engine.add_invariant(|st| {
                if st.balance < 0 {
                    Err(format!("Balance went negative: {}", st.balance))
                } else {
                    Ok(())
                }
            });
            engine.schedule(BankEvent::Withdraw(100), 5);
            engine
        };

        let result = run_single_seed(0xDEADBEEF, builder, 10);
        assert!(result.is_err());
        let failure = result.unwrap_err();
        assert_eq!(failure.seed, 0xDEADBEEF);
        assert!(failure.error_message.contains("Balance went negative"));
        assert!(failure.repair_prompt.contains("0x00000000DEADBEEF"));
        assert!(failure.repair_prompt.contains("Agent Action Required"));
    }

    #[test]
    fn test_run_simulation_sweep() {
        let builder = |seed| {
            let machine = BankMachine {
                state: BankState {
                    balance: 1000,
                    transfers: 0,
                },
            };
            let mut engine = DstEngine::new(machine, seed, FaultConfig::default());
            engine.schedule(BankEvent::Deposit(10), 1);
            engine.schedule(BankEvent::Withdraw(5), 2);
            engine
        };

        let options = SimulateOptions {
            seeds: 10,
            ..Default::default()
        };
        let result = run_simulation(&options, builder);
        assert_eq!(result.total_seeds, 10);
        assert_eq!(result.passed_seeds, 10);
        assert!(result.failed_seed.is_none());
    }

    #[test]
    fn test_go_concurrency_sweep_no_go_files() {
        let temp = tempfile::tempdir().unwrap();
        let config = GoConcurrencyConfig::default();
        let res = run_go_concurrency_sweep(temp.path(), &config).unwrap();
        assert!(res.passed);
        assert_eq!(res.passes_executed, 0);
    }

    #[test]
    fn test_go_repair_prompt_generation() {
        let prompt = generate_go_repair_prompt(
            4,
            true,
            false,
            "WARNING: DATA RACE\nWrite at 0x00c000094010 by goroutine 7",
        );
        assert!(prompt.contains("GOMAXPROCS=4"));
        assert!(prompt.contains("DATA RACE"));
        assert!(prompt.contains("Agent Action Required"));
    }
}
