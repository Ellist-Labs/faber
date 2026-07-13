# Faber — Agent Instructions

## Project

Faber is an opinionated code editor — lean by design, fast by construction, open to the community.
Built in Rust with GPUI, it was started as a reaction against bloated, Electron-based editors.
**Its core invariant: lower RAM and CPU than Electron editors while remaining extensible.**
Every code decision must protect that premise.

Stack: GPUI (GPU UI), Ropey (rope buffer), Tree-sitter (incremental parsing),
LSP (language intelligence), WASM/wasmtime (extensions
). Async via GPUI's own executor (no tokio).

## Workspace structure

7-crate Cargo workspace. Dependency direction is strictly downward — gpui is absent from every
crate except faber-app, enforced by the compiler.

```
crates/faber-core/      NO gpui — rope helpers, Selection/SelectionSet, Transaction/ChangeSet,
                        Anchor+Bias, movement, search, utf16 helpers
crates/faber-lang/      NO gpui — Language, LanguageId, LanguageRegistry, grammar loading,
                        OutlineItem/Outline/OutlineCache (project-wide symbol types)
crates/faber-editor/    NO gpui — Document (text+syntax+history), UI-free editing engine
crates/faber-index/     NO gpui — IndexEngine, background indexing, LMDB store, watcher.
                        New project-wide computation = new IndexModule here, never a bespoke scan.
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

## Indexing engine (faber-index)

- **New project-wide computation ⇒ new `IndexModule`, never a bespoke scan.** See `.claude/skills/index-module/SKILL.md`.
- `IndexEngine` owns all background indexing: one serial run at a time, modules register at startup.
- The engine is GPUI-free; faber-app polls `ProgressReceiver` on a 50ms timer and updates `Entity<IndexStatus>`.
- Modules are pure: `index(&FileInput) -> Vec<(KeySuffix, Vec<u8>)>` with no filesystem access, no globals.
- Storage: LMDB at `~/.cache/faber/index/<blake3(project_root)>/`. Safe to delete — rebuilds automatically.
- Bumping a module's `version()` rebuilds only that module on next start.

## Internationalization

- Every user-facing string in `faber-app` must use `t!("namespace.key")` — never hardcode literals.
- New string = new key in `crates/faber-app/locales/en.toml` first, then `t!()` in code.
- Exceptions: app name `"Faber"`, GPUI element IDs, log/stderr messages, serde config keys, symbolic chips (`"Aa"`, `"W"`, `".*"`).
- Before finishing string-touching work: run `/i18n-guardrails` skill + `cargo test -p faber --test i18n_parity`.
- Adding a locale: copy `en.toml`, translate values (keep all keys), add `Language` variant to `faber-settings`. See `.claude/skills/i18n-guardrails/SKILL.md`.

## Zed as gold standard

- **Before implementing any editor feature, inspect how Zed does it first.** Zed is cloned at
  `/Users/rodrigo/Codes/ellist/zed` — read the equivalent feature there before writing a line.
  This is a required step, not optional.

## Code Style

- Comments: minimum. Only document non-obvious invariants or perf constraints.
- No README or summary files unless requested.
- Commits: one concise line, no co-author trailers.
