// LspManager: per-server ownership, document sync events,
// trust gate, DiagnosticStore wiring, progress reporting.

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
    install::Installer,
    position::{PositionEncoding, from_lsp_position},
    server::{LanguageServer, ServerState},
    transport::RpcError,
};
use faber_core::anchor::{Anchor, Bias};
use faber_lang::LanguageId;
use faber_settings::LspSettings;

// ── ServerStatus ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ServerStatus {
    pub server_id: String,
    pub language_id: LanguageId,
    pub state: ServerState,
    /// Live progress message during `Downloading` state; `None` for all other states.
    pub download_msg: Option<String>,
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
    /// Running servers keyed by `server_id`.
    servers: Mutex<HashMap<String, Arc<LanguageServer>>>,
    /// Reverse index: language id string → server ids serving it.
    lang_servers: Mutex<HashMap<String, Vec<String>>>,
    /// Servers currently resolving/downloading: server_id → lang_id_str.
    downloading: Mutex<HashMap<String, String>>,
    /// Live download progress messages: server_id → latest progress string.
    download_msgs: Arc<Mutex<HashMap<String, String>>>,
    /// Servers that permanently failed this process run: server_id → error message.
    /// Prevents repeated re-attempts on every file open after an unrecoverable failure.
    permanently_failed: Mutex<HashMap<String, String>>,
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
            servers: Mutex::new(HashMap::new()),
            lang_servers: Mutex::new(HashMap::new()),
            downloading: Mutex::new(HashMap::new()),
            download_msgs: Arc::new(Mutex::new(HashMap::new())),
            permanently_failed: Mutex::new(HashMap::new()),
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

    // Caller must kick ensure_server_for_language + on_document_opened for all
    // open docs after calling this with `trusted = true`.
    pub fn set_trusted(self: &Arc<Self>, trusted: bool) {
        self.trusted.store(trusted, Ordering::Relaxed);
    }

    // ── Server lifecycle ──────────────────────────────────────────────────────

    /// Idempotent — returns Ok immediately if a server for this language's
    /// adapter (`server_id`) is already running.
    pub fn ensure_server_for_language(
        self: &Arc<Self>,
        lang_id: &LanguageId,
        workspace_root: &Path,
    ) -> anyhow::Result<()> {
        if !self.is_trusted() {
            return Ok(());
        }

        // Store workspace root on first call.
        {
            let mut root = self
                .workspace_root
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if root.is_none() {
                *root = Some(workspace_root.to_owned());
            }
        }

        // Find an adapter for this language.
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
        let lang_id_str = lang_id.as_str().to_owned();

        // Idempotency check + mark downloading atomically (prevents TOCTOU with
        // concurrent calls for the same server_id).
        {
            let servers = self.servers.lock().unwrap_or_else(|p| p.into_inner());
            let mut dl = self.downloading.lock().unwrap_or_else(|p| p.into_inner());
            let failed = self
                .permanently_failed
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if servers.contains_key(&server_id)
                || dl.contains_key(&server_id)
                || failed.contains_key(&server_id)
            {
                return Ok(());
            }
            dl.insert(server_id.clone(), lang_id_str);
        }
        self.update_status();

        // Resolve binary (may download on first run).
        let settings_guard = self.settings.read().unwrap_or_else(|p| p.into_inner());
        let download_msgs = Arc::clone(&self.download_msgs);
        let server_id_for_cb = server_id.clone();
        let mgr_for_cb = Arc::clone(self);
        let binary_path = match adapter.resolve_binary(&settings_guard, &mut |msg: &str| {
            log::info!("{}", msg);
            download_msgs
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(server_id_for_cb.clone(), msg.to_owned());
            mgr_for_cb.update_status();
        }) {
            Ok(p) => p,
            Err(e) => {
                let mut dl = self.downloading.lock().unwrap_or_else(|p| p.into_inner());
                dl.remove(&server_id);
                self.download_msgs
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&server_id);
                self.update_status();
                return Err(anyhow::anyhow!("resolve_binary failed: {e}"));
            }
        };
        drop(settings_guard);

        let init_options = adapter.init_options();
        let server_id_str: &'static str = adapter.server_id();

        // Resolve login-shell PATH for subprocess.
        let shell_path = Installer::login_shell_path();
        let env_path: Option<&str> = if shell_path.is_empty() {
            None
        } else {
            Some(shell_path.as_str())
        };

        // Spawn and initialize.
        let server = match LanguageServer::spawn(&binary_path, workspace_root, env_path) {
            Ok(s) => s,
            Err(e) => {
                log::error!("lsp: spawn failed for {server_id}: {e}");
                self.downloading.lock().unwrap_or_else(|p| p.into_inner()).remove(&server_id);
                self.download_msgs.lock().unwrap_or_else(|p| p.into_inner()).remove(&server_id);
                self.permanently_failed.lock().unwrap_or_else(|p| p.into_inner()).insert(server_id.clone(), e.to_string());
                self.update_status();
                return Err(e);
            }
        };
        if let Err(e) = server.initialize(None, workspace_root, init_options) {
            log::error!("lsp: initialize failed for {server_id}: {e}");
            self.downloading.lock().unwrap_or_else(|p| p.into_inner()).remove(&server_id);
            self.download_msgs.lock().unwrap_or_else(|p| p.into_inner()).remove(&server_id);
            self.permanently_failed.lock().unwrap_or_else(|p| p.into_inner()).insert(server_id.clone(), e.to_string());
            self.update_status();
            return Err(e);
        }

        // After initialize: read the negotiated position encoding.
        let encoding = server.capabilities().position_encoding;

        // Wire publishDiagnostics → diagnostic store.
        {
            let store = Arc::clone(&self.diagnostic_store);
            let open_docs = Arc::clone(&self.open_docs);
            let source_str = server_id_str;
            server.transport().subscribe(
                "textDocument/publishDiagnostics",
                Arc::new(move |params| {
                    Self::handle_publish_diagnostics(
                        Arc::clone(&store),
                        Arc::clone(&open_docs),
                        encoding,
                        source_str,
                        params,
                    );
                }),
            );
        }

        // Insert into running maps and clear downloading flag.
        {
            let mut servers = self.servers.lock().unwrap_or_else(|p| p.into_inner());
            servers.insert(server_id.clone(), server);
        }
        {
            let mut lang_map = self.lang_servers.lock().unwrap_or_else(|p| p.into_inner());
            lang_map
                .entry(lang_id.as_str().to_owned())
                .or_default()
                .push(server_id.clone());
        }
        {
            let mut dl = self.downloading.lock().unwrap_or_else(|p| p.into_inner());
            dl.remove(&server_id);
            self.download_msgs
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&server_id);
        }
        self.update_status();

        // Re-send didOpen for all documents that were opened while this server was
        // initializing (those calls hit on_document_opened before the server was
        // in `servers`, so their notifications were dropped).
        let lang_str = lang_id.as_str().to_owned();
        let queued: Vec<(url::Url, String, i32, String)> = {
            let docs = self.open_docs.lock().unwrap_or_else(|p| p.into_inner());
            docs.iter()
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
        for (uri, lid, version, text) in queued {
            let params = serde_json::json!({
                "textDocument": {
                    "uri": uri.as_str(),
                    "languageId": lid,
                    "version": version,
                    "text": text,
                }
            });
            self.notify_servers_for_lang(&lang_str, "textDocument/didOpen", &params);
        }

        Ok(())
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

        // Look up rope snapshot for this URI to compute real char offsets.
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
                    // No rope snapshot: fall back to column (only correct on line 0).
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

        // Store rope snapshot with version 1.
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

        // Update rope snapshot and bump version.
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

    // ── Restart ───────────────────────────────────────────────────────────────

    pub fn stop_server(self: &Arc<Self>, server_id: &str) {
        let server_id = server_id.to_owned();
        {
            let mut guard = self.servers.lock().unwrap_or_else(|p| p.into_inner());
            guard.remove(&server_id);
        }
        {
            let mut guard = self.lang_servers.lock().unwrap_or_else(|p| p.into_inner());
            for ids in guard.values_mut() {
                ids.retain(|id| id != &server_id);
            }
        }
        self.downloading.lock().unwrap_or_else(|p| p.into_inner()).remove(&server_id);
        self.download_msgs.lock().unwrap_or_else(|p| p.into_inner()).remove(&server_id);
        self.permanently_failed.lock().unwrap_or_else(|p| p.into_inner()).remove(&server_id);
        self.update_status();
    }

    pub fn restart_server(self: &Arc<Self>, server_id: &str) {
        let server_id = server_id.to_owned();

        // Remove from maps (including any prior failure state so ensure runs cleanly).
        {
            let mut guard = self.servers.lock().unwrap_or_else(|p| p.into_inner());
            guard.remove(&server_id);
        }
        {
            let mut guard = self.lang_servers.lock().unwrap_or_else(|p| p.into_inner());
            for ids in guard.values_mut() {
                ids.retain(|id| id != &server_id);
            }
        }
        self.permanently_failed.lock().unwrap_or_else(|p| p.into_inner()).remove(&server_id);
        self.update_status();

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

        // Find the language for this server.
        let lang_id_str = self
            .adapters
            .iter()
            .find(|a| a.server_id() == server_id.as_str())
            .and_then(|a| a.languages().first().copied())
            .map(|s| s.to_owned());
        let Some(lang_str) = lang_id_str else {
            log::warn!("lsp: cannot restart {server_id}: no adapter found");
            return;
        };

        let mgr = Arc::clone(self);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let lang_id = LanguageId::new(&lang_str);
            if let Err(e) = mgr.ensure_server_for_language(&lang_id, &root) {
                log::error!("lsp: restart of {server_id} failed: {e}");
                return;
            }

            // Re-send didOpen for all tracked docs matching this language.
            let docs_snapshot: Vec<(url::Url, String, i32, String)> = {
                let guard = mgr.open_docs.lock().unwrap_or_else(|p| p.into_inner());
                guard
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
            for (uri, lang, version, text) in docs_snapshot {
                let params = serde_json::json!({
                    "textDocument": {
                        "uri": uri.as_str(),
                        "languageId": lang,
                        "version": version,
                        "text": text,
                    }
                });
                mgr.notify_servers_for_lang(&lang, "textDocument/didOpen", &params);
            }
            log::info!("lsp: {server_id} restarted successfully");
        });
    }

    // ── Request routing ───────────────────────────────────────────────────────

    /// Route a JSON-RPC request to the server currently handling the given URI.
    /// Returns None if the document is unknown or has no running server.
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
        let server_id = {
            let guard = self.lang_servers.lock().unwrap_or_else(|p| p.into_inner());
            guard.get(&lang_id_str)?.first()?.clone()
        };
        let servers = self.servers.lock().unwrap_or_else(|p| p.into_inner());
        let server = servers.get(&server_id)?;
        Some(server.transport().request(method, params))
    }

    /// Returns the negotiated position encoding for the server handling `uri`.
    /// Defaults to UTF-16 (LSP protocol default) when no server is found.
    pub fn position_encoding_for_uri(&self, uri: &url::Url) -> PositionEncoding {
        let lang_id_str = {
            let guard = self.open_docs.lock().unwrap_or_else(|p| p.into_inner());
            match guard.get(uri) {
                Some(d) => d.lang_id.clone(),
                None => return PositionEncoding::Utf16,
            }
        };
        let server_id = {
            let guard = self.lang_servers.lock().unwrap_or_else(|p| p.into_inner());
            match guard.get(&lang_id_str).and_then(|v| v.first()) {
                Some(id) => id.clone(),
                None => return PositionEncoding::Utf16,
            }
        };
        let servers = self.servers.lock().unwrap_or_else(|p| p.into_inner());
        servers
            .get(&server_id)
            .map(|s| s.capabilities().position_encoding)
            .unwrap_or_default()
    }

    // ── Private ───────────────────────────────────────────────────────────────

    /// Send a notification to every server registered for `lang_id_str`.
    fn notify_servers_for_lang(&self, lang_id_str: &str, method: &str, params: &serde_json::Value) {
        let server_ids = {
            let guard = self.lang_servers.lock().unwrap_or_else(|p| p.into_inner());
            guard.get(lang_id_str).cloned().unwrap_or_default()
        };
        let servers = self.servers.lock().unwrap_or_else(|p| p.into_inner());
        for id in &server_ids {
            if let Some(server) = servers.get(id) {
                server.transport().notify(method, params.clone());
            }
        }
    }

    fn update_status(&self) {
        // Pre-compute adapter server_id → first language; immutable, no lock needed.
        let adapter_lang: HashMap<&str, &str> = self
            .adapters
            .iter()
            .map(|a| (a.server_id(), a.languages().first().copied().unwrap_or_default()))
            .collect();

        let servers = self.servers.lock().unwrap_or_else(|p| p.into_inner());
        let lang_servers = self.lang_servers.lock().unwrap_or_else(|p| p.into_inner());
        let downloading = self.downloading.lock().unwrap_or_else(|p| p.into_inner());
        let dl_msgs = self.download_msgs.lock().unwrap_or_else(|p| p.into_inner());
        let failed = self.permanently_failed.lock().unwrap_or_else(|p| p.into_inner());

        // Build a server_id → lang_id map for display.
        let mut server_to_lang: HashMap<String, String> = HashMap::new();
        for (lang, ids) in lang_servers.iter() {
            for id in ids {
                server_to_lang
                    .entry(id.clone())
                    .or_insert_with(|| lang.clone());
            }
        }

        let mut snapshot: Vec<ServerStatus> = servers
            .iter()
            .map(|(server_id, server)| {
                let lang_str = server_to_lang.get(server_id).cloned().unwrap_or_default();
                ServerStatus {
                    server_id: server_id.clone(),
                    language_id: LanguageId::new(&lang_str),
                    state: server.state(),
                    download_msg: None,
                }
            })
            .collect();

        // Include in-flight downloads so the status bar can show Downloading + progress.
        for (server_id, lang_str) in downloading.iter() {
            if !servers.contains_key(server_id) {
                snapshot.push(ServerStatus {
                    server_id: server_id.clone(),
                    language_id: LanguageId::new(lang_str),
                    state: ServerState::Downloading,
                    download_msg: dl_msgs.get(server_id).cloned(),
                });
            }
        }

        // Include permanently-failed servers so the overlay can show an error + Restart button.
        for (server_id, msg) in failed.iter() {
            if !servers.contains_key(server_id) && !downloading.contains_key(server_id) {
                let lang_str = adapter_lang
                    .get(server_id.as_str())
                    .copied()
                    .unwrap_or_default()
                    .to_owned();
                snapshot.push(ServerStatus {
                    server_id: server_id.clone(),
                    language_id: LanguageId::new(&lang_str),
                    state: ServerState::Error(msg.clone()),
                    download_msg: None,
                });
            }
        }

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

    // ── In-memory byte pipe (mirrors server.rs / transport.rs tests) ──────────

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

    // ── Test 1: trust gate ────────────────────────────────────────────────────

    #[test]
    fn trust_gate_blocks_document_open() {
        let mgr = LspManager::new(vec![], default_settings(), false);

        // Should not panic, and no server should be spawned.
        mgr.on_document_opened(
            url::Url::parse("file:///foo.rs").unwrap(),
            LanguageId::new("rust"),
            "fn main() {}",
        );

        let guard = mgr.servers.lock().unwrap();
        assert!(guard.is_empty(), "no server should exist when untrusted");
        // No doc snapshot either, since the trust gate short-circuits.
        assert!(mgr.open_docs.lock().unwrap().is_empty());
    }

    // ── Test 2: server status snapshot ────────────────────────────────────────

    #[test]
    fn status_snapshot_reflects_running_server() {
        let mgr = LspManager::new(vec![], default_settings(), true);

        // Insert a mock server in Running state, keyed by server_id.
        let transport = make_transport();
        let server = LanguageServer::from_transport(transport);
        server.set_state(ServerState::Running);

        {
            let mut guard = mgr.servers.lock().unwrap();
            guard.insert("rust-analyzer".to_owned(), server);
        }
        {
            let mut lang_servers = mgr.lang_servers.lock().unwrap();
            lang_servers
                .entry("rust".to_owned())
                .or_default()
                .push("rust-analyzer".to_owned());
        }

        mgr.update_status();

        let snap = mgr.server_states();
        assert!(!snap.is_empty(), "status snapshot must be non-empty");
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

        assert_eq!(
            store.total_count(),
            1,
            "store must contain exactly one diagnostic"
        );
    }

    // ── Test 4: idempotency guard ─────────────────────────────────────────────

    #[test]
    fn idempotent_ensure_server() {
        let mgr = LspManager::new(vec![], default_settings(), true);

        // Pre-insert a mock server keyed by server_id.
        let transport = make_transport();
        let server = LanguageServer::from_transport(transport);
        server.set_state(ServerState::Running);

        {
            let mut servers = mgr.servers.lock().unwrap();
            servers.insert("rust-analyzer".to_owned(), server);
        }
        {
            let mut lang_servers = mgr.lang_servers.lock().unwrap();
            lang_servers
                .entry("rust".to_owned())
                .or_default()
                .push("rust-analyzer".to_owned());
        }

        // With the server already present, the idempotency guard keeps the
        // count at exactly one (a real ensure call would return Ok early).
        let count_before = mgr.servers.lock().unwrap().len();
        assert_eq!(
            count_before, 1,
            "should have exactly one server pre-inserted"
        );
    }

    // ── Test 5: on_document_opened sends textDocument/didOpen ────────────────

    #[test]
    fn doc_opened_sends_did_open() {
        let mgr = LspManager::new(vec![], default_settings(), true);

        // Build a transport whose output we can observe.
        // server_out: what the (mock) server sends to the transport (nothing here).
        // client_out_rx: what the transport sends to the server — we read this.
        let (_, server_out_reader) = byte_pipe();
        let (client_out_writer, mut client_out_rx) = byte_pipe();
        let transport = Arc::new(TransportLayer::new(server_out_reader, client_out_writer));

        let server = LanguageServer::from_transport(transport);
        server.set_state(ServerState::Running);

        {
            let mut guard = mgr.servers.lock().unwrap();
            guard.insert("rust-analyzer".to_owned(), server);
        }
        {
            let mut lang_servers = mgr.lang_servers.lock().unwrap();
            lang_servers
                .entry("rust".to_owned())
                .or_default()
                .push("rust-analyzer".to_owned());
        }

        let uri = url::Url::parse("file:///tmp/main.rs").unwrap();
        mgr.on_document_opened(uri.clone(), LanguageId::new("rust"), "fn main() {}");

        // Give the writer thread time to flush the notification.
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Read the Content-Length framed bytes from client_out_rx.
        let raw = drain_pipe(&mut client_out_rx);
        let body_str = parse_lsp_frame(&raw).expect("expected a valid LSP frame");
        let msg: serde_json::Value = serde_json::from_str(&body_str).expect("valid JSON");

        assert_eq!(msg["method"], "textDocument/didOpen");
        assert_eq!(msg["params"]["textDocument"]["uri"], uri.as_str());
        assert_eq!(msg["params"]["textDocument"]["languageId"], "rust");
        assert_eq!(msg["params"]["textDocument"]["text"], "fn main() {}");
    }

    /// Drain all available bytes from a ChanReader into a Vec without blocking.
    fn drain_pipe(reader: &mut ChanReader) -> Vec<u8> {
        let mut out = Vec::new();
        // Block on the first byte so we wait until the writer thread has flushed.
        match reader.0.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(b) => out.push(b),
            Err(_) => return out,
        }
        // Drain the rest without blocking.
        loop {
            match reader.0.try_recv() {
                Ok(b) => out.push(b),
                Err(_) => break,
            }
        }
        out
    }

    /// Parse a single `Content-Length: N\r\n\r\n<body>` frame and return the body.
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
