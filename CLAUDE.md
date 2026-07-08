# Faber — Agent Instructions

## Project

Faber is a lean, GPU-accelerated code editor (Rust + GPUI). Its core value proposition is
**lower RAM and CPU than Electron editors while being extensible like VSCode**.
Every code decision must protect that premise.

Stack: GPUI (GPU UI), Ropey (rope buffer), Tree-sitter (incremental parsing),
LSP (language intelligence), WASM/wasmtime (extensions
). Async via GPUI's own executor (no tokio).

## Workspace structure

6-crate Cargo workspace. Dependency direction is strictly downward — gpui is absent from every
crate except faber-app, enforced by the compiler.

```
crates/faber-core/      NO gpui — rope helpers, Selection/SelectionSet, Transaction/ChangeSet,
                        Anchor+Bias, movement, search, utf16 helpers
crates/faber-lang/      NO gpui — Language, LanguageId, LanguageRegistry, grammar loading
crates/faber-editor/    NO gpui — Document (text+syntax+history), UI-free editing engine
crates/faber-settings/  NO gpui — Settings, AppState, project history
crates/faber-theme/     NO gpui — Theme, Palette, semantic color definitions
crates/faber-app/       gpui shell — EditorView, virtualized render, keybindings, Workspace
```

## Testing

- Logic lives in gpui-free crates (`faber-core`, `faber-editor`, `faber-settings`) and is unit/behavior-tested.
- Pure helpers extracted from gpui views belong in `*_logic.rs` modules alongside their tests.
- Run `cargo test --workspace` before committing. CI enforces fmt, clippy, and the full test suite.
- New subsystems: ship at least one test covering the stable public API path.

## Architecture Rules

- **faber-core and faber-editor must never import gpui.** The workspace enforces this at compile time.
- **faber-app is the thin GPUI shell only** — UI wiring, no business logic.
- All document mutations flow through a single choke-point: `Document::apply(Transaction)`.
- New subsystems: logic in faber-core/faber-editor, UI wiring in faber-app.
- Extension API design is a long-term contract; design the surface carefully before stabilizing it.

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
