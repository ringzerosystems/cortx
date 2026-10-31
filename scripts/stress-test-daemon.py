#!/usr/bin/env python3
"""
stress-test-daemon.py - High-throughput concurrent load generator and benchmark
tool for the cortx indexing daemon.

Exercises the daemon wire protocol v2 over Unix domain sockets, testing concurrent
Distill (AST skeleton stripping, caller/callee hierarchy, trait contracts, bundle),
PostReadHook, Ping, and Status requests. Measures end-to-end request latency,
throughput (req/s), data transfer rates, memory stability, and context bloat reduction.
"""

from __future__ import annotations

import argparse
import glob
import json
import math
import os
import random
import shutil
import signal
import socket
import struct
import subprocess
import sys
import threading
import time
from typing import Any, Dict, List, Optional, Tuple


# ==============================================================================
# Wire Protocol & Socket Management
# ==============================================================================

def test_socket_alive(sock_path: str, timeout: float = 0.5) -> bool:
    """Probes whether a Unix domain socket at sock_path is alive and responsive."""
    if not os.path.exists(sock_path):
        return False
    try:
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(timeout)
        sock.connect(sock_path)
        sock.close()
        return True
    except (socket.error, OSError):
        return False


def resolve_default_socket(explicit_socket: Optional[str] = None) -> Tuple[str, bool]:
    """
    Resolves the daemon socket path following precedence hierarchy:
    1. Explicit socket path override
    2. XDG_RUNTIME_DIR/cortx/daemon.sock
    3. /run/user/<uid>/cortx/daemon.sock
    4. macOS ~/Library/Caches/cortx/daemon.sock
    5. /tmp/cortx-user-<uid>/daemon.sock
    6. Active /tmp/cortx-*.sock or /tmp/cortx-user-*/daemon.sock

    Returns (socket_path, is_alive).
    """
    if explicit_socket:
        p = os.path.abspath(os.path.expanduser(explicit_socket))
        return p, test_socket_alive(p)

    candidates: List[str] = []

    # 1. Check XDG_RUNTIME_DIR
    xdg_runtime = os.environ.get("XDG_RUNTIME_DIR")
    if xdg_runtime:
        candidates.append(os.path.join(xdg_runtime, "cortx", "daemon.sock"))

    # 2. Check /run/user/<uid>/cortx/daemon.sock
    uid = os.getuid() if hasattr(os, "getuid") else 1000
    candidates.append(f"/run/user/{uid}/cortx/daemon.sock")

    # 3. Check macOS cache directory
    home = os.environ.get("HOME")
    if home and sys.platform == "darwin":
        candidates.append(os.path.join(home, "Library", "Caches", "cortx", "daemon.sock"))

    # 4. Check /tmp/cortx-user-<uid>/daemon.sock
    candidates.append(f"/tmp/cortx-user-{uid}/daemon.sock")

    # 5. Check wildcards in /tmp
    for pattern in ["/tmp/cortx-*.sock", f"/tmp/cortx-user-{uid}/daemon.sock", "/tmp/cortx-user-*/daemon.sock"]:
        for p in glob.glob(pattern):
            if p not in candidates:
                candidates.append(p)

    # Probe candidates for an actively responding daemon
    for cand in candidates:
        if test_socket_alive(cand):
            return cand, True

    # Return primary user socket fallback
    primary_fallback = candidates[0] if candidates else f"/run/user/{uid}/cortx/daemon.sock"
    return primary_fallback, False


def recv_exact(sock: socket.socket, n: int) -> bytes:
    """Reads exactly n bytes from the socket, looping until complete."""
    buf = bytearray()
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise ConnectionResetError("Daemon closed connection prematurely")
        buf.extend(chunk)
    return bytes(buf)


def send_ipc_message(sock: socket.socket, payload_dict: Any) -> Tuple[Dict[str, Any], int, int]:
    """
    Sends a length-prefixed JSON message and awaits the response.
    Returns (response_dict, bytes_sent, bytes_received).
    """
    data = json.dumps(payload_dict).encode("utf-8")
    wire_header = struct.pack(">I", len(data))
    sock.sendall(wire_header + data)
    sent_len = 4 + len(data)

    resp_hdr = recv_exact(sock, 4)
    resp_len = struct.unpack(">I", resp_hdr)[0]
    resp_data = recv_exact(sock, resp_len)
    recv_len = 4 + resp_len

    parsed = json.loads(resp_data.decode("utf-8"))
    return parsed, sent_len, recv_len


