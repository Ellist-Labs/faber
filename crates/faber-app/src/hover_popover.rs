// Hover popover content: flattens a parsed MarkdownDoc into selectable text
// segments, mirroring Zed's hover popover (crates/editor/src/hover_popover.rs +
// crates/markdown). Each segment is one laid-out StyledText whose TextLayout is
// retained for hit-testing, so text is selectable and links are clickable.

use std::{
    cell::{Cell, RefCell},
    ops::Range,
    rc::Rc,
    sync::Arc,
};

use faber_editor::markdown::{Block, BlockKind, InlineRun, MarkdownDoc};
use gpui::{
    Font, FontStyle, FontWeight, Hsla, Pixels, SharedString, StyledText, TextLayout, TextRun,
    UnderlineStyle, px,
};

use crate::buffer_view::token_color;
use crate::theme::RuntimeTheme;

// ── Segment model ──────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SegmentKind {
    /// Prose (paragraph / heading / list / quote / table row).
    Text,
    /// One line of a fenced code block — mono font, grouped into a sunken panel.
    Code,
    /// Horizontal rule — not selectable, renders as a divider.
    Rule,
    /// Remote image (badge / screenshot) — rendered as an img element, not text.
    /// `link` is the enclosing anchor target (the badge-click URL).
    Image { url: String, link: Option<String> },
}

impl SegmentKind {
    /// Only text-bearing segments participate in selection hit-testing.
    pub fn is_text(&self) -> bool {
        matches!(self, SegmentKind::Text | SegmentKind::Code)
    }
}

/// One selectable line of popover content plus everything needed to hit-test
/// and lay it out (sizes/gaps are resolved from the theme at build time).
pub struct Segment {
    pub kind: SegmentKind,
    /// Plain text exactly as laid out (byte indices match `runs` and `layout`).
    pub text: String,
    pub runs: Vec<TextRun>,
    /// (byte range, url) for clickable spans.
    pub links: Vec<(Range<usize>, String)>,
    /// Filled during paint; used for selection hit-testing across frames.
    pub layout: TextLayout,
    /// Painted bounds recorded by a canvas each frame. `None` until first paint —
    /// the guard that makes it safe to query `layout` (which panics pre-paint).
    pub bounds: Rc<Cell<Option<gpui::Bounds<Pixels>>>>,
    /// Font size in px (body / code / heading scale).
    pub text_size: f32,
    /// Inside a blockquote — rendered in a bordered callout group.
    pub quote: bool,
    /// Left indent in px (nested lists).
    pub indent: f32,
    /// Vertical gap before this segment (block boundaries).
    pub top_gap: f32,
    /// `(row, col, n_cols)` when this segment is a table cell; row 0 = header.
    pub table_cell: Option<(usize, usize, usize)>,
}

/// Selection endpoints as (segment index, byte offset within segment).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HoverSelection {
    pub start: (usize, usize),
    pub end: (usize, usize),
}

