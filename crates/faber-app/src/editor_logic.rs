//! Pure helpers extracted from gpui view files — no gpui dependency.
//! Tested in isolation and called from the view layer.

use faber_editor::{
    SyntaxToken,
    markdown::InlineRun,
    outline::{Outline, OutlineItem},
};

/// Build a breadcrumb stack from the outline at `top_line`.
/// Returns each enclosing item (outermost → innermost).
pub(crate) fn breadcrumb_stack<'a>(outline: &'a Outline, top_line: usize) -> Vec<&'a OutlineItem> {
    let mut stack: Vec<&'a OutlineItem> = Vec::new();
    for e in outline.items.iter().take_while(|e| e.source_line <= top_line) {
        while stack.last().is_some_and(|last| last.depth >= e.depth) {
            stack.pop();
        }
        stack.push(e);
    }
    stack
}

/// Map a breadcrumb `@context` keyword to the syntax token used for coloring.
pub(crate) fn context_to_token(context: Option<&str>) -> Option<SyntaxToken> {
    Some(match context? {
        "fn" => SyntaxToken::Function,
        "struct" | "enum" | "trait" | "type" | "impl" => SyntaxToken::Type,
        "mod" => SyntaxToken::Namespace,
        "const" => SyntaxToken::Constant,
        _ => return None,
    })
}

/// Split a slice of inline runs at `HardBreak` boundaries into line segments.
pub(crate) fn split_at_hard_breaks(inlines: &[InlineRun]) -> Vec<Vec<InlineRun>> {
    let mut lines: Vec<Vec<InlineRun>> = vec![vec![]];
    for inline in inlines {
        if matches!(inline, InlineRun::HardBreak) {
            lines.push(vec![]);
        } else {
            lines.last_mut().unwrap().push(inline.clone());
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use super::*;

    fn make_item(name: &str, depth: u32, line: usize) -> OutlineItem {
        OutlineItem {
            depth,
            name: name.to_string(),
            context: None,
            source_line: line,
            end_line: line,
            byte_range: Range { start: 0, end: 0 },
        }
    }

    fn text_run(s: &str) -> InlineRun {
        InlineRun::Text {
            text: s.to_string(),
            style: Default::default(),
            link: None,
        }
    }

    // ── breadcrumb_stack ───────────────────────────────────────────────────────

    #[test]
    fn breadcrumb_empty_outline() {
        let outline = Outline::default();
        assert!(breadcrumb_stack(&outline, 10).is_empty());
    }

    #[test]
    fn breadcrumb_single_item() {
        let outline = Outline { items: vec![make_item("main", 1, 0)] };
        let stack = breadcrumb_stack(&outline, 5);
        assert_eq!(stack.len(), 1);
        assert_eq!(stack[0].name, "main");
    }

    #[test]
    fn breadcrumb_item_after_top_line_excluded() {
        let outline = Outline { items: vec![make_item("foo", 1, 10)] };
        assert!(breadcrumb_stack(&outline, 5).is_empty());
    }

    #[test]
    fn breadcrumb_nesting_outermost_to_innermost() {
        let outline = Outline {
            items: vec![
                make_item("mod foo", 1, 0),
                make_item("fn bar", 2, 5),
                make_item("fn baz", 2, 20),
            ],
        };
        let stack = breadcrumb_stack(&outline, 10);
        assert_eq!(stack.len(), 2);
        assert_eq!(stack[0].name, "mod foo");
        assert_eq!(stack[1].name, "fn bar");
    }

    #[test]
    fn breadcrumb_sibling_replaces_sibling_at_same_depth() {
        let outline = Outline {
            items: vec![make_item("fn foo", 1, 0), make_item("fn bar", 1, 5)],
        };
        let stack = breadcrumb_stack(&outline, 10);
        assert_eq!(stack.len(), 1);
        assert_eq!(stack[0].name, "fn bar");
    }

    #[test]
    fn breadcrumb_exact_line_match_included() {
        let outline = Outline { items: vec![make_item("fn exact", 1, 5)] };
        let stack = breadcrumb_stack(&outline, 5);
        assert_eq!(stack.len(), 1);
    }

    // ── context_to_token ──────────────────────────────────────────────────────

    #[test]
    fn context_to_token_known_keywords() {
        assert_eq!(context_to_token(Some("fn")), Some(SyntaxToken::Function));
        assert_eq!(context_to_token(Some("struct")), Some(SyntaxToken::Type));
        assert_eq!(context_to_token(Some("enum")), Some(SyntaxToken::Type));
        assert_eq!(context_to_token(Some("trait")), Some(SyntaxToken::Type));
        assert_eq!(context_to_token(Some("type")), Some(SyntaxToken::Type));
        assert_eq!(context_to_token(Some("impl")), Some(SyntaxToken::Type));
        assert_eq!(context_to_token(Some("mod")), Some(SyntaxToken::Namespace));
        assert_eq!(context_to_token(Some("const")), Some(SyntaxToken::Constant));
    }

    #[test]
    fn context_to_token_unknown_returns_none() {
        assert_eq!(context_to_token(Some("let")), None);
        assert_eq!(context_to_token(Some("use")), None);
        assert_eq!(context_to_token(Some("")), None);
    }

    #[test]
    fn context_to_token_none_input_returns_none() {
        assert_eq!(context_to_token(None), None);
    }

    // ── split_at_hard_breaks ──────────────────────────────────────────────────

    #[test]
    fn split_empty_input_yields_one_empty_segment() {
        let lines = split_at_hard_breaks(&[]);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].is_empty());
    }

    #[test]
    fn split_no_breaks_yields_one_segment() {
        let inlines = vec![text_run("hello"), text_run(" world")];
        let lines = split_at_hard_breaks(&inlines);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), 2);
    }

    #[test]
    fn split_single_hard_break() {
        let inlines = vec![text_run("a"), InlineRun::HardBreak, text_run("b")];
        let lines = split_at_hard_breaks(&inlines);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), 1);
        assert_eq!(lines[1].len(), 1);
    }

    #[test]
    fn split_two_hard_breaks_three_segments() {
        let inlines = vec![
            text_run("a"),
            InlineRun::HardBreak,
            text_run("b"),
            InlineRun::HardBreak,
            text_run("c"),
        ];
        let lines = split_at_hard_breaks(&inlines);
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn split_leading_hard_break_first_segment_empty() {
        let inlines = vec![InlineRun::HardBreak, text_run("x")];
        let lines = split_at_hard_breaks(&inlines);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].is_empty());
        assert_eq!(lines[1].len(), 1);
    }
}
