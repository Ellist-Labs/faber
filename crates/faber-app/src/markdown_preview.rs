use std::{path::PathBuf, sync::Arc};

use faber_editor::{
    LanguageRegistry,
    SyntaxToken,
    highlight::HighlightSpan,
    markdown::{Block, BlockKind, InlineRun, ListItem, MarkdownDoc},
};
use gpui::{
    AnyElement, App, Context, Font, FontStyle, FontWeight, Hsla, IntoElement,
    MouseButton, MouseMoveEvent, Render, ScrollHandle, SharedString, StrikethroughStyle, Styled,
    TextRun, UnderlineStyle, Window, div, img, prelude::*, px,
};
use ropey::Rope;

use crate::settings_view::SettingsStore;
use crate::theme::RuntimeTheme;
use crate::ui::{ScrollbarDrag, render_scrollbar};
use crate::ui::scrollbar::{start_drag, update_drag};

// ── MarkdownPreviewView ───────────────────────────────────────────────────────

pub struct MarkdownPreviewView {
    pub md: Arc<MarkdownDoc>,
    pub scroll: ScrollHandle,
    pub base_dir: PathBuf,
    pub scrollbar_drag: Option<ScrollbarDrag>,
}

impl MarkdownPreviewView {
    pub fn new(rope: &Rope, path: &std::path::Path, registry: &LanguageRegistry) -> Self {
        let source = rope.to_string();
        let md = Arc::new(faber_editor::markdown::parse_markdown(&source, rope, registry));
        Self {
            md,
            scroll: ScrollHandle::new(),
            base_dir: path.parent().unwrap_or(std::path::Path::new(".")).to_path_buf(),
            scrollbar_drag: None,
        }
    }

    /// Synchronous re-parse — called at toggle time for instant correctness.
    pub fn reparse_now(&mut self, rope: &Rope, registry: &LanguageRegistry) {
        let source = rope.to_string();
        let md = faber_editor::markdown::parse_markdown(&source, rope, registry);
        self.apply_md(Arc::new(md));
    }

    /// Scroll the preview to the block whose source_lines best matches `line`.
    pub fn scroll_to_source_line(&self, line: usize) {
        let ix = self.md.blocks.partition_point(|b| b.source_lines.start <= line);
        let ix = ix.saturating_sub(1).min(self.md.blocks.len().saturating_sub(1));
        self.scroll.scroll_to_item(ix);
    }

    /// Returns the source line corresponding to the top-visible block.
    pub fn source_line_at_top(&self) -> usize {
        let ix = self.scroll.top_item();
        self.md.blocks.get(ix).map(|b| b.source_lines.start).unwrap_or(0)
    }

    /// Apply a new parsed document WITHOUT resetting the scroll position.
    pub fn apply_md(&mut self, md: Arc<MarkdownDoc>) {
        self.md = md;
        // Intentionally do NOT reset the scroll — the retained ScrollHandle
        // keeps its offset across re-renders, giving live updates without scroll jump.
    }

    /// Render a single block element.
    pub fn render_block(
        md: &Arc<MarkdownDoc>,
        ix: usize,
        base_dir: &PathBuf,
        t: &RuntimeTheme,
        cx: &mut App,
    ) -> AnyElement {
        let block = &md.blocks[ix];
        match &block.kind {
            BlockKind::Heading { level, inlines } => render_heading(*level, inlines, ix, base_dir, t, cx),
            BlockKind::Paragraph { inlines } => render_paragraph(inlines, base_dir, t, cx),
            BlockKind::CodeBlock { lang, text, highlights } => {
                render_code_block(lang.as_deref(), text, highlights, t)
            }
            BlockKind::Blockquote { children } => render_blockquote(children, base_dir, t, cx),
            BlockKind::List { ordered, start, items } => {
                render_list(*ordered, *start, items, base_dir, t, cx)
            }
            BlockKind::Table { head, rows } => render_table(head, rows, base_dir, t, cx),
            BlockKind::Rule => div().h(px(1.)).bg(t.separator).into_any_element(),
            BlockKind::HtmlBlock { text } => div()
                .font_family(t.mono_family.clone())
                .text_size(px(t.font_size_code))
                .text_color(t.text_muted)
                .child(SharedString::from(text.clone()))
                .into_any_element(),
        }
    }
}

