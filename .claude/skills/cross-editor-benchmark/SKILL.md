# Skill: cross-editor-benchmark

Use this skill at feature milestones to run the cross-editor comparison and verify
felix stays ahead of VS Code on startup time and idle RAM.

Run this when: a major feature lands (LSP, extension host, etc.), before a release,
or when the budget numbers in `perf/budgets.toml` need a reassessment.

## Prerequisites

Install once:
```bash
brew install hyperfine
```

Install editors with CLI launchers:
- **Zed**: https://zed.dev — after install, run `zed --install-cli` or ensure `zed` is in PATH
- **VS Code**: https://code.visualstudio.com — open VS Code → Command Palette → "Shell Command: Install 'code' command in PATH"

Verify:
```bash
which hyperfine zed code
```

## Running the comparison

```bash
perf/compare.sh
```

Output: `perf/report/comparison.md` — cold startup table + idle RSS table.

## Reading the results

Open `perf/report/comparison.md`. The key gate:

**felix must beat VS Code on BOTH:**
1. Cold startup (ms)
2. Idle RSS (MB)

**Aspirational target (next tier):** match or beat Zed.

If felix is slower than VS Code on any metric, that is a regression. File an issue,
investigate, and do not merge the milestone without resolution.

## Updating the baseline

After a comparison run where felix passes the gate:
```bash
perf/macro.sh --update-baseline
git add perf/baseline.json
git commit -m "perf: update baseline after <milestone>"
```

## Notes

- Run on the same physical machine every time — cross-machine comparisons are noise.
- Input-latency (typometer) comparison is manual: record a video of each editor typing
  into a large file, extract frame times. Not automated; do it only for major releases.
- Add `Sublime Text` to the comparison once `hyperfine` warmup/kill is verified for it.
