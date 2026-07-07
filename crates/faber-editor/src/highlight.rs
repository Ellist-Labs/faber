use std::sync::Arc;

use faber_lang::{Language, SyntaxToken};
use tree_sitter::{Query, QueryCursor, StreamingIterator, Tree};

/// A single syntax-highlighted span on one line (byte columns, 0-indexed).
/// Columns are bytes relative to line start, as returned by tree-sitter.
#[derive(Debug, Clone, Copy)]
pub struct HighlightSpan {
    pub start_byte_col: u32,
    pub end_byte_col: u32,
    pub token: SyntaxToken,
}

struct Inner {
    query: Query,
    cap_tokens: Vec<Option<SyntaxToken>>,
}

/// Per-document syntax highlight cache.
/// Rebuilt on every edit; not recomputed per frame.
pub struct HighlightCache {
    /// Spans indexed by line. Empty Vec for lines with no highlighted spans.
    pub lines: Vec<Vec<HighlightSpan>>,
    inner: Option<Inner>,
}

impl Default for HighlightCache {
    fn default() -> Self {
        Self { lines: Vec::new(), inner: None }
    }
}

impl HighlightCache {
    /// Build the query from `language`. Call once after Document is opened.
    pub fn setup(&mut self, language: Option<&Arc<Language>>) {
        self.inner = language
            .and_then(|l| l.make_highlight_query())
            .map(|(query, cap_tokens)| Inner { query, cap_tokens });
    }

    /// Run the highlight query over the current tree+source.
    /// O(node_count) — called once per edit, not per frame.
    pub fn compute(&mut self, tree: &Tree, source: &str) {
        let line_count = tree.root_node().end_position().row + 1;
        self.lines.clear();
        self.lines.resize_with(line_count, Vec::new);

        let Some(ref inner) = self.inner else { return };
        let src_bytes = source.as_bytes();
        let root = tree.root_node();

        let mut cursor = QueryCursor::new();
        let mut captures = cursor.captures(&inner.query, root, src_bytes);

        captures.advance();
        while let Some((mat, ci)) = captures.get() {
            let capture = &mat.captures[*ci];
            let token = match inner.cap_tokens.get(capture.index as usize).and_then(|t| *t) {
                Some(t) => t,
                None => {
                    captures.advance();
                    continue;
                }
            };

            let node = capture.node;
            let start = node.start_position();
            let end = node.end_position();

            // Split multi-line nodes into per-line spans.
            if start.row == end.row {
                // Common case: single-line node.
                if let Some(line_spans) = self.lines.get_mut(start.row) {
                    line_spans.push(HighlightSpan {
                        start_byte_col: start.column as u32,
                        end_byte_col: end.column as u32,
                        token,
                    });
                }
            } else {
                // Multi-line: first line goes to EOL, last line from BOL.
                if let Some(line_spans) = self.lines.get_mut(start.row) {
                    line_spans.push(HighlightSpan {
                        start_byte_col: start.column as u32,
                        end_byte_col: u32::MAX,
                        token,
                    });
                }
                for row in (start.row + 1)..end.row {
                    if let Some(line_spans) = self.lines.get_mut(row) {
                        line_spans.push(HighlightSpan {
                            start_byte_col: 0,
                            end_byte_col: u32::MAX,
                            token,
                        });
                    }
                }
                if let Some(line_spans) = self.lines.get_mut(end.row) {
                    line_spans.push(HighlightSpan {
                        start_byte_col: 0,
                        end_byte_col: end.column as u32,
                        token,
                    });
                }
            }

            captures.advance();
        }

        // Sort each line's spans by start column for efficient lookup.
        for line_spans in &mut self.lines {
            line_spans.sort_unstable_by_key(|s| s.start_byte_col);
        }
    }
}

/// Convert a byte column offset on a line to a char column.
/// `line_str`: the line content (without trailing newline).
#[inline]
pub fn byte_col_to_char_col(line_str: &str, byte_col: u32) -> usize {
    let byte_col = (byte_col as usize).min(line_str.len());
    // Fast path: pure ASCII (most code).
    if line_str.is_ascii() {
        return byte_col;
    }
    line_str[..byte_col].chars().count()
}
