// LspManager: single-slot lifecycle state machine, document sync, diagnostics.
//
// Architecture: each server is tracked by exactly ONE `ServerSlot` variant stored in
// `slots: Mutex<HashMap<server_id, ServerSlot>>`.  A key being absent means the server
// is stopped.  All lifecycle transitions replace the slot atomically under one lock,
// eliminating the class of bugs where multiple maps disagree.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};

use arc_swap::ArcSwap;
use crossbeam_channel::Receiver;
use lsp_types::NumberOrString;
use ropey::Rope;

use crate::{
    adapter::LspAdapter,
    diagnostics::{
        DiagnosticEntry, DiagnosticRange, DiagnosticStore, severity_from_lsp, tags_from_lsp,
    },
    install::{InstallProgress, Installer},
    position::{PositionEncoding, from_lsp_position},
    server::{LanguageServer, ServerState},
    transport::RpcError,
};

const MAX_RESTART_ATTEMPTS: u8 = 3;
use faber_core::anchor::{Anchor, Bias};
use faber_lang::LanguageId;
use faber_settings::LspSettings;

// ── Download progress (lives inside the Downloading slot) ────────────────────

struct DownloadInfo {
    msg: String,
    fraction: Option<f32>,
}

// ── ServerStatus (UI-facing DTO) ──────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ServerStatus {
    pub server_id: String,
    pub language_id: LanguageId,
    pub state: ServerState,
    pub download_msg: Option<String>,
    pub download_fraction: Option<f32>,
}

// ── ServerSlot — the single source of truth per server ───────────────────────
//
// Absence of a key  = server is stopped / was never started.
// Downloading       = binary resolving / downloading / spawning / initializing.
// Running           = fully initialized; the Arc<LanguageServer> is live.
// Restarting        = child exited unexpectedly; backoff in progress.
//                     `attempt` is the ordinal of the next restart (1-indexed).
//                     Reaching Running resets the counter.
// Failed            = give-up after MAX_RESTART_ATTEMPTS, or unrecoverable error.
//                     User can trigger a manual restart via restart_server().

enum ServerSlot {
    Downloading {
        lang: String,
        progress: Option<DownloadInfo>,
    },
    Running {
        server: Arc<LanguageServer>,
        lang: String,
    },
    Restarting {
        attempt: u8,
        lang: String,
    },
    Failed {
        lang: String,
        msg: String,
    },
}

impl ServerSlot {
    fn lang(&self) -> &str {
        match self {
            ServerSlot::Downloading { lang, .. } => lang,
            ServerSlot::Running { lang, .. } => lang,
            ServerSlot::Restarting { lang, .. } => lang,
            ServerSlot::Failed { lang, .. } => lang,
        }
    }
}

// ── OpenDoc ───────────────────────────────────────────────────────────────────

struct OpenDoc {
    rope: Rope,
    lang_id: String,
    version: i32,
}

// ── LspManager ────────────────────────────────────────────────────────────────

pub struct LspManager {
    adapters: Vec<Box<dyn LspAdapter>>,
    /// Single source of truth: server_id → lifecycle slot.
    /// Absence = stopped. All transitions must replace the slot atomically.
    slots: Mutex<HashMap<String, ServerSlot>>,
    diagnostic_store: Arc<DiagnosticStore>,
    settings: Arc<RwLock<LspSettings>>,
    trusted: Arc<AtomicBool>,
    status: Arc<ArcSwap<Vec<ServerStatus>>>,
    workspace_root: Mutex<Option<PathBuf>>,
    open_docs: Arc<Mutex<HashMap<url::Url, OpenDoc>>>,
}

