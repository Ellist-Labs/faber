use std::{ops::Range, sync::Arc, time::Duration};

use faber_editor::{
    LanguageRegistry, SyntaxToken,
    buffer::Document,
    cursor,
    edit_history::History,
    highlight::{HighlightSpan, byte_col_to_char_col},
    markdown::{OutlineEntry, parse_markdown, edit::{EnterAction, enter_action, smart_wrap, looks_like_url, toggle_checkbox}},
    search::Query,
    Selection,
};
use gpui::{
    AnyElement, App, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, IntoElement,
    KeyDownEvent, ListHorizontalSizingBehavior, MouseButton, MouseDownEvent, MouseMoveEvent,
    Render, ScrollHandle, ScrollStrategy, ScrollWheelEvent, SharedString, UniformListScrollHandle,
    Window, deferred, div, prelude::*, px, svg, uniform_list,
};

use crate::markdown_preview::MarkdownPreviewView;
use crate::settings_view::SettingsStore;
use crate::theme::RuntimeTheme;
use crate::ui::{IconName, ScrollbarDrag, render_scrollbar};
use crate::ui::scrollbar::{start_drag, update_drag};
use rust_i18n::t;

// ── layout ─────────────────────────────────────────────────────────────────────
// Line height and char width live on RuntimeTheme (settings-scaled).

const GUTTER_COLS: f32 = 6.0;
/// Max lines in a markdown file before switching back to the horizontal-scroll
/// virtualized path. Above this cap wrapping is too expensive without a display map.
const WRAP_LINE_CAP: usize = 5_000;

// ── actions ────────────────────────────────────────────────────────────────────

use crate::{
    Backspace, BoldSelection, CloseSearch, Copy, Cut, Delete, DeleteLine, DeleteToLineEnd,
    DeleteToLineStart, DeleteWordLeft, DeleteWordRight, Enter, FindNext, FindPrev,
    InputMoveEnd, InputMoveLeft, InputMoveRight, InputMoveStart,
    ItalicSelection, MoveDocEnd, MoveDocStart, MoveDown, MoveLeft, MoveLineEnd, MoveLineStart,
    MovePageDown, MovePageUp, MoveRight, MoveUp, MoveWordLeft, MoveWordRight, OpenReplace,
    OpenSearch, Paste, ProjectRoot, Redo, ReplaceAll, ReplaceBackspace, ReplaceOne,
    SearchBackspace, SelectAll, SelectDocEnd, SelectDocStart, SelectDown, SelectLeft,
    SelectLineEnd, SelectLineStart, SelectRight, SelectUp, SelectWordLeft, SelectWordRight,
    Tab, ToggleCheckbox, TogglePreview, ToggleReplace, ToggleSearchCase, ToggleSearchRegex,
    ToggleSearchWholeWord, Undo,
};

// ── EditorView ─────────────────────────────────────────────────────────────────

/// Emitted after every document mutation — drives auto-save and future
/// subscribers (LSP didChange, etc.).
pub enum EditorEvent {
    Edited,
}

pub struct EditorView {
    pub doc: Document,
    pub sel: Selection,
    pub history: History,
    pub focus_handle: FocusHandle,

    // Line display cache — rebuilt after every document mutation.
    pub line_cache: Vec<SharedString>,
    pub line_starts: Vec<usize>,
    pub widest_line: usize,

    pub scroll_handle: UniformListScrollHandle,
    last_scroll_line: usize,
    mouse_selecting: bool,

    // Markdown preview
    pub preview: Option<gpui::Entity<MarkdownPreviewView>>,
    pub show_preview: bool,
    pub outline: Arc<Vec<OutlineEntry>>,
    outline_gen: u64,

    // Search / replace
    pub show_search: bool,
    pub show_replace: bool,
    pub search_handle: FocusHandle,
    pub replace_handle: FocusHandle,
    pub search_query: String,
    pub replace_query: String,
    pub search_cursor: usize,
    pub replace_cursor: usize,
    pub matches: Vec<Range<usize>>,
    pub match_idx: usize,
    pub search_case_sensitive: bool,
    pub search_whole_word: bool,
    pub search_regex: bool,

    pub scrollbar_drag: Option<ScrollbarDrag>,

    // Non-virtualized markdown wrap scroll handle (used when is_wrap_mode())
    pub md_scroll: ScrollHandle,
    // Cached top visible line — used to avoid redundant cx.notify() on scroll
    last_top_line: usize,

    // Outline overlay (heading navigator for markdown files)
    pub outline_open: bool,
    pub outline_query: String,
    pub outline_cursor: usize,           // caret position in the query input
    pub outline_hover: Option<usize>,    // index into the filtered list
    pub outline_highlight: Option<(usize, usize)>, // (start_line, end_line) of hovered section
    pub outline_handle: FocusHandle,

    // Cursor blink
    pub cursor_blink_on: bool,
    cursor_blink_epoch: u64,
}

impl EditorView {
    pub fn new(path: &str, cx: &mut Context<EditorView>) -> Self {
        let doc = Document::open(path).unwrap_or_else(|_| {
            let mut d = Document::open("/dev/null").expect("can't open /dev/null");
            d.path = std::path::PathBuf::from(path);
            d
        });
        let view = Self::from_doc(doc, cx);
        view
    }

    pub fn from_doc(doc: Document, cx: &mut Context<EditorView>) -> Self {
        let mut view = Self {
            doc,
            sel: Selection::default(),
            history: History::new(),
            focus_handle: cx.focus_handle(),
            line_cache: Vec::new(),
            line_starts: Vec::new(),
            widest_line: 0,
            scroll_handle: UniformListScrollHandle::new(),
            last_scroll_line: 0,
            mouse_selecting: false,
            preview: None,
            show_preview: false,
            outline: Arc::new(vec![]),
            outline_gen: 0,
            show_search: false,
            show_replace: false,
            search_handle: cx.focus_handle(),
            replace_handle: cx.focus_handle(),
            search_query: String::new(),
            replace_query: String::new(),
            search_cursor: 0,
            replace_cursor: 0,
            matches: Vec::new(),
            match_idx: 0,
            search_case_sensitive: false,
            search_whole_word: false,
            search_regex: false,
            scrollbar_drag: None,
            md_scroll: ScrollHandle::new(),
            last_top_line: 0,
            outline_open: false,
            outline_query: String::new(),
            outline_cursor: 0,
            outline_hover: None,
            outline_highlight: None,
            outline_handle: cx.focus_handle(),
            cursor_blink_on: true,
            cursor_blink_epoch: 0,
        };
        view.rebuild_line_cache();
        if view.is_markdown() {
            let source = view.doc.rope.to_string();
            let registry = LanguageRegistry::with_defaults();
            let md = parse_markdown(&source, &view.doc.rope, &registry);
            view.outline = Arc::new(md.outline);
        }
        view
    }

    // ── helpers ────────────────────────────────────────────────────────────────

