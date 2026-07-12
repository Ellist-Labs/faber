/// Extract the optional `range` from a `textDocument/hover` JSON response.
/// Returns `None` if the server did not include a range.
pub fn extract_hover_range(val: &serde_json::Value) -> Option<lsp_types::Range> {
    let r = val.get("range")?;
    let start = r.get("start")?;
    let end = r.get("end")?;
    Some(lsp_types::Range {
        start: lsp_types::Position {
            line: start.get("line")?.as_u64()? as u32,
            character: start.get("character")?.as_u64()? as u32,
        },
        end: lsp_types::Position {
            line: end.get("line")?.as_u64()? as u32,
            character: end.get("character")?.as_u64()? as u32,
        },
    })
}

/// Extract `originSelectionRange` from a `textDocument/definition` LocationLink
/// response — the exact source span of the symbol under the cursor. Returns
/// `None` for plain Location responses (they carry no origin span).
pub fn extract_origin_selection_range(val: &serde_json::Value) -> Option<lsp_types::Range> {
    let item = if val.is_array() {
        val.as_array()?.first()?
    } else {
        val
    };
    let r = item.get("originSelectionRange")?;
    let parse_pos = |v: &serde_json::Value| -> Option<lsp_types::Position> {
        Some(lsp_types::Position {
            line: v.get("line")?.as_u64()? as u32,
            character: v.get("character")?.as_u64()? as u32,
        })
    };
    Some(lsp_types::Range {
        start: parse_pos(r.get("start")?)?,
        end: parse_pos(r.get("end")?)?,
    })
}

/// Extract human-readable text from a `textDocument/hover` JSON response.
///
/// Handles both the modern MarkupContent format (`{kind, value}`) and the
/// legacy MarkedString / MarkedString[] format. For MarkedString objects that
/// carry a `language` field (`{language, value}`), the code is wrapped in a
/// fenced code block so that the markdown renderer can syntax-highlight it.
pub fn extract_hover_text(val: &serde_json::Value) -> Option<String> {
    let contents = val.get("contents")?;

    // Log the raw contents format and the actual kind value so we can diagnose
    // rendering mismatches (e.g. "plaintext" vs "markdown").
    let markup_kind = contents.get("kind").and_then(|k| k.as_str()).unwrap_or("");
    log::debug!(
        "hover: contents format={} markup_kind={:?}",
        if contents.is_string() {
            "plain-string"
        } else if contents.is_array() {
            "marked-string-array"
        } else if !markup_kind.is_empty() {
            "markup-content"
        } else if contents.get("language").is_some() {
            "marked-string-object"
        } else {
            "unknown-object"
        },
        markup_kind,
    );

    let text = if let Some(s) = contents.as_str() {
        // Plain string — treat as markdown.
        s.to_owned()
    } else if let Some(obj) = contents.as_object() {
        if obj.contains_key("kind") {
            // MarkupContent: { kind: "markdown" | "plaintext", value }
            obj.get("value")?.as_str()?.to_owned()
        } else if let (Some(lang), Some(val)) = (
            obj.get("language").and_then(|l| l.as_str()),
            obj.get("value").and_then(|v| v.as_str()),
        ) {
            // Single MarkedString object with language — wrap in a code fence.
            format!("```{lang}\n{val}\n```")
        } else {
            obj.get("value")?.as_str()?.to_owned()
        }
    } else {
        let arr = contents.as_array()?;
        arr.iter()
            .filter_map(|item| {
                if let Some(s) = item.as_str() {
                    // Plain MarkedString — already markdown.
                    Some(s.to_owned())
                } else if let Some(lang) = item.get("language").and_then(|l| l.as_str()) {
                    // MarkedString object with language — wrap in a code fence
                    // so the markdown parser can syntax-highlight the block.
                    let val = item.get("value")?.as_str()?;
                    Some(format!("```{lang}\n{val}\n```"))
                } else {
                    // MarkedString object without language — plain markdown.
                    item.get("value")?.as_str().map(|s| s.to_owned())
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}
