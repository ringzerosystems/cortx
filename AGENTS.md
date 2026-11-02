# AGENTS.md

Guidance for AI coding agents working in this repository. This is a Rust **2024** (MSRV **1.85.0**) workspace focused on high-performance AST distillation and compiler-aware context delivery for large-scale codebases (>1.2M LOC). The rules below distill *Rust for Rustaceans* (Gjengset), *Programming Rust* (Blandy, Orendorff, Tindall), *The Rustonomicon*, *Zero To Production In Rust* (Berman), Tokio runtime and async design patterns, and the ground-truth verification methodologies from FoundationDB, TigerBeetle, and `pgrust`. Follow them unless a task explicitly says otherwise.

## Platform Targets

- **Windows 10 and 11**: Windows Service & Console application.
- **Linux GNU**: Systemd, and command line application.
- **MacOS Darwin**: Apple Silicon, Launchd and command line application.

## Project

- **Rust 2024 / MSRV 1.85.0**: `Cargo.toml` declares edition `2024` and `rust-version = "1.85.0"`. `rust-toolchain.toml` pins the active toolchain (`clippy`, `rustfmt`, `miri`, `rust-analyzer`, `rust-src`). Local toolchain overrides are managed exclusively through `rust-toolchain.toml`.
- **Layout (Cargo Workspace)**:
  - `crates/cargo-cortx` (or root binary crate): executable CLI entrypoint and cargo subcommand router (`cargo cortx <COMMAND>`). Dispatches to subcommands: `distill`, `daemon`.
  - `crates/cortx-core`: workspace foundation:
    - CLI argument parsing via `clap` (derive API with structured subcommands).
    - Configuration loading and management: strictly centralized configuration model loaded from the system path (`~/.config/cortx/cortx.toml`, `$XDG_CONFIG_HOME/cortx/cortx.toml`, `/etc/cortx/cortx.toml`) or explicitly via the `--config` switch. Enforces strict CWD isolation (no repository-local config candidate searching), supporting daemon infrastructure (`[daemon]`), declarative project namespaces (`[projects.<name>]` / `[<project-name>]`), `ProjectConfig`, `TypeScriptConfig`, `GoConfig`, and `CrossBoundaryConfig`.
    - Structured logging initialization (`tracing`, `tracing-subscriber`).
    - Standardized terminal diagnostics (`colored`, `anstream`).
    - Core error primitives (`thiserror`).
  - `crates/cortx-distill`: AST parsing and compiler-aware context distillation engine:
    - Fast Rust AST parser using `syn` and `quote` (with fallback to `tree-sitter-rust` for resilient parsing during syntax errors).
    - Fast TypeScript / TSX AST parser using `oxc_parser` (`oxc_allocator`, `oxc_ast`, `oxc_span`).
    - Fast Go AST parser and interface resolver using `tree-sitter-go`.
    - Polyglot type skeleton stripper: empties Rust function/method bodies (`syn::visit_mut::VisitMut`), strips TypeScript function bodies, React hook internals (`useEffect`, `useState`), and JSX elements (`extract_ts_skeleton`), and strips Go function and method bodies (`extract_go_skeleton`) to `/* implementation omitted */`, preserving visibility, exported types, interfaces, contracts, and signatures.
    - Implicit interface & method-set resolver: computes value receiver $\mathcal{M}(T)$ and pointer receiver $\mathcal{M}(*T)$ method sets to resolve structural interface satisfaction ($T \text{ satisfies } I \iff \mathcal{M}(I) \subseteq \mathcal{M}(T)$, $*T \text{ satisfies } I \iff \mathcal{M}(I) \subseteq \mathcal{M}(*T)$).
    - Semantic call-graph, contract, and cross-boundary indexer: ingests local SCIP (`scip-rust`) SQLite databases, interfaces via JSON-RPC with `rust-analyzer`, extracts Go HTTP routes (Gin, Chi, Echo, `net/http`), and correlates backend Rust/Go routes/models with frontend TypeScript fetch calls and consumer interfaces (`CrossBoundaryImpact`).
    - Markdown and JSON formatters for agent prompt injection.
  - `crates/cortx-daemon`: agent sidecar and background indexing daemon:
    - High-throughput system-wide multi-tenant IPC server (`$XDG_RUNTIME_DIR/cortx/daemon.sock`, fallback to `/tmp/cortx-<hash>.sock` or `--isolated`) servicing all repositories and AI agent sessions.
    - Lazy on-demand cold starts on first touch with single-flight initialization, Copy-on-Write overlay indexing for Git worktrees, longest-prefix project resolution against declarative system configuration, and strict CWD isolation (daemon never loads untrusted workspace-local configs).
    - Dynamic multi-path file watcher via `notify` monitoring `.rs`, `.ts`, `.tsx`, `.js`, `.jsx`, and `.go` scoped to per-project `watch_paths` and `ignored_dirs`.
    - Multi-tier file logging (timestamped daemon log + isolated per-workspace log files) and process RSS/heap memory telemetry in MB.
    - Non-destructive tool hook handlers (`hook post-read`).
