//! Pure editing helpers for Markdown source — no gpui, no rope, just string logic.
//! All unit-tested; these run inside editor action handlers.

/// What to do when the user presses Enter on a list line.
#[derive(Debug, PartialEq, Eq)]
pub enum EnterAction {
    /// Insert the given string (e.g. `"\n- "` or `"\n1. "`).
    ContinueList { insert: String },
    /// The item was empty: delete the marker characters (col range relative to
    /// line start) and insert a plain newline.
    ExitList { delete_cols: std::ops::Range<usize> },
    /// No special handling; caller inserts a plain newline.
    Plain,
}

/// Decide what Enter should do on `current_line` with cursor at byte `cursor_col`.
pub fn enter_action(current_line: &str, cursor_col: usize) -> EnterAction {
    // Only act when cursor is at or after the end of the line content.
    // (Mid-line Enter always inserts a plain newline.)
    let trimmed_end = current_line.trim_end_matches(['\n', '\r']).len();
    if cursor_col < trimmed_end {
        return EnterAction::Plain;
    }

    let line = current_line.trim_end_matches(['\n', '\r']);

    // Detect leading indentation.
    let indent_end = line.len() - line.trim_start().len();
    let indent = &line[..indent_end];
    let rest = &line[indent_end..];

    // ── task list item: `- [ ] ` or `- [x] ` ───────────────────────────────
    if let Some(body) = rest
        .strip_prefix("- [ ] ")
        .or_else(|| rest.strip_prefix("- [x] "))
    {
        let marker_len = "- [ ] ".len();
        let marker_start = indent_end;
        let marker_end = indent_end + marker_len;
        if body.trim().is_empty() {
            return EnterAction::ExitList {
                delete_cols: marker_start..marker_end,
            };
        }
        return EnterAction::ContinueList {
            insert: format!("\n{indent}- [ ] "),
        };
    }

    // ── unordered list: `- `, `* `, `+ ` ───────────────────────────────────
    for prefix in ["- ", "* ", "+ "] {
        if let Some(body) = rest.strip_prefix(prefix) {
            let marker_start = indent_end;
            let marker_end = indent_end + prefix.len();
            if body.trim().is_empty() {
                return EnterAction::ExitList {
                    delete_cols: marker_start..marker_end,
                };
            }
            let ch = &prefix[..1];
            return EnterAction::ContinueList {
                insert: format!("\n{indent}{ch} "),
            };
        }
    }

    // ── ordered list: `1. `, `1) ` ──────────────────────────────────────────
    if let Some(ordered) = parse_ordered_prefix(rest) {
        let marker_start = indent_end;
        let marker_end = indent_end + ordered.prefix_len;
        if ordered.body.trim().is_empty() {
            return EnterAction::ExitList {
                delete_cols: marker_start..marker_end,
            };
        }
        let next_n = ordered.n + 1;
        let sep = ordered.sep;
        return EnterAction::ContinueList {
            insert: format!("\n{indent}{next_n}{sep} "),
        };
    }

    EnterAction::Plain
}

struct OrderedPrefix<'a> {
    n: u64,
    sep: char,
    prefix_len: usize,
    body: &'a str,
}

fn parse_ordered_prefix(s: &str) -> Option<OrderedPrefix<'_>> {
    let digits_end = s.find(|c: char| !c.is_ascii_digit())?;
    if digits_end == 0 || digits_end > 9 {
        return None;
    }
    let n: u64 = s[..digits_end].parse().ok()?;
    let sep = s.as_bytes().get(digits_end).copied()?;
    if sep != b'.' && sep != b')' {
        return None;
    }
    let after_sep = &s[digits_end + 1..];
    if !after_sep.starts_with(' ') {
        return None;
    }
    Some(OrderedPrefix {
        n,
        sep: sep as char,
        prefix_len: digits_end + 2, // digits + sep + space
        body: &after_sep[1..],
    })
}

/// Wrap (or unwrap) `selected` with `marker` (`"**"` for bold, `"*"` for italic).
/// If the selection is already wrapped, strips the markers.
pub fn smart_wrap(selected: &str, marker: &str) -> String {
    if selected.starts_with(marker)
        && selected.ends_with(marker)
        && selected.len() > marker.len() * 2
    {
        selected[marker.len()..selected.len() - marker.len()].to_owned()
    } else {
        format!("{marker}{selected}{marker}")
    }
}