impl Render for MarkdownPreviewView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<RuntimeTheme>().clone();
        let show_scrollbar = cx.global::<SettingsStore>().0.show_scrollbar;
        let md = Arc::clone(&self.md);
        let base_dir = self.base_dir.clone();
        let t2 = t.clone();
        let block_count = md.blocks.len();
        let is_dragging = self.scrollbar_drag.is_some();
        let scroll = self.scroll.clone();

        let preview_scroll = render_scrollbar(
            "preview-scrollbar",
            "preview-scrollbar-thumb",
            &scroll,
            show_scrollbar,
            is_dragging,
            cx.listener(|view, ev, _, cx| {
                view.scrollbar_drag = Some(start_drag(ev, &view.scroll.clone()));
                cx.notify();
            }),
            &t,
        );

        let content = div()
            .id("md-preview")
            .flex_1()
            .min_h(px(0.))
            .min_w(px(0.))
            .bg(t.bg)
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(
                div()
                    .w_full()
                    .max_w(px(760.))
                    .mx_auto()
                    .px(px(t.sp6))
                    .py(px(t.sp4))
                    .flex()
                    .flex_col()
                    .gap(px(t.sp4))
                    .children((0..block_count).map(|ix| {
                        div()
                            .id(("md-block", ix))
                            .w_full()
                            .child(Self::render_block(&md, ix, &base_dir, &t2, cx))
                    })),
            );

        div()
            .flex()
            .flex_row()
            .flex_1()
            .min_h(px(0.))
            .min_w(px(0.))
            .when(is_dragging, |el| {
                el.on_mouse_move(cx.listener(|view, ev: &MouseMoveEvent, _, cx| {
                    if let Some(ref drag) = view.scrollbar_drag {
                        update_drag(drag, ev, &view.scroll.clone());
                        cx.notify();
                    }
                }))
                .on_mouse_up(MouseButton::Left, cx.listener(|view, _, _, cx| {
                    view.scrollbar_drag = None;
                    cx.notify();
                }))
            })
            .child(content)
            .child(preview_scroll)
    }
}

// ── block renderers ───────────────────────────────────────────────────────────

fn render_heading(
    level: u8,
    inlines: &[InlineRun],
    block_ix: usize,
    base_dir: &PathBuf,
    t: &RuntimeTheme,
    cx: &mut App,
) -> AnyElement {
    let size = match level {
        1 => t.font_size_heading * 1.6,
        2 => t.font_size_heading * 1.35,
        3 => t.font_size_heading * 1.15,
        4 => t.font_size_heading,
        5 => t.font_size_heading * 0.9,
        _ => t.font_size_heading * 0.85,
    };

    // Extra top margin to visually separate headings from preceding content.
    // Suppressed on the very first block so there's no leading whitespace.
    let top_margin = if block_ix == 0 {
        0.0
    } else if level <= 2 {
        t.sp6  // 16px for h1/h2
    } else {
        t.sp4  // 8px for h3–h6
    };

    let el = div()
        .mt(px(top_margin))
        .font_family(t.ui_family.clone())
        .text_size(px(size))
        .font_weight(FontWeight::BOLD)
        .text_color(t.text)
        .w_full()
        .child(render_inlines(inlines, t, base_dir));

    if level <= 2 {
        el.border_b_1().border_color(t.separator).pb(px(t.sp2)).into_any_element()
    } else {
        el.into_any_element()
    }
}

fn render_paragraph(inlines: &[InlineRun], base_dir: &PathBuf, t: &RuntimeTheme, _cx: &mut App) -> AnyElement {
    div()
        .font_family(t.ui_family.clone())
        .text_size(px(t.font_size_body))
        .text_color(t.text)
        .w_full()
        .child(render_inlines(inlines, t, base_dir))
        .into_any_element()
}