def query_daemon_status(sock_path: str, timeout: float = 2.0) -> Optional[Dict[str, Any]]:
    """Queries daemon Status telemetry if socket is alive."""
    try:
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(timeout)
        sock.connect(sock_path)
        resp, _, _ = send_ipc_message(sock, "Status")
        sock.close()
        if isinstance(resp, dict) and "Status" in resp:
            return resp["Status"]
    except Exception:
        pass
    return None


# ==============================================================================
# Source File Discovery & Line Offsets
# ==============================================================================

def compute_line_offsets(path: str) -> List[int]:
    """Computes byte offsets for each line in path for accurate span requests."""
    try:
        with open(path, "rb") as f:
            lines = f.read().splitlines(keepends=True)
    except OSError:
        return [0]
    offsets = [0]
    for line in lines:
        offsets.append(offsets[-1] + len(line))
    return offsets


def discover_source_files(target_paths: List[str]) -> Tuple[List[str], Dict[str, int]]:
    """
    Discovers Rust, TypeScript, and Go source files across target directories or files.
    Skips build artifacts and hidden environment folders.
    """
    extensions = {".rs", ".ts", ".tsx", ".js", ".jsx", ".go"}
    files: List[str] = []
    breakdown: Dict[str, int] = {}
    ignored_dirs = {
        ".git", "target", "node_modules", "dist", "build",
        ".claude", ".cursor", ".gemini", "venv", ".venv",
        "__pycache__", ".idea", ".vscode", "profiles",
    }

    for target in target_paths:
        abs_target = os.path.abspath(os.path.expanduser(target))
        if os.path.isfile(abs_target):
            ext = os.path.splitext(abs_target)[1].lower()
            if ext in extensions:
                files.append(abs_target)
                breakdown[abs_target] = 1
            continue

        if not os.path.isdir(abs_target):
            continue

        repo_files: List[str] = []
        for dirpath, dirnames, filenames in os.walk(abs_target):
            dirnames[:] = [d for d in dirnames if d not in ignored_dirs]
            for fname in filenames:
                ext = os.path.splitext(fname)[1].lower()
                if ext in extensions:
                    full_p = os.path.join(dirpath, fname)
                    repo_files.append(full_p)
        breakdown[abs_target] = len(repo_files)
        files.extend(repo_files)

    return sorted(list(set(files))), breakdown


# ==============================================================================
# Payload Construction (Distill, PostReadHook, Ping, Status)
# ==============================================================================

def build_distill_request(
    target_file: str,
    offsets: List[int],
    rng: random.Random,
    req_count: int,
    distill_mode: str = "skeleton",
    distill_format: str = "markdown",
    cross_boundary: bool = False,
    vendor: Optional[str] = None,
    vary_bounds: bool = True,
) -> Dict[str, Any]:
    """Constructs a DaemonRequest::Distill payload."""
    abs_path = os.path.abspath(target_file)
    total_lines = max(0, len(offsets) - 1)

    mode = distill_mode
    if mode == "random":
        mode = rng.choice(["skeleton", "hierarchy", "contracts", "bundle"])

    fmt = distill_format
    if fmt == "random":
        fmt = rng.choice(["markdown", "json", "text"])

    v = vendor
    if v == "random":
        v = rng.choice(["claude", "agy", "opencode", "generic", None])

    start_line: Optional[int] = None
    end_line: Optional[int] = None

    if vary_bounds and total_lines > 5 and req_count % 3 != 0:
        start_line = rng.randint(1, total_lines)
        if req_count % 2 == 1:
            # Micro-read bounded inspection
            end_line = min(total_lines, start_line + rng.randint(5, 80))
        else:
            # Macro-read bounded inspection
            end_line = rng.randint(start_line, total_lines)

    cb = cross_boundary
    if distill_mode == "random" and not cb:
        cb = (req_count % 4 == 0)

    distill_body: Dict[str, Any] = {
        "file": abs_path,
        "mode": mode,
        "format": fmt,
        "cross_boundary": cb,
    }
    if start_line is not None:
        distill_body["start_line"] = start_line
    if end_line is not None:
        distill_body["end_line"] = end_line
    if v is not None:
        distill_body["vendor"] = v

    return {"Distill": distill_body}