- **NON-DESTRUCTIVE ADVISORY CONTEXT INJECTION (CRITICAL INVARIANT)**:
  - **Preserve target source files strictly byte-identical on disk at all times during tool hook execution** (e.g., `hook post-read` across `.rs`, `.ts`, `.tsx`, `.js`, `.jsx`, `.go`). Keep line numbers and byte offsets perfectly stable so downstream patch and diff editing tools (Claude Code, Antigravity, OpenCode) operate with complete accuracy.
  - **Deliver all injected context** (skeletons, caller hierarchies, trait contracts, cross-boundary bindings, invariants) **exclusively out-of-band** as advisory stdout, JSON-RPC responses, or formatted markdown blocks.
- **`make` & `cargo` Workflow**:
  - Execute builds, tests, and quality checks exclusively through defined `make <target>` recipes and standard `cargo` commands. Maintain all tool dependencies (`cargo-audit`, `cargo-deny`, `cargo-machete`, `cargo-kani`, `cargo-bolero`, `cargo-miri`) strictly pinned in workspace configuration.

## Commands

```sh
make help              # list available targets
make all               # build all workspace binaries and crates
make check             # fast workspace compile check (cargo check --workspace --all-targets)
make build             # optimized release binaries -> target/release/*
make build-debug       # unoptimized debug binaries -> target/debug/*
make install           # compile release build and install into system cargo path
make test              # unit & integration tests (cargo test --workspace --all-targets)
make bench             # run criterion benchmarks -> target/criterion/
make bench-compare     # compare baseline against candidate benchmark results
make qa                # strict QA sequence: fmt-check -> clippy -> deny -> machete -> loc
make fmt               # cargo fmt --all (rewrites in-place)
make fmt-check         # cargo fmt --all -- --check (fails on unformatted files)
make clippy            # cargo clippy --workspace --all-targets --all-features -- -D warnings
make deny              # cargo deny check (licenses, bans, duplicate crates, advisories)
make machete           # cargo machete (detect unused dependencies in Cargo.toml)
make audit             # cargo audit (RustSec security vulnerability scan; requires network)
make loc               # scripts/loc.sh: fail on any Rust file over 1500 lines of code
make clean             # cargo clean

# CLI & Engine Tool Invocations (`cargo cortx`)
cargo cortx distill <FILE>                                           # extract type skeleton (Rust / TS / TSX / Go)
cargo cortx distill <FILE> --cross-boundary                          # distill with Rust <-> TS <-> Go route & interface correlation
cargo cortx daemon                                                 # start system-wide multi-tenant indexing daemon (or --isolated)
cargo cortx daemon --config <PATH>                                 # start daemon with explicit configuration file override
cargo cortx daemon --ping                                          # probe running daemon liveness and latency
cargo cortx daemon --status                                        # query multi-tenant registry, RSS memory in MB, and log paths
cargo cortx daemon --stop                                          # gracefully stop running background daemon
cargo cortx daemon --hook-post-read --file <PATH>                  # advisory post-read tool hook (.rs / .ts / .tsx / .go)
```

