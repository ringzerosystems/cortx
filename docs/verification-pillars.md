# The 5 Ground-Truth Verification Pillars

`cortx` embeds five verification oracles (`cortx-harness` and `cortx-state`) to ensure AI-generated code changes do not introduce subtle concurrency, model, or cross-boundary contract regressions.

---

## 1. Pillar 1: Deterministic Simulation Testing (DST) & Concurrency Sweeps (`cortx simulate`)

- **Discrete-Event Simulation:** Runs simulation loops driven by virtual clocks and deterministic pseudo-random seeds.
- **Fault Injection:** Deterministically injects network dropouts, message reordering, latency jitter, and simulated node restarts inspired by FoundationDB and TigerBeetle.
- **Deterministic Replay:** Single-seed reproduction (`--seed <HEX>`) enables instant reproduction of intermittent failures.
- **Go Concurrency Sweeps:** Runs `go test -race` paired with `uber-go/goleak` across randomized `GOMAXPROCS` sweeps to catch race conditions and thread/goroutine leaks.
- **AI Repair Prompts:** When an invariant or seed fails, `cortx` synthesizes a minimal prompt containing seed, trace history, and failing condition ready for an agent to fix.

```bash
# Multi-seed simulation sweep
cortx simulate --seeds 100

# Deterministic replay of a failing seed
cortx simulate --seed 0xdeadbeef

# Go concurrency and leak sweep
cortx simulate --lang go
```

---

## 2. Pillar 2: Differential Fuzzing Oracle & Cross-Boundary Contracts (`cortx diff-fuzz`)

- **Shadow Trait Fuzzing:** Validates candidate implementations against reference models or baseline versions over arbitrary input sequences.
- **Polyglot Serde $\leftrightarrow$ TypeScript $\leftrightarrow$ Go Contracts:**
  - **JavaScript Int53 Bounds:** Flags numeric values exceeding `Number.MAX_SAFE_INTEGER` ($2^{53} - 1$) in serialized 64-bit integers.
  - **Casing Conventions:** Detects divergence between backend fields (`snake_case`) and frontend clients (`camelCase`).
  - **Null vs Empty Slice Invariants:** Detects Go `nil` vs empty slice `[]` JSON drift (`null` vs `[]`).
- **Input Shrinking & Test Export:** Automatically minimizes failing sequence lengths and exports standalone compilable reproduction tests (`--export-repro`).

```bash
# Differential fuzzing against a reference trait
cortx diff-fuzz --target-trait MyContract

# Validate cross-boundary contracts (Serde <-> TS <-> Go)
cortx diff-fuzz --cross-boundary

# Export standalone reproducer test
cortx diff-fuzz --export-repro ./tests/repro_test.rs
```

---

## 3. Pillar 3: Bounded Model Checking via Kani (`cortx verify`)

- **SMT-Based Formal Verification:** Interfaces directly with `cargo kani` to mathematically prove assertions, absence of panics, and array bounds across all possible inputs up to an unwinding depth.
- **Counterexample Synthesis:** Automatically parses Kani JSON solver counterexamples into concrete Rust struct initializers for instant debugging.
- **Proof Scaffolding:** Emits starter Kani verification harnesses with bounded assumptions (`--scaffold <SYMBOL>`).

```bash
# Model check with loop unwinding bounds
cortx verify --unwind 10

# Scaffold a starter Kani proof harness for a function
cortx verify --scaffold process_packet
```

---

## 4. Pillar 4: Metamorphic Invariant Testing (`cortx invariants`)

Validates higher-order domain relationships where absolute output values cannot be known in advance:

- **Round-Trip Invariance:** $f^{-1}(f(x)) = x$ (serialization, AST parsing/unparsing, compression, encryption).
- **Idempotence Invariance:** $f(f(x)) = f(x)$ (formatters, AST distillation, code sanitization, state reconciliation loops).
- **Permutation Invariance:** $f(\pi(x)) = f(x)$ (set operations, metric aggregators, order-independent message handling).
- **State Boundary Invariance:** Monotonic counters, capacity limits, and invariant ranges under randomized event sequences.
- **Multi-Language Scaffolding:** Generates ready-to-run property test suites targeting `proptest` (Rust), `fast-check` (TypeScript), and `pgregory.net/rapid` (Go).

```bash
# Run configured invariant test suites
cortx invariants

# Scaffold property tests for Rust, TypeScript, or Go
cortx invariants --scaffold parse_token --lang rust
cortx invariants --scaffold formatUser --lang typescript
cortx invariants --scaffold ValidateTx --lang go
```

---

## 5. Pillar 5: Sans-I/O Pure Deterministic State Machine Engine (`cortx state-machine`)

- **Pure `#![no_std]` Core:** Trait `StateMachine` decouples state transitions and emitted actions completely from I/O, network sockets, and async runtimes.
- **High-Throughput Fuzzing:** Drives pseudo-random state transitions at $>10,000,000$ transitions/second to detect deadlocks, unhandled events, and transition invariant violations.
- **Transition Matrix Analysis:** Static reachability analysis discovers unreachable states, terminal dead-ends, and unhandled event matrix entries.

```bash
# Execute transition fuzzing at >10M ops/sec
cortx state-machine --fuzz-transitions --transitions 50000

# Run static transition matrix and reachability analysis
cortx state-machine --matrix
```
