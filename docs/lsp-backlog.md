# LSP Backlog — Gate C (Feature Wave)

All Gate A + Gate B + 0A (BufferView) items are shipped and merged.
This backlog tracks Gate C: the interactive LSP feature wave.

---

## C0. DiagnosticsPanel — implement all_entries()  [HIGH]

**Status:** shipped (#12/#13). `rebuild_rows` reads `DiagnosticStore::get_all()`, groups by file, click navigates.

**Description:**
- Read `LspManager::diagnostic_store()` and collect all entries.
- Group by file path, sort by severity (Error → Warning → Info → Hint).
- Each entry renders as a row: severity icon + message + file:line.
- Clicking a row opens the file and positions the cursor at the diagnostic location.

**Key files:**
- `crates/faber-app/src/diagnostics_panel.rs` (`all_entries`, `render`)
- `crates/faber-lsp/src/diagnostics.rs` (`DiagnosticStore`)
- `crates/faber-app/src/workspace.rs` (navigate to file+line)

---

## C1. Hover  [TS]

**Status:** shipped (#12). 400ms dwell timer, caret-anchored popover, dismisses on move.

**Description:**
- On cursor dwell (~500ms), call `textDocument/hover` via `LspManager::request_for_document`.
- Render response in a caret-anchored popover (reuse `popover_container` + `deferred(anchored())`).
- Dismiss on cursor move or Escape.
- i18n: no new strings needed.

**Key files:**
- `crates/faber-app/src/editor_view.rs` (hover timer, popover render)
- `crates/faber-lsp/src/manager.rs` (`request_for_document`)
- `crates/faber-app/src/ui/modal.rs` (`popover_container`)

---

## C2. Go-to-Definition  [TS]

**Status:** shipped. F12 + Cmd+Click → `textDocument/definition`; same-file scrolls editor; cross-file opens via `Workspace::navigate_to`.

**Description:**
- `Cmd+Click` or `F12` sends `textDocument/definition`.
- Single result → navigate directly (`workspace.navigate_to_file_location`).
- Multiple results → show inline references panel (reuse References UI from C3).

**Key files:**
- `crates/faber-app/src/editor_view.rs` (keybinding, click handler)
- `crates/faber-app/src/workspace.rs` (`navigate_to_file_location`)

---

## C3. References  [TS]

**Status:** not started.

**Description:**
- `Shift+F12` / `Cmd+Shift+F12` sends `textDocument/references`.
- Results rendered in a new `ReferencesPanel` tab (uses `TabItem` trait from Gate A).
- Each row: file path + line preview. Click → navigate.

**Key files:**
- `crates/faber-app/src/references_panel.rs` (new)
- `crates/faber-app/src/workspace.rs` (open references tab)

---

## C4. Completion  [TS]

**Status:** not started.

**Description:**
- Trigger on typing (after a character or `.`) — call `textDocument/completion`.
- Render a dropdown anchored below the cursor (reuse popover anchor).
- Select item → insert, resolve with `completionItem/resolve` for docs.
- Snippet support: tab stop placeholders.

**Key files:**
- `crates/faber-app/src/editor_view.rs` (trigger, dropdown render, insertion)

---

## C5. Signature Help  [TS]

**Status:** not started.

**Description:**
- Trigger on `(` or `,` — call `textDocument/signatureHelp`.
- Render active signature + active parameter highlighted in a popover above or below the cursor.
- Dismiss on `)` or Escape.

**Key files:**
- `crates/faber-app/src/editor_view.rs`

---

## C6. Rename  [TS]

**Status:** not started.

**Description:**
- `F2` triggers `textDocument/prepareRename` (validates position), shows inline input.
- On confirm: `textDocument/rename` → `WorkspaceEdit` → apply edits across files.
- Multi-file edits route through `Document::apply(Transaction)` per file.

**Key files:**
- `crates/faber-app/src/editor_view.rs` (keybinding, inline input)
- `crates/faber-app/src/workspace.rs` (apply multi-file workspace edit)

---

## C7. Quick Fix / Code Actions  [TS]

**Status:** not started.

**Description:**
- `Cmd+.` or clicking a squiggle sends `textDocument/codeAction` with the diagnostic context.
- Render a small popup menu of actions. Select → `workspace/executeCommand` or apply `WorkspaceEdit`.

**Key files:**
- `crates/faber-app/src/editor_view.rs`

---

## C8. Format on Save  [TS]

**Status:** not started.

**Description:**
- On `Cmd+S`, after saving: send `textDocument/formatting`.
- Response is a list of `TextEdit`s; apply as a single `Transaction` via `Document::apply`.
- Respect `settings.lsp.format_on_save` flag (default: true).

**Key files:**
- `crates/faber-app/src/editor_view.rs` (save handler)
- `crates/faber-settings/src/lib.rs` (`format_on_save` flag)

---

## C9. Integration Test  [Medium]

**Status:** not started (echo_server path already tested; real rust-analyzer path missing).

**Description:**
- `crates/faber-app/tests/lsp_integration.rs`, marked `#[ignore]`.
- Temp Cargo project + deliberate type error → diagnostic in `DiagnosticStore` <5s → fix → clears.
- CI skips; dev runs with `-- --include-ignored`.

---

## C10. Trust UX gap — recent projects skip modal  [Low]

**Status:** known gap. LSP stays gated (safe); UX only.

**Description:**
- Opening a recent project via the welcome view bypasses `check_and_show_trust_modal`.
- Fix: dispatch the same trust check action from the recent-project click handler.

**Key files:**
- `crates/faber-app/src/workspace.rs` (`on_open_recent`)

---

## Sequencing

```
C0 (Problems panel)
C1 (hover)          — highest value / lowest cost
C2 (goto-def)       — reuses hover anchor
C3 (references)     — new panel, reuses TabItem
C4 (completion)     — largest single item
C5 (signature help) — small after C4 trigger infra
C6 (rename)         — needs prepareRename + multi-file edit
C7 (quickfix)       — shares code-action infra with C6
C8 (format on save) — trivial after C6 workspace-edit path
C9 + C10            — parallel / low-friction anytime
```