- **Run `cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test` immediately after editing any Rust source file (`.rs`).** Restrict execution of the QA suite strictly to Rust source file modifications, reserving documentation, Markdown files (`.md`), and non-Rust assets for direct updates. In the event of a compiler or linter diagnostic, report the complete output immediately, and proceed only once both clippy and the test suite pass with zero errors and zero warnings.
- `make qa` execution sequence:
  1. `fmt-check`: verify syntax layout so subsequent compiler line reports remain perfectly accurate.
  2. `clippy`: enforce strict static analysis with `-D warnings`, surfacing optimal idioms, clean borrows, and minimal allocations.
  3. `deny`: verify crate security advisories, confirm license compliance, and enforce unified dependency trees.
  4. `machete`: ensure `Cargo.toml` contains exclusively active dependencies to keep build times minimal.
  5. `loc`: enforce modularity by requiring every Rust source file to remain within 1,500 lines of code (`scripts/loc.sh`).
- **Dependency updates**: Run `cargo check` immediately after editing `Cargo.toml` to synchronize `Cargo.lock`. Commit `Cargo.toml` and `Cargo.lock` paired together in the same atomic commit.
- **Microbenchmarks**: Place benchmarks in `benches/` using `criterion`. Fully isolate benchmark setup using `b.iter_batched` and evaluate outputs through `criterion::black_box` to guarantee accurate compiler measurement. Capture baseline numbers with `make bench` and validate improvements via `make bench-compare`.

## Architecture Governance & Crate Boundaries

The codebase enforces strict layer boundaries and structural rules:

- **Dependency Boundaries**:
  - `crates/cortx-distill`: Restrict dependencies strictly to AST parsing (`syn`, `quote`, `tree-sitter`, `tree-sitter-rust`, `tree-sitter-go`, `oxc_parser`, `oxc_allocator`, `oxc_ast`, `oxc_span`, `oxc_resolver`, `globset`, `toml`) and symbol query layers.
  - `crates/cortx-core`: Maintain total independence as the shared foundation, containing exclusively common types, configuration models, and error primitives.
  - `crates/cargo-cortx` (CLI router) and `crates/cortx-daemon`: Assemble and coordinate the lower-tier library crates.
- **Function and File Caps**:
  - Domain logic, parsers, and AST distillation algorithms: Keep functions within 80 lines, parameters to 4 or fewer (bundle larger parameter sets into typed request structs or builders), and return values to structured structs or enums.
  - CLI and daemon command routers: Keep functions within 100 lines and parameters to 4 or fewer.
  - All source files: Keep total length within 1,500 lines of code (`scripts/loc.sh`, excluding comments and blank lines).

## Using Imported Crates: `cargo doc` First

Always inspect APIs directly through `cargo doc` or local crate documentation to confirm method signatures and exact trait implementations. Treat the local `Cargo.lock` as the authoritative source of truth:

```sh
cargo doc --package syn --no-deps --open          # inspect syn AST types and visitor traits
cargo doc --package oxc_parser --no-deps --open   # inspect oxc TypeScript/JS parser AST and options
cargo doc --package tokio --no-deps --open        # view tokio runtime and synchronization primitives
cargo doc --package madsim --no-deps --open       # view deterministic simulation runtime APIs
cargo doc --package bolero --no-deps --open       # view differential fuzzing harness macros
cargo doc --package kani --no-deps --open         # view bounded model checking proof attributes
cargo doc --package tracing --no-deps --open      # view structured diagnostic instrument APIs
```

