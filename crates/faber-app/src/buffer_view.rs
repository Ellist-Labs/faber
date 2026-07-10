// Rendering primitives shared by EditorView, FilePreview, and ProjectSearchView.
// `build_text_runs` and `token_color` live here so they are a single source of
// truth; each view that needs per-line shaping imports them directly.

use faber_editor::{
    SyntaxToken,
    highlight::{HighlightSpan, char_col_to_byte_col},
};
use gpui::{AnyElement, Hsla, SharedString, TextRun, div, font, prelude::*, px};

use crate::theme::RuntimeTheme;

/// Convert a line of syntax-highlighted text into styled text runs for canvas
/// shaping. Diagnostics add wavy underlines, strikethrough, or muted color.
/// Passing an empty diagnostics slice produces syntax-only runs.
pub(crate) fn build_text_runs(
    line_str: &str,
    raw_spans: &[HighlightSpan],
    t: &RuntimeTheme,
    diagnostics: &[faber_lsp::diagnostics::DiagnosticEntry],
) -> Vec<TextRun> {
    let line_bytes = line_str.len();
    if line_bytes == 0 {
        return Vec::new();
    }
    let default_font = font(t.mono_family.clone());
    if raw_spans.is_empty() {
        return vec![TextRun {
            len: line_bytes,
            font: default_font,
            color: t.text,
            background_color: None,
            underline: None,
            strikethrough: None,
        }];
    }
    // Clamp a span's byte offsets to the actual line length.
    let span_range = |s: &HighlightSpan| {
        let sb = (s.start_byte_col as usize).min(line_bytes);
        let eb = if s.end_byte_col == u32::MAX {
            line_bytes
        } else {
            (s.end_byte_col as usize).min(line_bytes)
        };
        (sb, eb)
    };
    let mut breakpoints: Vec<usize> = vec![0, line_bytes];
    for s in raw_spans {
        let (sb, eb) = span_range(s);
        if sb < eb {
            breakpoints.push(sb);
            breakpoints.push(eb);
        }
    }
    breakpoints.sort_unstable();
    breakpoints.dedup();
    let mut runs = Vec::new();
    for i in 0..breakpoints.len().saturating_sub(1) {
        let start = breakpoints[i];
        let end = breakpoints[i + 1];
        if start >= end {
            continue;
        }
        let color = raw_spans
            .iter()
            .rfind(|s| {
                let (sb, eb) = span_range(s);
                start >= sb && end <= eb
            })
            .map(|s| token_color(s.token, t))
            .unwrap_or(t.text);
        runs.push(TextRun {
            len: end - start,
            font: default_font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }

    // Apply diagnostic underlines and tag styles over the base runs.
    for diag in diagnostics {
        let start_char = diag.range.start.offset;
        let end_char = diag.range.end.offset;
        let start_byte = char_col_to_byte_col(line_str, start_char).min(line_bytes);
        let end_byte = char_col_to_byte_col(line_str, end_char).min(line_bytes);
        if start_byte >= end_byte {
            continue;
        }
        let underline_color = match diag.severity {
            faber_lsp::diagnostics::Severity::Error => t.error,
            faber_lsp::diagnostics::Severity::Warning => t.warning,
            faber_lsp::diagnostics::Severity::Information => t.info,
            faber_lsp::diagnostics::Severity::Hint => t.text_muted,
        };
        let is_deprecated = diag
            .tags
            .contains(&faber_lsp::diagnostics::DiagnosticTag::Deprecated);
        let is_unnecessary = diag
            .tags
            .contains(&faber_lsp::diagnostics::DiagnosticTag::Unnecessary);
        let mut pos = 0usize;
        for run in &mut runs {
            let run_end = pos + run.len;
            if pos < end_byte && run_end > start_byte {
                if run.underline.is_none() {
                    run.underline = Some(gpui::UnderlineStyle {
                        thickness: px(1.0),
                        color: Some(underline_color),
                        wavy: true,
                    });
                }
                if is_deprecated && run.strikethrough.is_none() {
                    run.strikethrough = Some(gpui::StrikethroughStyle {
                        thickness: px(1.0),
                        color: Some(t.text_muted),
                    });
                }
                if is_unnecessary {
                    run.color = t.text_muted;
                }
            }
            pos = run_end;
        }
    }

    runs
}

pub(crate) fn token_color(token: SyntaxToken, t: &RuntimeTheme) -> Hsla {
    match token {
        SyntaxToken::Keyword => t.syntax_keyword,
        SyntaxToken::Function => t.syntax_function,
        SyntaxToken::Type => t.syntax_type,
        SyntaxToken::String => t.syntax_string,
        SyntaxToken::Number => t.syntax_number,
        SyntaxToken::Comment => t.syntax_comment,
        SyntaxToken::Constant => t.syntax_constant,
        SyntaxToken::Operator => t.syntax_operator,
        SyntaxToken::Punctuation => t.syntax_punctuation,
        SyntaxToken::Variable => t.syntax_variable,
        SyntaxToken::Property => t.syntax_property,
        SyntaxToken::Attribute => t.syntax_attribute,
        SyntaxToken::Namespace => t.syntax_namespace,
        SyntaxToken::Tag => t.syntax_tag,
        SyntaxToken::Label => t.syntax_label,
    }
}

/// Build a div-based row of syntax-colored spans for use in list views that
/// cannot use canvas (e.g. project-search context lines nested in a results list).
pub(crate) fn build_syntax_spans(
    line_str: &str,
    raw_spans: &[HighlightSpan],
    t: &RuntimeTheme,
) -> AnyElement {
    let runs = build_text_runs(line_str, raw_spans, t, &[]);
    let bytes = line_str.as_bytes();
    let mut pos = 0usize;
    let mut children: Vec<AnyElement> = Vec::new();
    for run in &runs {
        let end = (pos + run.len).min(bytes.len());
        let text = std::str::from_utf8(&bytes[pos..end])
            .unwrap_or("")
            .to_string();
        children.push(
            div()
                .flex_shrink_0()
                .text_color(run.color)
                .child(SharedString::from(text))
                .into_any_element(),
        );
        pos = end;
    }

    div()
        .flex()
        .flex_row()
        .min_w(px(0.))
        .overflow_hidden()
        .children(children)
        .into_any_element()
}
