// JSON-RPC 2.0 transport: Content-Length framing, reader/writer threads,
// pending-request map, notification subscriptions.

use std::{
    collections::HashMap,
    io::{Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI32, Ordering},
    },
    thread,
};

use crossbeam_channel::{Receiver, Sender, unbounded};
use serde_json::Value;

// ── Public types ─────────────────────────────────────────────────────────────

pub type RequestId = i32;
pub type ResponseHandler = Box<dyn FnOnce(Result<Value, RpcError>) + Send>;
pub type NotificationHandler = Arc<dyn Fn(Value) + Send + Sync>;

#[derive(Debug)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
}

// ── Internal write message ────────────────────────────────────────────────────

enum WriteMessage {
    Request {
        id: RequestId,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
}

// ── TransportLayer ────────────────────────────────────────────────────────────

pub struct TransportLayer {
    pending: Arc<Mutex<HashMap<RequestId, ResponseHandler>>>,
    subscriptions: Arc<Mutex<HashMap<String, Vec<NotificationHandler>>>>,
    next_id: Arc<AtomicI32>,
    write_tx: Sender<WriteMessage>,
    _reader: thread::JoinHandle<()>,
    _writer: thread::JoinHandle<()>,
}

impl TransportLayer {
    pub fn new(reader: impl Read + Send + 'static, writer: impl Write + Send + 'static) -> Self {
        let pending: Arc<Mutex<HashMap<RequestId, ResponseHandler>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let subscriptions: Arc<Mutex<HashMap<String, Vec<NotificationHandler>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let next_id = Arc::new(AtomicI32::new(1));
        let (write_tx, write_rx) = unbounded::<WriteMessage>();

        let reader_handle =
            Self::spawn_reader(reader, Arc::clone(&pending), Arc::clone(&subscriptions));
        let writer_handle = Self::spawn_writer(writer, write_rx);

        Self {
            pending,
            subscriptions,
            next_id,
            write_tx,
            _reader: reader_handle,
            _writer: writer_handle,
        }
    }

    /// Send a request; returns a channel receiver that yields the result when the server replies.
    pub fn request(
        &self,
        method: &str,
        params: impl serde::Serialize,
    ) -> Receiver<Result<Value, RpcError>> {
        self.request_with_id(method, params).1
    }

    /// Send a notification (no response expected).
    pub fn notify(&self, method: &str, params: impl serde::Serialize) {
        let params = match serde_json::to_value(params) {
            Ok(v) => v,
            Err(e) => {
                log::error!("transport: failed to serialize notification params: {e}");
                return;
            }
        };
        if let Err(e) = self.write_tx.send(WriteMessage::Notification {
            method: method.to_owned(),
            params,
        }) {
            log::error!("transport: writer channel closed, cannot send notification: {e}");
        }
    }

    /// Register a persistent handler for inbound notifications of `method`.
    pub fn subscribe(&self, method: impl Into<String>, handler: Arc<dyn Fn(Value) + Send + Sync>) {
        let mut subs = self.subscriptions.lock().unwrap_or_else(|p| p.into_inner());
        subs.entry(method.into()).or_default().push(handler);
    }

    /// Like `request`, but also returns the request ID so the caller can cancel it.
    pub fn request_with_id(
        &self,
        method: &str,
        params: impl serde::Serialize,
    ) -> (RequestId, Receiver<Result<Value, RpcError>>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let params = match serde_json::to_value(params) {
            Ok(v) => v,
            Err(e) => {
                log::error!("transport: failed to serialize request params: {e}");
                let (tx, rx) = unbounded();
                let _ = tx.send(Err(RpcError {
                    code: -32700,
                    message: format!("serialize error: {e}"),
                    data: None,
                }));
                return (id, rx);
            }
        };

        let (tx, rx) = unbounded::<Result<Value, RpcError>>();
        let handler: ResponseHandler = Box::new(move |result| {
            let _ = tx.send(result);
        });

        {
            let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
            pending.insert(id, handler);
        }

        if let Err(e) = self.write_tx.send(WriteMessage::Request {
            id,
            method: method.to_owned(),
            params,
        }) {
            log::error!("transport: writer channel closed: {e}");
            let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(handler) = pending.remove(&id) {
                drop(pending);
                handler(Err(RpcError {
                    code: -32603,
                    message: "transport closed".into(),
                    data: None,
                }));
            }
        }

        (id, rx)
    }