- **Inspect imported crate documentation prior to first use in any file**: Confirm required trait bounds (`Send`, `Sync`, `Clone`, `Deref`), lifetime constraints, and error variants before calling unfamiliar APIs.
- **Prioritize standard library primitives and existing workspace crates**: Implement functionality with existing dependencies and standard library features whenever possible. Keep the dependency graph lean and minimal.
- **Strictly adhere to documented concurrency invariants**: When crate documentation outlines thread-safety or async constraints (such as cancellation sensitivity or lock duration), follow those constraints rigorously and annotate the call site with explicit architectural rationale.

---

## 1. Concurrency & Async Architecture (Gjengset, Blandy et al., Tokio)

**Concurrency is structural; parallelism is hardware.** In Rust, the type system enforces data race freedom through `Send` and `Sync`, while deadlocks, starvation, cancellation races, and non-deterministic event orderings require rigorous architectural discipline.

- **Communicate via Explicit Ownership Transfer (Channels & Ownership)**:
  - Default to transferring data ownership across threads and tasks via channels.
  - Reserve mutexes strictly for protecting small, local internal state (an in-memory cache, a token bucket, a state machine).
  - Decision rule: Transferring ownership or pipeline coordination $\to$ channels; protecting shared in-memory structs $\to$ `parking_lot::Mutex` or `tokio::sync::Mutex`.
- **Channel Discipline & Bounded Queues**:
  - **Enforce bounded channels across all production pipelines**: Use `tokio::sync::mpsc::channel(capacity)` with deliberate, measured capacity to guarantee backpressure propagation and protect process memory.
  - **Single-producer/single-consumer single result**: Use `tokio::sync::oneshot` for request-response RPC pairs between actors and callers.
  - **State distribution / configuration updates**: Use `tokio::sync::watch` for broadcasting the latest value to multiple consumers where intermediate dropped states are acceptable.
  - **Fan-out event pub/sub**: Use `tokio::sync::broadcast` for broadcast delivery where every receiver must observe messages within a ring buffer.
  - **Channel ownership**: Assign channel lifecycle ownership strictly to the sender task. Allow sender handles to drop to close the channel cleanly; handle closure on the receiving end by matching `None` on `.recv().await`.
- **Async Task Lifecycles & Graceful Shutdown**:
  - **Define explicit exit conditions and designated owners for every spawned task.** Ensure every `tokio::spawn(async move { ... })` call has an explicit owner and an identifiable completion or cancellation path.
  - Retain `JoinHandle<T>` or register spawned tasks in a `tokio::task::JoinSet`. Await task completion and handle panics and errors explicitly.
  - **Hierarchical shutdown trees**: Use `tokio_util::sync::CancellationToken` for hierarchical cancellation. Pass child tokens down from parent services; on `SIGINT` or `SIGTERM`, cancel the root token to cascade shutdown deterministically down the entire task tree.
- **Cancellation Safety in `tokio::select!`**:
  - In Rust async, dropping a `Future` immediately cancels it. In a `tokio::select!` block, the first branch that completes causes all other branches to drop.
  - **Ensure every future evaluated inside a `tokio::select!` branch is provably cancellation-safe.** Restrict `select!` branches to operations whose drop behavior leaves system state completely consistent.
  - Cancellation-safe operations: `mpsc::Receiver::recv`, `watch::Receiver::changed`, `oneshot::Receiver`.
  - Isolate multi-step stateful operations (`AsyncReadExt::read_exact`, unbuffered framing decoders, multi-stage database transactions) inside dedicated actor tasks, or execute them to completion prior to entering a `select!` block.
  - Apply `biased;` in `tokio::select!` when explicit priority ordering is required; otherwise rely on standard pseudo-random fairness.
- **Mutex Discipline Across `.await` Boundaries**:
  - **Release all synchronous mutex guards (`std::sync::Mutex`, `parking_lot::Mutex`) before reaching an `.await` boundary.** Keep synchronous critical sections small and tightly scoped so guards drop immediately before async suspension points:

    ```rust
    let payload = {
        let mut guard = state.lock();
        guard.prepare_payload()
    }; // guard dropped here
    client.send(payload).await?;
    ```

  - Use `tokio::sync::Mutex` exclusively when mutable state must remain locked across an `.await` call.
