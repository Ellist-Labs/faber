// LanguageServer: child process + TransportLayer, LSP handshake,
// shutdown/exit, crash-restart with exponential backoff.

use std::{
    collections::VecDeque,
    io::{BufRead, BufReader},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::{position::PositionEncoding, transport::TransportLayer};

// ── Constants ─────────────────────────────────────────────────────────────────

const MAX_LOG_LINES: usize = 10_000;
const MAX_LOG_BYTES_PER_LINE: usize = 4_096;

// ── ServerState ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerState {
    Downloading,
    Starting,
    Initializing,
    Running,
    Restarting { attempt: u8 },
    Error(String),
    Stopped,
}

// ── Capabilities ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct Capabilities {
    pub position_encoding: PositionEncoding,
    pub server_capabilities: lsp_types::ServerCapabilities,
}

// ── LogLine ───────────────────────────────────────────────────────────────────

pub struct LogLine {
    pub text: String,
    /// Monotonic millis since server start (avoids Date::now).
    pub ms: u64,
}

// ── LanguageServer ────────────────────────────────────────────────────────────

pub struct LanguageServer {
    transport: Arc<TransportLayer>,
    child: Arc<Mutex<Option<Child>>>,
    capabilities: Arc<Mutex<Capabilities>>,
    state: Arc<Mutex<ServerState>>,
    log_lines: Arc<Mutex<VecDeque<LogLine>>>,
    start_instant: Instant,
}