/// Render inlines, handling hard breaks (split into stacked lines) and images.
fn render_inlines(inlines: &[InlineRun], t: &RuntimeTheme, base_dir: &PathBuf) -> AnyElement {
    // Split at HardBreak boundaries → stacked lines.
    let has_hard_break = inlines.iter().any(|i| matches!(i, InlineRun::HardBreak));
    if has_hard_break {
        let lines = split_at_hard_breaks(inlines);
        let els: Vec<AnyElement> = lines.iter().map(|seg| render_inline_line(seg, t, base_dir)).collect();
        return div().flex().flex_col().w_full().children(els).into_any_element();
    }
    render_inline_line(inlines, t, base_dir)
}

fn split_at_hard_breaks(inlines: &[InlineRun]) -> Vec<Vec<InlineRun>> {
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

/// Render a single line of inlines (no HardBreaks), splitting on Image nodes.
fn render_inline_line(inlines: &[InlineRun], t: &RuntimeTheme, base_dir: &PathBuf) -> AnyElement {
    let has_image = inlines.iter().any(|i| matches!(i, InlineRun::Image { .. }));
    if !has_image {
        return render_text_runs(inlines, t);
    }

    // Split into text segments and image elements, put them in a flex row.
    let mut children: Vec<AnyElement> = Vec::new();
    let mut text_buf: Vec<InlineRun> = Vec::new();

    for inline in inlines {
        match inline {
            InlineRun::Image { alt, dest } => {
                if !text_buf.is_empty() {
                    children.push(render_text_runs(&text_buf, t));
                    text_buf.clear();
                }
                let is_remote = dest.starts_with("http://") || dest.starts_with("https://");
                if is_remote {
                    let label = if alt.is_empty() { "[image]" } else { alt.as_str() };
                    children.push(
                        div()
                            .text_color(t.text_muted)
                            .text_size(px(t.font_size_body))
                            .child(SharedString::from(label.to_string()))
                            .into_any_element(),
                    );
                } else {
                    let path = base_dir.join(dest);
                    children.push(
                        img(path).max_w_full().rounded(px(t.radius_md)).into_any_element(),
                    );
                }
            }
            other => text_buf.push(other.clone()),
        }
    }
    if !text_buf.is_empty() {
        children.push(render_text_runs(&text_buf, t));
    }

    div().flex().flex_row().flex_wrap().items_baseline().children(children).into_any_element()
}

/// Build a `StyledText` from a slice of text/soft-break inlines (no Image, no HardBreak).
fn render_text_runs(inlines: &[InlineRun], t: &RuntimeTheme) -> AnyElement {
    let mut text = String::new();
    let mut runs: Vec<TextRun> = Vec::new();
    let default_font = Font {
        family: t.ui_family.clone(),
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
        features: gpui::FontFeatures::default(),
        fallbacks: None,
    };

    for inline in inlines {
        match inline {
            InlineRun::Text { text: t_text, style, link } => {
                let start = text.len();
                text.push_str(t_text);
                let run_len = text.len() - start;
                if run_len == 0 {
                    continue;
                }
                let color = if link.is_some() { t.accent } else { t.text };
                let underline = link.as_ref().map(|_| UnderlineStyle {
                    thickness: px(1.),
                    color: Some(t.accent),
                    wavy: false,
                });
                let strikethrough = if style.strike {
                    Some(StrikethroughStyle { thickness: px(1.), color: Some(t.text_muted) })
                } else {
                    None
                };
                let (family, bg) = if style.code {
                    (t.mono_family.clone(), Some(t.bg_sunken))
                } else {
                    (t.ui_family.clone(), None)
                };
                runs.push(TextRun {
                    len: run_len,
                    font: Font {
                        family,
                        weight: if style.bold { FontWeight::BOLD } else { FontWeight::NORMAL },
                        style: if style.italic { FontStyle::Italic } else { FontStyle::Normal },
                        ..default_font.clone()
                    },
                    color,
                    background_color: bg,
                    underline,
                    strikethrough,
                });
            }
            InlineRun::SoftBreak => {
                let start = text.len();
                text.push(' ');
                runs.push(TextRun {
                    len: text.len() - start,
                    font: default_font.clone(),
                    color: t.text,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                });
            }
            // Image and HardBreak handled before this point — skip silently.
            _ => {}
        }
    }

    if text.is_empty() {
        return div().into_any_element();
    }

    gpui::StyledText::new(SharedString::from(text))
        .with_runs(runs)
        .into_any_element()
}

fn render_code_block(
    lang: Option<&str>,
    text: &str,
    highlights: &[Vec<HighlightSpan>],
    t: &RuntimeTheme,
) -> AnyElement {
    let lines: Vec<&str> = text.lines().collect();
    let header_opt = lang.map(|l| {
        div()
            .px(px(t.sp3))
            .py(px(1.))
            .text_size(px(t.font_size_caption))
            .text_color(t.text_subtle)
            .font_family(t.ui_family.clone())
            .child(SharedString::from(l.to_string()))
    });

    let line_els = lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let spans = highlights.get(i).map(|s| s.as_slice()).unwrap_or(&[]);
            render_highlighted_line(line, spans, t)
        })
        .collect::<Vec<_>>();

    let code_block = div()
        .id("code-block")
        .w_full()
        .bg(t.bg_sunken)
        .rounded(px(t.radius_md))
        .overflow_x_scroll();

    let with_header = match header_opt {
        Some(h) => code_block.child(h.border_b_1().border_color(t.separator)),
        None => code_block,
    };

    with_header
        .child(div().px(px(t.sp3)).py(px(t.sp2)).children(line_els))
        .into_any_element()
}