- **Hot Reloading & Pointer Swaps via `ArcSwap`**:
  - Manage large, read-heavy data structures (e.g., symbol graphs, parsed ASTs, routing tables) via `arc_swap::ArcSwap<T>` or `arc_swap::ArcSwapOption<T>`.
  - Enable readers to load pointers concurrently in sub-nanoseconds (`state.load()`), while writers update the shared reference atomically (`state.store(Arc::new(new_index))`) with zero downtime.
- **Atomics & Memory Ordering**:
  - Use `std::sync::atomic` (`AtomicBool`, `AtomicU64`, `AtomicPtr`) for lock-free flags and counters.
  - Select the loosest sufficient memory ordering: `Ordering::Relaxed` for monotonic counters, `Ordering::Acquire`/`Ordering::Release` for synchronizing handshakes, and `Ordering::SeqCst` strictly when a globally consistent total order across multiple variables is mandatory.
  - Guard multi-field invariants using a mutex or atomic pointer swap; atomics protect single words.

---

## 2. API Design, Error Handling & Type Safety (Gjengset, Berman)

- **Explicit `Result<T, E>` and `Option<T>` Returns**:
  - Surface all expected runtime failures (I/O failures, syntax errors, timeouts, invalid user input) as strongly typed `Result<T, E>` values. Reserve panics strictly for broken internal programmer invariants.
  - **Propagate errors consistently using the `?` operator.** Restrict `.expect()` strictly to unit tests or to lines where invariants are formally proven in an adjacent comment: `// SAFETY: checked len > 0 above`.
- **Domain-Specific Errors with `thiserror`**:
  - In library crates (`cortx-core`, `cortx-distill`, `cortx-harness`, `cortx-state`, `cortx-daemon`), define strongly typed error enums derived via `thiserror::Error`:

    ```rust
    #[derive(thiserror::Error, Debug)]
    pub enum distillError {
        #[error("failed to parse AST for file {path}: {source}")]
        ParseFailure {
            path: PathBuf,
            #[source]
            source: syn::Error,
        },
        #[error("symbol '{0}' not found in index")]
        SymbolNotFound(String),
        #[error("I/O error reading source file: {0}")]
        Io(#[from] std::io::Error),
    }
    ```

  - Restrict `anyhow::Result` and `miette::Result` exclusively to binary and CLI entrypoint boundaries (`crates/cargo-cortx/src/main.rs`) for human-facing error reporting.
- **Make Valid States Representable by Construction (Typestate & Newtypes)**:
  - Enforce domain constraints through the Rust type system.
  - Wrap raw primitives in validated newtypes (`struct VerifiedSeed(u64);`, `struct SymbolPath(String);`).
  - Apply the typestate pattern for multi-step lifecycles (`StateEngine<Uninitialized>` $\to$ `StateEngine<Indexed>` $\to$ `StateEngine<Verified>`), consuming the previous state by value (`self`) to verify transitions at compile time.
- **Borrowing Hygiene & Clean Lifetimes**:
  - Accept borrowed distills (`&str`, `&[T]`, `&Path`) in function arguments to allow callers maximum flexibility.
  - Use owned or reference-counted smart pointers (`Arc<str>`, `Arc<[T]>`, `bytes::Bytes`) for long-lived shared data to keep struct signatures clean and decoupled from nested lifetime parameters.
- **Deterministic Resource Cleanup via RAII (`Drop`)**:
  - Encapsulate all external resources (temporary directories, background threads, IPC sockets, file locks) in RAII guard types that implement `Drop` to guarantee automatic, deterministic cleanup.

---

## 3. Performance, Memory & Zero-Cost Abstractions (Blandy, Gjengset)

- **Empirically Driven Optimization**:
  - Base all performance optimizations strictly on empirical measurements. Follow the disciplined engineering cycle: clean implementation $\to$ criterion benchmark $\to$ flamegraph/heap profile $\to$ targeted optimization $\to$ commit with comparative benchmark validation (`make bench-compare`).
