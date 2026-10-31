#!/usr/bin/env bash
# ==============================================================================
# profile-daemon.sh - Automated Profiling for cargo-cortx Daemon via perf
#
# Locates running cortx daemon, records performance samples for N seconds,
# and exports the profile into human-readable plaintext reports.
# ==============================================================================
set -euo pipefail

DEFAULT_DURATION=20
DEFAULT_OUTPUT_DIR="profiles/daemon-$(date +%Y%m%d-%H%M%S)"

DURATION=""
OUTPUT_DIR=""
EXPLICIT_PID=""

show_help() {
    cat << EOF
Usage: $0 [OPTIONS] [DURATION_SECONDS]

Profiles the running 'cortx' indexing daemon using Linux perf and converts
the resulting binary profile into human-readable plaintext files.

Arguments:
  DURATION_SECONDS     Number of seconds to profile (default: 20)

Options:
  -d, --duration SECS  Profiling duration in seconds (default: 20)
  -o, --output DIR     Destination directory for plaintext profile files
                       (default: profiles/daemon-YYYYMMDD-HHMMSS)
  -p, --pid PID        Explicit PID of cortx daemon (skips auto-detection)
  -h, --help           Show this help message

Output Plaintext Files:
  profile-report.txt   Human-readable call-graph report (perf report --stdio)
  profile-stacks.txt   Raw unfolded stack samples (perf script)
  daemon-telemetry.txt Daemon internal metrics, memory RSS, and symbol counts
EOF
}

# Parse positional or named arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        -h|--help)
            show_help
            exit 0
            ;;
        -d|--duration)
            DURATION="$2"
            shift 2
            ;;
        -o|--output)
            OUTPUT_DIR="$2"
            shift 2
            ;;
        -p|--pid)
            EXPLICIT_PID="$2"
            shift 2
            ;;
        *)
            if [[ -z "$DURATION" && "$1" =~ ^[0-9]+$ ]]; then
                DURATION="$1"
                shift
            elif [[ -z "$OUTPUT_DIR" && ! "$1" =~ ^- ]]; then
                OUTPUT_DIR="$1"
                shift
            else
                echo "Error: Unknown argument '$1'" >&2
                show_help
                exit 1
            fi
            ;;
    esac
done

DURATION="${DURATION:-$DEFAULT_DURATION}"
OUTPUT_DIR="${OUTPUT_DIR:-$DEFAULT_OUTPUT_DIR}"

if ! [[ "$DURATION" =~ ^[0-9]+$ ]] || [ "$DURATION" -le 0 ]; then
    echo "Error: Duration must be a positive integer (got: '$DURATION')." >&2
    exit 1
fi

# Ensure perf is installed
if ! command -v perf >/dev/null 2>&1; then
    echo "Error: Linux 'perf' tool is not installed or not in PATH." >&2
    echo "Install via your package manager (e.g. 'sudo apt-get install linux-tools-generic')." >&2
    exit 1
fi

# Detect daemon PID
PID=""
if [ -n "$EXPLICIT_PID" ]; then
    if kill -0 "$EXPLICIT_PID" 2>/dev/null; then
        PID="$EXPLICIT_PID"
    else
        echo "Error: Specified PID $EXPLICIT_PID is not a running process." >&2
        exit 1
    fi
else
    # 1. Query via cortx daemon status JSON
    if command -v cortx >/dev/null 2>&1; then
        STATUS_JSON=$(cortx daemon --status --json 2>/dev/null || true)
        if [ -n "$STATUS_JSON" ]; then
            if command -v jq >/dev/null 2>&1; then
                DETECTED_PID=$(echo "$STATUS_JSON" | jq -r '.pid // empty' 2>/dev/null || true)
                if [[ "$DETECTED_PID" =~ ^[0-9]+$ ]] && kill -0 "$DETECTED_PID" 2>/dev/null; then
                    PID="$DETECTED_PID"
                fi
            fi
        fi
    fi

    # 2. Fall back to process table search
    if [ -z "$PID" ]; then
        CANDIDATES=$(pgrep -x cortx || pgrep -x cargo-cortx || true)
        if [ -n "$CANDIDATES" ]; then
            # Pick the most recent PID
            PID=$(echo "$CANDIDATES" | tail -n 1)
        fi
    fi

    # 3. Last fallback: grep in process list
    if [ -z "$PID" ]; then
        PID=$(pgrep -f "cortx.*daemon" || true)
        if [ -n "$PID" ]; then
            PID=$(echo "$PID" | tail -n 1)
        fi
    fi