    /// Remove a pending request handler and send `$/cancelRequest` to the server.
    /// Call this when a `recv_timeout` expires to prevent handler leaks.
    pub fn cancel(&self, id: RequestId) {
        {
            let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
            pending.remove(&id);
        }
        self.notify("$/cancelRequest", serde_json::json!({ "id": id }));
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    fn spawn_reader(
        mut reader: impl Read + Send + 'static,
        pending: Arc<Mutex<HashMap<RequestId, ResponseHandler>>>,
        subscriptions: Arc<Mutex<HashMap<String, Vec<NotificationHandler>>>>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            loop {
                // Read headers until \r\n\r\n
                let content_length = match read_content_length(&mut reader) {
                    Ok(Some(n)) => n,
                    Ok(None) => {
                        log::info!("transport: reader EOF");
                        break;
                    }
                    Err(e) => {
                        log::error!("transport: header read error: {e}");
                        break;
                    }
                };

                // Read body
                let mut body = vec![0u8; content_length];
                if let Err(e) = reader.read_exact(&mut body) {
                    log::error!("transport: body read error: {e}");
                    break;
                }

                let msg: Value = match serde_json::from_slice(&body) {
                    Ok(v) => v,
                    Err(e) => {
                        log::warn!("transport: JSON parse error: {e}");
                        continue;
                    }
                };

                route_message(msg, &pending, &subscriptions);
            }
        })
    }

    fn spawn_writer(
        mut writer: impl Write + Send + 'static,
        rx: Receiver<WriteMessage>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            for msg in rx {
                let body = match build_json(&msg) {
                    Ok(b) => b,
                    Err(e) => {
                        log::error!("transport: JSON build error: {e}");
                        continue;
                    }
                };
                let header = format!("Content-Length: {}\r\n\r\n", body.len());
                if let Err(e) = writer
                    .write_all(header.as_bytes())
                    .and_then(|_| writer.write_all(&body))
                    .and_then(|_| writer.flush())
                {
                    log::error!("transport: write error: {e}");
                    break;
                }
            }
        })
    }
}

// ── Framing helpers ───────────────────────────────────────────────────────────

/// Read bytes until `\r\n\r\n`, then parse `Content-Length`.
/// Returns `Ok(None)` on clean EOF before any bytes are read.
fn read_content_length(reader: &mut impl Read) -> anyhow::Result<Option<usize>> {
    let mut header_buf = Vec::with_capacity(64);
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => {
                if header_buf.is_empty() {
                    return Ok(None);
                }
                anyhow::bail!("EOF mid-header");
            }
            Ok(_) => {
                header_buf.push(byte[0]);
                if header_buf.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            Err(e) => return Err(e.into()),
        }
    }

    let header_str =
        std::str::from_utf8(&header_buf).map_err(|e| anyhow::anyhow!("non-UTF8 header: {e}"))?;

    for line in header_str.split("\r\n") {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("content-length:") {
            let n: usize = rest
                .trim()
                .parse()
                .map_err(|e| anyhow::anyhow!("bad Content-Length value: {e}"))?;
            return Ok(Some(n));
        }
    }

    anyhow::bail!("no Content-Length header found in: {header_str:?}")
}