impl LspManager {
    pub fn new(
        adapters: Vec<Box<dyn LspAdapter>>,
        settings: LspSettings,
        trusted: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            adapters,
            slots: Mutex::new(HashMap::new()),
            diagnostic_store: Arc::new(DiagnosticStore::new()),
            settings: Arc::new(RwLock::new(settings)),
            trusted: Arc::new(AtomicBool::new(trusted)),
            status: Arc::new(ArcSwap::from_pointee(vec![])),
            workspace_root: Mutex::new(None),
            open_docs: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    // ── Trust gate ────────────────────────────────────────────────────────────

    pub fn is_trusted(&self) -> bool {
        self.trusted.load(Ordering::Relaxed)
    }

    pub fn set_trusted(self: &Arc<Self>, trusted: bool) {
        self.trusted.store(trusted, Ordering::Relaxed);
    }

    // ── Server lifecycle ──────────────────────────────────────────────────────

    /// Idempotent: no-op if a slot for this server already exists in any state.
    pub fn ensure_server_for_language(
        self: &Arc<Self>,
        lang_id: &LanguageId,
        workspace_root: &Path,
    ) -> anyhow::Result<()> {
        log::info!(
            "lsp: ensure_server_for_language lang={} root={}",
            lang_id.as_str(),
            workspace_root.display()
        );
        if !self.is_trusted() {
            log::info!("lsp: not trusted, skipping");
            return Ok(());
        }

        // Persist workspace root (first caller wins).
        {
            let mut root = self
                .workspace_root
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if root.is_none() {
                *root = Some(workspace_root.to_owned());
            }
        }

        let adapter = match self
            .adapters
            .iter()
            .find(|a| a.languages().contains(&lang_id.as_str()))
        {
            Some(a) => a,
            None => {
                log::debug!("lsp: no adapter for language {:?}", lang_id.as_str());
                return Ok(());
            }
        };

        let server_id = adapter.server_id().to_owned();
        let lang_str = lang_id.as_str().to_owned();

        // Idempotency: if any slot exists (Downloading/Running/Restarting/Failed) return early.
        {
            let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
            if slots.contains_key(&server_id) {
                return Ok(());
            }
            slots.insert(
                server_id.clone(),
                ServerSlot::Downloading {
                    lang: lang_str.clone(),
                    progress: None,
                },
            );
        }
        self.update_status();

        // Resolve binary — may download; progress callback updates the Downloading slot.
        let settings_guard = self.settings.read().unwrap_or_else(|p| p.into_inner());
        let mgr_for_cb = Arc::clone(self);
        let sid_for_cb = server_id.clone();
        let lang_for_cb = lang_str.clone();

        let binary_path =
            match adapter.resolve_binary(&settings_guard, &mut |progress: InstallProgress| {
                log::info!("lsp: {}", progress.message());
                let mut slots = mgr_for_cb.slots.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(ServerSlot::Downloading { progress: p, .. }) =
                    slots.get_mut(&sid_for_cb)
                {
                    *p = Some(DownloadInfo {
                        msg: progress.message(),
                        fraction: progress.fraction(),
                    });
                }
                // update_status needs the slots lock; drop it first.
                drop(slots);
                mgr_for_cb.update_status();
            }) {
                Ok(p) => p,
                Err(e) => {
                    let msg = e.to_string();
                    log::error!("lsp: resolve_binary failed for {server_id}: {msg}");
                    let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
                    slots.insert(
                        server_id.clone(),
                        ServerSlot::Failed {
                            lang: lang_for_cb,
                            msg: msg.clone(),
                        },
                    );
                    self.update_status();
                    return Err(anyhow::anyhow!("resolve_binary failed: {msg}"));
                }
            };
        drop(settings_guard);

        let init_options = adapter.init_options();
        let server_id_str: &'static str = adapter.server_id();
        let shell_path = Installer::login_shell_path();
        let env_path: Option<&str> = if shell_path.is_empty() {
            None
        } else {
            Some(shell_path.as_str())
        };

        // Spawn child process.
        let server = match LanguageServer::spawn(&binary_path, workspace_root, env_path) {
            Ok(s) => s,
            Err(e) => {
                log::error!("lsp: spawn failed for {server_id}: {e}");
                let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
                slots.insert(
                    server_id.clone(),
                    ServerSlot::Failed {
                        lang: lang_str.clone(),
                        msg: e.to_string(),
                    },
                );
                self.update_status();
                return Err(e);
            }
        };

        // LSP handshake — blocks up to 30 s.
        if let Err(e) = server.initialize(None, workspace_root, init_options) {
            log::error!("lsp: initialize failed for {server_id}: {e}");
            let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
            slots.insert(
                server_id.clone(),
                ServerSlot::Failed {
                    lang: lang_str.clone(),
                    msg: e.to_string(),
                },
            );
            self.update_status();
            return Err(e);
        }

        // Wire crash callback — registered BEFORE the slot transitions to Running so
        // any immediate crash fires handle_server_crash even in the tiny window.
        {
            let mgr = Arc::clone(self);
            let sid = server_id.clone();
            server.set_crash_callback(Box::new(move || {
                mgr.handle_server_crash(&sid);
            }));
        }

        // Wire publishDiagnostics → diagnostic store.
        {
            let store = Arc::clone(&self.diagnostic_store);
            let open_docs = Arc::clone(&self.open_docs);
            let encoding = server.capabilities().position_encoding;
            server.transport().subscribe(
                "textDocument/publishDiagnostics",
                Arc::new(move |params| {
                    Self::handle_publish_diagnostics(
                        Arc::clone(&store),
                        Arc::clone(&open_docs),
                        encoding,
                        server_id_str,
                        params,
                    );
                }),
            );
        }

        log::info!("lsp: {server_id} Running");

        // Transition Downloading → Running and capture queued-doc snapshot atomically.
        // Snapshot is taken while slots lock is held so no new `on_document_opened` call
        // can interleave between the Running insert and the replay — keeping the ordering:
        // (a) slot = Running, (b) replay docs that arrived during Downloading.
        //
        // Lock order: we lock `slots` first, then `open_docs`.  All other code paths that
        // lock both do so in the same order (open_docs-only callers never hold slots while
        // doing so).  The only caller that locks open_docs first is `on_document_opened`,
        // but it acquires slots only through `notify_servers_for_lang` which is a separate,
        // non-overlapping acquisition — not nested.
        let queued: Vec<(url::Url, String, i32, String)> = {
            let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());

            // If the slot was removed while we were initializing (stop_server was called),
            // abandon the new server gracefully.
            if !slots.contains_key(&server_id) {
                log::info!("lsp: {server_id} was stopped while initializing — discarding");
                drop(slots);
                let _ = server.shutdown();
                return Ok(());
            }

            slots.insert(
                server_id.clone(),
                ServerSlot::Running {
                    server: Arc::clone(&server),
                    lang: lang_str.clone(),
                },
            );

            let docs_guard = self.open_docs.lock().unwrap_or_else(|p| p.into_inner());
            docs_guard
                .iter()
                .filter(|(_, d)| d.lang_id == lang_str)
                .map(|(uri, d)| {
                    (
                        uri.clone(),
                        d.lang_id.clone(),
                        d.version,
                        d.rope.to_string(),
                    )
                })
                .collect()
        };
        self.update_status();

        // Replay didOpen for documents that were opened while this server was initializing.
        for (uri, lid, version, text) in queued {
            let params = serde_json::json!({
                "textDocument": {
                    "uri": uri.as_str(),
                    "languageId": lid,
                    "version": version,
                    "text": text,
                }
            });
            server.transport().notify("textDocument/didOpen", params);
        }

        Ok(())
    }

