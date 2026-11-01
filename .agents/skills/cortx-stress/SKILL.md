---
name: cortx-stress
description: >-
  Builds and previews high-throughput concurrent stress test commands against the running cortx daemon,
  discovering target project directories from cortx.toml, and executing load tests upon user confirmation.
---

# `cortx-stress`

Automated high-throughput load and stress testing workflow for `cortx daemon`. Dynamically discovers project directories from `cortx.toml` (`~/.config/cortx/cortx.toml`) and runs [scripts/stress-test-daemon.py](../../../scripts/stress-test-daemon.py).

> [!IMPORTANT]
> **Execution Rule: Build Command & Await Confirmation**
> When invoked (e.g. via `/cortx-stress`):
> 1. Probe daemon liveness and status (`cargo cortx daemon --ping` or `--status`).
> 2. Run `./.agents/skills/cortx-stress/scripts/run-stress.py --dry-run` to resolve project directories and construct the execution command.
> 3. Present the discovered workspaces, proposed parameters (duration, threads, mode, vendor), and the exact command to the user.
> 4. **Halt and wait for explicit user confirmation before executing any stress test or profiler.**

## Quick Start

### 1. Dry Run / Command Preview
Inspect the dynamically discovered workspace paths without executing:
```bash
./.agents/skills/cortx-stress/scripts/run-stress.py --dry-run
```

### 2. Standard 30-Second Stress Run
Runs with default 8 worker threads in `hook` mode simulating `claude` agent read caps:
```bash
./.agents/skills/cortx-stress/scripts/run-stress.py --duration 30
```

### 3. Profiling Run Preparation
When pairing with [scripts/profile-daemon.sh](../../../scripts/profile-daemon.sh):
1. Start the profiler:
   ```bash
   ./scripts/profile-daemon.sh --duration 30
   ```
2. Immediately launch the stress load:
   ```bash
   ./.agents/skills/cortx-stress/scripts/run-stress.py --duration 30
   ```

## Options & Arguments

[run-stress.py](scripts/run-stress.py) accepts the following parameters and forwards any extra flags directly to [scripts/stress-test-daemon.py](../../../scripts/stress-test-daemon.py):

| Flag | Default | Description |
| :--- | :--- | :--- |
| `--duration <SECS>` | `30.0` | Test run duration in seconds (`0` = count based) |
| `--threads <N>` | `8` | Number of concurrent worker threads |
| `--mode <MODE>` | `hook` | Request type: `hook` (advisory read hooks), `distill` (raw skeletons), `mixed`, `ping`, `status` |
| `--vendor <NAME>` | `claude` | Agent read-cap profile: `claude`, `agy`, `opencode`, `generic`, `random` |
| `--session-id <ID>` | `randomized` | Conversation session ID override (random per project if omitted) |
| `--config <PATH>` | `~/.config/cortx/cortx.toml` | Custom config override path |
| `--dry-run` | `false` | Resolves config folders and prints command without running |

Any additional arguments at the end are forwarded directly (e.g. `--reconnect`, `--json`, `--cross-boundary`):
```bash
./.agents/skills/cortx-stress/scripts/run-stress.py --duration 10 --threads 16 -- --reconnect
```

## Profile Reports & Review Locations

Whenever profiling is executed via [scripts/profile-daemon.sh](../../../scripts/profile-daemon.sh), all outputs and call graphs are exported to timestamped folders under `profiles/`:

- **Profile Directory:** `profiles/daemon-<YYYYMMDD-HHMMSS>/` (e.g. `profiles/daemon-20261031-023057/`)
- **Call-Tree & Hotspot Report:** `<output-dir>/profile-report.txt`
  - Shows CPU samples, self/child percentage breakdown, and hot symbol names across Tokio and Rayon threads.
  - Quick top-hotspots filter:
    ```bash
    grep -E '^\s+[0-9]+\.[0-9]+%\s+[0-9]+\.[0-9]+%' profiles/daemon-*/profile-report.txt | sort -k2 -nr | head -n 30
    ```
- **Collapsed Stack Traces:** `<output-dir>/profile-stacks.txt`
  - Collapsed stacks ready for flamegraph generators.
- **Daemon Telemetry Snapshot:** `<output-dir>/daemon-telemetry.txt`
  - Captures pre- and post-run memory RSS, hook counts, and delivered supplemental context.
- **Raw Perf Recording:** `<output-dir>/perf.data`
  - Raw Linux perf events for deep interactive inspection via `perf report -i <output-dir>/perf.data`.

## Post-Run Verification

Check daemon telemetry and active workspace registry:
```bash
cargo cortx daemon --status
```
Or check the live daemon logs:
```bash
tail -n 50 ~/.local/state/cortx/logs/daemon-*.log
```