fn render_highlighted_line(line: &str, spans: &[HighlightSpan], t: &RuntimeTheme) -> AnyElement {
    if spans.is_empty() {
        return div()
            .font_family(t.mono_family.clone())
            .text_size(px(t.font_size_code))
            .text_color(t.text)
            .whitespace_nowrap()
            .child(SharedString::from(line.to_string()))
            .into_any_element();
    }

    let text = line.to_string();
    let mut runs: Vec<TextRun> = Vec::new();
    let mut byte_cursor = 0usize;

    for span in spans {
        let start = (span.start_byte_col as usize).min(line.len());
        let end = if span.end_byte_col == u32::MAX {
            line.len()
        } else {
            (span.end_byte_col as usize).min(line.len())
        };
        if start > byte_cursor {
            runs.push(plain_run(start - byte_cursor, t.text, t.mono_family.clone()));
        }
        if end > start {
            let col = token_color(span.token, t);
            runs.push(plain_run(end - start, col, t.mono_family.clone()));
        }
        byte_cursor = end.max(byte_cursor);
    }
    if byte_cursor < line.len() {
        runs.push(plain_run(line.len() - byte_cursor, t.text, t.mono_family.clone()));
    }

    if runs.is_empty() {
        return div()
            .font_family(t.mono_family.clone())
            .text_size(px(t.font_size_code))
            .text_color(t.text)
            .whitespace_nowrap()
            .child(SharedString::from(text))
            .into_any_element();
    }

    gpui::StyledText::new(SharedString::from(text))
        .with_runs(runs)
        .into_any_element()
}