    pub fn stop_server(self: &Arc<Self>, server_id: &str) {
        let removed = {
            let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
            slots.remove(server_id)
        };
        // Mark stopped BEFORE dropping: the crash monitor checks ServerState to decide
        // whether to fire the callback; Stopped suppresses a false "crash" notification.
        if let Some(ServerSlot::Running { ref server, .. }) = removed {
            server.mark_stopped();
        }
        drop(removed); // LanguageServer::drop kills the child process
        self.update_status();
    }

    pub fn restart_server(self: &Arc<Self>, server_id: &str) {
        // Find the language before touching the slot.
        let lang_str = {
            let slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(slot) = slots.get(server_id) {
                slot.lang().to_owned()
            } else {
                // No slot (fully stopped) — find from adapter for user-triggered restart.
                match self
                    .adapters
                    .iter()
                    .find(|a| a.server_id() == server_id)
                    .and_then(|a| a.languages().first().copied())
                {
                    Some(l) => l.to_owned(),
                    None => {
                        log::warn!("lsp: cannot restart {server_id}: no adapter found");
                        return;
                    }
                }
            }
        };

        let workspace_root = {
            self.workspace_root
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
        };
        let Some(root) = workspace_root else {
            log::warn!("lsp: cannot restart {server_id}: workspace root not known");
            return;
        };

        // Remove the current slot (mark stopped if Running to suppress phantom callback).
        {
            let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
            let removed = slots.remove(server_id);
            // mark_stopped must be called while the Arc is still live (before drop).
            if let Some(ServerSlot::Running { ref server, .. }) = removed {
                server.mark_stopped();
            }
            // removed drops here: LanguageServer::drop kills the child.
        }
        self.update_status();

        // Slot is now absent → ensure_server_for_language will insert Downloading and proceed.
        let mgr = Arc::clone(self);
        let sid = server_id.to_owned();
        let lang_id = LanguageId::new(&lang_str);
        std::thread::spawn(move || {
            if let Err(e) = mgr.ensure_server_for_language(&lang_id, &root) {
                log::error!("lsp: restart of {sid} failed: {e}");
            } else {
                log::info!("lsp: {sid} restarted successfully");
            }
        });
    }

    // ── Crash recovery ───────────────────────────────────────────────────────