- **Contiguous Layout & Cache Locality**:
  - Store collections in contiguous memory (`Vec<T>`, `Box<[T]>`) to maximize CPU prefetcher efficiency and cache locality.
  - For small collections or token buffers, eliminate heap allocation using `smallvec::SmallVec<[T; N]>`, `tinyvec`, or `compact_str::CompactStr` for strings $\le 24$ bytes.
- **Arena Allocation for AST Processing**:
  - Utilize arena allocators (`bumpalo::Bump`) for transient AST transformations and syntax passes. Clear arenas in bulk to deallocate all transient AST nodes in a single pointer reset ($O(1)$).
- **Zero-Copy I/O & Buffering**:
  - Wrap synchronous file I/O in `std::io::BufReader` / `BufWriter` (default buffer size 8KB) and async I/O in `tokio::io::BufReader` / `BufWriter`.
  - Use `bytes::Bytes` and `bytes::BytesMut` for network and IPC buffer management. distill `Bytes` handles to increment atomic reference counts while retaining shared backing byte arrays.
  - Use `std::borrow::Cow<'a, str>` to preserve zero-allocation borrowed views on unmodified paths.
- **Cache-Line Alignment for Multi-Threaded Counters**:
  - Align per-worker statistics and atomic event counters to 64-byte cache lines using `crossbeam_utils::CachePadded<AtomicU64>` to guarantee independent cache lines and maximize multi-core throughput.
- **Compile-Time Monomorphization & Inlining**:
  - Mark small, performance-critical cross-crate functions with `#[inline]`. Leave large functions unadorned to optimize instruction cache efficiency and binary size.
  - Use `&dyn Trait` or `Box<dyn Trait>` when dynamic dispatch is acceptable and minimizing compile times or binary sizes takes precedence over monomorphization.

---

## 4. Core Implementation Invariants & Runtime Rules

1. **Asynchronous Execution Hygiene**:
   - Route blocking filesystem calls (`std::fs`), synchronous thread delays, heavy AST parsing, and long CPU computations through Tokio's dedicated blocking pool via `tokio::task::spawn_blocking(move || { ... }).await`.
2. **Deterministic Time Injection**:
   - Inject simulated clocks (`madsim::time::Instant` or `tokio::time::pause()`) throughout tests and state machines to ensure 100% reproducible execution.
3. **Out-of-Band Context Delivery**:
   - Deliver all distillation outputs and advisor blocks strictly out-of-band (stdout, IPC socket, formatted markdown). Keep source files on disk perfectly byte-identical during tool executions to preserve IDE and agent editing offsets.
4. **Cancellation-Safe Async Design**:
   - Ensure all operations in raceable or selectable async blocks uphold cancellation safety. Retain intermediate state in transactional buffers or isolate partial operations inside dedicated actors.
5. **Ownership & Borrowing Precision**:
   - Leverage borrowed distills (`&T`), read-only smart pointers (`Arc<T>`), and structured ownership transfers. Keep `.clone()` strictly for intentional duplications of independent resources.
6. **Explicit Task Lifecycle Governance**:
   - Retain every spawned task's `JoinHandle` or register it in a `tokio::task::JoinSet`. Await task completion and propagate or log errors explicitly.
7. **Collection Capacity Management**:
   - Proactively call `.shrink_to_fit()` or reallocate collections after processing large temporary bursts to keep working set memory bounded.
8. **Formal Safety Documentation & Verification for `unsafe`**:
   - Enforce `#![forbid(unsafe_code)]` across all library crates. Where `unsafe` is mathematically proven essential, isolate it behind a safe wrapper, annotate it with a comprehensive `// SAFETY:` invariant comment, and verify pointer validity with `cargo miri test`.

---

## 5. Production CLI & Daemon Services (Berman)

