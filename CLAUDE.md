# Felix — Agent Instructions

## Project

Felix is a lean, GPU-accelerated code editor (Rust + GPUI). Its core value proposition is
**lower RAM and CPU than Electron editors while being extensible like VSCode**.
Every code decision must protect that premise.

Stack: GPUI (GPU UI), Ropey (rope buffer), Tree-sitter (incremental parsing),
LSP (language intelligence), WASM/wasmtime (extensions), Tokio (async I/O).

## Performance Guardrails

Performance is a first-class feature. Regressions are bugs.

### Before committing any runtime change

1. Run `perf/fixtures/gen.sh` if fixtures don't exist yet.
2. Run `cargo build --release && perf/macro.sh`.
   - All four budget checks must pass (exit 0). A breach blocks the commit.
3. For changes touching **hot paths** (rope ops, parse/reparse, GPUI render/layout):
   run `cargo bench` and compare against the previous run.
   Investigate any bench that regresses >5% before committing.
4. After a clean milestone, run `perf/macro.sh --update-baseline` to commit
   updated numbers in `perf/baseline.json`.

Use the `/perf-guardrails` skill for a guided checklist.

### Hot-path discipline

- **No per-frame heap allocations.** Profile before assuming.
- **Don't clone the rope buffer** unless necessary. Prefer borrows and slices.
- **Profile first, optimize second.** Cite the before/after numbers in the commit.
- Every new core subsystem (input handling, LSP, extension host) must ship with:
  - At least one `benches/` entry covering its hot path.
  - If it affects startup or RAM: a new line in `perf/budgets.toml` + `macro.sh` check.

### Budget tiers

Current tier = **"beat VS Code"** (Electron baseline). Numbers in `perf/budgets.toml`.
Next tier = **"match Zed"** — tighten when the app is feature-stable.
Budgets are comments in `perf/budgets.toml`; enforcement is in `perf/macro.sh`.

### Cross-editor comparison

Run `perf/compare.sh` at feature milestones (requires `brew install hyperfine`
plus Zed and VS Code installed with CLI launchers).
Felix must beat VS Code on startup time and idle RAM. See the `/cross-editor-benchmark` skill.

### Deferred metrics (add once input handling exists)

- `input_latency_ms` — keystroke → repaint round-trip.
- `frame_time_ms` — 120fps target (8.3 ms/frame).
Add to `perf/budgets.toml` and `perf/macro.sh` when the infrastructure is in place.

## Architecture Rules

- `src/lib.rs` owns all core operations (rope, parse, reparse). Keep it dependency-free from GPUI.
- `src/main.rs` is the thin GPUI shell only — UI wiring, no business logic.
- New subsystems follow the same split: lib crate = logic, main/app = UI wiring.
- Extension API design is a long-term contract; design the surface carefully before stabilizing it.

## Code Style

- Comments: minimum. Only document non-obvious invariants or perf constraints.
- No README or summary files unless requested.
- Commits: one concise line, no co-author trailers.