/// Route a parsed JSON-RPC message to the correct handler.
fn route_message(
    msg: Value,
    pending: &Mutex<HashMap<RequestId, ResponseHandler>>,
    subscriptions: &Mutex<HashMap<String, Vec<NotificationHandler>>>,
) {
    let has_id = msg.get("id").map(|v| !v.is_null()).unwrap_or(false);
    let has_method = msg.get("method").and_then(Value::as_str).is_some();

    if has_id && !has_method {
        // Response (result or error)
        let id = match msg["id"].as_i64() {
            Some(n) => n as RequestId,
            None => {
                log::warn!("transport: response with non-integer id: {:?}", msg["id"]);
                return;
            }
        };

        let handler = {
            let mut guard = pending.lock().unwrap_or_else(|p| p.into_inner());
            guard.remove(&id)
        };

        if let Some(handler) = handler {
            if let Some(err_obj) = msg.get("error") {
                let code = err_obj
                    .get("code")
                    .and_then(Value::as_i64)
                    .unwrap_or(-32603) as i32;
                let message = err_obj
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_owned();
                let data = err_obj.get("data").cloned();
                handler(Err(RpcError {
                    code,
                    message,
                    data,
                }));
            } else {
                let result = msg.get("result").cloned().unwrap_or(Value::Null);
                handler(Ok(result));
            }
        } else {
            log::warn!("transport: received response for unknown id {id}");
        }
    } else if has_method {
        // Notification (no id, or null id)
        let method = msg["method"].as_str().unwrap().to_owned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        // Clone the handler Arc refs while holding the lock, then drop the lock
        // before invoking them. This avoids a deadlock if a handler calls back
        // into subscribe()/notify().
        let handlers: Vec<NotificationHandler> = {
            let guard = subscriptions.lock().unwrap_or_else(|p| p.into_inner());
            guard.get(&method).map(|hs| hs.to_vec()).unwrap_or_default()
        };

        if handlers.is_empty() {
            log::debug!("transport: unhandled notification: {method}");
        } else {
            for handler in handlers {
                handler(params.clone());
            }
        }
    } else {
        log::warn!("transport: unrecognised message shape: {msg:?}");
    }
}

