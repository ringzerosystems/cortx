# Configuration Guide (`cortx.toml`)

`cortx` uses a centralized, declarative configuration file loaded from:

1. `--config <PATH>` CLI switch override.
2. `$XDG_CONFIG_HOME/cortx/cortx.toml`
3. `~/.config/cortx/cortx.toml`
4. `/etc/cortx/cortx.toml`

> [!IMPORTANT]
> `cortx` enforces **strict working directory isolation**: it never searches or loads untrusted repository-local configuration candidates unless explicitly passed via `--config`.

---

## Configuration Reference

```toml
[daemon]
system_daemon = true                  # Multi-tenant mode servicing all workspaces
lazy_indexing = true                  # Defers AST indexing until first touch on workspace
idle_ttl_secs = 7200                  # Unindexes idle workspaces after 2 hours
enable_file_watcher = true            # Background notify file watcher across active repos
workspace_logging = true              # Isolated logs under <log_dir>/workspaces/<project>.log

[simulate]
default_seeds = 100                   # Default seed count for simulation runs
timeout_seconds = 60                  # Timeout per simulation scenario

[verify]
default_unwind = 5                    # Default Kani loop unwinding limit
solver = "cadical"                    # SMT solver backend

[diff_fuzz]
default_iterations = 1000             # Default iteration budget for shadow fuzzing
corpus_dir = "tests/corpus"           # Corpus storage path

[projects.acme-platform]
qualified_path = "/home/user/projects/acme-platform"
has_worktrees = true                  # Enables Copy-on-Write overlay indexing for Git worktrees
lang = ["rust", "typescript", "go"]
watch_paths = ["crates", "frontend/src", "backend"]
ignored_dirs = [".git", "target", "node_modules", "dist"]

[projects.acme-platform.cross_boundary]
rust_models = "crates/shared-types/src"
go_routes = "backend/pkg/api"
ts_types = "frontend/src/types"

[projects.acme-platform.go_concurrency]
race_detector = true                  # Run `go test -race`
goleak_check = true                   # Run uber-go/goleak checks
gomaxprocs_sweep = [1, 2, 4, 8]       # Randomized GOMAXPROCS concurrency sweep
```

See [configs/cortx.example.toml](../configs/cortx.example.toml) for the complete annotated configuration template.