- **Structured Logging via `tracing`**:
  - Route all operational diagnostics and telemetry exclusively through the `tracing` ecosystem (`tracing::info!`, `tracing::warn!`, `tracing::error!`, `tracing::debug!`, `tracing::trace!`).
  - **Emit all log events via `tracing` macros with structured key-value attributes in library crates**:

    ```rust
    tracing::info!(
        target: "distill",
        file = %path.display(),
        symbols_found = symbols.len(),
        duration_ms = elapsed.as_millis(),
        "AST distill completed successfully"
    );
    ```

  - Spans: Instrument key async workflows with `#[tracing::instrument(skip(self))]` to automatically correlate nested operations and capture timing.
  - Initializer: In `main`, initialize `tracing-subscriber` supporting standard environment filtering (`RUST_LOG=info,cortx_distill=debug`) with JSON formatting for machine consumers or ANSI colored output for human terminals.
- **CLI Design with `clap` (Derive API)**:
  - Define all CLI arguments and subcommands using `clap` derive macros with clear doc comments that serve as `--help` documentation.
  - Utilize `miette` or `anstream` to format terminal error diagnostics with clear source code snippets, colored underlines, and specific error codes.
- **Daemon Architecture & IPC**:
  - Deliver background indexing and sub-millisecond agent tool hook execution via `crates/cortx-daemon`.
  - Conduct IPC over Unix domain sockets (`$XDG_RUNTIME_DIR/cortx/daemon.sock` for system multi-tenant mode, fallback to `/tmp/cortx-<hash>.sock` or `--isolated`) using length-prefixed JSON framing.
  - Load configuration strictly from system paths (`~/.config/cortx/cortx.toml`, `/etc/cortx/cortx.toml`) or `--config`; preserve CWD isolation across multi-tenant sessions.
  - Configure `notify::RecommendedWatcher` with debouncing (e.g., 50ms) to consolidate rapid multi-file IDE save events before triggering AST re-indexing.
- **Graceful Shutdown**:
  - Monitor termination signals via `tokio::signal::ctrl_c()` and Unix `tokio::signal::unix::signal(SignalKind::terminate())`.
  - On signal reception, close listening sockets, flush `tracing` subscribers, notify the `CancellationToken` tree, await in-flight tasks with a timeout, and exit cleanly with code 0.

---

## 6. Verification Oracles & Ground-Truth Testing

In codebases >1M LOC, coding agents cannot maintain global architecture in-context. This repository enforces closed-loop verification where generated or refactored code must satisfy automated ground-truth oracles.

```text
                           ┌────────────────────────────────────────┐
                           │        AI Coding Agent (Claude/AGY)    │
                           └───────┬────────────────────────▲───────┘
                                   │                        │
                 Tool / Hook Calls │                        │ distills / Counterexamples
                                   ▼                        │
      ┌─────────────────────────────────────────────────────┴───────────────┐
      │                    `cargo-cortx` CLI Router                         │
      └───────┬─────────────────────────────────────────────────────────────┘
              │
              ▼
        ┌───────────┐
        │  distill    │
        │ (AST/SCIP)│
        └────┬──────┘
             │
             ▼
      [Fast Skeleton]
      [Call Graph   ]
```

### Compiler-Aware Context Distillation (`distill`)

- **Purpose**: Delivering compact, compiler-accurate context to agents, achieving $>85\%$ token reduction while retaining full type fidelity across Rust, TypeScript/TSX, and Go codebases.
- **Modes & Flags**:
  - `--mode skeleton` (Default): Uses `syn` for Rust, `oxc_parser` for TypeScript/TSX, and `tree-sitter-go` for Go to strip function bodies, React hook internals, and JSX blocks, emitting exclusively visibility, structs, interfaces, enums, type aliases, and function signatures in $< 10$ms.
  - `--mode hierarchy`: Queries local SCIP/LSP symbol graphs to extract upstream callers and downstream callees up to `--depth <N>`.
  - `--mode contracts`: Extracts all `impl Trait for Target` blocks (Rust) and resolves structural interface satisfaction for value $\mathcal{M}(T)$ and pointer $\mathcal{M}(*T)$ receiver method sets (Go).
  - `--mode bundle`: Combines skeleton, contracts, and call hierarchy into a single markdown block for LLM prompts.
  - `--cross-boundary`: Analyzes cross-boundary impact between Rust/Go backend route bindings/data models (Gin, Chi, Echo, `net/http`) and TypeScript frontend client call sites and consumer interfaces.