/// Serialise a `WriteMessage` to a JSON byte vec.
fn build_json(msg: &WriteMessage) -> anyhow::Result<Vec<u8>> {
    let value = match msg {
        WriteMessage::Request { id, method, params } => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }),
        WriteMessage::Notification { method, params } => serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }),
    };
    Ok(serde_json::to_vec(&value)?)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io,
        sync::{Arc, Mutex},
    };

    // ── In-memory byte channel adaptors ──────────────────────────────────────

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
                    // Drain as many additional bytes as are immediately available.
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
                Err(_) => Ok(0), // EOF
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

    /// Frame a JSON-RPC message the way a real LSP server would.
    fn frame(body: &str) -> Vec<u8> {
        let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        out.extend_from_slice(body.as_bytes());
        out
    }

    /// Write framed bytes into a ChanWriter.
    fn inject(writer: &mut ChanWriter, bytes: &[u8]) {
        writer.write_all(bytes).unwrap();
    }

    // ── Test 1: round-trip request / response ─────────────────────────────────

    #[test]
    fn test_request_roundtrip() {
        // server → transport side: the transport reads from `server_out_reader`
        // (pretend-server writes responses here)
        let (mut server_out_writer, server_out_reader) = byte_pipe();
        // transport → server side: we don't care about these bytes in this test
        let (transport_out_writer, _transport_out_reader) = byte_pipe();

        let transport = TransportLayer::new(server_out_reader, transport_out_writer);

        // The transport assigns id=1 to the first request.
        let rx = transport.request("textDocument/hover", serde_json::json!({"position": 0}));

        // Give the writer thread a moment to send the request before we inject the response.
        std::thread::sleep(std::time::Duration::from_millis(20));

        // Inject a valid JSON-RPC response for id=1.
        let response = r#"{"jsonrpc":"2.0","id":1,"result":{"contents":"hello"}}"#;
        inject(&mut server_out_writer, &frame(response));

        let result = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("timed out waiting for response");

        assert!(result.is_ok());
        assert_eq!(result.unwrap()["contents"], "hello");
    }

    // ── Test 2: notification dispatch ────────────────────────────────────────

    #[test]
    fn test_notification_dispatch() {
        let (mut server_out_writer, server_out_reader) = byte_pipe();
        let (transport_out_writer, _) = byte_pipe();

        let transport = TransportLayer::new(server_out_reader, transport_out_writer);

        let received: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
        let received_clone = Arc::clone(&received);

        transport.subscribe(
            "textDocument/publishDiagnostics",
            Arc::new(move |params| {
                let mut guard = received_clone.lock().unwrap();
                *guard = Some(params);
            }),
        );

        let notification = r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///foo.rs","diagnostics":[]}}"#;
        inject(&mut server_out_writer, &frame(notification));

        // Poll until handler fires (up to 2 s).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            {
                let guard = received.lock().unwrap();
                if guard.is_some() {
                    assert_eq!(guard.as_ref().unwrap()["uri"], "file:///foo.rs");
                    return;
                }
            }
            if std::time::Instant::now() > deadline {
                panic!("notification handler was never called");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    // ── Test 3: error response ────────────────────────────────────────────────

    #[test]
    fn test_error_response() {
        let (mut server_out_writer, server_out_reader) = byte_pipe();
        let (transport_out_writer, _) = byte_pipe();

        let transport = TransportLayer::new(server_out_reader, transport_out_writer);

        let rx = transport.request("workspace/symbol", serde_json::json!({"query": ""}));

        std::thread::sleep(std::time::Duration::from_millis(20));

        let error_response =
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}"#;
        inject(&mut server_out_writer, &frame(error_response));

        let result = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("timed out waiting for error response");

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, -32601);
        assert_eq!(err.message, "Method not found");
    }

    // ── Test 4: concurrent requests routed by id ──────────────────────────────

    #[test]
    fn test_concurrent_requests_routed_correctly() {
        let (mut server_out_writer, server_out_reader) = byte_pipe();
        let (transport_out_writer, _transport_out_reader) = byte_pipe();

        let transport = TransportLayer::new(server_out_reader, transport_out_writer);

        // Two simultaneous requests: id=1 and id=2 are assigned in order.
        let rx_a = transport.request("method/a", serde_json::json!({}));
        let rx_b = transport.request("method/b", serde_json::json!({}));

        // Let the writer thread deliver both requests.
        std::thread::sleep(std::time::Duration::from_millis(20));

        // Inject responses out of order: id=2 first, then id=1.
        let response_b = r#"{"jsonrpc":"2.0","id":2,"result":"result_b"}"#;
        let response_a = r#"{"jsonrpc":"2.0","id":1,"result":"result_a"}"#;
        inject(&mut server_out_writer, &frame(response_b));
        inject(&mut server_out_writer, &frame(response_a));

        let result_a = rx_a
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("timed out waiting for rx_a");
        let result_b = rx_b
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("timed out waiting for rx_b");

        assert_eq!(result_a.unwrap(), "result_a");
        assert_eq!(result_b.unwrap(), "result_b");
    }

    // ── Test 5: dropped writer surfaces an error to request() ─────────────────

    #[test]
    fn test_dropped_writer_returns_error() {
        struct BrokenWriter;
        impl Write for BrokenWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let (_server_out_writer, server_out_reader) = byte_pipe();

        let transport = TransportLayer::new(server_out_reader, BrokenWriter);

        // First request: the writer thread errors on write_all and exits,
        // dropping its Receiver<WriteMessage>.
        let _rx0 = transport.request("prime", serde_json::json!({}));

        // Let the writer thread observe the error and exit.
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Now the write channel has no receiver; request() must hit the
        // error-recovery path and yield an RpcError instead of blocking.
        let rx = transport.request("test", serde_json::json!({}));
        let result = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("timed out waiting for error");

        let err = result.expect_err("expected an error from a dead writer");
        assert_eq!(err.code, -32603);
    }

    // ── Test 6: invalid JSON body is skipped, reader keeps going ──────────────

    #[test]
    fn test_invalid_json_body_skipped_reader_continues() {
        let (mut server_out_writer, server_out_reader) = byte_pipe();
        let (transport_out_writer, _transport_out_reader) = byte_pipe();

        let transport = TransportLayer::new(server_out_reader, transport_out_writer);

        let rx = transport.request("method/x", serde_json::json!({}));

        std::thread::sleep(std::time::Duration::from_millis(20));

        // A well-framed frame whose body is not valid JSON. The reader should
        // log a parse error and `continue` to the next frame.
        inject(&mut server_out_writer, &frame("hello"));

        // A valid response for id=1 follows.
        let response = r#"{"jsonrpc":"2.0","id":1,"result":"ok"}"#;
        inject(&mut server_out_writer, &frame(response));

        let result = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("timed out; reader did not continue after bad JSON");

        assert_eq!(result.unwrap(), "ok");
    }
}