fi

if [ -z "$PID" ]; then
    echo "Error: Could not find a running 'cortx' daemon process." >&2
    echo "" >&2
    echo "Start the daemon first with:" >&2
    echo "  cortx daemon &" >&2
    echo "or specify the target PID manually with --pid <PID>." >&2
    exit 1
fi

echo "=========================================================="
echo "  cortx Daemon Profiler"
echo "=========================================================="
echo "Target Daemon PID:  $PID"
echo "Process Name:       $(ps -p "$PID" -o comm= 2>/dev/null || echo "cortx")"
echo "Profile Duration:   ${DURATION}s"
echo "Output Directory:   $OUTPUT_DIR"
echo "=========================================================="

# Determine if sudo is needed based on kernel.perf_event_paranoid
PERF_CMD=(perf)
PARANOID=$(cat /proc/sys/kernel/perf_event_paranoid 2>/dev/null || echo 2)
if [ "$EUID" -ne 0 ] && [ "$PARANOID" -ge 2 ]; then
    echo "Notice: /proc/sys/kernel/perf_event_paranoid is $PARANOID."
    echo "        Elevating to 'sudo perf' for CPU hardware/tracepoint access."
    PERF_CMD=(sudo perf)
fi

mkdir -p "$OUTPUT_DIR"
PERF_DATA="$OUTPUT_DIR/perf.data"
REPORT_TXT="$OUTPUT_DIR/profile-report.txt"
STACKS_TXT="$OUTPUT_DIR/profile-stacks.txt"
TELEMETRY_TXT="$OUTPUT_DIR/daemon-telemetry.txt"

# Capture daemon telemetry before profiling if accessible
if command -v cortx >/dev/null 2>&1; then
    cortx daemon --status > "$TELEMETRY_TXT" 2>&1 || true
fi

echo ""
echo "Recording profile with perf (frequency: 99 Hz, call-graph: fp/dwarf)..."
echo "Run your load test now (e.g. ./scripts/stress-test-daemon.py --duration $DURATION)!"

# Record call-graph profile for specified duration
# -F 99: sample at 99Hz to prevent lock-stepping with timers
# -g: capture full call stack
# -p: attach to daemon PID
"${PERF_CMD[@]}" record -F 99 -g -p "$PID" -o "$PERF_DATA" -- sleep "$DURATION"

echo ""
echo "Profile recording complete. Generating plaintext reports..."

# 1. Generate human-readable hierarchical call tree report in plaintext
"${PERF_CMD[@]}" report -i "$PERF_DATA" --stdio --show-nr-samples > "$REPORT_TXT"

# 2. Generate raw unfolded stack trace dump in plaintext
"${PERF_CMD[@]}" script -i "$PERF_DATA" > "$STACKS_TXT"

# Append updated post-profile daemon status
if command -v cortx >/dev/null 2>&1; then
    echo -e "\n\n=== Daemon Status After Profile ===" >> "$TELEMETRY_TXT"
    cortx daemon --status >> "$TELEMETRY_TXT" 2>&1 || true
fi

# Fix ownership if sudo created files as root
if [ -n "${SUDO_USER:-}" ] && [ "$EUID" -eq 0 ]; then
    chown -R "$SUDO_USER":"$SUDO_USER" "$OUTPUT_DIR"
elif [ "$EUID" -ne 0 ] && [ -n "${USER:-}" ]; then
    sudo chown -R "$USER":"$(id -gn)" "$OUTPUT_DIR" 2>/dev/null || true
fi

echo ""
echo "=========================================================="
echo "  Plaintext Profile Reports Generated Successfully"
echo "=========================================================="
echo "1. Call Tree Report:  $REPORT_TXT"
echo "2. Stack Traces:      $STACKS_TXT"
echo "3. Daemon Telemetry:  $TELEMETRY_TXT"
echo ""

# Preview top hot functions if report is non-empty
if [ -s "$REPORT_TXT" ]; then
    echo "--- Top Hot Functions (preview) ---"
    grep -E '^\s+[0-9]+\.[0-9]+%' "$REPORT_TXT" | head -n 15 || true
    echo "-----------------------------------"
    echo "Full report available at: $REPORT_TXT"
fi
