use std::{path::PathBuf, sync::Arc};

use felix_editor::{
    LanguageRegistry,
    SyntaxToken,
    highlight::HighlightSpan,
    markdown::{Block, BlockKind, InlineRun, ListItem, MarkdownDoc},
};
use gpui::{
    AnyElement, App, Context, Font, FontStyle, FontWeight, Hsla, IntoElement,
    ListAlignment, ListState, ParentElement, Render, SharedString, StrikethroughStyle, Styled,
    TextRun, UnderlineStyle, Window, div, list, prelude::*, px,
};
use ropey::Rope;

use crate::theme::RuntimeTheme;

// ── MarkdownPreviewView ───────────────────────────────────────────────────────

pub struct MarkdownPreviewView {
    pub md: Arc<MarkdownDoc>,
    pub list_state: ListState,
    pub base_dir: PathBuf,
    generation: u64,
    last_parsed_generation: u64,
}

impl MarkdownPreviewView {
    pub fn new(rope: &Rope, path: &std::path::Path, registry: &LanguageRegistry) -> Self {
        let source = rope.to_string();
        let md = Arc::new(felix_editor::markdown::parse_markdown(&source, rope, registry));
        let block_count = md.blocks.len();
        let list_state = ListState::new(block_count, ListAlignment::Top, px(512.));
        Self {
            md,
            list_state,
            base_dir: path.parent().unwrap_or(std::path::Path::new(".")).to_path_buf(),
            generation: 0,
            last_parsed_generation: 0,
        }
    }

    /// Synchronous re-parse — called at toggle time for instant correctness.
    pub fn reparse_now(&mut self, rope: &Rope, registry: &LanguageRegistry) {
        let source = rope.to_string();
        let md = felix_editor::markdown::parse_markdown(&source, rope, registry);
        self.apply_md(Arc::new(md));
    }

    /// Scroll the preview list to the block whose source_lines best matches `line`.
    pub fn scroll_to_source_line(&self, line: usize) {
        let ix = self.md.blocks.partition_point(|b| b.source_lines.start <= line);
        let ix = ix.saturating_sub(1).min(self.md.blocks.len().saturating_sub(1));
        if !self.md.blocks.is_empty() {
            self.list_state.scroll_to_reveal_item(ix);
        }
    }

    pub fn apply_md(&mut self, md: Arc<MarkdownDoc>) {
        let count = md.blocks.len();
        self.md = md;
        self.last_parsed_generation = self.generation;
        self.list_state.reset(count);
    }

    /// Render a single block element. Called from render() via closure.
    pub fn render_block(
        md: &Arc<MarkdownDoc>,
        ix: usize,
        base_dir: &PathBuf,
        t: &RuntimeTheme,
        cx: &mut App,
    ) -> AnyElement {
        let block = &md.blocks[ix];
        match &block.kind {
            BlockKind::Heading { level, inlines } => {
                render_heading(*level, inlines, t, cx)
            }
            BlockKind::Paragraph { inlines } => {
                render_paragraph(inlines, t, cx)
            }
            BlockKind::CodeBlock { lang, text, highlights } => {
                render_code_block(lang.as_deref(), text, highlights, t)
            }
            BlockKind::Blockquote { children } => {
                render_blockquote(children, base_dir, t, cx)
            }
            BlockKind::List { ordered, start, items } => {
                render_list(*ordered, *start, items, base_dir, t, cx)
            }
            BlockKind::Table { head, rows } => {
                render_table(head, rows, t, cx)
            }
            BlockKind::Rule => {
                div().my_3().h(px(1.)).bg(t.separator).into_any_element()
            }
            BlockKind::HtmlBlock { text } => {
                div()
                    .font_family(t.mono_family.clone())
                    .text_size(px(t.font_size_code))
                    .text_color(t.text_muted)
                    .child(SharedString::from(text.clone()))
                    .into_any_element()
            }
        }
    }
}

impl Render for MarkdownPreviewView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<RuntimeTheme>().clone();
        let md = Arc::clone(&self.md);
        let base_dir = self.base_dir.clone();
        let t2 = t.clone();

        let list_state = self.list_state.clone();

        div()
            .size_full()
            .bg(t.bg)
            .overflow_hidden()
            .child(
                div()
                    .size_full()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(760.))
                            .px(px(t.sp6))
                            .py(px(t.sp4))
                            .child(
                                list(list_state, move |ix, _window, cx| {
                                    Self::render_block(&md, ix, &base_dir, &t2, cx)
                                })
                                .size_full()
                            )
                    )
            )
    }
}

// ── block renderers ───────────────────────────────────────────────────────────

fn render_heading(
    level: u8,
    inlines: &[InlineRun],
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

    let el = div()
        .font_family(t.ui_family.clone())
        .text_size(px(size))
        .font_weight(FontWeight::BOLD)
        .text_color(t.text)
        .mt(px(t.sp5))
        .mb(px(t.sp2))
        .child(render_inlines(inlines, t, false, cx));

    if level <= 2 {
        el.border_b_1().border_color(t.separator).pb(px(t.sp2)).into_any_element()
    } else {
        el.into_any_element()
    }
}

fn render_paragraph(inlines: &[InlineRun], t: &RuntimeTheme, cx: &mut App) -> AnyElement {
    div()
        .font_family(t.ui_family.clone())
        .text_size(px(t.font_size_body))
        .text_color(t.text)
        .my(px(t.sp2))
        .child(render_inlines(inlines, t, false, cx))
        .into_any_element()
}