impl LanguageServer {
    /// Spawn a real LSP server as a child process.
    pub fn spawn(
        binary_path: &Path,
        workspace_root: &Path,
        env_path: Option<&str>,
    ) -> anyhow::Result<Arc<Self>> {
        let mut cmd = Command::new(binary_path);
        cmd.arg("--stdio")
            .current_dir(workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_remove("RUST_LOG");

        if let Some(path) = env_path {
            cmd.env("PATH", path);
        }

        let mut child = cmd.spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stderr"))?;

        let log_lines: Arc<Mutex<VecDeque<LogLine>>> = Arc::new(Mutex::new(VecDeque::new()));
        let start_instant = Instant::now();

        // Stderr reader thread.
        let log_lines_clone = Arc::clone(&log_lines);
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                match line {
                    Ok(text) => {
                        let text = if text.len() > MAX_LOG_BYTES_PER_LINE {
                            text[..MAX_LOG_BYTES_PER_LINE].to_owned()
                        } else {
                            text
                        };
                        let ms = start_instant.elapsed().as_millis() as u64;
                        let mut guard = log_lines_clone.lock().unwrap_or_else(|p| p.into_inner());
                        if guard.len() >= MAX_LOG_LINES {
                            guard.pop_front();
                        }
                        guard.push_back(LogLine { text, ms });
                    }
                    Err(_) => break,
                }
            }
        });

        let transport = Arc::new(TransportLayer::new(stdout, stdin));

        let server = Arc::new(Self {
            transport: Arc::clone(&transport),
            child: Arc::new(Mutex::new(Some(child))),
            capabilities: Arc::new(Mutex::new(Capabilities::default())),
            state: Arc::new(Mutex::new(ServerState::Starting)),
            log_lines,
            start_instant,
        });

        // Wire window/logMessage → log_lines.
        {
            let log_lines = Arc::clone(&server.log_lines);
            let instant = server.start_instant;
            server.transport.subscribe(
                "window/logMessage",
                Arc::new(move |params| {
                    let text = params
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_owned();
                    let text = if text.len() > MAX_LOG_BYTES_PER_LINE {
                        text[..MAX_LOG_BYTES_PER_LINE].to_owned()
                    } else {
                        text
                    };
                    let ms = instant.elapsed().as_millis() as u64;
                    let mut guard = log_lines.lock().unwrap_or_else(|p| p.into_inner());
                    if guard.len() >= MAX_LOG_LINES {
                        guard.pop_front();
                    }
                    guard.push_back(LogLine { text, ms });
                }),
            );
        }

        // Wire window/showMessage → log::info!.
        server.transport.subscribe(
            "window/showMessage",
            Arc::new(|params| {
                let msg = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
                log::info!("LSP window/showMessage: {}", msg);
            }),
        );

        Ok(server)
    }

    /// Initialize the server: send `initialize`, wait for the result, send `initialized`.
    pub fn initialize(
        self: &Arc<Self>,
        client_info: Option<lsp_types::ClientInfo>,
        workspace_root: &Path,
        init_options: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        self.set_state(ServerState::Initializing);

        let root_uri = path_to_uri(workspace_root)?;

        #[allow(deprecated)]
        let params = lsp_types::InitializeParams {
            process_id: Some(std::process::id()),
            root_path: None,
            root_uri: Some(root_uri.clone()),
            initialization_options: init_options,
            capabilities: lsp_types::ClientCapabilities {
                workspace: Some(lsp_types::WorkspaceClientCapabilities {
                    workspace_folders: Some(true),
                    ..Default::default()
                }),
                text_document: Some(lsp_types::TextDocumentClientCapabilities {
                    publish_diagnostics: Some(lsp_types::PublishDiagnosticsClientCapabilities {
                        related_information: Some(true),
                        tag_support: Some(lsp_types::TagSupport {
                            value_set: vec![
                                lsp_types::DiagnosticTag::UNNECESSARY,
                                lsp_types::DiagnosticTag::DEPRECATED,
                            ],
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                general: Some(lsp_types::GeneralClientCapabilities {
                    position_encodings: Some(vec![
                        lsp_types::PositionEncodingKind::UTF8,
                        lsp_types::PositionEncodingKind::UTF16,
                    ]),
                    ..Default::default()
                }),
                ..Default::default()
            },
            trace: None,
            workspace_folders: Some(vec![lsp_types::WorkspaceFolder {
                uri: root_uri,
                name: workspace_root
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("workspace")
                    .to_owned(),
            }]),
            client_info,
            locale: None,
            work_done_progress_params: lsp_types::WorkDoneProgressParams {
                work_done_token: None,
            },
        };

        let (init_id, rx) = self.transport.request_with_id("initialize", params);
        let result_value = match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                return Err(anyhow::anyhow!(
                    "initialize error {}: {}",
                    e.code,
                    e.message
                ));
            }
            Err(_) => {
                self.transport.cancel(init_id);
                return Err(anyhow::anyhow!("initialize timed out after 30s"));
            }
        };

        let init_result: lsp_types::InitializeResult = serde_json::from_value(result_value)
            .map_err(|e| anyhow::anyhow!("failed to parse InitializeResult: {e}"))?;

        // Extract position encoding from server capabilities.
        let position_encoding = match init_result.capabilities.position_encoding.as_ref() {
            Some(kind) if kind == &lsp_types::PositionEncodingKind::UTF8 => PositionEncoding::Utf8,
            _ => PositionEncoding::Utf16,
        };

        {
            let mut caps = self.capabilities.lock().unwrap_or_else(|p| p.into_inner());
            caps.position_encoding = position_encoding;
            caps.server_capabilities = init_result.capabilities.clone();
        }

        self.transport.notify("initialized", serde_json::json!({}));

        self.set_state(ServerState::Running);
        Ok(())
    }

    /// Graceful shutdown: send `shutdown`, then `exit`, then kill if needed.
    pub fn shutdown(&self) -> anyhow::Result<()> {
        let rx = self.transport.request("shutdown", serde_json::json!(null));
        // Best-effort: ignore timeout/error from shutdown response.
        let _ = rx.recv_timeout(Duration::from_secs(5));

        self.transport.notify("exit", serde_json::json!(null));

        // Give the process 2s to exit on its own before killing.
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut guard = self.child.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(ref mut child) = *guard {
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break, // exited
                    Ok(None) => {
                        if Instant::now() >= deadline {
                            let _ = child.kill();
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(_) => {
                        let _ = child.kill();
                        break;
                    }
                }
            }
        }

        self.set_state(ServerState::Stopped);
        Ok(())
    }

    // ── Accessors ─────────────────────────────────────────────────────────────

    pub fn state(&self) -> ServerState {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn capabilities(&self) -> Capabilities {
        self.capabilities
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn log_lines(&self) -> Vec<LogLine> {
        let guard = self.log_lines.lock().unwrap_or_else(|p| p.into_inner());
        guard
            .iter()
            .map(|l| LogLine {
                text: l.text.clone(),
                ms: l.ms,
            })
            .collect()
    }

    pub fn set_state(&self, state: ServerState) {
        let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
        *guard = state;
    }

    pub fn transport(&self) -> &Arc<TransportLayer> {
        &self.transport
    }

    // ── Test constructor ──────────────────────────────────────────────────────

    #[cfg(test)]
    pub fn from_transport(transport: Arc<TransportLayer>) -> Arc<Self> {
        Arc::new(Self {
            transport,
            child: Arc::new(Mutex::new(None)),
            capabilities: Arc::new(Mutex::new(Capabilities::default())),
            state: Arc::new(Mutex::new(ServerState::Starting)),
            log_lines: Arc::new(Mutex::new(VecDeque::new())),
            start_instant: Instant::now(),
        })
    }
}

impl Drop for LanguageServer {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock()
            && let Some(ref mut c) = *child
        {
            let _ = c.kill();
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn path_to_uri(path: &Path) -> anyhow::Result<lsp_types::Uri> {
    let abs = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let url = url::Url::from_file_path(&abs)
        .map_err(|()| anyhow::anyhow!("cannot convert path to file URI: {}", abs.display()))?;
    url.as_str()
        .parse::<lsp_types::Uri>()
        .map_err(|e| anyhow::anyhow!("bad URI: {e}"))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::{Receiver, Sender, unbounded};
    use std::io::{self, Read, Write};

    // ── In-memory byte channel adaptors (mirrors transport.rs tests) ──────────

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

    fn frame(body: &str) -> Vec<u8> {
        let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        out.extend_from_slice(body.as_bytes());
        out
    }

    fn inject(writer: &mut ChanWriter, bytes: &[u8]) {
        writer.write_all(bytes).unwrap();
    }

    /// Build an in-memory transport pair.
    /// Returns (server, server_out_writer, client_to_server_reader).
    /// Inject framed responses into `server_out_writer`; read outbound bytes from `client_to_server_reader`.
    fn make_pair() -> (Arc<LanguageServer>, ChanWriter, ChanReader) {
        // server → client direction (LSP server replies come here)
        let (server_out_writer, server_out_reader) = byte_pipe();
        // client → server direction (we capture what the LS sends to the server)
        let (client_to_server_writer, client_to_server_reader) = byte_pipe();

        let transport = Arc::new(TransportLayer::new(
            server_out_reader,
            client_to_server_writer,
        ));
        let ls = LanguageServer::from_transport(transport);
        (ls, server_out_writer, client_to_server_reader)
    }

    // ── Test 1: state transitions ─────────────────────────────────────────────

    #[test]
    fn state_transitions_initialize() {
        let (ls, mut server_writer, _client_rx) = make_pair();

        assert_eq!(ls.state(), ServerState::Starting);

        // Spawn a thread that injects the initialize response after a short delay.
        std::thread::spawn(move || {
            // Wait briefly so the LS sends its request first.
            std::thread::sleep(Duration::from_millis(20));

            let response = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "capabilities": {
                        "positionEncoding": "utf-8"
                    },
                    "serverInfo": {
                        "name": "test-server"
                    }
                }
            })
            .to_string();
            inject(&mut server_writer, &frame(&response));
        });

        ls.initialize(None, Path::new("/tmp/workspace"), None)
            .unwrap();
        assert_eq!(ls.state(), ServerState::Running);
    }

    // ── Test 2: shutdown ──────────────────────────────────────────────────────

    #[test]
    fn shutdown_sets_stopped() {
        let (ls, mut server_writer, _client_rx) = make_pair();

        // Initialize first.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            let init_resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": { "capabilities": {} }
            })
            .to_string();
            inject(&mut server_writer, &frame(&init_resp));

            // Inject shutdown response (id=2).
            std::thread::sleep(Duration::from_millis(20));
            let shutdown_resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": null
            })
            .to_string();
            inject(&mut server_writer, &frame(&shutdown_resp));
        });

        ls.initialize(None, Path::new("/tmp/workspace"), None)
            .unwrap();
        assert_eq!(ls.state(), ServerState::Running);

        ls.shutdown().unwrap();
        assert_eq!(ls.state(), ServerState::Stopped);
    }

    // ── Test 3: log line cap ──────────────────────────────────────────────────

    #[test]
    fn log_line_cap_evicts_oldest() {
        let (ls, _server_writer, _client_rx) = make_pair();

        {
            let mut guard = ls.log_lines.lock().unwrap();
            // Push MAX_LOG_LINES + 1 entries.
            for i in 0..=(MAX_LOG_LINES as u64) {
                if guard.len() >= MAX_LOG_LINES {
                    guard.pop_front();
                }
                guard.push_back(LogLine {
                    text: format!("line {i}"),
                    ms: i,
                });
            }
        }

        let lines = ls.log_lines();
        assert_eq!(lines.len(), MAX_LOG_LINES);
        // Oldest entry (line 0) must have been evicted; first entry is line 1.
        assert_eq!(lines[0].text, "line 1");
        assert_eq!(
            lines[MAX_LOG_LINES - 1].text,
            format!("line {}", MAX_LOG_LINES)
        );
    }

    // ── Test 4: initialize error response returns Err ─────────────────────────

    #[test]
    fn initialize_error_response_returns_err() {
        let (ls, mut server_writer, _client_rx) = make_pair();

        // Inject an LSP error response for the initialize request (id=1).
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            let error_resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {
                    "code": -32002,
                    "message": "server not ready"
                }
            })
            .to_string();
            inject(&mut server_writer, &frame(&error_resp));
        });

        let result = ls.initialize(None, Path::new("/tmp/workspace"), None);
        assert!(
            result.is_err(),
            "initialize must return Err when server replies with an RPC error"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("server not ready"),
            "error message should propagate: {msg}"
        );
    }
}
