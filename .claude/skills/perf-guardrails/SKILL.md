# Skill: perf-guardrails

Use this skill before committing any change that touches runtime behaviour: rope ops,
parse/reparse, GPUI render/layout, startup path, file loading, or anything that touches
the main loop.

## When to invoke

- About to commit a change to `src/lib.rs`, `src/main.rs`, or any future core module.
- Introduced a new data structure or algorithm on a hot path.
- After optimizing something — verify you actually improved it.

## Step-by-step checklist

### 1. Ensure fixtures exist
```bash
ls perf/fixtures/large.rs 2>/dev/null || bash perf/fixtures/gen.sh
```

### 2. Build release
```bash
cargo build --release
```

### 3. Run macro harness (Tier 1)
```bash
perf/macro.sh
```
- Exit 0 = all budgets pass → proceed.
- Exit 1 = breach → **do not commit**. Investigate the printed metric.

### 4. Run micro-benches if touching hot paths (Tier 2)
```bash
cargo bench
```
Compare output to the previous run (or `perf/baseline.json` for macro numbers).

**Regression threshold: >5% slowdown on any bench requires investigation.**

To capture a new baseline after deliberate improvement:
```bash
perf/macro.sh --update-baseline
# then commit perf/baseline.json with the change
```

### 5. Read and act on results

| Outcome | Action |
|---|---|
| All pass, benches stable | Commit the change. |
| Macro breach | Revert or fix before committing. Never merge a breach. |
| Bench >5% regression | Profile (`cargo flamegraph` or Instruments), fix, re-bench. |
| Bench improvement | Update baseline, note numbers in commit message. |

## Key files

| Path | Purpose |
|---|---|
| `perf/budgets.toml` | Human-readable budget definitions + context |
| `perf/macro.sh` | Tier-1 enforcement script |
| `perf/baseline.json` | Committed numbers; diff = regression signal |
| `benches/rope_ops.rs` | Rope hot-path micro-benches |
| `benches/parse.rs` | Tree-sitter parse/reparse micro-benches |
| `CLAUDE.md` | Project-wide performance rules |

## Deferred metrics

`input_latency_ms` and `frame_time_ms` are not yet in `macro.sh` — they require
real input handling. Add budget lines and checks when that infrastructure lands.