/// Returns true if `text` looks like an HTTP(S) URL.
pub fn looks_like_url(text: &str) -> bool {
    text.starts_with("https://") || text.starts_with("http://")
}

/// Find the `[ ]` / `[x]` checkbox on `line` and return the char-column range
/// and the replacement string (`"[x]"` or `"[ ]"`).
pub fn toggle_checkbox(line: &str) -> Option<(std::ops::Range<usize>, &'static str)> {
    let unchecked = line.find("[ ]");
    let checked = line.find("[x]").or_else(|| line.find("[X]"));
    match (unchecked, checked) {
        (Some(u), None) => Some((u..u + 3, "[x]")),
        (None, Some(c)) => Some((c..c + 3, "[ ]")),
        (Some(u), Some(c)) => {
            if u < c {
                Some((u..u + 3, "[x]"))
            } else {
                Some((c..c + 3, "[ ]"))
            }
        }
        (None, None) => None,
    }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn cont(s: &str) -> EnterAction {
        EnterAction::ContinueList {
            insert: s.to_owned(),
        }
    }
    fn exit(r: std::ops::Range<usize>) -> EnterAction {
        EnterAction::ExitList { delete_cols: r }
    }

    #[test]
    fn unordered_continue() {
        assert_eq!(enter_action("- hello", 7), cont("\n- "));
        assert_eq!(enter_action("* hello", 7), cont("\n* "));
        assert_eq!(enter_action("+ hello", 7), cont("\n+ "));
    }

    #[test]
    fn unordered_exit_empty() {
        assert_eq!(enter_action("- ", 2), exit(0..2));
        assert_eq!(enter_action("-  ", 3), exit(0..2));
    }

    #[test]
    fn ordered_continue_dot() {
        assert_eq!(enter_action("1. hello", 8), cont("\n2. "));
        assert_eq!(enter_action("3. item", 7), cont("\n4. "));
    }

    #[test]
    fn ordered_continue_paren() {
        assert_eq!(enter_action("1) hello", 8), cont("\n2) "));
    }

    #[test]
    fn ordered_exit_empty() {
        assert_eq!(enter_action("1. ", 3), exit(0..3));
    }

    #[test]
    fn task_continue() {
        assert_eq!(enter_action("- [ ] do it", 11), cont("\n- [ ] "));
        assert_eq!(enter_action("- [x] done", 10), cont("\n- [ ] "));
    }

    #[test]
    fn task_exit_empty() {
        assert_eq!(enter_action("- [ ] ", 6), exit(0..6));
    }

    #[test]
    fn indented_list() {
        assert_eq!(enter_action("  - item", 8), cont("\n  - "));
        assert_eq!(enter_action("  - ", 4), exit(2..4));
    }

    #[test]
    fn plain_line() {
        assert_eq!(enter_action("just text", 9), EnterAction::Plain);
        assert_eq!(enter_action("# Heading", 9), EnterAction::Plain);
    }

    #[test]
    fn mid_line_enter_is_plain() {
        assert_eq!(enter_action("- hello", 3), EnterAction::Plain);
    }

    #[test]
    fn smart_wrap_adds_bold() {
        assert_eq!(smart_wrap("hello", "**"), "**hello**");
    }

    #[test]
    fn smart_wrap_removes_bold() {
        assert_eq!(smart_wrap("**hello**", "**"), "hello");
    }

    #[test]
    fn smart_wrap_adds_italic() {
        assert_eq!(smart_wrap("hi", "*"), "*hi*");
    }

    #[test]
    fn smart_wrap_removes_italic() {
        assert_eq!(smart_wrap("*hi*", "*"), "hi");
    }

    #[test]
    fn looks_like_url_https() {
        assert!(looks_like_url("https://example.com"));
        assert!(looks_like_url("http://example.com"));
        assert!(!looks_like_url("not a url"));
    }

    #[test]
    fn toggle_checkbox_unchecked() {
        let (r, rep) = toggle_checkbox("- [ ] task").unwrap();
        assert_eq!(&"- [ ] task"[r], "[ ]");
        assert_eq!(rep, "[x]");
    }

    #[test]
    fn toggle_checkbox_checked() {
        let (r, rep) = toggle_checkbox("- [x] done").unwrap();
        assert_eq!(&"- [x] done"[r], "[x]");
        assert_eq!(rep, "[ ]");
    }

    #[test]
    fn toggle_checkbox_none() {
        assert!(toggle_checkbox("- plain item").is_none());
    }
}
