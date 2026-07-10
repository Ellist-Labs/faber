# LSP + Diagnostics Dashboard Backlog

## 15. Integration test and trust gate UX

End-to-end test with real rust-analyzer; nudge for untrusted projects.

**Description:** Integration test: create a temp Rust project, open a .rs file with a deliberate type error. Verify rust-analyzer starts (or downloads), diagnostic appears within 5 seconds. Fix the error, verify diagnostic clears. Trust gate UX: on opening a file in a project not in `TrustedProjects`, show a subtle status-bar nudge (not a modal) with text like "This project's language servers are paused. Trust it?" and a "Trust" button. Clicking "Trust" adds the project path to `TrustedProjects`, saves settings, and spawns servers.

**Key files:** `crates/faber-app/tests/lsp_integration.rs`, `crates/faber-app/src/status_bar.rs` (trust_nudge), `crates/faber-app/locales/en.toml`.

---

## 17. Ensure server spawned before first document sync

Prevent a race where `on_document_opened` fires before `ensure_server_for_language` completes.

**Description:** `on_document_opened` calls `ensure_server_for_language` then immediately sends `textDocument/didOpen`. Server startup is slow (spawn + initialize handshake), so the notification is silently dropped. Fix: queue pending `didOpen` while server is in `Initializing` state; flush when it transitions to `Running`.

**Key files:** `crates/faber-lsp/src/manager.rs` (`ensure_server_for_language`, `on_document_opened`), `crates/faber-lsp/src/server.rs`.

---

## 26. Remaining SOLID cleanup

Two sub-items from the cleanup bundle that were not completed.

**Description:**
1. **Duplicate `LanguageId`** (`manager.rs:32`): manager.rs defines its own `LanguageId(String)` instead of using `faber_lang::LanguageId`. Delete the local definition and migrate all uses to the canonical type.
2. **`update_status` reports aggregate counts per server** (`manager.rs:471`): each `ServerStatus` carries identical totals summed across all servers. Either remove the counts from `ServerStatus` and compute them in `faber-app`, or filter the store by `source == server_id` before summing.

**Key files:** `crates/faber-lsp/src/manager.rs`.

---

## 27. Remaining testability work

Two sub-items from the testability bundle that were not completed.

**Description:**
1. **Fixture echo server binary**: Add `crates/faber-lsp/src/bin/echo_server.rs` — reads Content-Length frames on stdin, echoes back a valid `InitializeResult`, then echoes everything else verbatim. Needed to test `LanguageServer::spawn` without a real rust-analyzer.
2. **Shared test helpers**: `ChanReader`/`ChanWriter`/`byte_pipe` are copy-pasted verbatim in `transport.rs`, `manager.rs`, and `server.rs`. Extract to `crates/faber-lsp/src/test_helpers.rs` with `#[cfg(test)]` visibility.

**Key files:** `crates/faber-lsp/src/bin/echo_server.rs` (new), `crates/faber-lsp/src/test_helpers.rs` (new), `crates/faber-lsp/src/{transport,manager,server}.rs` (import instead of duplicate).

---

## 28. Remaining coverage gaps

Three of the eight required tests from the coverage bundle were not written.

**Description:**
1. **Transport request timeout + pending map leak**: caller holds `rx`, server never replies; assert the pending map entry is cleaned up (expose `pending_count()` in tests or use `Arc::strong_count`).
2. **`initialize` timeout branch**: use `from_transport` with a pipe that never writes; assert `initialize` returns `Err` within a short synthetic deadline.
3. **Doc sync flow with mock server**: insert a `LanguageServer::from_transport(…)` into the manager while `trusted=true`, call `on_document_opened`, drain the outbound pipe, assert a valid `textDocument/didOpen` JSON frame was written.

**Key files:** `crates/faber-lsp/src/transport.rs`, `crates/faber-lsp/src/server.rs`, `crates/faber-lsp/src/manager.rs` (tests modules).