def build_post_read_hook(
    target_file: str,
    offsets: List[int],
    rng: random.Random,
    req_count: int,
    vendor: Optional[str] = None,
) -> Dict[str, Any]:
    """Constructs a DaemonRequest::PostReadHook payload."""
    abs_path = os.path.abspath(target_file)
    total_lines = max(0, len(offsets) - 1)

    v = vendor
    if v == "random":
        v = rng.choice(["claude", "agy", "opencode", "generic", None])

    # 1 out of 5 requests simulate bare full-file reads (no range specified)
    if total_lines <= 0 or req_count % 5 == 0:
        hook_body: Dict[str, Any] = {"file": abs_path}
        if v:
            hook_body["vendor"] = v
        return {"PostReadHook": hook_body}

    start_line = rng.randint(1, total_lines)
    if req_count % 2 == 1:
        # Micro-read window (<= 150 lines)
        end_line = min(total_lines, start_line + rng.randint(0, 79))
        read_kind = "micro"
    else:
        # Macro-read window
        end_line = rng.randint(start_line, total_lines)
        read_kind = "macro" if (end_line - start_line + 1) > 150 else "micro"

    raw_read_bytes = max(0, offsets[end_line] - offsets[start_line - 1])
    is_full = (start_line == 1 and end_line == total_lines)

    hook_body = {
        "file": abs_path,
        "start_line": start_line,
        "end_line": end_line,
        "raw_read_bytes": raw_read_bytes,
        "read_kind": read_kind,
        "total_lines": total_lines,
        "is_full_read": is_full,
    }
    if v:
        hook_body["vendor"] = v

    return {"PostReadHook": hook_body}


# ==============================================================================
# Worker Loop & Statistics
# ==============================================================================

class WorkerStats:
    def __init__(self) -> None:
        self.latencies: List[float] = []
        self.success: int = 0
        self.errors: int = 0
        self.bytes_sent: int = 0
        self.bytes_received: int = 0
        self.delivered_content_bytes: int = 0
        self.error_messages: List[str] = []
        self.category_latencies: Dict[str, List[float]] = {
            "distill": [],
            "hook": [],
            "ping": [],
            "status": [],
        }


def worker_loop(
    sock_path: str,
    files: List[str],
    file_offsets: Dict[str, List[int]],
    mode: str,
    distill_mode: str,
    distill_format: str,
    cross_boundary: bool,
    vendor: Optional[str],
    duration_secs: float,
    max_requests: int,
    reconnect_per_req: bool,
    stats: WorkerStats,
    seed: int,
) -> None:
    rng = random.Random(seed)
    start_time = time.monotonic()
    req_count = 0
    file_idx = rng.randint(0, len(files) - 1) if files else 0
    num_files = len(files)

    sock: Optional[socket.socket] = None

    def get_socket() -> Optional[socket.socket]:
        try:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.settimeout(60.0)
            s.connect(sock_path)
            return s
        except Exception as e:
            stats.errors += 1
            stats.error_messages.append(f"Connection failed: {e}")
            return None

    if not reconnect_per_req:
        sock = get_socket()
        if not sock:
            return

    while True:
        if duration_secs > 0 and (time.monotonic() - start_time) >= duration_secs:
            break
        if max_requests > 0 and req_count >= max_requests:
            break

        target_file = files[file_idx % num_files] if num_files > 0 else ""
        file_idx += 1
        offsets = file_offsets.get(target_file, [0])

        category = mode
        if mode == "distill":
            category = "distill"
            req = build_distill_request(
                target_file, offsets, rng, req_count,
                distill_mode=distill_mode,
                distill_format=distill_format,
                cross_boundary=cross_boundary,
                vendor=vendor,
            )
        elif mode == "hook":
            category = "hook"
            req = build_post_read_hook(target_file, offsets, rng, req_count, vendor=vendor)
        elif mode == "ping":
            category = "ping"
            req = "Ping"
        elif mode == "status":
            category = "status"
            req = "Status"
        elif mode == "mixed":
            # Realistic agent distribution: 50% distill, 35% hook, 10% ping, 5% status
            roll = rng.random()
            if roll < 0.50:
                category = "distill"
                req = build_distill_request(
                    target_file, offsets, rng, req_count,
                    distill_mode=distill_mode,
                    distill_format=distill_format,
                    cross_boundary=cross_boundary,
                    vendor=vendor,
                )
            elif roll < 0.85:
                category = "hook"
                req = build_post_read_hook(target_file, offsets, rng, req_count, vendor=vendor)
            elif roll < 0.95:
                category = "ping"
                req = "Ping"
            else:
                category = "status"
                req = "Status"
        else:
            category = "ping"
            req = "Ping"

        if reconnect_per_req:
            active_sock = get_socket()
            if not active_sock:
                req_count += 1
                continue
        else:
            if sock is None:
                sock = get_socket()
                if not sock:
                    time.sleep(0.01)
                    req_count += 1
                    continue
            active_sock = sock

        t0 = time.monotonic()
        try:
            resp, sent_bytes, recv_bytes = send_ipc_message(active_sock, req)
            lat = (time.monotonic() - t0) * 1000.0  # ms
            stats.latencies.append(lat)
            stats.category_latencies[category].append(lat)
            stats.bytes_sent += sent_bytes
            stats.bytes_received += recv_bytes

            if isinstance(resp, dict) and "Error" in resp:
                stats.errors += 1
                err_msg = resp["Error"].get("message", "Unknown error")
                stats.error_messages.append(err_msg)
            else:
                stats.success += 1
                if isinstance(resp, dict):
                    if "DistillResult" in resp:
                        content = resp["DistillResult"].get("content", "")
                        stats.delivered_content_bytes += len(content.encode("utf-8"))
                    elif "AdvisoryContext" in resp:
                        block = resp["AdvisoryContext"].get("advisory_block", "")
                        stats.delivered_content_bytes += len(block.encode("utf-8"))

        except Exception as e:
            stats.errors += 1
            stats.error_messages.append(str(e))
            if not reconnect_per_req:
                try:
                    active_sock.close()
                except Exception:
                    pass
                sock = None
        finally:
            if reconnect_per_req:
                try:
                    active_sock.close()
                except Exception:
                    pass

        req_count += 1

    if sock:
        try:
            sock.close()
        except Exception:
            pass