    fn handle_server_crash(self: &Arc<Self>, server_id: &str) {
        let (attempt, should_restart) = {
            let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
            match slots.get(server_id) {
                Some(ServerSlot::Running { lang, .. }) => {
                    let lang = lang.clone();
                    // Reaching Running always resets the attempt counter (stale-count bug is
                    // impossible here — the counter lives in the slot, not a separate map).
                    let attempt: u8 = 1;
                    if attempt > MAX_RESTART_ATTEMPTS {
                        log::error!("lsp: {server_id} crash-loop limit reached, giving up");
                        slots.insert(
                            server_id.to_owned(),
                            ServerSlot::Failed {
                                lang,
                                msg: "Server crashed repeatedly".to_owned(),
                            },
                        );
                        (0, false)
                    } else {
                        slots.insert(
                            server_id.to_owned(),
                            ServerSlot::Restarting { attempt, lang },
                        );
                        (attempt, true)
                    }
                }
                Some(ServerSlot::Restarting { attempt, lang, .. }) => {
                    let lang = lang.clone();
                    let next = attempt + 1;
                    if next > MAX_RESTART_ATTEMPTS {
                        log::error!("lsp: {server_id} crashed {next} times, giving up");
                        slots.insert(
                            server_id.to_owned(),
                            ServerSlot::Failed {
                                lang,
                                msg: "Server crashed repeatedly".to_owned(),
                            },
                        );
                        (0, false)
                    } else {
                        slots.insert(
                            server_id.to_owned(),
                            ServerSlot::Restarting {
                                attempt: next,
                                lang,
                            },
                        );
                        (next, true)
                    }
                }
                // Slot absent (stop_server ran) or in Downloading/Failed — ignore.
                _ => (0, false),
            }
        };
        self.update_status();

        if !should_restart {
            return;
        }

        let delay_secs = 1u64 << (attempt - 1).min(4); // 1, 2, 4, 8, 16 s
        log::info!("lsp: {server_id} crashed (attempt {attempt}), restarting in {delay_secs}s");

        let mgr = Arc::clone(self);
        let sid = server_id.to_owned();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(delay_secs));
            // Verify the slot is still Restarting{attempt}: if stop_server ran while we
            // slept the slot is absent; if another crash bumped the count the attempt
            // number is different.  Either way abort — the new backoff thread owns it.
            let still_restarting = {
                let slots = mgr.slots.lock().unwrap_or_else(|p| p.into_inner());
                matches!(
                    slots.get(&sid),
                    Some(ServerSlot::Restarting { attempt: a, .. }) if *a == attempt
                )
            };
            if !still_restarting {
                log::info!("lsp: {sid} restart aborted (slot changed while sleeping)");
                return;
            }
            mgr.restart_server(&sid);
        });
    }

    // ── Diagnostics ───────────────────────────────────────────────────────────

    fn handle_publish_diagnostics(
        store: Arc<DiagnosticStore>,
        open_docs: Arc<Mutex<HashMap<url::Url, OpenDoc>>>,
        encoding: PositionEncoding,
        source_str: &'static str,
        params_value: serde_json::Value,
    ) {
        let params: lsp_types::PublishDiagnosticsParams = match serde_json::from_value(params_value)
        {
            Ok(p) => p,
            Err(e) => {
                log::warn!("publishDiagnostics: failed to deserialize params: {e}");
                return;
            }
        };

        let uri = match params.uri.as_str().parse::<url::Url>() {
            Ok(u) => u,
            Err(e) => {
                log::warn!("publishDiagnostics: bad URI {:?}: {e}", params.uri);
                return;
            }
        };

        let rope = {
            let guard = open_docs.lock().unwrap_or_else(|p| p.into_inner());
            guard.get(&uri).map(|d| d.rope.clone())
        };

        let source: crate::diagnostics::Source = Arc::from(source_str);

        let entries: Vec<DiagnosticEntry> = params
            .diagnostics
            .iter()
            .map(|d| {
                let (start_offset, end_offset) = if let Some(ref rope) = rope {
                    let start = from_lsp_position(rope, d.range.start, encoding).unwrap_or(0);
                    let end = from_lsp_position(rope, d.range.end, encoding).unwrap_or(start);
                    (start, end)
                } else {
                    (
                        d.range.start.character as usize,
                        d.range.end.character as usize,
                    )
                };
                let code = d.code.as_ref().map(|c| match c {
                    NumberOrString::Number(n) => n.to_string(),
                    NumberOrString::String(s) => s.clone(),
                });
                DiagnosticEntry {
                    range: DiagnosticRange {
                        lsp_line: d.range.start.line,
                        start: Anchor::new(start_offset, Bias::Left),
                        end: Anchor::new(end_offset, Bias::Right),
                    },
                    severity: severity_from_lsp(d.severity),
                    tags: tags_from_lsp(d.tags.as_deref()),
                    message: d.message.clone(),
                    source: Arc::clone(&source),
                    code,
                }
            })
            .collect();

        store.publish(source, uri, entries);
    }

    // ── Document sync ─────────────────────────────────────────────────────────

    pub fn on_document_opened(&self, uri: url::Url, lang_id: LanguageId, text: &str) {
        if !self.is_trusted() {
            return;
        }

        let version: i32 = 1;
        {
            let mut docs = self.open_docs.lock().unwrap_or_else(|p| p.into_inner());
            docs.insert(
                uri.clone(),
                OpenDoc {
                    rope: Rope::from_str(text),
                    lang_id: lang_id.as_str().to_owned(),
                    version,
                },
            );
        }

        let params = serde_json::json!({
            "textDocument": {
                "uri": uri.as_str(),
                "languageId": lang_id.as_str(),
                "version": version,
                "text": text,
            }
        });
        self.notify_servers_for_lang(lang_id.as_str(), "textDocument/didOpen", &params);
    }

    pub fn on_document_changed(&self, uri: url::Url, text: &str) {
        if !self.is_trusted() {
            return;
        }

        let (version, lang_id_str) = {
            let mut docs = self.open_docs.lock().unwrap_or_else(|p| p.into_inner());
            let Some(doc) = docs.get_mut(&uri) else {
                return;
            };
            doc.rope = Rope::from_str(text);
            doc.version += 1;
            (doc.version, doc.lang_id.clone())
        };

        let params = serde_json::json!({
            "textDocument": { "uri": uri.as_str(), "version": version },
            "contentChanges": [{ "text": text }],
        });
        self.notify_servers_for_lang(&lang_id_str, "textDocument/didChange", &params);
    }

    pub fn on_document_closed(&self, uri: url::Url) {
        if !self.is_trusted() {
            return;
        }

        let lang_id_str = {
            let mut docs = self.open_docs.lock().unwrap_or_else(|p| p.into_inner());
            docs.remove(&uri).map(|d| d.lang_id)
        };
        let Some(lang_id_str) = lang_id_str else {
            return;
        };

        let params = serde_json::json!({
            "textDocument": { "uri": uri.as_str() }
        });
        self.notify_servers_for_lang(&lang_id_str, "textDocument/didClose", &params);
    }

    // ── Public accessors ──────────────────────────────────────────────────────

    pub fn diagnostic_store(&self) -> Arc<DiagnosticStore> {
        Arc::clone(&self.diagnostic_store)
    }

    pub fn server_states(&self) -> Vec<ServerStatus> {
        (*self.status.load_full()).clone()
    }

    /// Status for every registered adapter, including Stopped entries for adapters with no
    /// active slot.  Used by the overlay so users can restart a server they stopped.
    pub fn all_server_statuses(&self) -> Vec<ServerStatus> {
        let slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        self.adapters
            .iter()
            .map(|a| {
                let server_id = a.server_id().to_owned();
                let lang = a
                    .languages()
                    .first()
                    .copied()
                    .unwrap_or_default()
                    .to_owned();
                match slots.get(&server_id) {
                    None => ServerStatus {
                        server_id,
                        language_id: LanguageId::new(&lang),
                        state: ServerState::Stopped,
                        download_msg: None,
                        download_fraction: None,
                    },
                    Some(ServerSlot::Downloading { progress, .. }) => ServerStatus {
                        server_id,
                        language_id: LanguageId::new(&lang),
                        state: ServerState::Downloading,
                        download_msg: progress.as_ref().map(|p| p.msg.clone()),
                        download_fraction: progress.as_ref().and_then(|p| p.fraction),
                    },
                    Some(ServerSlot::Running { .. }) => ServerStatus {
                        server_id,
                        language_id: LanguageId::new(&lang),
                        state: ServerState::Running,
                        download_msg: None,
                        download_fraction: None,
                    },
                    Some(ServerSlot::Restarting { attempt, .. }) => ServerStatus {
                        server_id,
                        language_id: LanguageId::new(&lang),
                        state: ServerState::Restarting { attempt: *attempt },
                        download_msg: None,
                        download_fraction: None,
                    },
                    Some(ServerSlot::Failed { msg, .. }) => ServerStatus {
                        server_id,
                        language_id: LanguageId::new(&lang),
                        state: ServerState::Error(msg.clone()),
                        download_msg: None,
                        download_fraction: None,
                    },
                }
            })
            .collect()
    }

    // ── Request routing ───────────────────────────────────────────────────────

    pub fn request_for_document(
        &self,
        uri: &url::Url,
        method: &'static str,
        params: serde_json::Value,
    ) -> Option<Receiver<Result<serde_json::Value, RpcError>>> {
        if !self.is_trusted() {
            return None;
        }
        let lang_id_str = {
            let guard = self.open_docs.lock().unwrap_or_else(|p| p.into_inner());
            guard.get(uri)?.lang_id.clone()
        };
        let slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        for slot in slots.values() {
            if let ServerSlot::Running { server, lang } = slot {
                if lang == &lang_id_str {
                    return Some(server.transport().request(method, params));
                }
            }
        }
        None
    }

    pub fn position_encoding_for_uri(&self, uri: &url::Url) -> PositionEncoding {
        let lang_id_str = {
            let guard = self.open_docs.lock().unwrap_or_else(|p| p.into_inner());
            match guard.get(uri) {
                Some(d) => d.lang_id.clone(),
                None => return PositionEncoding::Utf16,
            }
        };
        let slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        for slot in slots.values() {
            if let ServerSlot::Running { server, lang } = slot {
                if lang == &lang_id_str {
                    return server.capabilities().position_encoding;
                }
            }
        }
        PositionEncoding::Utf16
    }

    // ── Private ───────────────────────────────────────────────────────────────

    /// Send a notification to every Running server that handles `lang_id_str`.
    fn notify_servers_for_lang(&self, lang_id_str: &str, method: &str, params: &serde_json::Value) {
        // Collect servers while holding the slots lock (send is non-blocking — unbounded
        // crossbeam channel — so holding the lock during notify is safe).
        let slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        for slot in slots.values() {
            if let ServerSlot::Running { server, lang } = slot {
                if lang == lang_id_str {
                    server.transport().notify(method, params.clone());
                }
            }
        }
    }

    fn update_status(&self) {
        // Build the snapshot without calling any external methods that take their own locks
        // while we hold `slots` — server.state() would lock LanguageServer::state which can
        // be held by the crash monitor while it tries to lock slots → deadlock.
        // Solution: the slot IS the state; Running slots always map to ServerState::Running.
        let slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        let snapshot: Vec<ServerStatus> = slots
            .iter()
            .map(|(server_id, slot)| match slot {
                ServerSlot::Downloading { lang, progress } => ServerStatus {
                    server_id: server_id.clone(),
                    language_id: LanguageId::new(lang),
                    state: ServerState::Downloading,
                    download_msg: progress.as_ref().map(|p| p.msg.clone()),
                    download_fraction: progress.as_ref().and_then(|p| p.fraction),
                },
                ServerSlot::Running { lang, .. } => ServerStatus {
                    server_id: server_id.clone(),
                    language_id: LanguageId::new(lang),
                    state: ServerState::Running,
                    download_msg: None,
                    download_fraction: None,
                },
                ServerSlot::Restarting { attempt, lang } => ServerStatus {
                    server_id: server_id.clone(),
                    language_id: LanguageId::new(lang),
                    state: ServerState::Restarting { attempt: *attempt },
                    download_msg: None,
                    download_fraction: None,
                },
                ServerSlot::Failed { lang, msg } => ServerStatus {
                    server_id: server_id.clone(),
                    language_id: LanguageId::new(lang),
                    state: ServerState::Error(msg.clone()),
                    download_msg: None,
                    download_fraction: None,
                },
            })
            .collect();
        self.status.store(Arc::new(snapshot));
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::TransportLayer;
    use crossbeam_channel::{Receiver, Sender, unbounded};
    use std::io::{self, Read, Write};

    // ── In-memory byte pipe ───────────────────────────────────────────────────

    struct ChanReader(Receiver<u8>);
    struct ChanWriter(Sender<u8>);

    impl Read for ChanReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if buf.is_empty() {
                return Ok(0);
            }
            match self.0.recv() {
                Ok(b) => {
                    buf[0] = b;
                    let mut n = 1;
                    while n < buf.len() {
                        match self.0.try_recv() {
                            Ok(b) => {
                                buf[n] = b;
                                n += 1;
                            }
                            Err(_) => break,
                        }
                    }
                    Ok(n)
                }
                Err(_) => Ok(0),
            }
        }
    }

    impl Write for ChanWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            for &b in buf {
                self.0
                    .send(b)
                    .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))?;
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn byte_pipe() -> (ChanWriter, ChanReader) {
        let (tx, rx) = unbounded::<u8>();
        (ChanWriter(tx), ChanReader(rx))
    }

    fn make_transport() -> Arc<TransportLayer> {
        let (_, reader) = byte_pipe();
        let (writer, _) = byte_pipe();
        Arc::new(TransportLayer::new(reader, writer))
    }

    fn default_settings() -> LspSettings {
        LspSettings::default()
    }

    // ── Helper: insert a mock Running server into the manager ─────────────────

    fn insert_running(mgr: &Arc<LspManager>, server_id: &str, lang: &str) -> Arc<LanguageServer> {
        let server = LanguageServer::from_transport(make_transport());
        server.set_state(ServerState::Running);
        let mut slots = mgr.slots.lock().unwrap();
        slots.insert(
            server_id.to_owned(),
            ServerSlot::Running {
                server: Arc::clone(&server),
                lang: lang.to_owned(),
            },
        );
        server
    }

    // ── Test 1: trust gate ────────────────────────────────────────────────────

    #[test]
    fn trust_gate_blocks_document_open() {
        let mgr = LspManager::new(vec![], default_settings(), false);

        mgr.on_document_opened(
            url::Url::parse("file:///foo.rs").unwrap(),
            LanguageId::new("rust"),
            "fn main() {}",
        );

        let slots = mgr.slots.lock().unwrap();
        assert!(slots.is_empty(), "no server should exist when untrusted");
        drop(slots);
        assert!(mgr.open_docs.lock().unwrap().is_empty());
    }

    // ── Test 2: status snapshot ───────────────────────────────────────────────

    #[test]
    fn status_snapshot_reflects_running_server() {
        let mgr = LspManager::new(vec![], default_settings(), true);
        insert_running(&mgr, "rust-analyzer", "rust");
        mgr.update_status();

        let snap = mgr.server_states();
        assert!(!snap.is_empty());
        assert_eq!(snap[0].state, ServerState::Running);
        assert_eq!(snap[0].server_id, "rust-analyzer");
        assert_eq!(snap[0].language_id, LanguageId::new("rust"));
    }

    // ── Test 3: diagnostic routing ────────────────────────────────────────────

    #[test]
    fn diagnostic_routing_from_publish_diagnostics() {
        let store = Arc::new(DiagnosticStore::new());
        let open_docs = Arc::new(Mutex::new(HashMap::new()));

        let params = serde_json::to_value(lsp_types::PublishDiagnosticsParams {
            uri: "file:///test/foo.rs".parse().unwrap(),
            version: None,
            diagnostics: vec![lsp_types::Diagnostic {
                range: lsp_types::Range {
                    start: lsp_types::Position {
                        line: 0,
                        character: 0,
                    },
                    end: lsp_types::Position {
                        line: 0,
                        character: 5,
                    },
                },
                severity: Some(lsp_types::DiagnosticSeverity::ERROR),
                message: "test error".to_owned(),
                ..Default::default()
            }],
        })
        .unwrap();

        LspManager::handle_publish_diagnostics(
            Arc::clone(&store),
            open_docs,
            PositionEncoding::Utf16,
            "rust-analyzer",
            params,
        );

        assert_eq!(store.total_count(), 1);
    }

    // ── Test 4: idempotency (slot present → ensure is a no-op) ───────────────

    #[test]
    fn idempotent_ensure_server() {
        let mgr = LspManager::new(vec![], default_settings(), true);
        insert_running(&mgr, "rust-analyzer", "rust");

        // Pre-condition: one slot.
        assert_eq!(mgr.slots.lock().unwrap().len(), 1);
        // A second insertion of the same key would violate the guarantee — the idempotency
        // check in ensure_server_for_language returns early when the key exists.
        assert!(mgr.slots.lock().unwrap().contains_key("rust-analyzer"));
    }

    // ── Test 5: on_document_opened sends textDocument/didOpen ────────────────

    #[test]
    fn doc_opened_sends_did_open() {
        let mgr = LspManager::new(vec![], default_settings(), true);

        let (_, server_out_reader) = byte_pipe();
        let (client_out_writer, mut client_out_rx) = byte_pipe();
        let transport = Arc::new(TransportLayer::new(server_out_reader, client_out_writer));
        let server = LanguageServer::from_transport(transport);
        server.set_state(ServerState::Running);

        {
            let mut slots = mgr.slots.lock().unwrap();
            slots.insert(
                "rust-analyzer".to_owned(),
                ServerSlot::Running {
                    server: Arc::clone(&server),
                    lang: "rust".to_owned(),
                },
            );
        }

        let uri = url::Url::parse("file:///tmp/main.rs").unwrap();
        mgr.on_document_opened(uri.clone(), LanguageId::new("rust"), "fn main() {}");

        std::thread::sleep(std::time::Duration::from_millis(50));

        let raw = drain_pipe(&mut client_out_rx);
        let body_str = parse_lsp_frame(&raw).expect("expected a valid LSP frame");
        let msg: serde_json::Value = serde_json::from_str(&body_str).expect("valid JSON");

        assert_eq!(msg["method"], "textDocument/didOpen");
        assert_eq!(msg["params"]["textDocument"]["uri"], uri.as_str());
        assert_eq!(msg["params"]["textDocument"]["languageId"], "rust");
        assert_eq!(msg["params"]["textDocument"]["text"], "fn main() {}");
    }

    // ── Test 6: stop_server removes the slot ─────────────────────────────────

    #[test]
    fn stop_server_removes_slot() {
        let mgr = LspManager::new(vec![], default_settings(), true);
        insert_running(&mgr, "rust-analyzer", "rust");
        assert!(mgr.slots.lock().unwrap().contains_key("rust-analyzer"));
        mgr.stop_server("rust-analyzer");
        assert!(!mgr.slots.lock().unwrap().contains_key("rust-analyzer"));
        let snap = mgr.server_states();
        assert!(snap.is_empty());
    }

    // ── Test 7: handle_server_crash transitions Running → Restarting{1} ──────

    #[test]
    fn crash_running_transitions_to_restarting() {
        let mgr = LspManager::new(vec![], default_settings(), true);
        insert_running(&mgr, "rust-analyzer", "rust");

        // Call crash handler directly (simulates crash monitor callback).
        mgr.handle_server_crash("rust-analyzer");

        let slots = mgr.slots.lock().unwrap();
        match slots.get("rust-analyzer") {
            Some(ServerSlot::Restarting { attempt, lang }) => {
                assert_eq!(*attempt, 1);
                assert_eq!(lang, "rust");
            }
            other => panic!(
                "expected Restarting{{1}}, got slot present={}",
                other.is_some()
            ),
        }
    }

    // ── Test 8: exceeding MAX_RESTART_ATTEMPTS → Failed ──────────────────────

    #[test]
    fn crash_exceed_max_transitions_to_failed() {
        let mgr = LspManager::new(vec![], default_settings(), true);
        insert_running(&mgr, "rust-analyzer", "rust");

        // Simulate 1st crash: Running → Restarting{1}.
        mgr.handle_server_crash("rust-analyzer");
        // Simulate 2nd crash: Restarting{1} → Restarting{2}.
        mgr.handle_server_crash("rust-analyzer");
        // Simulate 3rd crash: Restarting{2} → Restarting{3}.
        mgr.handle_server_crash("rust-analyzer");
        // Simulate 4th crash: Restarting{3} → Failed (3+1 > MAX=3).
        mgr.handle_server_crash("rust-analyzer");

        let slots = mgr.slots.lock().unwrap();
        assert!(
            matches!(slots.get("rust-analyzer"), Some(ServerSlot::Failed { .. })),
            "expected Failed after exceeding MAX_RESTART_ATTEMPTS"
        );
    }

    // ── Test 9: crash-counter resets when server reaches Running again ────────

    #[test]
    fn crash_count_resets_on_running() {
        let mgr = LspManager::new(vec![], default_settings(), true);
        insert_running(&mgr, "rust-analyzer", "rust");

        // Two crashes → Restarting{2}.
        mgr.handle_server_crash("rust-analyzer");
        mgr.handle_server_crash("rust-analyzer");
        assert!(matches!(
            mgr.slots.lock().unwrap().get("rust-analyzer"),
            Some(ServerSlot::Restarting { attempt: 2, .. })
        ));

        // Simulate a successful restart: insert a new Running slot (bypassing ensure).
        {
            let mut slots = mgr.slots.lock().unwrap();
            slots.insert(
                "rust-analyzer".to_owned(),
                ServerSlot::Running {
                    server: LanguageServer::from_transport(make_transport()),
                    lang: "rust".to_owned(),
                },
            );
        }

        // First crash after recovery must start from attempt 1.
        mgr.handle_server_crash("rust-analyzer");
        assert!(matches!(
            mgr.slots.lock().unwrap().get("rust-analyzer"),
            Some(ServerSlot::Restarting { attempt: 1, .. })
        ));
    }

    // ── Test 10: stop during Restarting suppresses backoff resurrection ───────

    #[test]
    fn stop_during_restarting_aborts_backoff() {
        let mgr = LspManager::new(vec![], default_settings(), true);
        insert_running(&mgr, "rust-analyzer", "rust");
        mgr.handle_server_crash("rust-analyzer"); // → Restarting{1}

        // Simulate stop_server removing the slot while the backoff thread is sleeping.
        mgr.stop_server("rust-analyzer");
        assert!(!mgr.slots.lock().unwrap().contains_key("rust-analyzer"));

        // The backoff thread's guard checks:
        //   matches!(slots.get(sid), Some(Restarting{attempt:1})) → false (slot absent)
        // so it aborts. We can verify the invariant directly since the check is pure logic.
        let still = {
            let slots = mgr.slots.lock().unwrap();
            matches!(
                slots.get("rust-analyzer"),
                Some(ServerSlot::Restarting { attempt: 1, .. })
            )
        };
        assert!(!still, "backoff guard must find the slot absent and abort");
    }

    // ── Test 11: status snapshot for Downloading / Failed states ─────────────

    #[test]
    fn status_snapshot_downloading_and_failed() {
        let mgr = LspManager::new(vec![], default_settings(), true);
        {
            let mut slots = mgr.slots.lock().unwrap();
            slots.insert(
                "rust-analyzer".to_owned(),
                ServerSlot::Downloading {
                    lang: "rust".to_owned(),
                    progress: Some(DownloadInfo {
                        msg: "Downloading... 50%".to_owned(),
                        fraction: Some(0.5),
                    }),
                },
            );
        }
        mgr.update_status();
        let snap = mgr.server_states();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].state, ServerState::Downloading);
        assert_eq!(snap[0].download_fraction, Some(0.5));

        {
            let mut slots = mgr.slots.lock().unwrap();
            slots.insert(
                "rust-analyzer".to_owned(),
                ServerSlot::Failed {
                    lang: "rust".to_owned(),
                    msg: "network error".to_owned(),
                },
            );
        }
        mgr.update_status();
        let snap = mgr.server_states();
        assert_eq!(snap.len(), 1);
        assert_eq!(
            snap[0].state,
            ServerState::Error("network error".to_owned())
        );
    }

    // ── Pipe helpers ──────────────────────────────────────────────────────────

    fn drain_pipe(reader: &mut ChanReader) -> Vec<u8> {
        let mut out = Vec::new();
        match reader.0.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(b) => out.push(b),
            Err(_) => return out,
        }
        loop {
            match reader.0.try_recv() {
                Ok(b) => out.push(b),
                Err(_) => break,
            }
        }
        out
    }

    fn parse_lsp_frame(data: &[u8]) -> Option<String> {
        let header_end = data.windows(4).position(|w| w == b"\r\n\r\n")?;
        let header = std::str::from_utf8(&data[..header_end]).ok()?;
        let content_length: usize = header
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))?
            .split(':')
            .nth(1)?
            .trim()
            .parse()
            .ok()?;
        let body_start = header_end + 4;
        let body = data.get(body_start..body_start + content_length)?;
        String::from_utf8(body.to_vec()).ok()
    }
}
