# Cortx Architecture & Internal Design

`cortx` pairs sub-millisecond AST skeleton distillation with strictly non-destructive, out-of-band context delivery and ground-truth verification oracles.

---

## The Core Problems in Large-Scale AI Development

When AI coding agents operate on enterprise monorepos, two systemic failure modes emerge:

1. **Context Window Dilution & Semantic Drift:**
   Dumping raw implementation bodies into an LLM context window causes severe attention degradation. The model loses track of trait bounds, implicit interface contracts, and cross-boundary types defined thousands of lines or dozens of files away.
2. **Patch Tool Desynchronization:**
   Naive pre-read or post-read hooks inject comments, annotations, or synthetic types directly into files on disk. This mutates byte offsets and line numbers, causing subsequent patch and diff editing tools to fail with offset mismatches.

`cortx` solves both problems by pairing sub-millisecond AST skeleton distillation with **strictly non-destructive, out-of-band context delivery**. Target source files remain 100% byte-identical on disk at all times.

---

## High-Level Architecture

```mermaid
flowchart TD
    subgraph Clients["Clients & CLI"]
        Agent["AI Coding Agent (Claude Code / Antigravity / Cursor)"]
        CLI["CLI Router (cortx)"]
    end

    subgraph DistillPipeline["Context Distillation Pipeline"]
        Hook["Out-of-Band Advisory Hook"]
        Daemon["cortx Daemon (Multi-Tenant IPC)"]
        Engine["cortx-distill Engine"]
        RustAST["Rust AST (syn)"]
        TSAST["TypeScript / TSX AST (oxc)"]
        GoAST["Go AST (tree-sitter-go)"]
        CrossBound["Cross-Boundary Indexer"]
        
        Hook -->|"JSON-RPC (UDS)"| Daemon
        Daemon --> Engine
        Engine --- RustAST
        Engine --- TSAST
        Engine --- GoAST
        Engine --- CrossBound
    end

    subgraph VerifyPipeline["Ground-Truth Verification Oracles"]
        Harness["cortx-harness"]
        StateEngine["cortx-state (Sans-I/O #![no_std])"]
        
        DST["1. DST & Concurrency Sweep (simulate)"]
        DiffFuzz["2. Shadow Diff & Contracts (diff-fuzz)"]
        Kani["3. Model Checking (verify)"]
        Invariants["4. Metamorphic Invariants (invariants)"]
        Matrix["5. Transition Matrix & Fuzzer (state-machine)"]
        
        Harness --- DST
        Harness --- DiffFuzz
        Harness --- Kani
        Harness --- Invariants
        StateEngine --- Matrix
    end

    Disk["Source Files on Disk (Byte-Identical Invariant)"]

    Agent -->|"Post-Read / Edit Event"| Hook
    Hook -.->|"Advisory Skeletons"| Agent
    Hook -.->|"Zero Disk Mutation"| Disk

    CLI -->|"distill / daemon"| Daemon
    CLI -->|"simulate / diff-fuzz / verify / invariants"| Harness
    CLI -->|"state-machine"| StateEngine
```

---

## Context Amplification & Search Elimination

In large codebases, AI coding assistants don't just fail from reading one large file—they fail from **exploratory context bloat**. To understand a single function or struct, an agent typically initiates a cascade of 5 to 15 recursive file reads and `grep` sweeps across the repository, filling the context window with thousands of lines of unrelated implementation logic.

`cortx` inverts this pattern:

- **Single-Turn Topological Awareness:** When the agent touches a target file, the advisory hook injects its AST skeleton, trait/interface contracts, call hierarchy, and cross-boundary routes out-of-band in sub-milliseconds.
- **Eliminating Exploratory Greps:** The agent receives exact upstream contracts and downstream caller sets immediately, eliminating the need to read entire dependent files just to inspect signatures.
- **Focused Distillation on Demand:** When agents or scripts invoke `cortx distill <FILE>`, the engine strips implementation bodies on the fly, emitting only compilable structural contracts.

---

## Sub-Millisecond AST Performance

Because `cortx` is written in Rust using native, memory-efficient parsers, context extraction executes entirely within sub-millisecond budgets without adding perceptual delay to agent tool calls:

| Parser Engine | Implementation | Target Languages | Processing Latency |
| :--- | :--- | :--- | :--- |
| **Rust AST Visitor** | `syn` / `quote` | `.rs` | `< 0.5 ms` |
| **JS/TS Parser** | `oxc_parser` / `oxc_allocator` | `.ts`, `.tsx`, `.js`, `.jsx` | `< 0.3 ms` |
| **Go Structural Resolver** | `tree-sitter-go` | `.go` | `< 0.4 ms` |
| **Cross-Boundary Indexer** | AST Route & Interface Correlator | Go routes $\leftrightarrow$ TS interfaces | `< 1.2 ms` |
