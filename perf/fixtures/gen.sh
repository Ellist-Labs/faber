#!/usr/bin/env bash
# Generates reproducible Rust source fixtures for benchmarking.
# Fixtures are gitignored — run this before perf/macro.sh or cargo bench.
#
# Sizes:
#   small.rs  ~500 lines   — startup + idle RSS baseline
#   medium.rs ~5000 lines  — micro-bench seed (used by cargo bench)
#   large.rs  ~50000 lines — large-file open test
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$SCRIPT_DIR")")"
SEED="$PROJECT_DIR/src/main.rs"

if [ ! -f "$SEED" ]; then
  echo "ERROR: seed file not found: $SEED"
  exit 1
fi

generate() {
  local target_lines="$1"
  local output="$2"
  local seed_lines
  seed_lines=$(wc -l < "$SEED" | tr -d ' ')
  local reps=$(( (target_lines / seed_lines) + 1 ))
  local file
  file=$(basename "$output")

  : > "$output"
  for _ in $(seq 1 "$reps"); do
    cat "$SEED" >> "$output"
    printf '\n' >> "$output"
  done

  local actual_lines
  actual_lines=$(wc -l < "$output" | tr -d ' ')
  echo "  $file  $actual_lines lines  ($(du -sh "$output" | cut -f1))"
}

echo "Generating fixtures from $SEED..."
generate 500   "$SCRIPT_DIR/small.rs"
generate 5000  "$SCRIPT_DIR/medium.rs"
generate 50000 "$SCRIPT_DIR/large.rs"
echo "Done. Fixtures in $SCRIPT_DIR/"
