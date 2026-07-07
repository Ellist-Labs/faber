#!/usr/bin/env bash
# Cross-editor performance comparison.
# Runs faber, Zed, and VS Code on the same fixture and writes a report.
#
# Prereqs:
#   brew install hyperfine
#   Install Zed:     https://zed.dev  → ensure `zed` CLI in PATH
#   Install VS Code: https://code.visualstudio.com → ensure `code` CLI in PATH
#
# Usage:
#   perf/compare.sh          — compare all available editors
#   perf/compare.sh --quick  — skip large-file tests
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
FIXTURES_DIR="$SCRIPT_DIR/fixtures"
REPORT_DIR="$SCRIPT_DIR/report"
REPORT="$REPORT_DIR/comparison.md"
QUICK="${1:-}"

# ── Prereq check ─────────────────────────────────────────────────────────────
if ! command -v hyperfine &>/dev/null; then
  echo "ERROR: hyperfine not found. Install with: brew install hyperfine"
  exit 1
fi

mkdir -p "$REPORT_DIR"

# Generate fixtures if needed
if [ ! -f "$FIXTURES_DIR/small.rs" ]; then
  bash "$FIXTURES_DIR/gen.sh"
fi

# Build faber release
cd "$PROJECT_DIR"
cargo build --release -q
FELIX="$PROJECT_DIR/target/release/felix"

# ── Editor discovery ──────────────────────────────────────────────────────────
declare -a EDITOR_CMDS EDITOR_NAMES

EDITOR_CMDS+=("$FELIX")
EDITOR_NAMES+=("faber")

for ed in zed code; do
  if command -v "$ed" &>/dev/null; then
    EDITOR_CMDS+=("$ed")
    EDITOR_NAMES+=("$ed")
  else
    echo "SKIP: '$ed' not in PATH (install + add CLI launcher for comparison)"
  fi
done

if [ ${#EDITOR_CMDS[@]} -lt 2 ]; then
  echo "WARNING: Only faber found. Install Zed and/or VS Code for a full comparison."
fi

# ── Startup benchmark via hyperfine ──────────────────────────────────────────
echo ""
echo "=== Cold startup: small fixture ==="
# hyperfine measures wall-clock time of the process; we pass --shell=none and
# immediately quit each editor after 3 seconds (enough to record startup).
FIXTURE="$FIXTURES_DIR/small.rs"
HF_CMDS=()
for i in "${!EDITOR_CMDS[@]}"; do
  cmd="${EDITOR_CMDS[$i]}"
  name="${EDITOR_NAMES[$i]}"
  # Wrap each command: launch, wait 3s (to let it start), kill, record time
  HF_CMDS+=("--command-name" "$name")
  HF_CMDS+=("timeout 5 $cmd $FIXTURE || true")
done

hyperfine --warmup 1 --runs 3 "${HF_CMDS[@]}" \
  --export-markdown "$REPORT_DIR/startup_small.md" 2>&1 | tail -20

echo ""
echo "Startup results → $REPORT_DIR/startup_small.md"

# ── RSS snapshot ─────────────────────────────────────────────────────────────
echo ""
echo "=== Idle RSS: small fixture ==="
rss_mb() {
  local cmd="$1"
  local fixture="$2"
  $cmd "$fixture" &>/dev/null &
  local PID=$!
  sleep 2
  local KB
  KB=$(ps -o rss= -p "$PID" 2>/dev/null | tr -d ' ' || echo 0)
  kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null
  echo $(( KB / 1024 ))
}

RSS_REPORT=""
for i in "${!EDITOR_CMDS[@]}"; do
  cmd="${EDITOR_CMDS[$i]}"
  name="${EDITOR_NAMES[$i]}"
  mb=$(rss_mb "$cmd" "$FIXTURE")
  printf "  %-12s  %4s MB idle RSS\n" "$name" "$mb"
  RSS_REPORT+="| $name | ${mb} MB |\n"
done

# ── Write report ─────────────────────────────────────────────────────────────
TODAY=$(date -u +%Y-%m-%d)
{
  echo "# Cross-editor comparison — $TODAY"
  echo ""
  echo "Machine: $(uname -srm)"
  echo ""
  echo "## Cold startup (small fixture)"
  echo ""
  cat "$REPORT_DIR/startup_small.md" 2>/dev/null || echo "_hyperfine output not available_"
  echo ""
  echo "## Idle RSS (small fixture)"
  echo ""
  echo "| editor | idle RAM |"
  echo "|--------|----------|"
  printf "%b" "$RSS_REPORT"
  echo ""
  echo "## Budget gate"
  echo ""
  echo "faber must beat VS Code on startup time and idle RAM."
  echo "_(Results above should show faber < code on both metrics.)_"
  echo ""
  echo "## Notes"
  echo "- Measurements are wall-clock on this machine only; reproduce on same hardware."
  echo "- Input-latency comparison (typometer) is a manual appendix — not automated."
} > "$REPORT"

echo ""
echo "Report written → $REPORT"
