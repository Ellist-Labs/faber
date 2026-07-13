/// Parse and strip helpers for `textDocument/completion` responses.
/// Follows the same hand-written extractor convention as hover.rs.

/// A single text edit (insert or replace range + new text).
#[derive(Debug, Clone)]
pub struct CompletionEdit {
    pub range: lsp_types::Range,
    pub new_text: String,
}

/// A parsed completion item ready for the UI layer.
#[derive(Debug, Clone)]
pub struct ParsedCompletion {
    pub label: String,
    /// Raw LSP CompletionItemKind integer value (1=Text, 2=Method, 3=Function, …).
    pub kind: Option<i32>,
    pub detail: Option<String>,
    /// Used for sorting within the list (server-provided).
    pub sort_text: Option<String>,
    /// Used for client-side fuzzy filtering; falls back to `label`.
    pub filter_text: Option<String>,
    /// Plain-text insert (used when `text_edit` is absent).
    pub insert_text: Option<String>,
    /// LSP textEdit / InsertReplaceEdit — preferred over `insert_text`.
    pub text_edit: Option<CompletionEdit>,
    /// Auto-import or other edits to apply alongside the main edit.
    pub additional_text_edits: Vec<CompletionEdit>,
    /// Whether the insert text uses snippet syntax (`${1:…}`, `$0`).
    pub is_snippet: bool,
    /// Inline documentation (from the item itself, not resolve).
    pub documentation: Option<String>,
    /// Raw server data blob — needed for `completionItem/resolve`.
    pub data: serde_json::Value,
}

impl ParsedCompletion {
    /// Text to use for fuzzy matching; prefers `filter_text`, then `label`.
    pub fn match_text(&self) -> &str {
        self.filter_text.as_deref().unwrap_or(&self.label)
    }
}

/// Result of parsing a `textDocument/completion` response.
pub struct ParsedCompletionList {
    pub items: Vec<ParsedCompletion>,
    /// Server indicates more items exist; re-request on next keystroke.
    pub is_incomplete: bool,
}

/// Parse a `textDocument/completion` JSON response into `ParsedCompletionList`.
/// Handles both `CompletionItem[]` and `CompletionList` shapes.
pub fn parse_completion_response(val: &serde_json::Value) -> ParsedCompletionList {
    if val.is_null() {
        return ParsedCompletionList {
            items: vec![],
            is_incomplete: false,
        };
    }

    let (raw_items, is_incomplete) = if val.is_array() {
        // CompletionItem[]
        (val.as_array().map(|a| a.as_slice()).unwrap_or(&[]), false)
    } else if let Some(arr) = val.get("items").and_then(|v| v.as_array()) {
        // CompletionList { isIncomplete, items }
        let inc = val
            .get("isIncomplete")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        (arr.as_slice(), inc)
    } else {
        return ParsedCompletionList {
            items: vec![],
            is_incomplete: false,
        };
    };

    let items = raw_items.iter().filter_map(parse_item).collect();
    ParsedCompletionList {
        items,
        is_incomplete,
    }
}

fn parse_item(item: &serde_json::Value) -> Option<ParsedCompletion> {
    let label = item.get("label")?.as_str()?.to_owned();
    let kind = item.get("kind").and_then(|v| v.as_i64()).map(|n| n as i32);
    let detail = item
        .get("detail")
        .and_then(|v| v.as_str())
        .map(String::from);
    let sort_text = item
        .get("sortText")
        .and_then(|v| v.as_str())
        .map(String::from);
    let filter_text = item
        .get("filterText")
        .and_then(|v| v.as_str())
        .map(String::from);
    let insert_text = item
        .get("insertText")
        .and_then(|v| v.as_str())
        .map(String::from);

    let insert_text_format = item.get("insertTextFormat").and_then(|v| v.as_u64());
    // LSP InsertTextFormat::Snippet == 2
    let is_snippet = insert_text_format == Some(2);

    let text_edit = parse_text_edit(item.get("textEdit"));
    let additional_text_edits = item
        .get("additionalTextEdits")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(parse_text_edit_direct).collect())
        .unwrap_or_default();

    let documentation = extract_doc_text(item.get("documentation"));
    let data = item.get("data").cloned().unwrap_or(serde_json::Value::Null);

    Some(ParsedCompletion {
        label,
        kind,
        detail,
        sort_text,
        filter_text,
        insert_text,
        text_edit,
        additional_text_edits,
        is_snippet,
        documentation,
        data,
    })
}

fn parse_text_edit(val: Option<&serde_json::Value>) -> Option<CompletionEdit> {
    let val = val?;
    // Handles both TextEdit {range, newText} and InsertReplaceEdit {insert, replace, newText}.
    // For InsertReplaceEdit we use the `insert` range (Zed default for unspecified mode).
    let range = if let Some(r) = val.get("range") {
        parse_range(r)?
    } else if let Some(r) = val.get("insert") {
        parse_range(r)?
    } else {
        return None;
    };
    let new_text = val.get("newText")?.as_str()?.to_owned();
    Some(CompletionEdit { range, new_text })
}

