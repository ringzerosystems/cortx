# Cortx

[![RingZero Systems](https://img.shields.io/badge/Vendor-RingZero_Systems_LLC-00599C?style=for-the-badge&logo=rust)](https://ringzerosystems.com)
[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-red.svg?style=for-the-badge)](./LICENSE)
[![Commercial License](https://img.shields.io/badge/License-Commercial-blue.svg?style=for-the-badge)](./LICENSE-COMMERCIAL.md)
[![Rust 2024](https://img.shields.io/badge/Rust-2024_Edition-black.svg?style=for-the-badge&logo=rust)](Cargo.toml)

> **Compiler-Aware Context Distillation & Ground-Truth Verification Oracles for AI Coding Agents**

`cortx` is an ultra-fast AST context distillation engine, background daemon, and ground-truth verification suite built in Rust. It eliminates context window dilution, attention decay, and silent code regressions when deploying AI coding assistants (Claude Code, Antigravity, OpenCode, Cursor) on large-scale polyglot codebases (>1.2M LOC).

Target source files remain **100% byte-identical on disk** at all times: all context is delivered out-of-band to prevent patch tool desynchronization.

---

## Quick Start

```bash
# Clone and build release binaries
git clone https://github.com/ringzerosystems/cortx.git
cd cortx
make build && make install

# Start background indexing daemon
cortx daemon

# Distill a file down to its type skeleton
cortx distill src/engine.rs
```

---

## CLI Reference

| Command | Example | Description |
| :--- | :--- | :--- |
| **`distill`** | `cortx distill src/engine.rs` | Extracts stripped AST type skeleton for prompt injection |
| **`distill --cross-boundary`** | `cortx distill api.go --cross-boundary` | Correlates backend routes/models with frontend TS interfaces |
| **`daemon`** | `cortx daemon` | Starts background multi-tenant symbol indexing daemon |
| **`daemon --status`** | `cortx daemon --status` | Reports active workspaces, memory usage, and served hooks |
| **`daemon --ping`** | `cortx daemon --ping` | Probes daemon liveness and IPC round-trip latency |
| **`daemon --stop`** | `cortx daemon --stop` | Gracefully stops the running background daemon |
| **`daemon --hook-post-read`** | `cortx daemon --hook-post-read --file src/lib.rs` | Delivers out-of-band advisory context with zero disk mutation |
| **`simulate`** | `cortx simulate --seeds 100` | Runs discrete-event simulation with virtual clocks & fault injection |
| **`simulate --seed`** | `cortx simulate --seed 0xdeadbeef` | Replays a specific simulation seed deterministically |
| **`simulate --lang go`** | `cortx simulate --lang go` | Executes `go test -race` + `goleak` across randomized `GOMAXPROCS` |
| **`diff-fuzz`** | `cortx diff-fuzz --target-trait MyTrait` | Fuzzes candidate implementation against reference oracle |
| **`diff-fuzz --cross-boundary`**| `cortx diff-fuzz --cross-boundary` | Checks int53 bounds, casing, and nil vs empty slice rules |
| **`diff-fuzz --export-repro`** | `cortx diff-fuzz --export-repro ./repro.rs` | Generates a standalone failing test reproducer |
| **`verify`** | `cortx verify --unwind 10` | Bounded model checks via Kani with JSON counterexample parsing |
| **`verify --scaffold`** | `cortx verify --scaffold process_packet` | Generates starter Kani formal verification proof harness |
| **`invariants`** | `cortx invariants` | Tests round-trip, idempotence, permutation, and bounds |
| **`invariants --scaffold`** | `cortx invariants --scaffold parse --lang rust` | Generates property test suite (Rust `proptest`, TS, or Go `rapid`) |
| **`state-machine`** | `cortx state-machine --fuzz-transitions` | Executes >10M transitions/sec pure state machine fuzzing |
| **`state-machine --matrix`** | `cortx state-machine --matrix` | Analyzes reachability matrix for dead-ends and orphan states |
| **`tui`** | `cortx tui` | Interactive terminal UI for workspace and project settings |

---

## Agent Integration

### Claude Code (`.claude/config.json`)

Configure `cortx` as an out-of-band advisory post-read hook. Target files remain completely untouched on disk:

```json
{
  "hooks": {
    "post_read": "cortx daemon --hook-post-read --file $FILE_PATH"
  }
}
```

---

## Documentation & Deep Dives

- [Architecture & Internal Design](./docs/architecture.md): Context amplification, data flow diagrams, and sub-millisecond AST parser benchmarks.
- [The 5 Ground-Truth Verification Pillars](./docs/verification-pillars.md): Deep dive into DST simulation, differential fuzzing, Kani BMC, metamorphic invariants, and pure Sans-I/O state machines.
- [Configuration Guide](./docs/configuration.md): System configuration reference (`cortx.toml`), project namespaces, and path filters (see also [configs/cortx.example.toml](./configs/cortx.example.toml)).

---

## Development & Verification

`cortx` enforces strict compiler and modularity guarantees across all workspace crates:

```bash
make qa        # Full QA gate: fmt-check -> clippy (-D warnings) -> deny -> machete -> loc
make test      # Workspace unit and integration tests
make bench     # Criterion microbenchmarks for AST distillation parsers
```

---

## License & Commercial Use

Copyright &copy; 2026 Mark Horila / RingZero Systems LLC.

`cortx` is dual-licensed:

1. **Open Source ([AGPL-3.0](./LICENSE)):** Free for open-source development, academic evaluation, and workflows that comply with the GNU Affero General Public License v3.0.
2. **Commercial License ([LICENSE-COMMERCIAL.md](./LICENSE-COMMERCIAL.md)):** Organizations deploying, embedding, or modifying `cortx` in proprietary commercial environments or hosted internal developer platforms without AGPL-3.0 copyleft obligations may obtain a commercial license from **RingZero Systems LLC**.

For commercial licensing, enterprise pilots, or custom AST/compiler integration sprints, contact:

- **Web:** [https://ringzerosystems.com](https://ringzerosystems.com)
- **Direct:** `licensing@ringzerosystems.com`