fn render_inlines(
    inlines: &[InlineRun],
    t: &RuntimeTheme,
    _in_code_context: bool,
    _cx: &mut App,
) -> AnyElement {
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
                if run_len == 0 { continue; }

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
            InlineRun::Image { alt: _, dest } => {
                // Inline images rendered as a separate element (can't mix into StyledText).
                // We flush any accumulated text first, then add the image.
                // For simplicity in this text accumulation model, render alt text inline.
                let start = text.len();
                text.push_str("[image]");
                let run_len = text.len() - start;
                if run_len > 0 {
                    runs.push(TextRun {
                        len: run_len,
                        font: default_font.clone(),
                        color: t.text_muted,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    });
                }
                let _ = dest; // image rendered as block elsewhere
            }
            InlineRun::SoftBreak => {
                let start = text.len();
                text.push(' ');
                let run_len = text.len() - start;
                runs.push(TextRun {
                    len: run_len,
                    font: default_font.clone(),
                    color: t.text,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                });
            }
            InlineRun::HardBreak => {
                let start = text.len();
                text.push('\n');
                let run_len = text.len() - start;
                runs.push(TextRun {
                    len: run_len,
                    font: default_font.clone(),
                    color: t.text,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                });
            }
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

    let line_els = lines.iter().enumerate().map(|(i, line)| {
        let spans = highlights.get(i).map(|s| s.as_slice()).unwrap_or(&[]);
        render_highlighted_line(line, spans, t)
    }).collect::<Vec<_>>();

    let code_block = div()
        .id("code-block")
        .bg(t.bg_sunken)
        .rounded(px(t.radius_md))
        .my(px(t.sp2))
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
            let gap = start - byte_cursor;
            runs.push(plain_run(gap, t.text, t.mono_family.clone()));
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
        font: Font { family, weight: FontWeight::NORMAL, style: FontStyle::Normal, features: gpui::FontFeatures::default(), fallbacks: None },
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
    let md_wrap = Arc::new(felix_editor::markdown::MarkdownDoc {
        blocks: children.to_vec(),
        outline: vec![],
    });
    let inner = children.iter().enumerate().map(|(i, _)| {
        MarkdownPreviewView::render_block(&md_wrap, i, base_dir, t, cx)
    }).collect::<Vec<_>>();

    div()
        .border_l_2()
        .border_color(t.accent_muted)
        .pl(px(t.sp4))
        .my(px(t.sp2))
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
    let item_els = items.iter().enumerate().map(|(i, item)| {
        let marker: AnyElement = if let Some(checked) = item.task {
            // Task checkbox
            let (bg, border) = if checked {
                (t.accent, t.accent)
            } else {
                (t.bg, t.border)
            };
            div()
                .size(px(14.))
                .flex_shrink_0()
                .rounded(px(t.radius_sm))
                .bg(bg)
                .border_1()
                .border_color(border)
                .into_any_element()
        } else if ordered {
            div()
                .w(px(24.))
                .flex_shrink_0()
                .text_color(t.text_muted)
                .font_family(t.ui_family.clone())
                .text_size(px(t.font_size_body))
                .child(SharedString::from(format!("{}.", start + i as u64)))
                .into_any_element()
        } else {
            div()
                .w(px(16.))
                .flex_shrink_0()
                .text_color(t.text_muted)
                .font_family(t.ui_family.clone())
                .text_size(px(t.font_size_body))
                .child("•")
                .into_any_element()
        };

        let md_wrap = Arc::new(felix_editor::markdown::MarkdownDoc {
            blocks: item.blocks.clone(),
            outline: vec![],
        });
        let content = div().flex_1().children(
            item.blocks.iter().enumerate().map(|(j, _)| {
                MarkdownPreviewView::render_block(&md_wrap, j, base_dir, t, cx)
            })
        );

        div().flex().flex_row().items_start().gap_2().py(px(1.))
            .child(marker).child(content)
    }).collect::<Vec<_>>();

    div().my(px(t.sp2)).children(item_els).into_any_element()
}

fn render_table(
    head: &[Vec<InlineRun>],
    rows: &[Vec<Vec<InlineRun>>],
    t: &RuntimeTheme,
    cx: &mut App,
) -> AnyElement {
    let header_row = div().flex().flex_row().bg(t.bg_elevated).border_b_1().border_color(t.separator)
        .children(head.iter().map(|cell| {
            div().flex_1().px(px(t.sp2)).py(px(t.sp1)).font_weight(FontWeight::BOLD)
                .text_size(px(t.font_size_body)).font_family(t.ui_family.clone())
                .child(render_inlines(cell, t, false, cx))
        }));

    let body_rows = rows.iter().map(|row| {
        div().flex().flex_row().border_b_1().border_color(t.separator)
            .children(row.iter().map(|cell| {
                div().flex_1().px(px(t.sp2)).py(px(t.sp1))
                    .text_size(px(t.font_size_body)).font_family(t.ui_family.clone())
                    .text_color(t.text)
                    .child(render_inlines(cell, t, false, cx))
            }))
    }).collect::<Vec<_>>();

    div()
        .my(px(t.sp2))
        .rounded(px(t.radius_md))
        .border_1()
        .border_color(t.separator)
        .overflow_hidden()
        .child(header_row)
        .children(body_rows)
        .into_any_element()
}