fn parse_text_edit_direct(val: &serde_json::Value) -> Option<CompletionEdit> {
    parse_text_edit(Some(val))
}

fn parse_range(val: &serde_json::Value) -> Option<lsp_types::Range> {
    let start = val.get("start")?;
    let end = val.get("end")?;
    let pos = |v: &serde_json::Value| -> Option<lsp_types::Position> {
        Some(lsp_types::Position {
            line: v.get("line")?.as_u64()? as u32,
            character: v.get("character")?.as_u64()? as u32,
        })
    };
    Some(lsp_types::Range {
        start: pos(start)?,
        end: pos(end)?,
    })
}

pub fn extract_doc_text(val: Option<&serde_json::Value>) -> Option<String> {
    let val = val?;
    let text = if let Some(s) = val.as_str() {
        s.to_owned()
    } else if let Some(s) = val.get("value").and_then(|v| v.as_str()) {
        s.to_owned()
    } else {
        return None;
    };
    (!text.trim().is_empty()).then_some(text)
}

/// Strip LSP snippet syntax from `text`, returning plain text suitable for
/// insertion when full tab-stop navigation is not yet implemented.
///
/// - `${N:placeholder}` → `placeholder`
/// - `${N}` / `$N` → `""` (tab stops without defaults)
/// - `$0` → `""` (final cursor position marker)
pub fn strip_snippet(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('{') => {
                chars.next(); // consume '{'
                // Skip optional digit(s) and ':'
                while chars.peek().is_some_and(|&c| c.is_ascii_digit()) {
                    chars.next();
                }
                if chars.peek() == Some(&':') {
                    chars.next(); // consume ':'
                    // Copy the default value until the matching '}'
                    let mut depth = 1usize;
                    while let Some(c) = chars.next() {
                        match c {
                            '{' => {
                                depth += 1;
                                out.push(c);
                            }
                            '}' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                                out.push(c);
                            }
                            _ => out.push(c),
                        }
                    }
                } else {
                    // `${N}` with no default — drop until '}'
                    while let Some(c) = chars.next() {
                        if c == '}' {
                            break;
                        }
                    }
                }
            }
            Some(&c) if c.is_ascii_digit() => {
                // `$N` — consume the number, emit nothing
                while chars.peek().is_some_and(|&c| c.is_ascii_digit()) {
                    chars.next();
                }
            }
            _ => out.push('$'),
        }
    }
    out
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_completion_array() {
        let val = serde_json::json!([
            {"label": "println!", "kind": 3, "detail": "macro"},
            {"label": "print!",   "kind": 3}
        ]);
        let list = parse_completion_response(&val);
        assert!(!list.is_incomplete);
        assert_eq!(list.items.len(), 2);
        assert_eq!(list.items[0].label, "println!");
        assert_eq!(list.items[0].detail.as_deref(), Some("macro"));
    }

    #[test]
    fn parse_completion_list_incomplete() {
        let val = serde_json::json!({
            "isIncomplete": true,
            "items": [{"label": "foo"}]
        });
        let list = parse_completion_response(&val);
        assert!(list.is_incomplete);
        assert_eq!(list.items.len(), 1);
    }

    #[test]
    fn parse_text_edit_range() {
        let val = serde_json::json!({
            "label": "bar",
            "textEdit": {
                "range": {
                    "start": {"line": 0, "character": 3},
                    "end":   {"line": 0, "character": 6}
                },
                "newText": "bar"
            }
        });
        let item = parse_item(&val).unwrap();
        let te = item.text_edit.unwrap();
        assert_eq!(te.new_text, "bar");
        assert_eq!(te.range.start.character, 3);
        assert_eq!(te.range.end.character, 6);
    }

    #[test]
    fn strip_snippet_placeholder() {
        assert_eq!(strip_snippet("println!(${1:msg})"), "println!(msg)");
    }

    #[test]
    fn strip_snippet_bare_stop() {
        assert_eq!(strip_snippet("foo($1, $2)"), "foo(, )");
    }

    #[test]
    fn strip_snippet_final_cursor() {
        assert_eq!(strip_snippet("fn foo() {$0}"), "fn foo() {}");
    }

    #[test]
    fn strip_snippet_no_snippets() {
        assert_eq!(strip_snippet("plain text"), "plain text");
    }

    #[test]
    fn strip_snippet_nested_braces() {
        // ${1:HashMap<$2, $3>} → HashMap<$2, $3> (inner $2,$3 left as-is since we're copying default)
        let result = strip_snippet("${1:HashMap<$2, $3>}");
        // Default is copied verbatim including inner snippet markers — that's fine for now
        assert!(result.contains("HashMap"));
    }

    #[test]
    fn null_response_is_empty() {
        let list = parse_completion_response(&serde_json::Value::Null);
        assert!(list.items.is_empty());
        assert!(!list.is_incomplete);
    }
}
