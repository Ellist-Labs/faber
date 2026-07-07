# Faber — Agent Instructions

## Project

Faber is a lean, GPU-accelerated code editor (Rust + GPUI). Its core value proposition is
**lower RAM and CPU than Electron editors while being extensible like VSCode**.
Every code decision must protect that premise.

Stack: GPUI (GPU UI), Ropey (rope buffer), Tree-sitter (incremental parsing),
LSP (language intelligence), WASM/wasmtime (extensions
). Async via GPUI's own executor (no tokio).

## Workspace structure

4-crate Cargo workspace. Dependency direction is strictly downward — gpui is absent from every
crate except faber-app, enforced by the compiler.

```
crates/faber-core/    NO gpui — rope helpers, Selection/SelectionSet, Transaction/ChangeSet,
                      Anchor+Bias, movement, search, utf16 helpers
crates/faber-lang/    NO gpui — Language, LanguageId, LanguageRegistry, grammar loading
crates/faber-editor/  NO gpui — Document (text+syntax+history+per-view selections),
                      Command + dispatch (UI-free editing engine)
crates/faber-app/     gpui shell — EditorView, virtualized render, keybindings, Workspace
```

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
Faber must beat VS Code on startup time and idle RAM. See the `/cross-editor-benchmark` skill.

### Deferred metrics (add once input handling exists)

- `input_latency_ms` — keystroke → repaint round-trip.
- `frame_time_ms` — 120fps target (8.3 ms/frame).
Add to `perf/budgets.toml` and `perf/macro.sh` when the infrastructure is in place.

## Architecture Rules

- **faber-core and faber-editor must never import gpui.** The workspace enforces this at compile time.
- **faber-app is the thin GPUI shell only** — UI wiring, no business logic.
- All document mutations flow through a single choke-point: `Document::apply(Transaction)`.
- New subsystems: logic in faber-core/faber-editor, UI wiring in faber-app.
- Extension API design is a long-term contract; design the surface carefully before stabilizing it.
- Benches live in `crates/faber-editor/benches/` (hot-path coverage, divan, harness=false).

## Internationalization

- Every user-facing string in `faber-app` must use `t!("namespace.key")` — never hardcode literals.
- New string = new key in `crates/faber-app/locales/en.toml` first, then `t!()` in code.
- Exceptions: app name `"Faber"`, GPUI element IDs, log/stderr messages, serde config keys, symbolic chips (`"Aa"`, `"W"`, `".*"`).
- Before finishing string-touching work: run `/i18n-guardrails` skill + `cargo test -p faber --test i18n_parity`.
- Adding a locale: copy `en.toml`, translate values (keep all keys), add `Language` variant to `faber-settings`. See `.claude/skills/i18n-guardrails/SKILL.md`.

## Code Style

- Comments: minimum. Only document non-obvious invariants or perf constraints.
- No README or summary files unless requested.
- Commits: one concise line, no co-author trailers.
