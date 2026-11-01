#!/usr/bin/env python3
"""
Helper utility for cortx-stress skill.
Extracts declared project folder paths from cortx.toml configuration
and runs or prints the stress test command.
"""

import argparse
import os
import subprocess
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib  # type: ignore


def find_cortx_config(explicit_path: str | None = None) -> Path:
    if explicit_path:
        p = Path(explicit_path).expanduser()
        if p.is_file():
            return p
        raise FileNotFoundError(f"Specified config file not found: {p}")

    candidates = [
        Path(os.environ.get("XDG_CONFIG_HOME", "~/.config")).expanduser() / "cortx" / "cortx.toml",
        Path("~/.config/cortx/cortx.toml").expanduser(),
        Path("/etc/cortx/cortx.toml"),
    ]
    for c in candidates:
        if c.is_file():
            return c
    raise FileNotFoundError("Could not find cortx.toml in standard system config paths.")


def get_project_folders(config_path: Path) -> list[str]:
    with open(config_path, "rb") as f:
        data = tomllib.load(f)

    folders: list[str] = []
    projects = data.get("projects", {})
    for name, p_data in projects.items():
        if isinstance(p_data, dict):
            q_path = p_data.get("qualified_path")
            if q_path and os.path.isdir(q_path):
                folders.append(os.path.abspath(q_path))
            elif q_path:
                print(f"Warning: Project '{name}' path does not exist on disk: {q_path}", file=sys.stderr)
    return folders


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Run daemon stress test using project directories declared in cortx.toml."
    )
    parser.add_argument(
        "--config",
        type=str,
        default=None,
        help="Path to cortx.toml (default: system path)",
    )
    parser.add_argument(
        "--duration",
        type=float,
        default=30.0,
        help="Duration of the load test in seconds (default: 30.0)",
    )
    parser.add_argument(
        "--threads",
        type=int,
        default=8,
        help="Number of concurrent worker threads (default: 8)",
    )
    parser.add_argument(
        "--mode",
        choices=["hook", "distill", "ping", "status", "mixed"],
        default="hook",
        help="Request payload type (default: hook)",
    )
    parser.add_argument(
        "--vendor",
        choices=["claude", "agy", "opencode", "generic", "random"],
        default="claude",
        help="Simulate specific agent vendor read cap behavior (default: claude)",
    )
    parser.add_argument(
        "--session-id",
        type=str,
        default=None,
        help="Explicit agent session ID override (default: randomized per target project)",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print the resolved command and folders without executing",
    )
    parser.add_argument(
        "extra_args",
        nargs=argparse.REMAINDER,
        help="Additional arguments forwarded directly to stress-test-daemon.py",
    )

    args = parser.parse_args()

    try:
        cfg_file = find_cortx_config(args.config)
        folders = get_project_folders(cfg_file)
    except Exception as e:
        print(f"Error reading configuration: {e}", file=sys.stderr)
        sys.exit(1)

    if not folders:
        print("Error: No valid project directories found in config.", file=sys.stderr)
        sys.exit(1)

    repo_root = Path(__file__).resolve().parent.parent.parent.parent.parent
    stress_script = repo_root / "scripts" / "stress-test-daemon.py"

    if not stress_script.is_file():
        print(f"Error: Could not locate stress script at {stress_script}", file=sys.stderr)
        sys.exit(1)

    cmd = [
        sys.executable,
        str(stress_script),
        "--duration",
        str(args.duration),
        "--threads",
        str(args.threads),
        "--mode",
        args.mode,
        "--vendor",
        args.vendor,
        "--target-dirs",
        *folders,
    ]

    if args.session_id:
        cmd.extend(["--session-id", args.session_id])

    if args.extra_args:
        # Strip any leading '--' if passed by argparse REMAINDER
        extra = args.extra_args[1:] if args.extra_args and args.extra_args[0] == "--" else args.extra_args
        cmd.extend(extra)

    print(f"Config loaded: {cfg_file}")
    print(f"Discovered {len(folders)} project folder(s):")
    for f in folders:
        print(f"  - {f}")

    if args.dry_run:
        print("\nCommand to execute:")
        print(" ".join(cmd))
        return

    print("\nRunning stress test...")
    try:
        ret = subprocess.run(cmd)
        sys.exit(ret.returncode)
    except KeyboardInterrupt:
        print("\nStress test interrupted by user.")
        sys.exit(130)


if __name__ == "__main__":
    main()
