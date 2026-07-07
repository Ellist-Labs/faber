#!/usr/bin/env bash
# Bench regression gate — fails if any hot-path bench regresses >10% vs baseline.
# Usage:
#   perf/bench_gate.sh              # check against perf/bench_baseline.json
#   perf/bench_gate.sh --update     # capture new baseline and exit 0
set -euo pipefail

BASELINE="perf/bench_baseline.json"
THRESHOLD=0.10   # 10% regression limit

UPDATE=false
[[ "${1:-}" == "--update" ]] && UPDATE=true

# ── run benches ──────────────────────────────────────────────────────────────
echo "Running cargo bench (this takes a moment)..."
BENCH_OUT=$(cargo bench 2>&1)

# Parse the median column from a divan bench line.
# divan output format (box-drawing chars, tab-separated columns):
#   ├─ bench_name   fastest │ slowest │ median │ mean │ samples │ iters
parse_median() {
    local name="$1"
    echo "$BENCH_OUT" | grep -E "[├╰]─ ${name}[[:space:]]" | head -1 | \
        awk '{
            split($0, cols, "│")
            # cols[1]=name+fastest, cols[2]=slowest, cols[3]=median
            val = cols[3]
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", val)
            print val
        }'
}

# Convert a divan timing string ("10.66 µs") to nanoseconds (integer).
to_ns() {
    python3 -c "
import re, sys
v = sys.argv[1].strip()
m = re.match(r'([0-9.]+)\s*(ns|µs|us|ms|s)', v)
if not m:
    print(0); sys.exit()
num = float(m.group(1))
unit = m.group(2)
mul = {'ns':1,'µs':1000,'us':1000,'ms':1_000_000,'s':1_000_000_000}[unit]
print(int(num * mul))
" "$1"
}

# Hot-path bench names (must match `fn` names in crates/faber-editor/benches/).
BENCH_NAMES=(
    "edit_insert_mid"
    "edit_delete_mid"
    "changeset_insert_mid"
    "changeset_compose"
    "search_scan"
    "parse_medium"
    "reparse_small_edit"
    "rope_insert_middle"
    "rope_remove_chunk"
    "highlight_open_medium"
    "highlight_incremental_insert"
)

if $UPDATE; then
    echo "Capturing baseline..."
    entries=()
    for name in "${BENCH_NAMES[@]}"; do
        val=$(parse_median "$name")
        if [[ -n "$val" ]]; then
            ns=$(to_ns "$val")
            entries+=("\"$name\":$ns")
            echo "  $name = $val (${ns} ns)"
        else
            echo "  SKIP $name (not found in bench output)"
        fi
    done
    # Build JSON manually (no jq dependency)
    ts=$(date +%Y-%m-%d)
    {
        echo "{"
        echo "  \"timestamp\": \"$ts\","
        echo "  \"benches\": {"
        for i in "${!entries[@]}"; do
            comma=$([[ $i -lt $((${#entries[@]}-1)) ]] && echo "," || echo "")
            echo "    ${entries[$i]}$comma"
        done
        echo "  }"
        echo "}"
    } > "$BASELINE"
    echo "Baseline written to $BASELINE"
    exit 0
fi

# ── compare against baseline ─────────────────────────────────────────────────
if [[ ! -f "$BASELINE" ]]; then
    echo "No baseline at $BASELINE — run: perf/bench_gate.sh --update"
    exit 1
fi

FAIL=0
for name in "${BENCH_NAMES[@]}"; do
    baseline_ns=$(python3 -c "
import json
d = json.load(open('$BASELINE'))
print(d.get('benches', {}).get('$name', 0))
" 2>/dev/null || echo 0)

    if [[ "$baseline_ns" == "0" ]]; then
        echo "  SKIP $name (no baseline entry)"
        continue
    fi

    current_val=$(parse_median "$name")
    if [[ -z "$current_val" ]]; then
        echo "  SKIP $name (bench not found in output)"
        continue
    fi
    current_ns=$(to_ns "$current_val")

    pct=$(python3 -c "
b, c = $baseline_ns, $current_ns
r = (c - b) / b if b else 0
print(f'{r:+.1%}')
" 2>/dev/null || echo "+?%")

    regressed=$(python3 -c "
b, c = $baseline_ns, $current_ns
print('yes' if b and (c - b) / b > $THRESHOLD else 'no')
" 2>/dev/null || echo "no")

    if [[ "$regressed" == "yes" ]]; then
        echo "  FAIL $name: $current_val ($pct vs baseline) — regressed >10%"
        FAIL=1
    else
        echo "  PASS $name: $current_val ($pct vs baseline)"
    fi
done

if [[ $FAIL -ne 0 ]]; then
    echo ""
    echo "Bench regression detected — investigate before committing."
    echo "To update baseline after intentional perf changes: perf/bench_gate.sh --update"
    exit 1
fi

echo ""
echo "All bench gates passed ✓"