# ==============================================================================
# Daemon Lifecycle Helper (Autostart)
# ==============================================================================

def find_binary() -> Optional[str]:
    """Finds cargo-cortx or cortx binary in target directories or system PATH."""
    candidates = [
        "target/release/cargo-cortx",
        "target/debug/cargo-cortx",
        "target/release/cortx",
        "target/debug/cortx",
    ]
    for c in candidates:
        if os.path.isfile(c) and os.access(c, os.X_OK):
            return os.path.abspath(c)
    system_cortx = shutil.which("cortx") or shutil.which("cargo-cortx")
    if system_cortx:
        return system_cortx
    if shutil.which("cargo"):
        return "cargo"
    return None


def autostart_daemon(sock_path: str) -> Optional[subprocess.Popen[bytes]]:
    """Starts a local daemon instance for benchmarking if none is running."""
    bin_path = find_binary()
    if not bin_path:
        return None

    if bin_path == "cargo":
        cmd = ["cargo", "run", "--quiet", "--bin", "cargo-cortx", "--", "daemon", "--socket", sock_path, "--start"]
    else:
        cmd = [bin_path, "daemon", "--socket", sock_path, "--start"]

    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(50):
        if test_socket_alive(sock_path):
            return proc
        time.sleep(0.1)

    try:
        proc.terminate()
    except Exception:
        pass
    return None


# ==============================================================================
# Telemetry Calculations & Formatting
# ==============================================================================

def compute_percentiles(sorted_list: List[float]) -> Dict[str, float]:
    """Computes standard percentiles and summary statistics from sorted latencies."""
    if not sorted_list:
        return {"avg": 0.0, "min": 0.0, "p50": 0.0, "p90": 0.0, "p95": 0.0, "p99": 0.0, "max": 0.0, "stddev": 0.0}
    n = len(sorted_list)
    avg = sum(sorted_list) / n
    variance = sum((x - avg) ** 2 for x in sorted_list) / n
    stddev = math.sqrt(variance)

    def pct(p: float) -> float:
        idx = min(n - 1, int(n * p))
        return sorted_list[idx]

    return {
        "avg": avg,
        "min": sorted_list[0],
        "p50": pct(0.50),
        "p90": pct(0.90),
        "p95": pct(0.95),
        "p99": pct(0.99),
        "max": sorted_list[-1],
        "stddev": stddev,
    }