fn plain_run(len: usize, color: Hsla, family: SharedString) -> TextRun {
    TextRun {
        len,
        font: Font {
            family,
            weight: FontWeight::NORMAL,
            style: FontStyle::Normal,
            features: gpui::FontFeatures::default(),
            fallbacks: None,
        },
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

fn token_color(token: SyntaxToken, t: &RuntimeTheme) -> Hsla {
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

fn render_blockquote(
    children: &[Block],
    base_dir: &PathBuf,
    t: &RuntimeTheme,
    cx: &mut App,
) -> AnyElement {
    let md_wrap = Arc::new(faber_editor::markdown::MarkdownDoc {
        blocks: children.to_vec(),
        outline: vec![],
    });
    let inner = children
        .iter()
        .enumerate()
        .map(|(i, _)| MarkdownPreviewView::render_block(&md_wrap, i, base_dir, t, cx))
        .collect::<Vec<_>>();

    div()
        .border_l_2()
        .border_color(t.accent_muted)
        .pl(px(t.sp4))
        .flex()
        .flex_col()
        .gap(px(t.sp1))
        .children(inner)
        .into_any_element()
}

fn render_list(
    ordered: bool,
    start: u64,
    items: &[ListItem],
    base_dir: &PathBuf,
    t: &RuntimeTheme,
    cx: &mut App,
) -> AnyElement {
    let item_els = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let marker: AnyElement = if let Some(checked) = item.task {
                let (bg, border) = if checked { (t.accent, t.accent) } else { (t.bg, t.border) };
                let check = if checked { "✓" } else { "" };
                div()
                    .flex_shrink_0()
                    .size(px(14.))
                    .rounded(px(t.radius_sm))
                    .bg(bg)
                    .border_1()
                    .border_color(border)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(10.))
                    .text_color(t.text_on_accent)
                    .child(check)
                    .into_any_element()
            } else if ordered {
                div()
                    .flex_shrink_0()
                    .min_w(px(t.char_w_code * 2.0))
                    .pr(px(t.sp1))
                    .text_color(t.text_muted)
                    .font_family(t.ui_family.clone())
                    .text_size(px(t.font_size_body))
                    .child(SharedString::from(format!("{}.", start + i as u64)))
                    .into_any_element()
            } else {
                div()
                    .flex_shrink_0()
                    .w(px(16.))
                    .text_color(t.text_muted)
                    .font_family(t.ui_family.clone())
                    .text_size(px(t.font_size_body))
                    .child("•")
                    .into_any_element()
            };

            let md_wrap = Arc::new(faber_editor::markdown::MarkdownDoc {
                blocks: item.blocks.clone(),
                outline: vec![],
            });
            let content = div().flex_1().min_w(px(0.)).flex().flex_col().gap(px(t.sp1)).children(
                item.blocks
                    .iter()
                    .enumerate()
                    .map(|(j, _)| MarkdownPreviewView::render_block(&md_wrap, j, base_dir, t, cx)),
            );

            div()
                .flex()
                .flex_row()
                .items_start()
                .gap_2()
                .py(px(1.))
                .child(marker)
                .child(content)
        })
        .collect::<Vec<_>>();

    div().flex().flex_col().gap(px(t.sp1)).children(item_els).into_any_element()
}

fn render_table(
    head: &[Vec<InlineRun>],
    rows: &[Vec<Vec<InlineRun>>],
    base_dir: &PathBuf,
    t: &RuntimeTheme,
    _cx: &mut App,
) -> AnyElement {
    let header_row = div()
        .flex()
        .flex_row()
        .bg(t.bg_elevated)
        .border_b_1()
        .border_color(t.separator)
        .children(head.iter().map(|cell| {
            div()
                .flex_1()
                .min_w(px(120.))
                .px(px(t.sp2))
                .py(px(t.sp1))
                .font_weight(FontWeight::BOLD)
                .text_size(px(t.font_size_body))
                .font_family(t.ui_family.clone())
                .child(render_inlines(cell, t, base_dir))
        }));

    let body_rows = rows
        .iter()
        .map(|row| {
            div()
                .flex()
                .flex_row()
                .border_b_1()
                .border_color(t.separator)
                .children(row.iter().map(|cell| {
                    div()
                        .flex_1()
                        .min_w(px(120.))
                        .px(px(t.sp2))
                        .py(px(t.sp1))
                        .text_size(px(t.font_size_body))
                        .font_family(t.ui_family.clone())
                        .text_color(t.text)
                        .child(render_inlines(cell, t, base_dir))
                }))
        })
        .collect::<Vec<_>>();

    div()
        .id("md-table")
        .w_full()
        .rounded(px(t.radius_md))
        .border_1()
        .border_color(t.separator)
        .overflow_x_scroll()
        .child(header_row)
        .children(body_rows)
        .into_any_element()
}
