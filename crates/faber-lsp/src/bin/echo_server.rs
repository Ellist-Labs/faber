//! Minimal LSP echo server used by faber-lsp tests.
//! Reads Content-Length frames, responds to initialize, echoes shutdown, exits on exit.

use std::io::{self, BufRead, BufReader, Write};

fn read_frame(reader: &mut impl BufRead) -> Option<String> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some(val) = line.strip_prefix("Content-Length: ") {
            content_length = val.trim().parse().ok();
        }
    }
    let n = content_length?;
    let mut body = vec![0u8; n];
    reader.read_exact(&mut body).ok()?;
    String::from_utf8(body).ok()
}

fn write_frame(writer: &mut impl Write, body: &str) {
    write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body).ok();
    writer.flush().ok();
}

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    while let Some(msg) = read_frame(&mut reader) {
        let val: serde_json::Value = match serde_json::from_str(&msg) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let method = val.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = val.get("id").cloned();

        match method {
            "initialize" => {
                if let Some(id) = id {
                    let response = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "capabilities": {},
                            "serverInfo": { "name": "echo-server", "version": "0.1" }
                        }
                    });
                    write_frame(&mut writer, &response.to_string());
                }
            }
            "shutdown" => {
                if let Some(id) = id {
                    let response = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": null
                    });
                    write_frame(&mut writer, &response.to_string());
                }
            }
            "exit" => {
                break;
            }
            _ => {
                if let Some(id) = id {
                    let response = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": null
                    });
                    write_frame(&mut writer, &response.to_string());
                }
            }
        }
    }
}