# ==============================================================================
# CLI Entrypoint & Main Execution
# ==============================================================================

def main() -> None:
    parser = argparse.ArgumentParser(
        description="High-throughput concurrent load generator for the cortx daemon."
    )
    parser.add_argument(
        "--socket",
        help="Path to daemon Unix domain socket (default: auto-detected)",
    )
    parser.add_argument(
        "--threads",
        type=int,
        default=8,
        help="Number of concurrent worker threads (default: 8)",
    )
    parser.add_argument(
        "--duration",
        type=float,
        default=5.0,
        help="Duration to run load test in seconds (default: 5.0, 0 = unlimited)",
    )
    parser.add_argument(
        "--requests",
        type=int,
        default=0,
        help="Total requests per thread (default: 0 = duration based)",
    )
    parser.add_argument(
        "--mode",
        choices=["distill", "hook", "ping", "status", "mixed", "slice"],
        default="hook",
        help="Request payload type (default: hook, 'slice' is aliased to distill)",
    )
    parser.add_argument(
        "--distill-mode",
        choices=["skeleton", "hierarchy", "contracts", "bundle", "random"],
        default="skeleton",
        help="Distillation extraction mode (default: skeleton)",
    )
    parser.add_argument(
        "--distill-format",
        choices=["markdown", "json", "text", "random"],
        default="markdown",
        help="Output format for distillation (default: markdown)",
    )
    parser.add_argument(
        "--cross-boundary",
        action="store_true",
        help="Enable cross-boundary HTTP route and model correlation in distill requests",
    )
    parser.add_argument(
        "--vendor",
        choices=["claude", "agy", "opencode", "generic", "random"],
        default="claude",
        help="Simulate specific agent vendor read cap behavior (default: claude)",
    )
    parser.add_argument(
        "--target-dirs",
        "--target-dir",
        "--files",
        nargs="+",
        default=["."],
        dest="target_dirs",
        help="One or more directory paths or source files to gather test files from (default: .)",
    )
    parser.add_argument(
        "--reconnect",
        action="store_true",
        help="Reconnect socket per request instead of keeping persistent connection (simulates CLI invocations)",
    )
    parser.add_argument(
        "--autostart",
        action="store_true",
        help="Automatically launch daemon in background if no running instance is detected",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        dest="output_json",
        help="Output test results as machine-readable JSON",
    )

    args = parser.parse_args()

    # Map legacy slice mode to distill
    if args.mode == "slice":
        args.mode = "distill"

    sock_path, is_alive = resolve_default_socket(args.socket)
    daemon_proc: Optional[subprocess.Popen[bytes]] = None

    if not is_alive:
        if args.autostart:
            if not args.output_json:
                print(f"[cortx-stress] Daemon not active at '{sock_path}'. Spawning background daemon...")
            daemon_proc = autostart_daemon(sock_path)
            if not daemon_proc:
                print(f"Error: Failed to autostart daemon on '{sock_path}'.", file=sys.stderr)
                sys.exit(1)
            is_alive = True
        else:
            print(f"Error: Daemon socket not found or not responsive at '{sock_path}'.", file=sys.stderr)
            print("Start the daemon first with:  cargo cortx daemon", file=sys.stderr)
            print("Or run stress test with:      --autostart", file=sys.stderr)
            sys.exit(1)

    # Query initial daemon status telemetry
    initial_status = query_daemon_status(sock_path)

    # Source files discovery
    files, breakdown = discover_source_files(args.target_dirs)
    if not files and args.mode in ("distill", "hook", "mixed"):
        print(f"Warning: No matching source files (.rs, .ts, .tsx, .js, .jsx, .go) found in {args.target_dirs}.", file=sys.stderr)

    # Shuffle files to interleave operations across packages and subtrees
    random.seed(42)
    random.shuffle(files)
    file_offsets = {f: compute_line_offsets(f) for f in files}

    if not args.output_json:
        print("=== cortx Daemon High-Throughput Stress Test ===")
        print(f"Socket:          {sock_path}")
        print(f"Concurrency:     {args.threads} worker thread(s)")
        print(f"Mode:            {args.mode}")
        if args.mode in ("distill", "mixed"):
            print(f"Distill Mode:    {args.distill_mode}")
            print(f"Distill Format:  {args.distill_format}")
            print(f"Cross-Boundary:  {args.cross_boundary}")
        if args.vendor:
            print(f"Vendor Cap:      {args.vendor}")
        print(f"Connection:      {'reconnect-per-request' if args.reconnect else 'persistent keep-alive'}")
        print(f"Target Roots:    {len(args.target_dirs)} path(s)")
        for repo, count in list(breakdown.items())[:5]:
            print(f"  - {repo}: {count} files")
        if len(breakdown) > 5:
            print(f"  ... and {len(breakdown) - 5} more directories")
        print(f"Total Discovered:{len(files)} files")
        print(f"Duration:        {args.duration}s" if args.duration > 0 else f"Requests/thread: {args.requests}")
        if initial_status:
            print(f"Daemon Info:     v{initial_status.get('daemon_version', '0.1.0')} (PID {initial_status.get('pid', '?')}, RSS {initial_status.get('memory_rss_mb', 0):.1f} MB)")
        print("\nGenerating concurrent load...")

    worker_stats = [WorkerStats() for _ in range(args.threads)]
    threads = []
    t_start = time.monotonic()

    for i in range(args.threads):
        t = threading.Thread(
            target=worker_loop,
            args=(
                sock_path,
                files,
                file_offsets,
                args.mode,
                args.distill_mode,
                args.distill_format,
                args.cross_boundary,
                args.vendor,
                args.duration,
                args.requests,
                args.reconnect,
                worker_stats[i],
                42 + i,
            ),
        )
        threads.append(t)
        t.start()

    for t in threads:
        t.join()

    total_time = time.monotonic() - t_start

    # Query updated daemon status telemetry after test
    final_status = query_daemon_status(sock_path)

    # Clean up autostarted daemon if we spawned one
    if daemon_proc:
        try:
            send_ipc_message(socket.socket(socket.AF_UNIX, socket.SOCK_STREAM), "Shutdown")
        except Exception:
            daemon_proc.terminate()
        daemon_proc.wait(timeout=3.0)

    # Aggregate metrics
    all_latencies: List[float] = []
    total_success = 0
    total_errors = 0
    total_bytes_sent = 0
    total_bytes_recv = 0
    total_delivered_content = 0
    all_errors: List[str] = []
    category_totals: Dict[str, List[float]] = {"distill": [], "hook": [], "ping": [], "status": []}

    for st in worker_stats:
        all_latencies.extend(st.latencies)
        total_success += st.success
        total_errors += st.errors
        total_bytes_sent += st.bytes_sent
        total_bytes_recv += st.bytes_received
        total_delivered_content += st.delivered_content_bytes
        all_errors.extend(st.error_messages)
        for cat, lats in st.category_latencies.items():
            category_totals[cat].extend(lats)

    all_latencies.sort()
    total_requests = total_success + total_errors
    rps = total_success / total_time if total_time > 0 else 0.0
    mb_sent = total_bytes_sent / (1024 * 1024)
    mb_recv = total_bytes_recv / (1024 * 1024)
    throughput_mb_s = (total_bytes_sent + total_bytes_recv) / (1024 * 1024) / total_time if total_time > 0 else 0.0

    percentiles = compute_percentiles(all_latencies)

    # Prepare JSON output if requested
    if args.output_json:
        result_payload: Dict[str, Any] = {
            "elapsed_seconds": round(total_time, 3),
            "total_requests": total_requests,
            "successful_requests": total_success,
            "failed_requests": total_errors,
            "throughput_rps": round(rps, 2),
            "throughput_mb_s": round(throughput_mb_s, 2),
            "bytes_sent": total_bytes_sent,
            "bytes_received": total_bytes_recv,
            "delivered_content_bytes": total_delivered_content,
            "latencies_ms": {k: round(v, 2) for k, v in percentiles.items()},
            "categories": {},
        }
        for cat, lats in category_totals.items():
            if lats:
                lats.sort()
                result_payload["categories"][cat] = {
                    "count": len(lats),
                    "latencies_ms": {k: round(v, 2) for k, v in compute_percentiles(lats).items()},
                }
        if initial_status and final_status:
            result_payload["daemon_telemetry"] = {
                "initial_rss_mb": initial_status.get("memory_rss_mb", 0),
                "final_rss_mb": final_status.get("memory_rss_mb", 0),
                "rss_delta_mb": round(final_status.get("memory_rss_mb", 0) - initial_status.get("memory_rss_mb", 0), 2),
                "hooks_served_delta": final_status.get("hooks_served", 0) - initial_status.get("hooks_served", 0),
                "bytes_saved_delta": final_status.get("bytes_saved", 0) - initial_status.get("bytes_saved", 0),
                "token_reduction_pct": final_status.get("token_reduction_pct", 0.0),
                "est_tokens_saved": final_status.get("est_tokens_saved", 0),
            }
        print(json.dumps(result_payload, indent=2))
        return

    # Print Plaintext Terminal Report
    print("\n" + "=" * 58)
    print("                LOAD TEST RESULTS")
    print("=" * 58)
    print(f"Elapsed Time:            {total_time:.2f} s")
    print(f"Total Requests:          {total_requests:,}")
    print(f"Successful Requests:     {total_success:,} ({100.0 * total_success / max(1, total_requests):.1f}%)")
    print(f"Failed Requests:         {total_errors:,}")
    print(f"Throughput:              {rps:,.1f} req/s")
    print(f"Wire Data Transferred:   {mb_sent:.2f} MB sent, {mb_recv:.2f} MB received ({throughput_mb_s:.2f} MB/s)")
    if total_delivered_content > 0:
        avg_delivered = total_delivered_content / max(1, total_success)
        print(f"Delivered Context:       {total_delivered_content / 1024:.1f} KB total (avg {avg_delivered:.0f} B/req)")

    print("\n--- Latency Distribution (ms) ---")
    print(f"Min:                     {percentiles['min']:.2f} ms")
    print(f"Avg:                     {percentiles['avg']:.2f} ms")
    print(f"p50 (Median):            {percentiles['p50']:.2f} ms")
    print(f"p90:                     {percentiles['p90']:.2f} ms")
    print(f"p95:                     {percentiles['p95']:.2f} ms")
    print(f"p99:                     {percentiles['p99']:.2f} ms")
    print(f"Max:                     {percentiles['max']:.2f} ms")
    print(f"StdDev:                  {percentiles['stddev']:.2f} ms")

    # Category breakdown if mixed or multi-operation
    active_cats = {k: v for k, v in category_totals.items() if v}
    if len(active_cats) > 1:
        print("\n--- Breakdown by Request Type ---")
        for cat, lats in sorted(active_cats.items()):
            lats.sort()
            cat_pct = compute_percentiles(lats)
            cat_rps = len(lats) / total_time if total_time > 0 else 0
            print(f"  [{cat.upper():<7}] {len(lats):>5} reqs ({cat_rps:>6.1f} req/s) | p50: {cat_pct['p50']:>5.2f} ms | p95: {cat_pct['p95']:>5.2f} ms | avg: {cat_pct['avg']:>5.2f} ms")

    # Daemon Telemetry & Context Bloat Reduction
    if initial_status and final_status:
        init_rss = initial_status.get("memory_rss_mb", 0.0)
        final_rss = final_status.get("memory_rss_mb", 0.0)
        rss_delta = final_rss - init_rss
        hooks_delta = final_status.get("hooks_served", 0) - initial_status.get("hooks_served", 0)
        saved_delta = final_status.get("bytes_saved", 0) - initial_status.get("bytes_saved", 0)
        reduction_pct = final_status.get("token_reduction_pct", 0.0)
        est_tokens = final_status.get("est_tokens_saved", 0)

        print("\n--- Daemon Telemetry & Context Reduction ---")
        print(f"Process Resident Memory: {init_rss:.1f} MB -> {final_rss:.1f} MB (delta: {rss_delta:+.2f} MB)")
        print(f"Hooks / Queries Served:  +{hooks_delta:,} requests in session")
        if saved_delta > 0 or reduction_pct > 0:
            print(f"Cumulative Bytes Saved:  +{saved_delta / 1024:.1f} KB")
            print(f"Token Reduction Ratio:   {reduction_pct:.1f}%")
            print(f"Estimated Tokens Saved:  ~{est_tokens:,} LLM tokens")

    if all_errors:
        print("\n--- Error Summary (Sample) ---")
        distinct_errors = list(dict.fromkeys(all_errors))[:5]
        for err in distinct_errors:
            print(f"  * {err}")

    print("=" * 58 + "\n")


if __name__ == "__main__":
    main()