    /// Reset blink to "on" and restart the blink loop. Call after every user interaction.
    fn reset_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_blink_on = true;
        self.cursor_blink_epoch += 1;
        let epoch = self.cursor_blink_epoch;
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(530)).await;
                let cont = view.update(cx, |this, cx| {
                    if this.cursor_blink_epoch != epoch { return false; }
                    this.cursor_blink_on = !this.cursor_blink_on;
                    cx.notify();
                    true
                }).unwrap_or(false);
                if !cont { break; }
            }
        }).detach();
    }

    fn clamp_sel(&mut self) {
        let len = self.doc.len_chars();
        self.sel.head = self.sel.head.min(len);
        self.sel.anchor = self.sel.anchor.min(len);
    }

    /// True when this is a markdown file small enough for non-virtualized word-wrap.
    fn is_wrap_mode(&self) -> bool {
        self.is_markdown() && self.doc.len_lines() <= WRAP_LINE_CAP
    }

    /// Scroll to `line` on whichever scroll handle is active.
    fn scroll_to_line(&self, line: usize) {
        if self.is_wrap_mode() {
            self.md_scroll.scroll_to_item(line);
        } else {
            self.scroll_handle.scroll_to_item(line, ScrollStrategy::Center);
        }
    }

    /// Top visible logical line, read from whichever scroll handle is active.
    fn top_visible_line(&self, t: &RuntimeTheme) -> usize {
        if self.is_wrap_mode() {
            self.md_scroll.top_item()
        } else {
            let off = self.scroll_handle.0.borrow().base_handle.offset();
            let y = f32::from(off.y);
            // offset.y is ≤ 0 when scrolled down; negate to get pixels scrolled
            (-y / t.line_height_code).floor().max(0.0) as usize
        }
    }

    /// Build a breadcrumb heading stack from the outline and the current top line.
    /// Returns only the `text` fields of headings whose source_line ≤ top_line,
    /// maintaining a stack that drops shallower entries when a deeper one arrives.
    fn breadcrumb_stack(outline: &[OutlineEntry], top_line: usize) -> Vec<String> {
        let mut stack: Vec<(u8, String)> = Vec::new();
        for e in outline.iter().take_while(|e| e.source_line <= top_line) {
            while stack.last().map_or(false, |(lvl, _)| *lvl >= e.level) {
                stack.pop();
            }
            stack.push((e.level, e.text.clone()));
        }
        stack.into_iter().map(|(_, t)| t).collect()
    }

    /// Post-mutation bookkeeping — every document edit funnels through here.
    fn after_edit(&mut self, cx: &mut Context<Self>) {
        self.update_matches();
        if self.is_markdown() {
            self.schedule_markdown_update(cx);
        }
        self.reset_blink(cx);
        cx.emit(EditorEvent::Edited);
        cx.notify();
    }

    /// Debounced (75 ms) background markdown parse → updates outline + preview.
    fn schedule_markdown_update(&mut self, cx: &mut Context<Self>) {
        self.outline_gen += 1;
        let current_gen = self.outline_gen;
        let rope = self.doc.rope.clone();
        let update_preview = self.show_preview && self.preview.is_some();
        cx.spawn(async move |view, cx| {
            cx.background_executor().timer(Duration::from_millis(75)).await;
            let source = rope.to_string();
            let registry = LanguageRegistry::with_defaults();
            let md = Arc::new(parse_markdown(&source, &rope, &registry));
            let outline = Arc::new(md.outline.clone());
            let _ = view.update(cx, |this, cx| {
                if this.outline_gen != current_gen { return; }
                this.outline = outline;
                if update_preview {
                    if let Some(ref preview) = this.preview {
                        let md2 = Arc::clone(&md);
                        preview.update(cx, |pv, _cx| pv.apply_md(md2));
                    }
                }
                cx.notify();
            });
        }).detach();
    }

    fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.history.commit();
        if !self.sel.is_empty() {
            let edit = self.doc.delete(self.sel.range());
            self.history.push_other(edit);
            let pos = self.sel.start();
            self.sel = Selection::collapsed(pos, &self.doc.rope);
        }
        let pos = self.sel.head;
        let edit = self.doc.insert(pos, text);
        let end = pos + text.chars().count();
        self.history.push_insert(edit, end);
        self.sel = Selection::collapsed(end, &self.doc.rope);
        self.after_edit(cx);
    }

    fn do_backspace(&mut self, cx: &mut Context<Self>) {
        self.history.commit();
        if !self.sel.is_empty() {
            let edit = self.doc.delete(self.sel.range());
            self.history.push_other(edit);
            let pos = self.sel.start();
            self.sel = Selection::collapsed(pos, &self.doc.rope);
        } else if self.sel.head > 0 {
            let pos = self.sel.head - 1;
            let edit = self.doc.delete(pos..self.sel.head);
            self.history.push_other(edit);
            self.sel = Selection::collapsed(pos, &self.doc.rope);
        }
        self.after_edit(cx);
    }

    fn do_delete_fwd(&mut self, cx: &mut Context<Self>) {
        self.history.commit();
        if !self.sel.is_empty() {
            let edit = self.doc.delete(self.sel.range());
            self.history.push_other(edit);
            let pos = self.sel.start();
            self.sel = Selection::collapsed(pos, &self.doc.rope);
        } else if self.sel.head < self.doc.len_chars() {
            let edit = self.doc.delete(self.sel.head..self.sel.head + 1);
            self.history.push_other(edit);
            self.sel = Selection::collapsed(self.sel.head, &self.doc.rope);
        }
        self.after_edit(cx);
    }

    fn do_delete_word_left(&mut self, cx: &mut Context<Self>) {
        self.history.commit();
        if !self.sel.is_empty() {
            let edit = self.doc.delete(self.sel.range());
            self.history.push_other(edit);
            let pos = self.sel.start();
            self.sel = Selection::collapsed(pos, &self.doc.rope);
        } else {
            let word_start = cursor::move_word_left(&self.doc.rope, self.sel, false).head;
            if word_start < self.sel.head {
                let edit = self.doc.delete(word_start..self.sel.head);
                self.history.push_other(edit);
                self.sel = Selection::collapsed(word_start, &self.doc.rope);
            }
        }
        self.after_edit(cx);
    }

    fn do_delete_word_right(&mut self, cx: &mut Context<Self>) {
        self.history.commit();
        if !self.sel.is_empty() {
            let edit = self.doc.delete(self.sel.range());
            self.history.push_other(edit);
            let pos = self.sel.start();
            self.sel = Selection::collapsed(pos, &self.doc.rope);
        } else {
            let word_end = cursor::move_word_right(&self.doc.rope, self.sel, false).head;
            if word_end > self.sel.head {
                let edit = self.doc.delete(self.sel.head..word_end);
                self.history.push_other(edit);
                self.sel = Selection::collapsed(self.sel.head, &self.doc.rope);
            }
        }
        self.after_edit(cx);
    }

    fn do_delete_to_line_start(&mut self, cx: &mut Context<Self>) {
        self.history.commit();
        let head = self.sel.head;
        let line_idx = self.doc.rope.char_to_line(head.min(self.doc.len_chars().saturating_sub(1)));
        let line_start = self.doc.rope.line_to_char(line_idx);
        if !self.sel.is_empty() {
            let edit = self.doc.delete(self.sel.range());
            self.history.push_other(edit);
            let pos = self.sel.start();
            self.sel = Selection::collapsed(pos, &self.doc.rope);
        } else if line_start < head {
            let edit = self.doc.delete(line_start..head);
            self.history.push_other(edit);
            self.sel = Selection::collapsed(line_start, &self.doc.rope);
        }
        self.after_edit(cx);
    }

    fn do_delete_to_line_end(&mut self, cx: &mut Context<Self>) {
        self.history.commit();
        if !self.sel.is_empty() {
            let edit = self.doc.delete(self.sel.range());
            self.history.push_other(edit);
            let pos = self.sel.start();
            self.sel = Selection::collapsed(pos, &self.doc.rope);
        } else {
            let head = self.sel.head;
            let len = self.doc.len_chars();
            let line_idx = self.doc.rope.char_to_line(head.min(len.saturating_sub(1)));
            let raw = self.doc.rope.line(line_idx).to_string();
            let content_chars = raw.trim_end_matches(['\n', '\r']).chars().count();
            let line_start = self.doc.rope.line_to_char(line_idx);
            let line_end = line_start + content_chars;
            if head < line_end {
                let edit = self.doc.delete(head..line_end);
                self.history.push_other(edit);
                self.sel = Selection::collapsed(head, &self.doc.rope);
            }
        }
        self.after_edit(cx);
    }

    fn do_delete_line(&mut self, cx: &mut Context<Self>) {
        self.history.commit();
        let len = self.doc.len_chars();
        if len == 0 { return; }
        let head = self.sel.head.min(len.saturating_sub(1));
        let line_idx = self.doc.rope.char_to_line(head);
        let line_start = self.doc.rope.line_to_char(line_idx);
        let line_count = self.doc.rope.len_lines();
        let (del_start, del_end, new_pos) = if line_idx + 1 < line_count {
            let next = self.doc.rope.line_to_char(line_idx + 1);
            (line_start, next, line_start)
        } else if line_start > 0 {
            (line_start - 1, len, line_start - 1)
        } else {
            (0, len, 0)
        };
        if del_start < del_end {
            let edit = self.doc.delete(del_start..del_end);
            self.history.push_other(edit);
            let clamped = new_pos.min(self.doc.len_chars());
            self.sel = Selection::collapsed(clamped, &self.doc.rope);
            self.after_edit(cx);
        }
    }

    pub fn rebuild_line_cache(&mut self) {
        let rope = &self.doc.rope;
        let line_count = rope.len_lines();
        self.line_cache.clear();
        self.line_cache.reserve(line_count);
        self.line_starts.clear();
        self.line_starts.reserve(line_count);
        let mut widest = 0usize;
        let mut widest_chars = 0usize;
        for i in 0..line_count {
            let char_start = rope.line_to_char(i);
            self.line_starts.push(char_start);
            let raw = rope.line(i).to_string();
            let content = raw.trim_end_matches(['\n', '\r']);
            let chars = content.chars().count();
            if chars > widest_chars {
                widest_chars = chars;
                widest = i;
            }
            self.line_cache.push(SharedString::from(content.to_string()));
        }
        self.widest_line = widest;
    }

    /// Map a window-space point to a rope char offset.
    /// Returns None if the scroll handle has not been painted yet (first frame).
    fn offset_at(
        &self,
        p: gpui::Point<gpui::Pixels>,
        t: &RuntimeTheme,
        show_line_numbers: bool,
    ) -> Option<usize> {
        let st = self.scroll_handle.0.borrow();
        let vb = st.base_handle.bounds();
        let off = st.base_handle.offset();
        drop(st);
        if vb.size.width == gpui::Pixels::ZERO {
            return None;
        }
        let gutter_px = if show_line_numbers {
            (GUTTER_COLS + 2.0) * t.char_w_code
        } else {
            0.0
        };
        let px_y = f32::from(p.y);
        let px_x = f32::from(p.x);
        let vb_y = f32::from(vb.origin.y);
        let vb_x = f32::from(vb.origin.x);
        let off_y = f32::from(off.y);
        let off_x = f32::from(off.x);
        // offset().y is ≤ 0 when scrolled down; subtracting it raises the coordinate.
        let rel_y = (px_y - vb_y) - off_y;
        let line = (rel_y / t.line_height_code).floor().max(0.0) as usize;
        let line = line.min(self.line_starts.len().saturating_sub(1));
        let rel_x = (px_x - vb_x) - 8.0 - gutter_px - off_x;
        // floor: clicking anywhere on a character selects it, not the next one
        let col_f = (rel_x / t.char_w_code).max(0.0).floor();
        let raw = self.doc.rope.line(line).to_string();
        let content_chars = raw.trim_end_matches(['\n', '\r']).chars().count();
        let col = (col_f as usize).min(content_chars);
        Some(self.line_starts[line] + col)
    }

    /// Char-offset from a click in the non-virtualized wrap path.
    /// Mirrors ScrollHandle::top_item logic: offset.y ≤ 0 when scrolled, so
    /// lookup_y = mouse_y - offset_y maps screen coords back to layout space.
    fn offset_at_wrap(
        &self,
        p: gpui::Point<gpui::Pixels>,
        t: &RuntimeTheme,
        show_line_numbers: bool,
    ) -> Option<usize> {
        let vb = self.md_scroll.bounds();
        if vb.size.width == gpui::Pixels::ZERO {
            return None;
        }
        let line_count = self.doc.len_lines();
        if line_count == 0 {
            return None;
        }

        // offset.y ≤ 0 when scrolled down.
        // lookup_y maps the mouse screen-y back to the original layout-space y,
        // matching how ScrollHandle::top_item computes: `top = bounds.top() - offset.y`.
        let offset_y = f32::from(self.md_scroll.offset().y);
        let lookup_y = f32::from(p.y) - offset_y;

        // Binary search over line bounds (bounds_for_item returns original layout coords).
        let mut lo = 0usize;
        let mut hi = line_count;
        let line = loop {
            if lo >= hi { break lo.min(line_count.saturating_sub(1)); }
            let mid = lo + (hi - lo) / 2;
            match self.md_scroll.bounds_for_item(mid) {
                None => break 0,
                Some(b) if lookup_y < f32::from(b.top()) => {
                    if mid == 0 { break 0; }
                    hi = mid;
                }
                Some(b) if lookup_y >= f32::from(b.bottom()) => lo = mid + 1,
                _ => break mid,
            }
        };

        let gutter_px = if show_line_numbers { (GUTTER_COLS + 2.0) * t.char_w_code } else { 0.0 };
        // 8.0 = px_2() left padding applied to each line row in wrap mode
        let rel_x = (f32::from(p.x) - f32::from(vb.origin.x) - 8.0 - gutter_px).max(0.0);
        let raw = self.doc.rope.line(line).to_string();
        let content_chars = raw.trim_end_matches(['\n', '\r']).chars().count();
        let col = ((rel_x / t.char_w_code).floor() as usize).min(content_chars);
        Some(self.line_starts[line] + col)
    }

    fn on_mouse_down_editor(
        &mut self,
        ev: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        let t = cx.global::<RuntimeTheme>().clone();
        let show_line_numbers = cx.global::<SettingsStore>().0.line_numbers;
        let Some(offset) = (if self.is_wrap_mode() {
            self.offset_at_wrap(ev.position, &t, show_line_numbers)
        } else {
            self.offset_at(ev.position, &t, show_line_numbers)
        }) else { return };
        if ev.modifiers.shift {
            let goal_col = cursor::col_of(&self.doc.rope, offset);
            self.sel = Selection {
                anchor: self.sel.anchor,
                head: offset,
                goal_col,
            };
        } else {
            self.sel = Selection::collapsed(offset, &self.doc.rope);
        }
        self.mouse_selecting = true;
        self.reset_blink(cx);
        // Suppress auto-recenter so clicking doesn't jump the view.
        self.last_scroll_line = self.doc.rope.char_to_line(
            offset.min(self.doc.len_chars().saturating_sub(1))
        );
        cx.notify();
    }

    fn update_matches(&mut self) {
        self.rebuild_line_cache();
        let q = Query::new(self.search_query.clone())
            .case_sensitive(self.search_case_sensitive)
            .whole_word(self.search_whole_word)
            .regex(self.search_regex);
        self.matches = if self.show_search || self.show_replace {
            q.all_matches(&self.doc.rope)
        } else {
            Vec::new()
        };
        if !self.matches.is_empty() {
            self.match_idx = self.match_idx.min(self.matches.len() - 1);
        } else {
            self.match_idx = 0;
        }
    }

    fn do_find_next(&mut self, cx: &mut Context<Self>) {
        if self.matches.is_empty() {
            return;
        }
        self.match_idx = (self.match_idx + 1) % self.matches.len();
        let m = self.matches[self.match_idx].clone();
        self.sel = Selection { anchor: m.start, head: m.end, goal_col: 0 };
        cx.notify();
    }

    fn do_find_prev(&mut self, cx: &mut Context<Self>) {
        if self.matches.is_empty() {
            return;
        }
        self.match_idx =
            if self.match_idx == 0 { self.matches.len() - 1 } else { self.match_idx - 1 };
        let m = self.matches[self.match_idx].clone();
        self.sel = Selection { anchor: m.start, head: m.end, goal_col: 0 };
        cx.notify();
    }

    fn do_replace_one(&mut self, cx: &mut Context<Self>) {
        if self.matches.is_empty() {
            return;
        }
        let m = self.matches[self.match_idx].clone();
        let replacement = self.replace_query.clone();
        let edit = self.doc.delete(m.clone());
        self.history.push_other(edit);
        let edit2 = self.doc.insert(m.start, &replacement);
        self.history.push_other(edit2);
        let new_pos = m.start + replacement.chars().count();
        self.sel = Selection::collapsed(new_pos, &self.doc.rope);
        self.after_edit(cx);
    }

    fn do_replace_all(&mut self, cx: &mut Context<Self>) {
        let replacement = self.replace_query.clone();
        let matches: Vec<_> = self.matches.clone();
        self.history.commit();
        for m in matches.iter().rev() {
            let edit = self.doc.delete(m.clone());
            self.history.push_other(edit);
            let edit2 = self.doc.insert(m.start, &replacement);
            self.history.push_other(edit2);
        }
        self.after_edit(cx);
    }

    // ── action handlers ────────────────────────────────────────────────────────

    fn on_move_left(&mut self, _: &MoveLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_left(&self.doc.rope, self.sel, false);
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_move_right(&mut self, _: &MoveRight, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_right(&self.doc.rope, self.sel, false);
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_move_up(&mut self, _: &MoveUp, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_up(&self.doc.rope, self.sel, false);
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_move_down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_down(&self.doc.rope, self.sel, false);
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_move_word_left(&mut self, _: &MoveWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_word_left(&self.doc.rope, self.sel, false);
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_move_word_right(&mut self, _: &MoveWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_word_right(&self.doc.rope, self.sel, false);
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_move_line_start(&mut self, _: &MoveLineStart, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_home(&self.doc.rope, self.sel, false);
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_move_line_end(&mut self, _: &MoveLineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_end(&self.doc.rope, self.sel, false);
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_move_doc_start(&mut self, _: &MoveDocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_doc_start(self.sel, false);
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_move_doc_end(&mut self, _: &MoveDocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_doc_end(&self.doc.rope, self.sel, false);
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_move_page_up(&mut self, _: &MovePageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_page_up(&self.doc.rope, self.sel, false, 30);
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_move_page_down(&mut self, _: &MovePageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_page_down(&self.doc.rope, self.sel, false, 30);
        self.reset_blink(cx);
        cx.notify();
    }

    fn on_select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.sel = cursor::move_left(&self.doc.rope, self.sel, true);
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.sel = cursor::move_right(&self.doc.rope, self.sel, true);
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.sel = cursor::move_up(&self.doc.rope, self.sel, true);
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.sel = cursor::move_down(&self.doc.rope, self.sel, true);
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.sel = cursor::move_word_left(&self.doc.rope, self.sel, true);
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_select_word_right(
        &mut self,
        _: &SelectWordRight,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sel = cursor::move_word_right(&self.doc.rope, self.sel, true);
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_select_line_start(
        &mut self,
        _: &SelectLineStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sel = cursor::move_home(&self.doc.rope, self.sel, true);
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_select_line_end(&mut self, _: &SelectLineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.sel = cursor::move_end(&self.doc.rope, self.sel, true);
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_select_doc_start(
        &mut self,
        _: &SelectDocStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sel = cursor::move_doc_start(self.sel, true);
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_select_doc_end(&mut self, _: &SelectDocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.sel = cursor::move_doc_end(&self.doc.rope, self.sel, true);
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.sel = cursor::select_all(&self.doc.rope);
        self.reset_blink(cx);
        cx.notify();
    }

    fn on_backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        self.do_backspace(cx);
    }
    fn on_delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        self.do_delete_fwd(cx);
    }
    fn on_delete_word_left(
        &mut self,
        _: &DeleteWordLeft,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.do_delete_word_left(cx);
    }
    fn on_delete_word_right(
        &mut self,
        _: &DeleteWordRight,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.do_delete_word_right(cx);
    }
    fn on_delete_to_line_start(
        &mut self,
        _: &DeleteToLineStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.do_delete_to_line_start(cx);
    }
    fn on_delete_to_line_end(
        &mut self,
        _: &DeleteToLineEnd,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.do_delete_to_line_end(cx);
    }
    fn on_delete_line(&mut self, _: &DeleteLine, _: &mut Window, cx: &mut Context<Self>) {
        self.do_delete_line(cx);
    }
    fn on_tab(&mut self, _: &Tab, _: &mut Window, cx: &mut Context<Self>) {
        self.insert_text("\t", cx);
    }
    fn on_enter(&mut self, _: &Enter, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_markdown() {
            let len = self.doc.len_chars();
            let head = self.sel.head.min(len.saturating_sub(1));
            let line_idx = self.doc.rope.char_to_line(head);
            let line_char_start = self.doc.rope.line_to_char(line_idx);
            let line_str: String = self.doc.rope.line(line_idx).to_string();
            let cursor_col = head - line_char_start;

            match enter_action(&line_str, cursor_col) {
                EnterAction::ContinueList { insert } => {
                    self.history.commit();
                    self.insert_text(&insert, cx);
                    self.history.commit();
                    return;
                }
                EnterAction::ExitList { delete_cols } => {
                    let char_start = line_char_start + line_str[..delete_cols.start].chars().count();
                    let char_end   = line_char_start + line_str[..delete_cols.end].chars().count();
                    self.history.commit();
                    let edit = self.doc.delete(char_start..char_end);
                    self.history.push_other(edit);
                    self.sel = Selection::collapsed(char_start, &self.doc.rope);
                    let edit = self.doc.insert(char_start, "\n");
                    let end = char_start + 1;
                    self.history.push_insert(edit, end);
                    self.sel = Selection::collapsed(end, &self.doc.rope);
                    self.after_edit(cx);
                    self.history.commit();
                    return;
                }
                EnterAction::Plain => {}
            }
        }
        self.history.commit();
        self.insert_text("\n", cx);
        self.history.commit();
    }

    fn on_copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.sel.is_empty() {
            let text: String = self.doc.rope.slice(self.sel.range()).to_string();
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }
    fn on_cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if !self.sel.is_empty() {
            let text: String = self.doc.rope.slice(self.sel.range()).to_string();
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.history.commit();
            let edit = self.doc.delete(self.sel.range());
            self.history.push_other(edit);
            let pos = self.sel.start();
            self.sel = Selection::collapsed(pos, &self.doc.rope);
            self.after_edit(cx);
        }
    }
    fn on_paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(item) = cx.read_from_clipboard() {
            if let Some(text) = item.text() {
                if self.is_markdown() && !self.sel.is_empty() && looks_like_url(&text) {
                    let sel_text: String = self.doc.rope.slice(self.sel.range()).to_string();
                    let linked = format!("[{sel_text}]({text})");
                    self.history.commit();
                    self.insert_text(&linked, cx);
                    return;
                }
                self.history.commit();
                self.insert_text(&text.clone(), cx);
            }
        }
    }

    fn on_bold_selection(&mut self, _: &BoldSelection, _: &mut Window, cx: &mut Context<Self>) {
        if self.sel.is_empty() { return; }
        let selected: String = self.doc.rope.slice(self.sel.range()).to_string();
        let wrapped = smart_wrap(&selected, "**");
        self.history.commit();
        self.insert_text(&wrapped, cx);
        self.history.commit();
    }

    fn on_italic_selection(&mut self, _: &ItalicSelection, _: &mut Window, cx: &mut Context<Self>) {
        if self.sel.is_empty() { return; }
        let selected: String = self.doc.rope.slice(self.sel.range()).to_string();
        let wrapped = smart_wrap(&selected, "*");
        self.history.commit();
        self.insert_text(&wrapped, cx);
        self.history.commit();
    }

    fn on_toggle_checkbox(&mut self, _: &ToggleCheckbox, _: &mut Window, cx: &mut Context<Self>) {
        let head = self.sel.head.min(self.doc.len_chars().saturating_sub(1));
        let line_idx = self.doc.rope.char_to_line(head);
        let line_char_start = self.doc.rope.line_to_char(line_idx);
        let line_str: String = self.doc.rope.line(line_idx).to_string();
        if let Some((byte_range, replacement)) = toggle_checkbox(&line_str) {
            let char_start = line_char_start + line_str[..byte_range.start].chars().count();
            let char_end   = line_char_start + line_str[..byte_range.end].chars().count();
            self.history.commit();
            let edit = self.doc.delete(char_start..char_end);
            self.history.push_other(edit);
            let edit = self.doc.insert(char_start, replacement);
            self.history.push_insert(edit, char_start + replacement.chars().count());
            self.sel = Selection::collapsed(self.sel.head, &self.doc.rope);
            self.after_edit(cx);
            self.history.commit();
        }
    }

    fn on_undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(pos) = self.history.undo(&mut self.doc) {
            self.clamp_sel();
            self.sel = Selection::collapsed(pos.min(self.doc.len_chars()), &self.doc.rope);
            self.after_edit(cx);
        }
    }
    fn on_redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(pos) = self.history.redo(&mut self.doc) {
            self.clamp_sel();
            self.sel = Selection::collapsed(pos.min(self.doc.len_chars()), &self.doc.rope);
            self.after_edit(cx);
        }
    }

    fn on_open_search(&mut self, _: &OpenSearch, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_search {
            self.on_close_search(&CloseSearch, window, cx);
            return;
        }
        self.show_search = true;
        self.show_replace = false;
        self.update_matches();
        window.focus(&self.search_handle);
        cx.notify();
    }
    fn on_open_replace(&mut self, _: &OpenReplace, window: &mut Window, cx: &mut Context<Self>) {
        self.show_search = true;
        self.show_replace = true;
        self.update_matches();
        window.focus(&self.search_handle);
        cx.notify();
    }
    fn on_close_search(&mut self, _: &CloseSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.show_search = false;
        self.show_replace = false;
        self.search_query.clear();
        self.replace_query.clear();
        self.search_cursor = 0;
        self.replace_cursor = 0;
        self.search_case_sensitive = false;
        self.search_whole_word = false;
        self.search_regex = false;
        self.matches.clear();
        self.match_idx = 0;
        window.focus(&self.focus_handle);
        cx.notify();
    }
    fn on_find_next(&mut self, _: &FindNext, _: &mut Window, cx: &mut Context<Self>) {
        self.do_find_next(cx);
    }
    fn on_find_prev(&mut self, _: &FindPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.do_find_prev(cx);
    }
    fn on_replace_one(&mut self, _: &ReplaceOne, _: &mut Window, cx: &mut Context<Self>) {
        self.do_replace_one(cx);
    }
    fn on_replace_all(&mut self, _: &ReplaceAll, _: &mut Window, cx: &mut Context<Self>) {
        self.do_replace_all(cx);
    }
    fn on_search_backspace(
        &mut self,
        _: &SearchBackspace,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.search_cursor > 0 {
            self.search_query = delete_char_before(&self.search_query, self.search_cursor);
            self.search_cursor -= 1;
            self.update_matches();
            cx.notify();
        }
    }
    fn on_replace_backspace(
        &mut self,
        _: &ReplaceBackspace,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.replace_cursor > 0 {
            self.replace_query = delete_char_before(&self.replace_query, self.replace_cursor);
            self.replace_cursor -= 1;
            cx.notify();
        }
    }
    fn on_input_move_left(&mut self, _: &InputMoveLeft, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_handle.is_focused(window) {
            self.search_cursor = self.search_cursor.saturating_sub(1);
        } else if self.replace_handle.is_focused(window) {
            self.replace_cursor = self.replace_cursor.saturating_sub(1);
        }
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_input_move_right(&mut self, _: &InputMoveRight, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_handle.is_focused(window) {
            self.search_cursor = (self.search_cursor + 1).min(self.search_query.chars().count());
        } else if self.replace_handle.is_focused(window) {
            self.replace_cursor = (self.replace_cursor + 1).min(self.replace_query.chars().count());
        }
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_input_move_start(&mut self, _: &InputMoveStart, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_handle.is_focused(window) {
            self.search_cursor = 0;
        } else if self.replace_handle.is_focused(window) {
            self.replace_cursor = 0;
        }
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_input_move_end(&mut self, _: &InputMoveEnd, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_handle.is_focused(window) {
            self.search_cursor = self.search_query.chars().count();
        } else if self.replace_handle.is_focused(window) {
            self.replace_cursor = self.replace_query.chars().count();
        }
        self.reset_blink(cx);
        cx.notify();
    }

    fn on_toggle_search_case(
        &mut self,
        _: &ToggleSearchCase,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search_case_sensitive = !self.search_case_sensitive;
        self.update_matches();
        cx.notify();
    }
    fn on_toggle_search_whole_word(
        &mut self,
        _: &ToggleSearchWholeWord,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search_whole_word = !self.search_whole_word;
        self.update_matches();
        cx.notify();
    }
    fn on_toggle_search_regex(
        &mut self,
        _: &ToggleSearchRegex,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search_regex = !self.search_regex;
        self.update_matches();
        cx.notify();
    }
    fn on_toggle_replace(
        &mut self,
        _: &ToggleReplace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_replace = !self.show_replace;
        if self.show_replace {
            window.focus(&self.replace_handle);
        } else {
            window.focus(&self.search_handle);
        }
        cx.notify();
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        // macOS routes cmd+backspace/delete through NSTextInputClient (doCommandBySelector:)
        // before GPUI keybinding dispatch fires. Handle here to guarantee execution.
        if ks.modifiers.platform && !ks.modifiers.control && !ks.modifiers.alt {
            if ks.key.as_str() == "backspace" && !ks.modifiers.shift {
                if self.outline_open && self.outline_handle.is_focused(window) {
                    self.outline_query.clear();
                    self.outline_cursor = 0;
                    cx.notify();
                    return;
                }
                if self.show_search && self.search_handle.is_focused(window) {
                    self.search_query.clear();
                    self.search_cursor = 0;
                    self.update_matches();
                    self.reset_blink(cx);
                    cx.notify();
                    return;
                }
                if self.show_replace && self.replace_handle.is_focused(window) {
                    self.replace_query.clear();
                    self.replace_cursor = 0;
                    self.reset_blink(cx);
                    cx.notify();
                    return;
                }
            }
            match (ks.key.as_str(), ks.modifiers.shift) {
                ("backspace", false) => { self.do_delete_to_line_start(cx); return; }
                ("delete", false)    => { self.do_delete_to_line_end(cx); return; }
                ("k", true)          => { self.do_delete_line(cx); return; }
                _ => {}
            }
        }
        // Outline overlay key handling — escape first (regardless of which child has focus)
        if self.outline_open && ks.key_char.is_none() && ks.key.as_str() == "escape" {
            self.outline_open = false;
            self.outline_highlight = None;
            self.outline_hover = None;
            window.focus(&self.focus_handle);
            cx.notify();
            return;
        }
        if self.outline_open && self.outline_handle.is_focused(window) {
            let Some(ref raw_text) = ks.key_char else {
                match ks.key.as_str() {
                    "escape" => { /* handled above */ }
                    "backspace" => {
                        if self.outline_cursor > 0 {
                            self.outline_query = delete_char_before(&self.outline_query, self.outline_cursor);
                            self.outline_cursor = self.outline_cursor.saturating_sub(1);
                            self.outline_hover = None;
                            cx.notify();
                        }
                    }
                    "left" => {
                        if self.outline_cursor > 0 {
                            self.outline_cursor -= 1;
                            cx.notify();
                        }
                    }
                    "right" => {
                        let max = self.outline_query.chars().count();
                        if self.outline_cursor < max {
                            self.outline_cursor += 1;
                            cx.notify();
                        }
                    }
                    "enter" => {
                        let q = self.outline_query.to_lowercase();
                        let first_line = self.outline.iter().find(|e| {
                            q.is_empty() || e.text.to_lowercase().contains(&q)
                        }).map(|e| e.source_line);
                        if let Some(line) = first_line {
                            self.scroll_to_line(line);
                        }
                        self.outline_open = false;
                        self.outline_highlight = None;
                        self.outline_hover = None;
                        window.focus(&self.focus_handle);
                        cx.notify();
                    }
                    _ => {}
                }
                return;
            };
            if ks.modifiers.control || ks.modifiers.platform { return; }
            if raw_text.chars().any(|c| c.is_control()) { return; }
            self.outline_query = insert_at(&self.outline_query, self.outline_cursor, raw_text);
            self.outline_cursor += raw_text.chars().count();
            self.outline_hover = None;
            cx.notify();
            return;
        }

        let Some(ref raw_text) = ks.key_char else { return };
        if ks.modifiers.control || ks.modifiers.platform {
            return;
        }
        if raw_text.chars().any(|c| c.is_control()) {
            return;
        }
        // GPUI's mac key_char ignores caps lock; apply it manually.
        let text_buf;
        let text: &str = if window.capslock().on {
            text_buf = raw_text
                .chars()
                .map(|c| if c.is_ascii_alphabetic() { if c.is_ascii_uppercase() { c.to_ascii_lowercase() } else { c.to_ascii_uppercase() } } else { c })
                .collect::<String>();
            &text_buf
        } else {
            raw_text.as_str()
        };
        if self.show_replace && self.replace_handle.is_focused(window) {
            self.replace_query = insert_at(&self.replace_query, self.replace_cursor, text);
            self.replace_cursor += text.chars().count();
            self.reset_blink(cx);
            cx.notify();
        } else if self.show_search && self.search_handle.is_focused(window) {
            self.search_query = insert_at(&self.search_query, self.search_cursor, text);
            self.search_cursor += text.chars().count();
            self.update_matches();
            self.reset_blink(cx);
            cx.notify();
        } else {
            self.insert_text(text, cx);
        }
    }

    // ── rendering helpers ──────────────────────────────────────────────────────

    /// Render one line.
    /// - `cursor_visible`: whether the cursor beam is painted.
    /// - `wrap`: use flex-wrap and min-h instead of fixed row height (for markdown wrap mode).
    /// - `outline_hl`: if this line falls within the range, apply a subtle section highlight.
    fn render_line(
        &self,
        line_idx: usize,
        t: &RuntimeTheme,
        show_line_numbers: bool,
        cursor_visible: bool,
        wrap: bool,
        outline_hl: Option<(usize, usize)>,
    ) -> AnyElement {
        let line_char_start = self.line_starts[line_idx];
        let next_line_start = self
            .line_starts
            .get(line_idx + 1)
            .copied()
            .unwrap_or_else(|| self.doc.len_chars() + 1);

        let head = self.sel.head;
        let cursor_on_line = head >= line_char_start && head < next_line_start;
        let has_sel = !self.sel.is_empty()
            && self.sel.start() < next_line_start
            && self.sel.end() > line_char_start;
        let has_match = self
            .matches
            .iter()
            .any(|m| m.start < next_line_start && m.end > line_char_start);

        let raw_spans: &[HighlightSpan] = self
            .doc
            .highlight_cache
            .lines
            .get(line_idx)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let has_spans = !raw_spans.is_empty();

        let hl_this_line = outline_hl.map_or(false, |(s, e)| line_idx >= s && line_idx < e);

        // ── Fast path: no decoration at all ───────────────────────────────────
        if !cursor_on_line && !has_sel && !has_match && !has_spans {
            let row = div()
                .flex()
                .flex_row()
                .when(!wrap, |r| r.h(px(t.line_height_code)))
                .when(wrap, |r| r.w_full().px_2().min_h(px(t.line_height_code)).flex_wrap())
                .when(hl_this_line, |r| r.bg(t.line_highlight))
                .font_family(t.mono_family.clone())
                .text_size(px(t.font_size_code))
                .text_color(t.text);
            let row = if show_line_numbers {
                row.child(
                    div()
                        .flex_shrink_0()
                        .w(px(GUTTER_COLS * t.char_w_code))
                        .text_size(px(t.font_size_gutter))
                        .text_color(t.gutter)
                        .child(format!("{:>4}", line_idx + 1)),
                )
                .child(div().flex_shrink_0().w(px(2.0 * t.char_w_code)))
            } else {
                row
            };
            return row.child(self.line_cache[line_idx].clone()).into_any_element();
        }

        // ── Slow path ─────────────────────────────────────────────────────────
        let rope = &self.doc.rope;
        let raw = rope.line(line_idx).to_string();
        let line_str: &str = raw.trim_end_matches(['\n', '\r']);
        let line_chars: Vec<char> = line_str.chars().collect();
        let line_char_count = line_chars.len();
        let line_char_end = line_char_start + line_char_count;

        // Convert syntax spans from byte-col to char-col (ASCII fast path inside).
        let hl_spans: Vec<(usize, usize, SyntaxToken)> = raw_spans
            .iter()
            .map(|s| {
                let sc = byte_col_to_char_col(line_str, s.start_byte_col).min(line_char_count);
                let ec = byte_col_to_char_col(line_str, s.end_byte_col).min(line_char_count);
                (sc, ec, s.token)
            })
            .filter(|(s, e, _)| s < e)
            .collect();

        let cursor_col = if cursor_on_line { head - line_char_start } else { 0 };

        let sel_start = self.sel.start();
        let sel_end = self.sel.end();
        let line_sel_start =
            if sel_start > line_char_start { sel_start - line_char_start } else { 0 };
        let line_sel_end =
            if sel_end < line_char_end { sel_end - line_char_start } else { line_char_count };

        let match_ranges_on_line: Vec<(usize, usize, bool)> = self
            .matches
            .iter()
            .enumerate()
            .filter(|(_, m)| m.start <= line_char_end && m.end >= line_char_start)
            .map(|(i, m)| {
                let s = if m.start > line_char_start { m.start - line_char_start } else { 0 };
                let e =
                    if m.end < line_char_end { m.end - line_char_start } else { line_char_count };
                (s, e, i == self.match_idx)
            })
            .collect();

        let mut content_children: Vec<AnyElement> = Vec::new();
        let mut breakpoints: Vec<usize> = vec![0, line_char_count];
        if has_sel {
            breakpoints.push(line_sel_start);
            breakpoints.push(line_sel_end);
        }
        for (s, e, _) in &match_ranges_on_line {
            breakpoints.push(*s);
            breakpoints.push(*e);
        }
        if cursor_on_line {
            breakpoints.push(cursor_col);
        }
        for (s, e, _) in &hl_spans {
            breakpoints.push(*s);
            breakpoints.push(*e);
        }
        breakpoints.sort_unstable();
        breakpoints.dedup();

        for i in 0..breakpoints.len().saturating_sub(1) {
            let seg_start = breakpoints[i];
            let seg_end = breakpoints[i + 1];
            if seg_start >= seg_end {
                continue;
            }
            if cursor_on_line && cursor_col == seg_start {
                let cursor_color = if cursor_visible { t.cursor } else { gpui::hsla(0., 0., 0., 0.) };
                content_children.push(
                    div()
                        .flex_shrink_0()
                        .w(px(1.5))
                        .h(px(t.line_height_code))
                        .bg(cursor_color)
                        .into_any(),
                );
            }
            let in_sel = has_sel && seg_start >= line_sel_start && seg_end <= line_sel_end;
            let match_color = match_ranges_on_line
                .iter()
                .find(|(s, e, _)| seg_start >= *s && seg_end <= *e)
                .map(|(_, _, active)| if *active { t.match_active } else { t.match_bg });
            let bg_color = match_color.or(if in_sel { Some(t.selection) } else { None });

            // Syntax color: find the innermost span covering this segment.
            let fg_color = hl_spans
                .iter()
                .filter(|(s, e, _)| seg_start >= *s && seg_end <= *e)
                .last()
                .map(|(_, _, tok)| Self::token_color(*tok, t))
                .unwrap_or(t.text);

            let seg_text: String = line_chars[seg_start..seg_end].iter().collect();
            if wrap {
                // Split at whitespace boundaries so flex_wrap can break between words.
                for word in split_words_for_wrap(&seg_text) {
                    let d = div().text_color(fg_color).child(word);
                    let d = if let Some(color) = bg_color { d.bg(color) } else { d };
                    content_children.push(d.into_any());
                }
            } else {
                let seg_div = div().flex_shrink_0().text_color(fg_color).child(seg_text);
                let seg_div = if let Some(color) = bg_color { seg_div.bg(color) } else { seg_div };
                content_children.push(seg_div.into_any());
            }
        }
        if cursor_on_line && cursor_col == line_char_count {
            let cursor_color = if cursor_visible { t.cursor } else { gpui::hsla(0., 0., 0., 0.) };
            content_children.push(
                div().flex_shrink_0().w(px(1.5)).h(px(t.line_height_code)).bg(cursor_color).into_any(),
            );
        }

        let content = div()
            .flex()
            .flex_row()
            .when(wrap, |d| d.flex_wrap())
            .flex_1()
            .children(content_children);
        let row = div()
            .flex()
            .flex_row()
            .when(!wrap, |r| r.h(px(t.line_height_code)))
            .when(wrap, |r| r.w_full().px_2().min_h(px(t.line_height_code)).flex_wrap())
            .when(hl_this_line, |r| r.bg(t.line_highlight))
            .font_family(t.mono_family.clone())
            .text_size(px(t.font_size_code));
        let row = if show_line_numbers {
            row.child(
                div()
                    .flex_shrink_0()
                    .w(px(GUTTER_COLS * t.char_w_code))
                    .text_size(px(t.font_size_gutter))
                    .text_color(t.gutter)
                    .child(format!("{:>4}", line_idx + 1)),
            )
            .child(div().flex_shrink_0().w(px(2.0 * t.char_w_code)))
        } else {
            row
        };
        row.child(content).into_any_element()
    }

    fn token_color(token: SyntaxToken, t: &RuntimeTheme) -> gpui::Hsla {
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

    fn render_search_bar(
        &self,
        window: &Window,
        t: &RuntimeTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // ── split query text at cursor for rendering ──
        let (s_before, s_after) = split_at_char(&self.search_query, self.search_cursor);
        let (r_before, r_after) = split_at_char(&self.replace_query, self.replace_cursor);
        let match_info = if !self.search_query.is_empty() && !self.matches.is_empty() {
            format!("{}/{}", self.match_idx + 1, self.matches.len())
        } else if !self.search_query.is_empty() {
            t!("search.no_matches").to_string()
        } else {
            String::new()
        };

        let search_focused = self.search_handle.is_focused(window);
        let replace_focused = self.replace_handle.is_focused(window);
        let caret_visible = self.cursor_blink_on;
        let show_replace = self.show_replace;

        // Theme values captured by value for closures.
        let hover_bg = t.line_highlight;
        let sep_color = t.separator;
        let radius = t.radius_sm;
        let radius_md = t.radius_md;

        // Cursor color: visible or transparent (no layout shift, same as editor cursor).
        let search_cursor_color = if search_focused && caret_visible { t.cursor } else { gpui::hsla(0., 0., 0., 0.) };
        let replace_cursor_color = if replace_focused && caret_visible { t.cursor } else { gpui::hsla(0., 0., 0., 0.) };
        let cursor_h = t.font_size_code + 2.;

        // ── icon button helper ─────────────────────────────────────────────────
        let icon_btn_base = move |id: &'static str| {
            div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .size(px(24.))
                .rounded(px(radius))
                .cursor_pointer()
                .flex_shrink_0()
                .hover(move |s| s.bg(hover_bg))
        };

        // ── vertical separator ─────────────────────────────────────────────────
        let vsep = move || div().w(px(1.)).h(px(14.)).bg(sep_color).flex_shrink_0();

        // ── toggle chip helper ─────────────────────────────────────────────────
        let chip = move |id: &'static str, label: &'static str, active: bool, t: &RuntimeTheme| {
            div()
                .id(id)
                .px_2()
                .rounded(px(radius))
                .cursor_pointer()
                .flex_shrink_0()
                .text_size(px(t.font_size_code - 1.))
                .font_family(t.mono_family.clone())
                .text_color(if active { t.text_on_accent } else { t.text_subtle })
                .when(active, move |el| el.bg(t.accent).hover(move |s| s.bg(t.accent_hover)))
                .when(!active, move |el| el.hover(move |s| s.bg(hover_bg)))
                .child(label)
        };

        // ── replace-toggle (leftmost, Add = show replace, Remove = hide) ───────
        let toggle_icon = if show_replace { IconName::Remove } else { IconName::Add };
        let toggle_color = if show_replace { t.accent } else { t.text_subtle };
        let replace_toggle = icon_btn_base("toggle-replace")
            .when(show_replace, |el| el.bg(t.line_highlight))
            .child(svg().path(toggle_icon.path()).size(px(14.)).text_color(toggle_color))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, _, window, cx| v.on_toggle_replace(&ToggleReplace, window, cx)),
            );

        // ── navigation group: [◄ prev] [count] [► next] ───────────────────────
        let prev_btn = icon_btn_base("search-prev")
            .child(svg().path(IconName::ChevronLeft.path()).size(px(14.)).text_color(t.text_subtle))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, _, window, cx| v.on_find_prev(&FindPrev, window, cx)),
            );

        let next_btn = icon_btn_base("search-next")
            .child(svg().path(IconName::ChevronRight.path()).size(px(14.)).text_color(t.text_subtle))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, _, window, cx| v.on_find_next(&FindNext, window, cx)),
            );

        let nav_group = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .child(prev_btn)
            .child(
                div()
                    .min_w(px(48.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(t.gutter)
                    .text_size(px(t.font_size_caption))
                    .child(match_info),
            )
            .child(next_btn);

        // ── filter chips ───────────────────────────────────────────────────────
        let case_chip = chip("toggle-case", "Aa", self.search_case_sensitive, t)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, _, window, cx| {
                    v.on_toggle_search_case(&ToggleSearchCase, window, cx)
                }),
            );

        let word_chip = chip("toggle-word", "W", self.search_whole_word, t)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, _, window, cx| {
                    v.on_toggle_search_whole_word(&ToggleSearchWholeWord, window, cx)
                }),
            );

        let regex_chip = chip("toggle-regex", ".*", self.search_regex, t)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, _, window, cx| {
                    v.on_toggle_search_regex(&ToggleSearchRegex, window, cx)
                }),
            );

        // ── close button (rightmost) ───────────────────────────────────────────
        let close_btn = icon_btn_base("search-close")
            .child(svg().path(IconName::Close.path()).size(px(13.)).text_color(t.text_subtle))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, _, window, cx| v.on_close_search(&CloseSearch, window, cx)),
            );

        // ── shared input style ─────────────────────────────────────────────────
        // Both inputs use the same height/padding/font so they're identical in size.
        let input_style = move |focused: bool, t: &RuntimeTheme| {
            div()
                .flex_1()
                .min_w(px(80.))
                .h(px(24.))
                .bg(if focused { t.line_highlight } else { t.bg_sunken })
                .px_2()
                .flex()
                .items_center()
                .rounded(px(radius))
                .font_family(t.mono_family.clone())
                .text_size(px(t.font_size_code))
        };

        // ── replace row buttons ────────────────────────────────────────────────
        let replace_one_btn = div()
            .id("replace-one")
            .px_3()
            .h(px(24.))
            .flex()
            .items_center()
            .rounded(px(radius_md))
            .bg(t.bg_overlay)
            .text_color(t.text)
            .text_size(px(t.font_size_body))
            .font_family(t.ui_family.clone())
            .cursor_pointer()
            .flex_shrink_0()
            .hover(move |s| s.bg(hover_bg))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, _, window, cx| v.on_replace_one(&ReplaceOne, window, cx)),
            )
            .child(t!("search.replace").to_string());

        let replace_all_btn = div()
            .id("replace-all")
            .px_3()
            .h(px(24.))
            .flex()
            .items_center()
            .rounded(px(radius_md))
            .bg(t.bg_overlay)
            .text_color(t.text)
            .text_size(px(t.font_size_body))
            .font_family(t.ui_family.clone())
            .cursor_pointer()
            .flex_shrink_0()
            .hover(move |s| s.bg(hover_bg))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, _, window, cx| v.on_replace_all(&ReplaceAll, window, cx)),
            )
            .child(t!("search.replace_all").to_string());

        // ── right-side wrappers (same fixed width → inputs are identical size) ──
        // Search: [|] [◄][count][►] [|] [Aa][W][.*] [|] [✕]
        let search_right = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .w(px(240.))
            .flex_shrink_0()
            .child(vsep())
            .child(nav_group)
            .child(vsep())
            .child(case_chip)
            .child(word_chip)
            .child(regex_chip)
            .child(vsep())
            .child(close_btn);

        // Replace: [|] [spacer fills left] [Replace] [Replace All]
        let replace_right = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .w(px(240.))
            .flex_shrink_0()
            .child(vsep())
            .child(div().flex_1())
            .child(replace_one_btn)
            .child(replace_all_btn);

        // ── search row ─────────────────────────────────────────────────────────
        // Layout: [toggle] [|] [search-input flex_1] [right-240px]
        let search_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .child(replace_toggle)
            .child(vsep())
            .child(
                input_style(search_focused, t)
                    .on_mouse_down(MouseButton::Left, cx.listener(|v, _, window, cx| {
                        window.focus(&v.search_handle);
                        v.search_cursor = v.search_query.chars().count();
                        v.reset_blink(cx);
                    }))
                    .child(if !search_focused && self.search_query.is_empty() {
                        div().text_color(t.text_subtle).child(t!("search.placeholder").to_string()).into_any()
                    } else {
                        div().flex().flex_row().items_center()
                            .child(div().text_color(t.text).child(SharedString::from(s_before)))
                            .child(div().w(px(1.5)).h(px(cursor_h)).flex_shrink_0().bg(search_cursor_color))
                            .child(div().text_color(t.text).child(SharedString::from(s_after)))
                            .into_any()
                    }),
            )
            .child(search_right);

        // ── replace row (left spacer matches toggle width → inputs left-aligned) ─
        let replace_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_t_1()
            .border_color(t.separator)
            .child(div().w(px(24.)).flex_shrink_0())
            .child(vsep())
            .child(
                input_style(replace_focused, t)
                    .on_mouse_down(MouseButton::Left, cx.listener(|v, _, window, cx| {
                        window.focus(&v.replace_handle);
                        v.replace_cursor = v.replace_query.chars().count();
                        v.reset_blink(cx);
                    }))
                    .child(if !replace_focused && self.replace_query.is_empty() {
                        div().text_color(t.text_subtle).child(t!("search.replace_placeholder").to_string()).into_any()
                    } else {
                        div().flex().flex_row().items_center()
                            .child(div().text_color(t.text).child(SharedString::from(r_before)))
                            .child(div().w(px(1.5)).h(px(cursor_h)).flex_shrink_0().bg(replace_cursor_color))
                            .child(div().text_color(t.text).child(SharedString::from(r_after)))
                            .into_any()
                    }),
            )
            .child(replace_right);

        div()
            .border_b_1()
            .border_color(t.separator)
            .bg(t.bg)
            .key_context("SearchBar")
            .track_focus(&self.search_handle)
            .child(search_row)
            .when(show_replace, |el| {
                el.child(
                    div()
                        .key_context("ReplaceBar")
                        .track_focus(&self.replace_handle)
                        .child(replace_row),
                )
            })
    }

    // ── markdown preview ───────────────────────────────────────────────────────

    pub fn is_markdown(&self) -> bool {
        self.doc.language.as_ref().map_or(false, |l| l.id.0 == "markdown")
    }

    fn on_toggle_preview(&mut self, _: &TogglePreview, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_markdown() { return; }
        let entering_preview = !self.show_preview;
        self.show_preview = entering_preview;

        if entering_preview {
            let source_line = self.doc.rope.char_to_line(
                self.sel.head.min(self.doc.len_chars().saturating_sub(1))
            );
            let registry = Arc::new(LanguageRegistry::with_defaults());
            if self.preview.is_none() {
                let rope = self.doc.rope.clone();
                let path = self.doc.path.clone();
                let reg = Arc::clone(&registry);
                let preview = cx.new(|_cx| MarkdownPreviewView::new(&rope, &path, &reg));
                preview.read(cx).scroll_to_source_line(source_line);
                self.preview = Some(preview);
            } else if let Some(ref preview) = self.preview {
                let rope = self.doc.rope.clone();
                preview.update(cx, |pv, _cx| {
                    pv.reparse_now(&rope, &registry);
                    pv.scroll_to_source_line(source_line);
                });
            }
        } else {
            // Leaving preview: sync scroll back to source.
            let source_line = if let Some(ref preview) = self.preview {
                preview.read(cx).source_line_at_top()
            } else {
                0
            };
            self.scroll_handle.scroll_to_item(source_line, ScrollStrategy::Top);
        }
        cx.notify();
    }
}