---

## 7. Testing Guidelines

- **Native Rust Harness & Verification Tooling**:
  - Locate unit tests next to the code in unit modules (`#[cfg(test)] mod tests { ... }`) and integration suites in `tests/`.
  - Use `proptest` for property testing, `kani` for bounded model proofs, `bolero` for fuzzing, and `madsim` for async simulation.
  - Rely exclusively on standard Rust assertions (`assert!`, `assert_eq!`, `assert_matches!`) and native verification primitives.
- **Event-Driven Synchronization**:
  - **Synchronize tests deterministically via event coordination mechanisms**: Rely on channels, `tokio::sync::Notify`, `tokio::sync::Barrier`, or task `JoinHandle` completion to coordinate async progression. For time-dependent logic, advance virtual time instantly using `tokio::time::pause()` or execute inside the `madsim` simulation environment.
- **Test Isolation & Hermetic Execution**:
  - Keep every test hermetic, reproducible, and self-contained, ensuring consistent passes regardless of execution order, environment variables, working directory, or network availability.
  - Manage filesystem operations within `tempfile::TempDir`, allowing automatic cleanup on `Drop`.
- **Falsifiable Bug Fixes**:
  - Accompany every bug fix with a reproducing test (a failing `#[test]`, a shrunk `proptest` case, or a deterministic `madsim` seed) demonstrating the bug before the fix, and passing cleanly after.

---

## Standing Rules

1. **Non-Destructive Context Delivery**: Preserve source files on disk strictly unmodified during tool hook execution (across `.rs`, `.ts`, `.tsx`, `.js`, `.jsx`, `.go`). Deliver all generated skeletons, contracts, call graphs, and cross-boundary impacts out-of-band to ensure agent line and byte offsets remain perfectly stable.
2. **Quality Gate Verification**: Run `cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test` immediately after every Rust source edit, ensuring compiler and linter passes are fully green before proceeding.
3. **Rigorous Ground-Truth Oracles**: Validate all critical domain algorithms, parsers, and state transitions using bounded model proofs (`#[kani::proof]`), differential property harnesses (`bolero`), or deterministic simulation passes (`madsim`).
4. **Sans-I/O Domain Purity**: Confine `crates/cortx-state` strictly to pure synchronous state transitions (`step(&mut State, Event) -> (State, Vec<Action>)`), accepting deterministic time from caller inputs.
5. **Concurrency & Cancellation Hygiene**: Enforce bounded channels across all production pipelines, release synchronous mutex guards before `.await` points, and verify cancellation safety for all futures evaluated inside `tokio::select!`.
6. **Explicit Typed Error Handling**: Return `Result<T, E>` with strongly typed domain errors derived via `thiserror`, reserving `anyhow` and `miette` exclusively for user-facing CLI binary boundaries.
7. **Relative Markdown Links**: Format links within markdown documentation using repository-relative paths (`[crates/cortx-core/src/lib.rs](crates/cortx-core/src/lib.rs)`).

---

## Commits & PRs

- Structure commits using Conventional Commits (`feat`, `fix`, `refactor`, `perf`, `docs`, `chore`) with an imperative subject line:
  - `feat(distill): implement syn visitor for stripping function bodies`
  - `fix(harness): prevent cancellation race in madsim event loop`
  - `perf(state): pad per-worker atomic counters to avoid false sharing`
- Document the exact invariant protected and name the verification oracle used (e.g., "Verified with 10,000 madsim seeds" or "Proved panic-freedom with Kani unwinding depth 4").
- Explicitly state the technical rationale, compile-time impact, and security footprint for every dependency addition or modification in `Cargo.toml`.
