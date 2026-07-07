# Faber

A lean, GPU-accelerated code editor built with Rust and GPUI. Lower RAM and CPU than Electron editors while remaining extensible.

**Stack:** GPUI (GPU UI) · Ropey (rope buffer) · Tree-sitter (incremental parsing) · LSP · WASM/wasmtime (extensions)

---

## Requirements

- macOS (primary target)
- Rust 1.93+ — install via [rustup](https://rustup.rs)

---

## Setup

```sh
# Clone
git clone git@github.com:ellist/felix.git
cd faber

# Install Rust toolchain (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

No additional system dependencies are required on macOS.

---

## Build & Run

```sh
# Development build (fast compile)
cargo build

# Run (dev)
cargo run

# Open a file or folder
cargo run -- path/to/file.rs
cargo run -- path/to/project/

# Release build
cargo build --release
./target/release/felix
```

---

## Performance Checks

Before committing runtime changes, run the perf guardrails:

```sh
# Generate fixtures (first time only)
./perf/fixtures/gen.sh

# Run macro benchmarks — all four budget checks must pass
cargo build --release && ./perf/macro.sh

# Hot-path micro-benchmarks (rope, parse, render)
cargo bench

# Cross-editor comparison (requires hyperfine, Zed, and VS Code with CLI launchers)
./perf/compare.sh
```

Budget thresholds are defined in `perf/budgets.toml`. Current tier: beat VS Code on startup time and idle RAM.

---

## Workspace Layout

```
crates/faber-core/      Rope helpers, Selection, Anchor, movement, search — no gpui
crates/faber-lang/      Language, LanguageRegistry, grammar loading — no gpui
crates/faber-editor/    Document, Command dispatch, syntax, history — no gpui
crates/faber-app/       GPUI shell: EditorView, Workspace, keybindings, UI
```

Dependency direction is strictly downward — `gpui` is absent from every crate except `faber-app`.