// ── GPUI impls ─────────────────────────────────────────────────────────────────

impl EventEmitter<EditorEvent> for EditorView {}

impl Focusable for EditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<RuntimeTheme>().clone();

        let is_md = self.is_markdown();
        let show_preview = self.show_preview;
        let cursor_visible = self.cursor_blink_on && self.focus_handle.is_focused(window);

        // Preview toggle button (only shown for markdown files)
        let toggle_btn: AnyElement = if is_md {
            let icon = if show_preview { IconName::Code } else { IconName::Visibility };
            let color = if show_preview { t.accent } else { t.text_subtle };
            div()
                .id("preview-toggle")
                .flex()
                .items_center()
                .px_2()
                .py_1()
                .rounded(px(t.radius_sm))
                .cursor_pointer()
                .hover(|s| s.bg(t.line_highlight))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|v, _, window, cx| v.on_toggle_preview(&TogglePreview, window, cx)),
                )
                .child(gpui::svg().path(icon.path()).size(px(14.)).text_color(color))
                .into_any_element()
        } else {
            div().into_any_element()
        };

        let root_folder = cx.try_global::<ProjectRoot>().and_then(|r| r.0.clone());
        let path_label: String = if self.doc.is_untitled() {
            t!("editor.untitled").to_string()
        } else {
            let path = &self.doc.path;
            if let Some(root) = root_folder.as_ref() {
                path.strip_prefix(root)
                    .map(|rel| rel.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| {
                        path.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    })
            } else {
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned())
            }
        };

        // Breadcrumb: compute the heading stack at the current top visible line.
        let top_line = self.top_visible_line(&t);
        let crumb_stack = if is_md && !self.outline.is_empty() {
            Self::breadcrumb_stack(&self.outline, top_line)
        } else {
            vec![]
        };

        // Build the full breadcrumb label: "path/to/file.md › H1 › H2"
        let breadcrumb_text = if crumb_stack.is_empty() {
            path_label.clone()
        } else {
            format!("{} › {}", path_label, crumb_stack.join(" › "))
        };

        let can_open_outline = is_md && !self.outline.is_empty();
        let crumb_color = if can_open_outline { t.text_subtle } else { t.text_subtle };
        let crumb_hover_bg = t.line_highlight;
        let outline_active = self.outline_open;

        let breadcrumb_label = div()
            .id("editor-breadcrumb")
            .flex()
            .flex_row()
            .items_center()
            .min_w(px(0.))
            .overflow_hidden()
            .px_1()
            .py(px(1.))
            .rounded(px(t.radius_sm))
            .font_family(t.ui_family.clone())
            .text_size(px(t.font_size_caption))
            .text_color(crumb_color)
            .when(outline_active, |el| el.bg(crumb_hover_bg))
            .when(can_open_outline, |el| {
                el.cursor_pointer().hover(move |s| s.bg(crumb_hover_bg))
            })
            .when(can_open_outline, |el| {
                el.on_mouse_down(MouseButton::Left, cx.listener(|v, _, window, cx| {
                    v.outline_open = !v.outline_open;
                    v.outline_query.clear();
                    v.outline_cursor = 0;
                    v.outline_hover = None;
                    if v.outline_open {
                        window.focus(&v.outline_handle);
                    } else {
                        window.focus(&v.focus_handle);
                    }
                    cx.notify();
                }))
            })
            .child(
                div()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .pr_2()
                    .child(breadcrumb_text),
            );

        let search_icon_color = if self.show_search { t.accent } else { t.text_subtle };
        let search_btn = div()
            .id("header-search")
            .flex()
            .items_center()
            .px_2()
            .py_1()
            .rounded(px(t.radius_sm))
            .cursor_pointer()
            .hover(|s| s.bg(t.line_highlight))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, _, window, cx| v.on_open_search(&OpenSearch, window, cx)),
            )
            .child(svg().path(IconName::Search.path()).size(px(14.)).text_color(search_icon_color));

        let header = div()
            .flex()
            .flex_row()
            .justify_between()
            .items_center()
            .px_3()
            .py(px(3.))
            .bg(t.bg_elevated)
            .border_b_1()
            .border_color(t.separator)
            .child(breadcrumb_label)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(search_btn)
                    .child(toggle_btn),
            );

        let line_count = self.doc.len_lines();
        let cursor_line = self.doc.rope.char_to_line(self.sel.head.min(self.doc.len_chars().saturating_sub(1)));
        if cursor_line != self.last_scroll_line {
            self.last_scroll_line = cursor_line;
            self.scroll_to_line(cursor_line);
        }

        let settings = &cx.global::<SettingsStore>().0;
        let show_line_numbers = settings.line_numbers;
        let show_scrollbar = settings.show_scrollbar;

        let is_dragging = self.scrollbar_drag.is_some();
        let outline_hl = self.outline_highlight;
        let use_wrap = self.is_wrap_mode();

        let editor_pane = if use_wrap {
            // ── Non-virtualized word-wrap path (markdown, ≤WRAP_LINE_CAP lines) ──
            // Lines are direct children of the scroll div so top_item() maps 1:1 to logical lines.
            let md_scroll = self.md_scroll.clone();
            let md_scroll_ref = self.md_scroll.clone();
            let t_wrap = t.clone();
            let all_lines: Vec<AnyElement> = (0..line_count)
                .map(|i| self.render_line(i, &t_wrap, show_line_numbers, cursor_visible, true, outline_hl))
                .collect();
            let wrapped = div()
                .id("md-wrap-scroll")
                .flex_1()
                .flex_col()
                .min_h(px(0.))
                .overflow_y_scroll()
                .bg(t.bg)
                .track_scroll(&md_scroll)
                .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down_editor))
                .on_scroll_wheel(cx.listener(|view, _ev: &ScrollWheelEvent, _, cx| {
                    let new_top = view.md_scroll.top_item();
                    if new_top != view.last_top_line {
                        view.last_top_line = new_top;
                        cx.notify();
                    }
                }))
                .children(all_lines);
            let md_scrollbar = render_scrollbar(
                "md-scrollbar",
                "md-scrollbar-thumb",
                &md_scroll_ref,
                show_scrollbar,
                is_dragging,
                cx.listener(|view, ev: &MouseDownEvent, _, cx| {
                    view.scrollbar_drag = Some(start_drag(ev, &view.md_scroll.clone()));
                    cx.notify();
                }),
                &t,
            );
            div()
                .flex()
                .flex_row()
                .flex_1()
                .min_w(px(0.))
                .min_h(px(0.))
                .child(div().flex().flex_col().flex_1().min_w(px(0.)).min_h(px(0.)).child(wrapped))
                .child(md_scrollbar)
        } else {
            // ── Virtualized horizontal-scroll path (all other files + large .md) ──
            let editor_base_handle = self.scroll_handle.0.borrow().base_handle.clone();
            let editor_scrollbar = render_scrollbar(
                "editor-scrollbar",
                "editor-scrollbar-thumb",
                &editor_base_handle,
                show_scrollbar,
                is_dragging,
                cx.listener(|view, ev: &MouseDownEvent, _, cx| {
                    let handle = view.scroll_handle.0.borrow().base_handle.clone();
                    view.scrollbar_drag = Some(start_drag(ev, &handle));
                    cx.notify();
                }),
                &t,
            );
            let entity = cx.entity();
            let t2 = t.clone();
            let widest = self.widest_line;
            let content = uniform_list(
                "editor-lines",
                line_count,
                move |range: std::ops::Range<usize>, _window, cx| {
                    let view = entity.read(cx);
                    range.map(|i| view.render_line(i, &t2, show_line_numbers, cursor_visible, false, outline_hl)).collect::<Vec<AnyElement>>()
                },
            )
            .flex_1()
            .px_2()
            .bg(t.bg)
            .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
            .with_width_from_item(Some(widest))
            .track_scroll(self.scroll_handle.clone())
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down_editor))
            .on_scroll_wheel(cx.listener(|view, _ev: &ScrollWheelEvent, _, cx| {
                // Trigger re-render so the breadcrumb heading stack refreshes.
                cx.notify();
            }));
            div()
                .flex()
                .flex_row()
                .flex_1()
                .min_w(px(0.))
                .min_h(px(0.))
                .child(div().flex().flex_col().flex_1().min_w(px(0.)).min_h(px(0.)).child(content))
                .child(editor_scrollbar)
        };

        // In preview mode: side-by-side split (editor left, preview right).
        if show_preview {
            if let Some(ref preview_entity) = self.preview {
                let preview = preview_entity.clone();
                let split = div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h(px(0.))
                    .child(editor_pane)
                    .child(div().w(px(1.)).bg(t.separator).flex_shrink_0())
                    .child(div().flex().flex_col().flex_1().min_w(px(0.)).min_h(px(0.)).child(preview));
                let root = div()
                    .flex()
                    .flex_col()
                    .size_full()
                    .relative()
                    .bg(t.bg)
                    .track_focus(&self.focus_handle(cx))
                    .on_key_down(cx.listener(Self::on_key_down))
                    .on_action(cx.listener(Self::on_move_left))
                    .on_action(cx.listener(Self::on_move_right))
                    .on_action(cx.listener(Self::on_move_up))
                    .on_action(cx.listener(Self::on_move_down))
                    .on_action(cx.listener(Self::on_move_word_left))
                    .on_action(cx.listener(Self::on_move_word_right))
                    .on_action(cx.listener(Self::on_move_line_start))
                    .on_action(cx.listener(Self::on_move_line_end))
                    .on_action(cx.listener(Self::on_move_doc_start))
                    .on_action(cx.listener(Self::on_move_doc_end))
                    .on_action(cx.listener(Self::on_move_page_up))
                    .on_action(cx.listener(Self::on_move_page_down))
                    .on_action(cx.listener(Self::on_select_left))
                    .on_action(cx.listener(Self::on_select_right))
                    .on_action(cx.listener(Self::on_select_up))
                    .on_action(cx.listener(Self::on_select_down))
                    .on_action(cx.listener(Self::on_select_word_left))
                    .on_action(cx.listener(Self::on_select_word_right))
                    .on_action(cx.listener(Self::on_select_line_start))
                    .on_action(cx.listener(Self::on_select_line_end))
                    .on_action(cx.listener(Self::on_select_doc_start))
                    .on_action(cx.listener(Self::on_select_doc_end))
                    .on_action(cx.listener(Self::on_select_all))
                    .on_action(cx.listener(Self::on_backspace))
                    .on_action(cx.listener(Self::on_delete))
                    .on_action(cx.listener(Self::on_delete_word_left))
                    .on_action(cx.listener(Self::on_delete_word_right))
                    .on_action(cx.listener(Self::on_delete_to_line_start))
                    .on_action(cx.listener(Self::on_delete_to_line_end))
                    .on_action(cx.listener(Self::on_delete_line))
                    .on_action(cx.listener(Self::on_tab))
                    .on_action(cx.listener(Self::on_enter))
                    .on_action(cx.listener(Self::on_copy))
                    .on_action(cx.listener(Self::on_cut))
                    .on_action(cx.listener(Self::on_paste))
                    .on_action(cx.listener(Self::on_undo))
                    .on_action(cx.listener(Self::on_redo))
                    .on_action(cx.listener(Self::on_open_search))
                    .on_action(cx.listener(Self::on_open_replace))
                    .on_action(cx.listener(Self::on_close_search))
                    .on_action(cx.listener(Self::on_find_next))
                    .on_action(cx.listener(Self::on_find_prev))
                    .on_action(cx.listener(Self::on_replace_one))
                    .on_action(cx.listener(Self::on_replace_all))
                    .on_action(cx.listener(Self::on_search_backspace))
                    .on_action(cx.listener(Self::on_replace_backspace))
                    .on_action(cx.listener(Self::on_input_move_left))
                    .on_action(cx.listener(Self::on_input_move_right))
                    .on_action(cx.listener(Self::on_input_move_start))
                    .on_action(cx.listener(Self::on_input_move_end))
                    .on_action(cx.listener(Self::on_toggle_search_case))
                    .on_action(cx.listener(Self::on_toggle_search_whole_word))
                    .on_action(cx.listener(Self::on_toggle_search_regex))
                    .on_action(cx.listener(Self::on_toggle_replace))
                    .on_action(cx.listener(Self::on_toggle_preview))
                    .on_action(cx.listener(Self::on_bold_selection))
                    .on_action(cx.listener(Self::on_italic_selection))
                    .on_action(cx.listener(Self::on_toggle_checkbox))
                    .when(is_dragging, |el| {
                        el.on_mouse_move(cx.listener(|view, ev: &MouseMoveEvent, _, cx| {
                            if let Some(ref drag) = view.scrollbar_drag {
                                if view.is_wrap_mode() {
                                    let handle = view.md_scroll.clone();
                                    update_drag(drag, ev, &handle);
                                } else {
                                    let handle = view.scroll_handle.0.borrow().base_handle.clone();
                                    update_drag(drag, ev, &handle);
                                }
                                cx.notify();
                            }
                        }))
                        .on_mouse_up(MouseButton::Left, cx.listener(|view, _, _, cx| {
                            view.scrollbar_drag = None;
                            cx.notify();
                        }))
                    })
                    .child(header);
                let key_ctx = if self.outline_open { "OutlineOverlay" } else if is_md { "Editor markdown" } else { "Editor" };
                let root = root.key_context(key_ctx);
                let root = if self.show_search {
                    root.child(self.render_search_bar(window, &t, cx))
                } else { root };
                let root = root.child(split);
                let root = if self.outline_open {
                    root.child(self.render_outline_overlay(&t, window, cx))
                } else { root };
                return root.into_any();
            }
        }

        let key_ctx = if self.outline_open { "OutlineOverlay" } else if is_md { "Editor markdown" } else { "Editor" };

        let root = div()
            .flex()
            .flex_col()
            .size_full()
            .relative()
            .bg(t.bg)
            .key_context(key_ctx)
            .track_focus(&self.focus_handle(cx))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(Self::on_move_left))
            .on_action(cx.listener(Self::on_move_right))
            .on_action(cx.listener(Self::on_move_up))
            .on_action(cx.listener(Self::on_move_down))
            .on_action(cx.listener(Self::on_move_word_left))
            .on_action(cx.listener(Self::on_move_word_right))
            .on_action(cx.listener(Self::on_move_line_start))
            .on_action(cx.listener(Self::on_move_line_end))
            .on_action(cx.listener(Self::on_move_doc_start))
            .on_action(cx.listener(Self::on_move_doc_end))
            .on_action(cx.listener(Self::on_move_page_up))
            .on_action(cx.listener(Self::on_move_page_down))
            .on_action(cx.listener(Self::on_select_left))
            .on_action(cx.listener(Self::on_select_right))
            .on_action(cx.listener(Self::on_select_up))
            .on_action(cx.listener(Self::on_select_down))
            .on_action(cx.listener(Self::on_select_word_left))
            .on_action(cx.listener(Self::on_select_word_right))
            .on_action(cx.listener(Self::on_select_line_start))
            .on_action(cx.listener(Self::on_select_line_end))
            .on_action(cx.listener(Self::on_select_doc_start))
            .on_action(cx.listener(Self::on_select_doc_end))
            .on_action(cx.listener(Self::on_select_all))
            .on_action(cx.listener(Self::on_backspace))
            .on_action(cx.listener(Self::on_delete))
            .on_action(cx.listener(Self::on_delete_word_left))
            .on_action(cx.listener(Self::on_delete_word_right))
            .on_action(cx.listener(Self::on_delete_to_line_start))
            .on_action(cx.listener(Self::on_delete_to_line_end))
            .on_action(cx.listener(Self::on_delete_line))
            .on_action(cx.listener(Self::on_tab))
            .on_action(cx.listener(Self::on_enter))
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_cut))
            .on_action(cx.listener(Self::on_paste))
            .on_action(cx.listener(Self::on_undo))
            .on_action(cx.listener(Self::on_redo))
            .on_action(cx.listener(Self::on_open_search))
            .on_action(cx.listener(Self::on_open_replace))
            .on_action(cx.listener(Self::on_close_search))
            .on_action(cx.listener(Self::on_find_next))
            .on_action(cx.listener(Self::on_find_prev))
            .on_action(cx.listener(Self::on_replace_one))
            .on_action(cx.listener(Self::on_replace_all))
            .on_action(cx.listener(Self::on_search_backspace))
            .on_action(cx.listener(Self::on_replace_backspace))
            .on_action(cx.listener(Self::on_input_move_left))
            .on_action(cx.listener(Self::on_input_move_right))
            .on_action(cx.listener(Self::on_input_move_start))
            .on_action(cx.listener(Self::on_input_move_end))
            .on_action(cx.listener(Self::on_toggle_search_case))
            .on_action(cx.listener(Self::on_toggle_search_whole_word))
            .on_action(cx.listener(Self::on_toggle_search_regex))
            .on_action(cx.listener(Self::on_toggle_replace))
            .on_action(cx.listener(Self::on_toggle_preview))
            .on_action(cx.listener(Self::on_bold_selection))
            .on_action(cx.listener(Self::on_italic_selection))
            .on_action(cx.listener(Self::on_toggle_checkbox))
            .when(is_dragging, |el| {
                el.on_mouse_move(cx.listener(|view, ev: &MouseMoveEvent, _, cx| {
                    if let Some(ref drag) = view.scrollbar_drag {
                        if view.is_wrap_mode() {
                            let handle = view.md_scroll.clone();
                            update_drag(drag, ev, &handle);
                        } else {
                            let handle = view.scroll_handle.0.borrow().base_handle.clone();
                            update_drag(drag, ev, &handle);
                        }
                        cx.notify();
                    }
                }))
                .on_mouse_up(MouseButton::Left, cx.listener(|view, _, _, cx| {
                    view.scrollbar_drag = None;
                    cx.notify();
                }))
            })
            .child(header);

        let root = if self.show_search {
            root.child(self.render_search_bar(window, &t, cx))
        } else { root };
        let root = root.child(editor_pane);
        let root = if self.outline_open {
            root.child(self.render_outline_overlay(&t, window, cx))
        } else { root };
        root.into_any()
    }
}

