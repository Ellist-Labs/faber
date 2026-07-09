# LSP + Diagnostics Dashboard Backlog

## 15. Integration test and trust gate UX

End-to-end test with real rust-analyzer; nudge for untrusted projects.

**Description:** Integration test: create a temp Rust project, open a .rs file with a deliberate type error. Verify rust-analyzer starts (or downloads), diagnostic appears within 5 seconds. Fix the error, verify diagnostic clears. Trust gate UX: on opening a file in a project not in `TrustedProjects`, show a subtle status-bar nudge (not a modal) with text like "This project's language servers are paused. Trust it?" and a "Trust" button. Clicking "Trust" adds the project path to `TrustedProjects`, saves settings, and spawns servers. The nudge disappears after ~3 seconds if not acted on (or on explicit dismiss). No blocking modal; users can still edit and use faber while untrusted.

**Key files:** `crates/faber-app/tests/lsp_integration.rs` (test), `crates/faber-app/src/status_bar.rs` (trust_nudge rendering), `crates/faber-app/locales/en.toml` (i18n: status_bar.trust_nudge_text, .trust_button).

---

## 17. Ensure server spawned before first document sync

Prevent a race where `on_document_opened` fires before `ensure_server_for_language` completes.

**Description:** `LspManager::on_document_opened` currently calls `ensure_server_for_language` and then immediately sends `textDocument/didOpen`. Because server startup is synchronous but slow (process spawn + initialize handshake), the notification can be sent before the server is ready, silently dropping it. Fix: in `ensure_server_for_language`, if a server is in `Initializing` state, queue the pending `didOpen` and flush the queue once the server transitions to `Running`. Alternatively, buffer notifications in `LanguageServer` until `initialized` is sent and flush on state change. Add a test that documents queued while initializing are synced correctly.

**Key files:** `crates/faber-lsp/src/manager.rs` (`ensure_server_for_language`, `on_document_opened`), `crates/faber-lsp/src/server.rs` (pending-notification buffer or state-change callback).