impl HoverSelection {
    pub fn normalized(&self) -> ((usize, usize), (usize, usize)) {
        if self.end < self.start {
            (self.end, self.start)
        } else {
            (self.start, self.end)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Byte range this selection covers within segment `ix`, if any.
    pub fn range_in_segment(&self, ix: usize, seg_len: usize) -> Option<Range<usize>> {
        let ((s_seg, s_off), (e_seg, e_off)) = self.normalized();
        if ix < s_seg || ix > e_seg {
            return None;
        }
        let start = if ix == s_seg { s_off.min(seg_len) } else { 0 };
        let end = if ix == e_seg {
            e_off.min(seg_len)
        } else {
            seg_len
        };
        (start < end).then_some(start..end)
    }
}

/// Extract the selected plain text, joining segments with newlines.
pub fn selected_text(segments: &[Segment], sel: &HoverSelection) -> String {
    let mut out = String::new();
    for (ix, seg) in segments.iter().enumerate() {
        if let Some(range) = sel.range_in_segment(ix, seg.text.len()) {
            if !out.is_empty() {
                out.push('\n');
            }
            // Clamp to char boundaries defensively.
            let start = ceil_char_boundary(&seg.text, range.start);
            let end = ceil_char_boundary(&seg.text, range.end);
            if start < end {
                out.push_str(&seg.text[start..end]);
            }
        }
    }
    out
}

fn ceil_char_boundary(s: &str, mut ix: usize) -> usize {
    ix = ix.min(s.len());
    while ix < s.len() && !s.is_char_boundary(ix) {
        ix += 1;
    }
    ix
}

// ── Run styling helpers ────────────────────────────────────────────────────────

/// Split `runs` at `range` boundaries and apply `style` to the covered slice.
/// Byte-exact: runs partially covered are split, never approximated.
pub fn style_run_range(runs: &mut Vec<TextRun>, range: Range<usize>, style: impl Fn(&mut TextRun)) {
    if range.start >= range.end {
        return;
    }
    let mut out: Vec<TextRun> = Vec::with_capacity(runs.len() + 2);
    let mut pos = 0usize;
    for run in runs.drain(..) {
        let run_start = pos;
        let run_end = pos + run.len;
        pos = run_end;
        let s = range.start.max(run_start);
        let e = range.end.min(run_end);
        if s >= e {
            out.push(run);
            continue;
        }
        if s > run_start {
            let mut head = run.clone();
            head.len = s - run_start;
            out.push(head);
        }
        let mut mid = run.clone();
        mid.len = e - s;
        style(&mut mid);
        out.push(mid);
        if e < run_end {
            let mut tail = run;
            tail.len = run_end - e;
            out.push(tail);
        }
    }
    *runs = out;
}

// ── Segment building ───────────────────────────────────────────────────────────

fn ui_font(t: &RuntimeTheme, bold: bool, italic: bool) -> Font {
    Font {
        family: t.ui_family.clone(),
        weight: if bold {
            FontWeight::BOLD
        } else {
            FontWeight::NORMAL
        },
        style: if italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        },
        features: gpui::FontFeatures::default(),
        fallbacks: None,
    }
}

fn mono_font(t: &RuntimeTheme) -> Font {
    Font {
        family: t.mono_family.clone(),
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
        features: gpui::FontFeatures::default(),
        fallbacks: None,
    }
}

fn plain_mono_run(len: usize, color: Hsla, t: &RuntimeTheme) -> TextRun {
    TextRun {
        len,
        font: mono_font(t),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// Immutable build context threaded through block recursion.
#[derive(Clone, Copy)]
struct SegCx {
    quote: bool,
    indent: f32,
    /// Font size for prose segments (heading levels override).
    size: f32,
    /// Bold everything (headings).
    bold: bool,
}

/// Vertical gap inserted before a new top-level block.
const BLOCK_GAP: f32 = 8.0;
/// Extra breathing room above headings.
const HEADING_GAP: f32 = 12.0;
/// Indent step per list nesting level.
const LIST_INDENT: f32 = 14.0;

/// Flatten a `MarkdownDoc` into popover segments, preserving block structure
/// (heading scale, quote grouping, list indentation, table cells, images).
pub fn build_segments(md: &MarkdownDoc, t: &RuntimeTheme) -> Vec<Segment> {
    let mut segments = Vec::new();
    let cx = SegCx {
        quote: false,
        indent: 0.0,
        size: t.font_size_body,
        bold: false,
    };
    for (ix, block) in md.blocks.iter().enumerate() {
        let gap = if ix == 0 {
            0.0
        } else if matches!(block.kind, BlockKind::Heading { .. }) {
            HEADING_GAP
        } else {
            BLOCK_GAP
        };
        push_block_with_gap(block, t, &cx, gap, &mut segments);
    }
    segments
}

/// Push a block's segments and stamp `gap` on the first one produced.
fn push_block_with_gap(
    block: &Block,
    t: &RuntimeTheme,
    cx: &SegCx,
    gap: f32,
    segments: &mut Vec<Segment>,
) {
    let before = segments.len();
    push_block(block, t, cx, segments);
    if let Some(seg) = segments.get_mut(before) {
        seg.top_gap = seg.top_gap.max(gap);
    }
}

fn heading_size(level: u8, t: &RuntimeTheme) -> f32 {
    match level {
        1 => t.font_size_body * 1.45,
        2 => t.font_size_body * 1.25,
        3 => t.font_size_body * 1.1,
        _ => t.font_size_body,
    }
}

fn push_block(block: &Block, t: &RuntimeTheme, cx: &SegCx, segments: &mut Vec<Segment>) {
    match &block.kind {
        BlockKind::Heading { level, inlines } => {
            let hcx = SegCx {
                size: heading_size(*level, t),
                bold: true,
                ..*cx
            };
            push_inline_segments(inlines, t, "", &hcx, segments);
        }
        BlockKind::Paragraph { inlines } => {
            push_inline_segments(inlines, t, "", cx, segments);
        }
        BlockKind::CodeBlock {
            text, highlights, ..
        } => {
            for (i, line) in text.lines().enumerate() {
                let spans = highlights.get(i).map(|s| s.as_slice()).unwrap_or(&[]);
                let runs = code_line_runs(line, spans, t);
                let mut seg = new_segment(SegmentKind::Code, line.to_string(), runs, vec![], t);
                seg.quote = cx.quote;
                seg.indent = cx.indent;
                segments.push(seg);
            }
            // An empty fence still occupies a visible panel row.
            if text.lines().next().is_none() {
                let mut seg = new_segment(SegmentKind::Code, String::new(), vec![], vec![], t);
                seg.quote = cx.quote;
                segments.push(seg);
            }
        }
        BlockKind::Blockquote { children } => {
            let qcx = SegCx { quote: true, ..*cx };
            for (ix, child) in children.iter().enumerate() {
                let gap = if ix == 0 { 0.0 } else { BLOCK_GAP / 2.0 };
                push_block_with_gap(child, t, &qcx, gap, segments);
            }
        }
        BlockKind::List {
            ordered,
            start,
            items,
        } => {
            for (i, item) in items.iter().enumerate() {
                let marker = match item.task {
                    Some(true) => "☑ ".to_string(),
                    Some(false) => "☐ ".to_string(),
                    None if *ordered => format!("{}. ", start + i as u64),
                    None => "• ".to_string(),
                };
                let item_gap = if i == 0 { 0.0 } else { 2.0 };
                let before = segments.len();
                let mut first = true;
                for child in &item.blocks {
                    match &child.kind {
                        // Nested list: indent one step deeper, no marker prefix.
                        BlockKind::List { .. } => {
                            let ncx = SegCx {
                                indent: cx.indent + LIST_INDENT,
                                ..*cx
                            };
                            push_block_with_gap(child, t, &ncx, 2.0, segments);
                        }
                        BlockKind::Paragraph { inlines } if first => {
                            push_inline_segments(inlines, t, &marker, cx, segments);
                        }
                        _ => {
                            let ncx = SegCx {
                                indent: cx.indent + LIST_INDENT,
                                ..*cx
                            };
                            push_block_with_gap(child, t, &ncx, 2.0, segments);
                        }
                    }
                    first = false;
                }
                if item.blocks.is_empty() {
                    let runs = vec![plain_ui_run(marker.len(), t.text_muted, t)];
                    let mut seg = new_segment(SegmentKind::Text, marker.clone(), runs, vec![], t);
                    seg.quote = cx.quote;
                    seg.indent = cx.indent;
                    segments.push(seg);
                }
                if let Some(seg) = segments.get_mut(before) {
                    seg.top_gap = seg.top_gap.max(item_gap);
                }
            }
        }
        BlockKind::Table { head, rows } => {
            let n_cols = head
                .len()
                .max(rows.iter().map(|r| r.len()).max().unwrap_or(0));
            if n_cols == 0 {
                return;
            }
            let mut push_row = |cells: &[Vec<InlineRun>], row_ix: usize, bold: bool| {
                for col in 0..n_cols {
                    let inlines = cells.get(col).map(|c| c.as_slice()).unwrap_or(&[]);
                    let ccx = SegCx { bold, ..*cx };
                    let mut cell = build_inline_segment(inlines, t, "", &ccx);
                    cell.table_cell = Some((row_ix, col, n_cols));
                    cell.quote = cx.quote;
                    segments.push(cell);
                }
            };
            push_row(head, 0, true);
            for (r, row) in rows.iter().enumerate() {
                push_row(row, r + 1, false);
            }
        }
        BlockKind::Rule => {
            segments.push(new_segment(
                SegmentKind::Rule,
                String::new(),
                vec![],
                vec![],
                t,
            ));
        }
        BlockKind::HtmlBlock { text } => {
            for line in text.lines() {
                let runs = vec![plain_mono_run(line.len(), t.text_muted, t)];
                let mut seg = new_segment(SegmentKind::Code, line.to_string(), runs, vec![], t);
                seg.quote = cx.quote;
                segments.push(seg);
            }
        }
    }
}

fn new_segment(
    kind: SegmentKind,
    text: String,
    runs: Vec<TextRun>,
    links: Vec<(Range<usize>, String)>,
    t: &RuntimeTheme,
) -> Segment {
    let text_size = if matches!(kind, SegmentKind::Code) {
        t.font_size_code
    } else {
        t.font_size_body
    };
    Segment {
        kind,
        text,
        runs,
        links,
        layout: TextLayout::default(),
        bounds: Rc::new(Cell::new(None)),
        text_size,
        quote: false,
        indent: 0.0,
        top_gap: 0.0,
        table_cell: None,
    }
}

fn plain_ui_run(len: usize, color: Hsla, t: &RuntimeTheme) -> TextRun {
    TextRun {
        len,
        font: ui_font(t, false, false),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// Convert inlines into exactly ONE segment (hard breaks become spaces) —
/// used for table cells where per-line splitting would break the grid.
fn build_inline_segment(
    inlines: &[InlineRun],
    t: &RuntimeTheme,
    prefix: &str,
    cx: &SegCx,
) -> Segment {
    let mut out: Vec<Segment> = Vec::new();
    push_inline_segments(inlines, t, prefix, cx, &mut out);
    // Merge multi-segment output (hard breaks / images) into the first text
    // segment; images inside table cells degrade to their alt text upstream.
    let mut iter = out.into_iter().filter(|s| s.kind.is_text());
    let mut first = match iter.next() {
        Some(s) => s,
        None => new_segment(SegmentKind::Text, String::new(), vec![], vec![], t),
    };
    for seg in iter {
        if !first.text.is_empty() {
            first.text.push(' ');
            first.runs.push(plain_ui_run(1, t.text, t));
        }
        let base = first.text.len();
        first.text.push_str(&seg.text);
        first.runs.extend(seg.runs);
        first.links.extend(
            seg.links
                .into_iter()
                .map(|(r, u)| (base + r.start..base + r.end, u)),
        );
    }
    first.text_size = cx.size;
    first
}

/// Convert a run of inlines into one segment per hard-break line; standalone
/// remote images become Image segments.
fn push_inline_segments(
    inlines: &[InlineRun],
    t: &RuntimeTheme,
    prefix: &str,
    cx: &SegCx,
    segments: &mut Vec<Segment>,
) {
    let mut text = String::new();
    let mut runs: Vec<TextRun> = Vec::new();
    let mut links: Vec<(Range<usize>, String)> = Vec::new();

    if !prefix.is_empty() {
        text.push_str(prefix);
        runs.push(plain_ui_run(prefix.len(), t.text_muted, t));
    }

    let flush = |text: &mut String,
                 runs: &mut Vec<TextRun>,
                 links: &mut Vec<(Range<usize>, String)>,
                 segments: &mut Vec<Segment>| {
        if !text.trim().is_empty() {
            let mut seg = new_segment(
                SegmentKind::Text,
                std::mem::take(text),
                std::mem::take(runs),
                std::mem::take(links),
                t,
            );
            seg.text_size = cx.size;
            seg.quote = cx.quote;
            seg.indent = cx.indent;
            segments.push(seg);
        } else {
            text.clear();
            runs.clear();
            links.clear();
        }
    };

    for inline in inlines {
        match inline {
            InlineRun::Text {
                text: t_text,
                style,
                link,
            } => {
                if t_text.is_empty() {
                    continue;
                }
                let start = text.len();
                text.push_str(t_text);
                let is_link = link
                    .as_ref()
                    .is_some_and(|u| u.starts_with("http://") || u.starts_with("https://"));
                if is_link && let Some(url) = link {
                    links.push((start..text.len(), url.clone()));
                }
                let (font, bg) = if style.code {
                    (mono_font(t), Some(t.bg_sunken))
                } else {
                    (ui_font(t, cx.bold || style.bold, style.italic), None)
                };
                runs.push(TextRun {
                    len: t_text.len(),
                    font,
                    color: if is_link { t.accent } else { t.text },
                    background_color: bg,
                    underline: is_link.then(|| UnderlineStyle {
                        thickness: px(1.),
                        color: Some(t.accent),
                        wavy: false,
                    }),
                    strikethrough: style.strike.then(|| gpui::StrikethroughStyle {
                        thickness: px(1.),
                        color: Some(t.text_muted),
                    }),
                });
            }
            InlineRun::SoftBreak => {
                // Zed: soft_break_as_hard_break = true for hover popover.
                // Doc comments often have `\n` within a logical block; treat
                // each line as its own visual line rather than flowing together.
                flush(&mut text, &mut runs, &mut links, segments);
            }
            InlineRun::HardBreak => flush(&mut text, &mut runs, &mut links, segments),
            InlineRun::Image { alt, dest, link } => {
                let is_remote = dest.starts_with("http://") || dest.starts_with("https://");
                if is_remote {
                    // Standalone image element (badges render side by side).
                    flush(&mut text, &mut runs, &mut links, segments);
                    let mut seg = new_segment(
                        SegmentKind::Image {
                            url: dest.clone(),
                            link: link.clone(),
                        },
                        String::new(),
                        vec![],
                        vec![],
                        t,
                    );
                    seg.quote = cx.quote;
                    seg.indent = cx.indent;
                    segments.push(seg);
                } else if !alt.is_empty() {
                    let start = text.len();
                    text.push_str(alt);
                    runs.push(TextRun {
                        len: text.len() - start,
                        font: ui_font(t, false, true),
                        color: t.text_muted,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    });
                }
            }
        }
    }
    flush(&mut text, &mut runs, &mut links, segments);
}

fn code_line_runs(
    line: &str,
    spans: &[faber_editor::highlight::HighlightSpan],
    t: &RuntimeTheme,
) -> Vec<TextRun> {
    if line.is_empty() {
        return vec![];
    }
    if spans.is_empty() {
        return vec![plain_mono_run(line.len(), t.text, t)];
    }
    let mut runs: Vec<TextRun> = Vec::new();
    let mut byte_cursor = 0usize;
    for span in spans {
        // Clamp to the uncovered tail: spans may overlap or arrive unsorted
        // (nested tree-sitter captures), and runs must tile the text exactly —
        // gpui's StyledText asserts on any mismatch.
        let start = (span.start_byte_col as usize)
            .min(line.len())
            .max(byte_cursor);
        let end = if span.end_byte_col == u32::MAX {
            line.len()
        } else {
            (span.end_byte_col as usize).min(line.len())
        };
        if end <= byte_cursor {
            continue;
        }
        if start > byte_cursor {
            runs.push(plain_mono_run(start - byte_cursor, t.text, t));
        }
        runs.push(plain_mono_run(end - start, token_color(span.token, t), t));
        byte_cursor = end;
    }
    if byte_cursor < line.len() {
        runs.push(plain_mono_run(line.len() - byte_cursor, t.text, t));
    }
    runs
}

// ── Hit-testing ────────────────────────────────────────────────────────────────

/// Map a window position to (segment index, byte offset). Positions outside any
/// segment resolve to the nearest one vertically, so drags past the popover edge
/// extend the selection naturally. Rule segments are skipped.
pub fn hit_test(
    segments: &Rc<RefCell<Vec<Segment>>>,
    pos: gpui::Point<Pixels>,
) -> Option<(usize, usize)> {
    let segments = segments.borrow();
    let mut best: Option<(usize, f32)> = None;
    for (ix, seg) in segments.iter().enumerate() {
        if !seg.kind.is_text() {
            continue;
        }
        let Some(bounds) = seg.bounds.get() else {
            continue;
        };
        let dist = if pos.y < bounds.top() {
            f32::from(bounds.top() - pos.y)
        } else if pos.y > bounds.bottom() {
            f32::from(pos.y - bounds.bottom())
        } else {
            0.0
        };
        match best {
            Some((_, d)) if d <= dist => {}
            _ => best = Some((ix, dist)),
        }
    }
    let (ix, _) = best?;
    let seg = &segments[ix];
    // `bounds` is Some, so the segment painted this frame and `layout` is measured.
    let byte = match seg.layout.index_for_position(pos) {
        Ok(b) => b,
        Err(b) => b,
    };
    Some((ix, byte.min(seg.text.len())))
}

/// Find a link at `pos`, returning (segment index, link index).
pub fn link_at(
    segments: &Rc<RefCell<Vec<Segment>>>,
    pos: gpui::Point<Pixels>,
) -> Option<(usize, usize)> {
    let segments_ref = segments.borrow();
    for (ix, seg) in segments_ref.iter().enumerate() {
        if seg.links.is_empty() {
            continue;
        }
        let Some(bounds) = seg.bounds.get() else {
            continue;
        };
        if !bounds.contains(&pos) {
            continue;
        }
        if let Ok(byte) = seg.layout.index_for_position(pos) {
            for (link_ix, (range, _)) in seg.links.iter().enumerate() {
                if range.contains(&byte) {
                    return Some((ix, link_ix));
                }
            }
        }
    }
    None
}

// ── Popover sizing ─────────────────────────────────────────────────────────────

/// Rough content height estimate used to decide above/below placement before
/// the popover has painted. Over-estimating flips to "below" early — harmless.
pub fn estimate_height(segments: &[Segment], t: &RuntimeTheme) -> f32 {
    let mut h = 20.0; // container padding
    for seg in segments {
        h += seg.top_gap
            + match &seg.kind {
                SegmentKind::Code => t.line_height_code,
                SegmentKind::Rule => 9.0,
                SegmentKind::Image { .. } => 24.0,
                SegmentKind::Text => {
                    let row = seg.text_size + 8.0;
                    match seg.table_cell {
                        // A row of N cells shares one line.
                        Some((_, _, n)) => row / n.max(1) as f32,
                        None => row,
                    }
                }
            };
    }
    h
}

/// True when `runs` cover `text` exactly, every boundary on a char boundary —
/// the invariant `StyledText::with_runs` asserts (it aborts the app otherwise).
fn runs_tile_text(runs: &[TextRun], text: &str) -> bool {
    let mut ix = 0usize;
    for run in runs {
        ix += run.len;
        if ix > text.len() || !text.is_char_boundary(ix) {
            return false;
        }
    }
    ix == text.len()
}

/// Build the StyledText element for a segment, applying the selection overlay.
pub fn segment_styled_text(
    seg: &Segment,
    sel_range: Option<Range<usize>>,
    selection_color: Hsla,
) -> StyledText {
    let mut runs = seg.runs.clone();
    if let Some(range) = sel_range {
        style_run_range(&mut runs, range, |run| {
            run.background_color = Some(selection_color);
        });
    }
    // Only Code segments go through `code_line_runs` which handles potentially
    // unsorted/overlapping tree-sitter spans; Text segments tile by construction.
    if matches!(seg.kind, SegmentKind::Code) && !runs_tile_text(&runs, &seg.text) {
        log::warn!(
            "hover: run list does not tile segment text (len {}), dropping styling",
            seg.text.len()
        );
        match runs.first() {
            Some(first) if !seg.text.is_empty() => {
                let mut run = first.clone();
                run.len = seg.text.len();
                run.background_color = None;
                runs = vec![run];
            }
            // No runs to borrow a style from — render with the parent's style.
            _ => return StyledText::new(SharedString::from(seg.text.clone())),
        }
    }
    StyledText::new(SharedString::from(seg.text.clone())).with_runs(runs)
}

pub type SharedSegments = Rc<RefCell<Vec<Segment>>>;

// ── Layout helpers ─────────────────────────────────────────────────────────────

/// Pixel anchor for the popover, computed ONCE from the hover symbol's start
/// glyph when the response arrives. The popover never follows the mouse.
#[derive(Clone, Copy, Debug)]
pub struct HoverAnchor {
    pub x: f32,
    pub line_top: f32,
    pub line_bottom: f32,
}

/// Rebuild the shared segment list from parsed markdown (called when content
/// changes, NOT per frame — TextLayout handles must stay stable so painted
/// layout state survives between mouse events).
pub fn rebuild_segments(shared: &SharedSegments, md: &Arc<MarkdownDoc>, t: &RuntimeTheme) {
    *shared.borrow_mut() = build_segments(md, t);
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn run(len: usize) -> TextRun {
        TextRun {
            len,
            font: Font {
                family: SharedString::from("mono"),
                weight: FontWeight::NORMAL,
                style: FontStyle::Normal,
                features: gpui::FontFeatures::default(),
                fallbacks: None,
            },
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }
    }

    #[test]
    fn style_range_splits_runs_exactly() {
        let mut runs = vec![run(4), run(6)];
        style_run_range(&mut runs, 2..7, |r| {
            r.background_color = Some(gpui::red());
        });
        let lens: Vec<usize> = runs.iter().map(|r| r.len).collect();
        assert_eq!(lens, vec![2, 2, 3, 3]);
        assert!(runs[1].background_color.is_some());
        assert!(runs[2].background_color.is_some());
        assert!(runs[0].background_color.is_none());
        assert!(runs[3].background_color.is_none());
        assert_eq!(lens.iter().sum::<usize>(), 10);
    }

    #[test]
    fn style_range_noop_outside() {
        let mut runs = vec![run(5)];
        style_run_range(&mut runs, 5..5, |r| {
            r.background_color = Some(gpui::red());
        });
        assert_eq!(runs.len(), 1);
        assert!(runs[0].background_color.is_none());
    }

    #[test]
    fn selection_normalizes_and_slices() {
        let sel = HoverSelection {
            start: (2, 3),
            end: (0, 1),
        };
        assert_eq!(sel.normalized(), ((0, 1), (2, 3)));
        assert_eq!(sel.range_in_segment(0, 10), Some(1..10));
        assert_eq!(sel.range_in_segment(1, 4), Some(0..4));
        assert_eq!(sel.range_in_segment(2, 10), Some(0..3));
        assert_eq!(sel.range_in_segment(3, 10), None);
    }

    fn test_theme() -> RuntimeTheme {
        RuntimeTheme::from(faber_theme::default::faber_dark())
    }

    #[test]
    fn selected_text_joins_segments() {
        let t = test_theme();
        let segments = vec![
            new_segment(SegmentKind::Text, "hello world".into(), vec![], vec![], &t),
            new_segment(SegmentKind::Text, "second".into(), vec![], vec![], &t),
        ];
        let sel = HoverSelection {
            start: (0, 6),
            end: (1, 3),
        };
        assert_eq!(selected_text(&segments, &sel), "world\nsec");
    }

    #[test]
    fn segments_preserve_block_structure() {
        use faber_editor::markdown::parse_markdown;
        use ropey::Rope;
        let t = test_theme();
        let src =
            "# Title\n\n> callout\n\n- item one\n- item two\n\n| A | B |\n|---|---|\n| 1 | 2 |\n";
        let rope = Rope::from_str(src);
        let registry = faber_editor::LanguageRegistry::default();
        let md = parse_markdown(src, &rope, &registry);
        let segs = build_segments(&md, &t);

        let title = segs.iter().find(|s| s.text == "Title").expect("heading");
        assert!(title.text_size > t.font_size_body, "headings scale up");
        let quote = segs.iter().find(|s| s.text.contains("callout")).unwrap();
        assert!(quote.quote, "blockquote flag set");
        assert!(
            segs.iter().any(|s| s.text.starts_with("• item one")),
            "list bullets present"
        );
        let cells: Vec<_> = segs.iter().filter(|s| s.table_cell.is_some()).collect();
        assert_eq!(cells.len(), 4, "2x2 table cells");
        assert_eq!(cells[0].table_cell, Some((0, 0, 2)));
    }

    #[test]
    fn overlapping_highlight_spans_still_tile_the_line() {
        use faber_editor::SyntaxToken;
        use faber_editor::highlight::HighlightSpan;
        let t = RuntimeTheme::from(faber_theme::default::faber_dark());
        let line = "pub fn hello_world() {}";
        // Nested/overlapping and unsorted spans, as tree-sitter captures can be.
        let spans = vec![
            HighlightSpan {
                start_byte_col: 0,
                end_byte_col: 10,
                token: SyntaxToken::Keyword,
            },
            HighlightSpan {
                start_byte_col: 4,
                end_byte_col: 6,
                token: SyntaxToken::Function,
            },
            HighlightSpan {
                start_byte_col: 2,
                end_byte_col: 20,
                token: SyntaxToken::Type,
            },
            HighlightSpan {
                start_byte_col: 15,
                end_byte_col: u32::MAX,
                token: SyntaxToken::Punctuation,
            },
        ];
        let runs = code_line_runs(line, &spans, &t);
        assert!(
            runs_tile_text(&runs, line),
            "runs must tile the line exactly"
        );
    }

    #[test]
    fn empty_selection_has_no_text() {
        let t = test_theme();
        let segments = vec![new_segment(
            SegmentKind::Text,
            "abc".into(),
            vec![],
            vec![],
            &t,
        )];
        let sel = HoverSelection {
            start: (0, 1),
            end: (0, 1),
        };
        assert!(sel.is_empty());
        assert_eq!(selected_text(&segments, &sel), "");
    }
}
