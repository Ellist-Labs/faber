#!/usr/bin/env bash
# Faber macro performance harness.
# Measures startup time and RSS against perf/budgets.toml thresholds.
# Must run in a graphical session (opens a real window).
#
# Usage:
#   perf/macro.sh                  — run + check budgets, exit 1 on breach
#   perf/macro.sh --update-baseline — run + update perf/baseline.json
#
# Prereqs: cargo, release build access, display (macOS GUI session).
set -euo pipefail

# ── Budget thresholds (keep in sync with perf/budgets.toml) ─────────────────
BUDGET_STARTUP_MS=1000
BUDGET_IDLE_RSS_MB=250
BUDGET_LARGE_OPEN_MS=2000
BUDGET_LARGE_OPEN_RSS_MB=1000  # loosened until virtual rendering lands (Phase 2)
# ────────────────────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
FIXTURES_DIR="$SCRIPT_DIR/fixtures"
BASELINE="$SCRIPT_DIR/baseline.json"
UPDATE_BASELINE="${1:-}"

echo "=== faber macro benchmark ==="
echo ""

# Generate fixtures if missing
if [ ! -f "$FIXTURES_DIR/large.rs" ]; then
  echo "Fixtures not found — generating..."
  bash "$FIXTURES_DIR/gen.sh"
  echo ""
fi

# Build release binary
echo "Building release binary..."
cd "$PROJECT_DIR"
cargo build --release -q
BINARY="$PROJECT_DIR/target/release/felix"
echo "  $BINARY"
echo ""

TMPOUT=$(mktemp /tmp/faber_perf_XXXXXX)
FAILED=0

# Run faber on a fixture, wait for FABER_READY, measure startup_ms + RSS.
# Prints two space-separated values: startup_ms rss_mb
run_faber() {
  local fixture="$1"

  "$BINARY" "$fixture" > "$TMPOUT" 2>&1 &
  local FPID=$!

  # Wait up to 15s for FABER_READY
  local timeout=150
  while [ $timeout -gt 0 ]; do
    if grep -q "FABER_READY" "$TMPOUT" 2>/dev/null; then
      break
    fi
    sleep 0.1
    timeout=$(( timeout - 1 ))
  done

  if ! grep -q "FABER_READY" "$TMPOUT" 2>/dev/null; then
    echo "ERROR: FABER_READY not seen within 15s for $fixture" >&2
    kill "$FPID" 2>/dev/null; wait "$FPID" 2>/dev/null || true
    rm -f "$TMPOUT"
    exit 1
  fi

  local STARTUP_MS
  STARTUP_MS=$(grep "FABER_READY" "$TMPOUT" | grep -oE 'startup_ms=[0-9]+' | head -1 | cut -d= -f2)

  # Settle 1s, then snapshot RSS
  sleep 1
  local RSS_KB
  RSS_KB=$(ps -o rss= -p "$FPID" 2>/dev/null | tr -d ' ' || echo 0)
  local RSS_MB=$(( RSS_KB / 1024 ))

  kill "$FPID" 2>/dev/null
  wait "$FPID" 2>/dev/null || true  # killed process returns non-zero; expected

  echo "$STARTUP_MS $RSS_MB"
}

check() {
  local metric="$1" value="$2" budget="$3"
  if [ "$value" -le "$budget" ]; then
    printf "  %-24s %6s ms/MB  budget %-6s  PASS\n" "$metric" "$value" "<=$budget"
  else
    printf "  %-24s %6s ms/MB  budget %-6s  FAIL ← BREACH\n" "$metric" "$value" "<=$budget"
    FAILED=1
  fi
}

# ── Test 1: small file (startup + idle RSS) ──────────────────────────────────
echo "--- small file (startup + idle RSS) ---"
read -r STARTUP_MS IDLE_RSS_MB < <(run_faber "$FIXTURES_DIR/small.rs")
check "startup_ms"   "$STARTUP_MS"  "$BUDGET_STARTUP_MS"
check "idle_rss_mb"  "$IDLE_RSS_MB" "$BUDGET_IDLE_RSS_MB"
echo ""

# ── Test 2: large file (open time + RSS) ─────────────────────────────────────
echo "--- large file (open time + RSS) ---"
read -r LARGE_OPEN_MS LARGE_RSS_MB < <(run_faber "$FIXTURES_DIR/large.rs")
check "large_open_ms"  "$LARGE_OPEN_MS"  "$BUDGET_LARGE_OPEN_MS"
check "large_rss_mb"   "$LARGE_RSS_MB"   "$BUDGET_LARGE_OPEN_RSS_MB"
echo ""

rm -f "$TMPOUT"

# ── Update baseline if requested ─────────────────────────────────────────────
if [ "$UPDATE_BASELINE" = "--update-baseline" ]; then
  TODAY=$(date -u +%Y-%m-%d)
  cat > "$BASELINE" <<EOF
{
  "timestamp": "$TODAY",
  "startup_ms": $STARTUP_MS,
  "idle_rss_mb": $IDLE_RSS_MB,
  "large_open_ms": $LARGE_OPEN_MS,
  "large_open_rss_mb": $LARGE_RSS_MB,
  "note": "Updated by perf/macro.sh --update-baseline"
}
EOF
  echo "Baseline updated → $BASELINE"
  echo ""
fi

# ── Final verdict ─────────────────────────────────────────────────────────────
if [ $FAILED -eq 0 ]; then
  echo "All budgets passed ✓"
  exit 0
else
  echo "BUDGET BREACH — fix before committing."
  exit 1
fi
