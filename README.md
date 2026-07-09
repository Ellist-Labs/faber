# Faber

Rust, GPU-rendered, open source, and deliberately small — an editor built like a tool, not a suite.

**Stack:** GPUI (GPU UI) · Ropey (rope buffer) · Tree-sitter (incremental parsing) · LSP · WASM/wasmtime (extensions)

---

## Requirements

- macOS (primary target)
- Rust 1.93+ — install via [rustup](https://rustup.rs)

---

## Setup

```sh
# Clone
git clone git@github.com:ellist/faber.git
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
./target/release/faber
```

---

## Workspace Layout

```
crates/faber-core/      Rope helpers, Selection, Anchor, movement, search — no gpui
crates/faber-lang/      Language, LanguageRegistry, grammar loading — no gpui
crates/faber-editor/    Document, Command dispatch, syntax, history — no gpui
crates/faber-app/       GPUI shell: EditorView, Workspace, keybindings, UI
```

Dependency direction is strictly downward — `gpui` is absent from every crate except `faber-app`.
