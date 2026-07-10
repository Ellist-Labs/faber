# LSP Implementation — Session Notes

## What Was Shipped (feat/lsp-v1 branch)

### Gate A — Foundation

**0B: LSP substrate + SOLID cleanup**
- `LspManager::request_for_document` — single method unblocking hover / definition / completion / rename / etc.
- Registry-derived language routing in `doc_uri_and_lang` (replaces hardcoded `"rs" → "rust"` match)
- Data-driven adapter registration via `default_lsp_adapters()` builder
- Unified `LanguageId`: deleted `manager.rs`-local type, use `faber_lang::LanguageId` everywhere
- `ServerStatus` cleaned: aggregate counts removed from per-server status, computed in `status_bar.rs`

**0D.27: Test infrastructure**
- `crates/faber-lsp/src/test_helpers.rs` — `ChanReader`, `ChanWriter`, `byte_pipe()` extracted from triplicated inline copies
- `crates/faber-lsp/src/bin/echo_server.rs` — echo fixture binary (`[[bin]]`, `CARGO_BIN_EXE_echo_server`)

**0C: TabItem trait (pane refactor)**
- Replaced closed `TabContent` enum with open `TabItem` struct + `TabKind` discriminant
- Type-erased `AnyView` + `FocusHandle` + `title_fn` closure per tab
- `Pane` gains typed push methods: `push_editor_tab`, `push_settings_tab`, `push_project_search_tab`, `push_problems_tab`, `push_tab_raw`
- Adding a new panel now requires ZERO match-site edits anywhere else

**0D.28: Coverage tests (3 new)**
- `transport::tests::pending_map_cleaned_up_via_cancel` — verifies `cancel(id)` removes from pending map
- `server::tests::initialize_error_response_returns_err` — LSP error response → `initialize()` returns Err
- `manager::tests::doc_opened_sends_did_open` — calls `on_document_opened`, asserts valid `didOpen` frame written to wire

### Gate B — Ignition

**1a: Ignition in push_editor_tab** (`workspace.rs:336`)
- Replaces synchronous `on_document_opened` with `std::thread::spawn`
- Thread calls `ensure_server_for_language` (idempotent, may download) then `on_document_opened`
- Ordering ensures `didOpen` always reaches a running server (fixes backlog item 17)

**1b: Trust modal** (`workspace.rs:check_and_show_trust_modal`)
- VSCode-style blocking confirm: "Do you trust the authors of this folder?"
- Buttons: **Trust** / **Open Restricted**
- Trust → `trust_project(folder)` persisted to `~/.config/faber/state.toml` + `manager.set_trusted(true)`
- Warm-up: kicks `ensure_server_for_language` + `on_document_opened` for all already-open editors
- Called from `Workspace::new` (session restore) and `on_open_folder` (Cmd+O)
- i18n keys: `trust.message`, `trust.btn_trust`, `trust.btn_restricted` in `en.toml`

**1d: ServerState::Downloading visibility** (`manager.rs:update_status`)
- New `downloading: Mutex<HashMap<String, String>>` field (server_id → lang_id_str)
- `ensure_server_for_language` inserts before `resolve_binary`, removes on success OR error
- `update_status()` includes downloading entries as `ServerState::Downloading` in the status snapshot
- Status bar immediately shows "Downloading" during the ~60s first-run binary download

---

## Known Gaps / v1 Limitations

| Gap | Impact | Priority |
|-----|--------|----------|
| Welcome view "recent projects" skips trust modal | LSP stays gated (safe); UX: user must reopen via Cmd+O to trigger modal | Low — fix with action dispatch refactor |
| Warm-up during index scan (1c) not implemented | Server starts on first file open, not before | Medium — optimization only |
| `DiagnosticsPanel::all_entries()` returns `vec![]` stub | Problems panel is empty | High — next session |
| No integration test hitting real rust-analyzer | CI only runs echo_server path | Medium — `#[ignore]` test for local runs |

---

## Remaining Phases

### Gate C — Feature Wave (after Gate B merges)

**LSP features** (all route through `request_for_document` from 0B, use caret popover from 0C):
1. Hover (`textDocument/hover`) — highest value, lowest cost
2. Go-to-definition (`textDocument/definition`)
3. References (`textDocument/references`)
4. Completion + resolve (`textDocument/completion`)
5. Signature help (`textDocument/signatureHelp`)
6. Rename (`textDocument/rename` + `prepareRename`)
7. Quick fix / code actions (`textDocument/codeAction`)
8. Format on save (`textDocument/formatting`)

**Infrastructure** (can land in parallel):
- Command registry + palette (clone symbol_finder pattern) — removes "edit main.rs twice per command" friction
- User-overridable keymap file (TOML, layered over defaults)
- `DiagnosticsPanel` — implement `all_entries()` from store, group by file, sort by severity
- Integration test (`#[ignore]`) — real rust-analyzer, type error → diagnostic <5s

### Gate C' — Second Language (JavaScript/TypeScript)
Pure registration after 0B substrate:
- `tree-sitter-javascript` dep + `javascript()` factory in faber-lang
- Outline query (`queries/javascript/outline.scm`)
- `TypeScriptLanguageServerAdapter` in faber-lsp
- +1 line in `adapters()` builder

### 0A — BufferView (deferred)
Collapse 3 divergent rendering paths into one `BufferView` component:
- `EditorView` monolith (editor_view.rs)
- `FilePreview` bespoke (file_preview.rs)
- Project-search syntax-unaware snippets (project_search_view.rs)

Enables: syntax highlighting in search snippets, unified file preview/peek, future hover/peek panels.