// ── Outline overlay ────────────────────────────────────────────────────────────

impl EditorView {
    fn render_outline_overlay(
        &self,
        t: &RuntimeTheme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let outline = Arc::clone(&self.outline);
        let query = self.outline_query.to_lowercase();
        let caret_visible = self.cursor_blink_on;
        let outline_focused = self.outline_handle.is_focused(window);

        // Filter entries by query substring.
        let filtered: Vec<(usize, OutlineEntry)> = outline
            .iter()
            .enumerate()
            .filter(|(_, e)| query.is_empty() || e.text.to_lowercase().contains(&query))
            .map(|(i, e)| (i, e.clone()))
            .collect();

        // ── search input ──────────────────────────────────────────────────────
        let (q_before, q_after) = split_at_char(&self.outline_query, self.outline_cursor);
        let is_query_empty = self.outline_query.is_empty();
        let caret_color = if outline_focused && caret_visible {
            t.cursor
        } else {
            gpui::hsla(0., 0., 0., 0.)
        };
        let caret_h = t.font_size_code + 2.;

        let search_input_base = div()
            .id("outline-search-input")
            .flex()
            .flex_row()
            .items_center()
            .px_3()
            .py(px(6.))
            .border_b_1()
            .border_color(t.separator)
            .track_focus(&self.outline_handle)
            .font_family(t.mono_family.clone())
            .text_size(px(t.font_size_code))
            .text_color(t.text)
            .child(svg().path(IconName::Search.path()).size(px(14.)).text_color(t.text_subtle).flex_shrink_0())
            .child(div().w(px(6.)).flex_shrink_0());

        // Mirror search bar: show placeholder only when NOT focused and query is empty.
        let search_input: gpui::AnyElement = if !outline_focused && is_query_empty {
            search_input_base
                .child(div().text_color(t.text_muted).child(t!("outline_overlay.placeholder").to_string()))
                .into_any()
        } else {
            search_input_base
                .child(div().flex_shrink_0().child(q_before))
                .child(div().flex_shrink_0().w(px(1.5)).h(px(caret_h)).bg(caret_color))
                .child(div().flex_shrink_0().child(q_after))
                .into_any()
        };

        // ── heading list ──────────────────────────────────────────────────────
        let hover_idx = self.outline_hover;

        let list_body: AnyElement = if outline.is_empty() {
            div()
                .flex_1()
                .flex()
                .min_h(px(200.))
                .items_center()
                .justify_center()
                .py(px(24.))
                .font_family(t.ui_family.clone())
                .text_size(px(t.font_size_caption))
                .text_color(t.text_muted)
                .child(t!("outline_overlay.no_headings").to_string())
                .into_any()
        } else if filtered.is_empty() {
            div()
                .flex_1()
                .flex()
                .min_h(px(200.))
                .items_center()
                .justify_center()
                .py(px(24.))
                .font_family(t.ui_family.clone())
                .text_size(px(t.font_size_caption))
                .text_color(t.text_muted)
                .child(t!("outline_overlay.no_matches").to_string())
                .into_any()
        } else {
            let entries: Vec<AnyElement> = filtered
                .iter()
                .enumerate()
                .map(|(list_idx, (orig_idx, entry))| {
                    let level = entry.level;
                    let source_line = entry.source_line;
                    let is_hovered = hover_idx == Some(list_idx);
                    let indent = (level.saturating_sub(1) as f32) * 12.0;

                    // Compute section end for highlight: next entry at same or higher level.
                    let section_end = filtered
                        .iter()
                        .skip(list_idx + 1)
                        .find(|(_, e)| e.level <= level)
                        .map(|(_, e)| e.source_line)
                        .unwrap_or(usize::MAX);

                    let t_clone = t.clone();
                    div()
                        .id(("outline-entry", list_idx))
                        .flex()
                        .flex_row()
                        .items_center()
                        .px(px(8. + indent))
                        .py(px(5.))
                        .gap_2()
                        .font_family(t.ui_family.clone())
                        .text_size(px(t.font_size_caption))
                        .text_color(t.text)
                        .when(is_hovered, |el| el.bg(t.line_highlight))
                        .cursor_pointer()
                        .hover(|el| el.bg(t_clone.line_highlight))
                        .on_mouse_move(cx.listener(move |view, _, _, cx| {
                            if view.outline_hover != Some(list_idx) {
                                view.outline_hover = Some(list_idx);
                                // Highlight section in editor
                                view.outline_highlight = Some((source_line, section_end));
                                // Scroll editor to heading
                                view.scroll_to_line(source_line);
                                cx.notify();
                            }
                        }))
                        .on_mouse_down(MouseButton::Left, cx.listener(move |view, _, window, cx| {
                            view.scroll_to_line(source_line);
                            view.outline_open = false;
                            view.outline_hover = None;
                            view.outline_highlight = None;
                            window.focus(&view.focus_handle);
                            cx.notify();
                        }))
                        .child(
                            div()
                                .text_color(t.text_muted)
                                .text_size(px(t.font_size_caption - 1.))
                                .child("#".repeat(level as usize))
                                .flex_shrink_0()
                        )
                        .child(
                            div()
                                .flex_1()
                                .overflow_hidden()
                                .child(SharedString::from(entry.text.clone()))
                        )
                        .into_any()
                })
                .collect();

            div()
                .id("outline-list")
                .flex_col()
                .overflow_y_scroll()
                .min_h(px(200.))
                .max_h(px(320.))
                .children(entries)
                .into_any()
        };

        // ── modal container ───────────────────────────────────────────────────
        // elevation: shadow_lg + large rounded corners clipped via overflow_hidden (Zed pattern).
        // stop_propagation on inner click so backdrop's on_mouse_down doesn't fire.
        let modal = div()
            .id("outline-modal")
            .occlude()
            .w(px(520.))
            .bg(t.bg_elevated)
            .rounded_lg()
            .border_1()
            .border_color(t.border)
            .shadow_lg()
            .overflow_hidden()
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(search_input)
            .child(list_body);

        // ── full-area backdrop — absolute+inset_0 so it fills the editor pane ──
        // (root has .relative(), so absolute+inset_0 always covers it exactly,
        //  and flex centering keeps the modal centered on resize — Zed modal_layer idiom)
        deferred(
            div()
                .id("outline-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_start()
                .justify_center()
                .pt(px(64.))
                .bg(gpui::hsla(0., 0., 0., 0.35))
                .on_mouse_down(MouseButton::Left, cx.listener(|view, _, window, cx| {
                    view.outline_open = false;
                    view.outline_hover = None;
                    view.outline_highlight = None;
                    window.focus(&view.focus_handle);
                    cx.notify();
                }))
                .child(modal),
        )
        .with_priority(2)
        .into_any()
    }
}

// ── string helpers for search/replace cursor ───────────────────────────────────

/// Split a text string into word-and-space tokens so that `flex_wrap` can break
/// between words. Each returned piece is either a run of non-space chars or a
/// run of space chars, preserving the original content exactly.
fn split_words_for_wrap(text: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_space = false;
    for c in text.chars() {
        let is_space = c == ' ' || c == '\t';
        if is_space != in_space && !current.is_empty() {
            parts.push(std::mem::take(&mut current));
        }
        in_space = is_space;
        current.push(c);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn insert_at(s: &str, char_idx: usize, text: &str) -> String {
    let mut chars: Vec<char> = s.chars().collect();
    let idx = char_idx.min(chars.len());
    for (i, c) in text.chars().enumerate() {
        chars.insert(idx + i, c);
    }
    chars.into_iter().collect()
}

fn delete_char_before(s: &str, char_idx: usize) -> String {
    if char_idx == 0 { return s.to_string(); }
    let chars: Vec<char> = s.chars().collect();
    let idx = char_idx.saturating_sub(1).min(chars.len().saturating_sub(1));
    chars[..idx].iter().chain(chars[char_idx..].iter()).copied().collect()
}

fn split_at_char(s: &str, char_idx: usize) -> (String, String) {
    let chars: Vec<char> = s.chars().collect();
    let idx = char_idx.min(chars.len());
    (chars[..idx].iter().collect(), chars[idx..].iter().collect())
}
