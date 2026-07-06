use std::ops::Range;

use felix_editor::{
    SyntaxToken,
    buffer::Document,
    cursor,
    edit_history::History,
    highlight::{HighlightSpan, byte_col_to_char_col},
    node_count,
    save::save,
    search::Query,
    Selection,
};
use gpui::{
    AnyElement, App, ClipboardItem, Context, FocusHandle, Focusable, IntoElement, KeyDownEvent,
    ReadGlobal, Render, ScrollStrategy, SharedString, UniformListScrollHandle, Window, div,
    prelude::*, px, uniform_list,
};

use crate::theme::RuntimeTheme;

// ── layout constants ───────────────────────────────────────────────────────────

const LINE_H: f32 = 20.0;
const CHAR_W: f32 = 8.4;
const GUTTER_COLS: f32 = 6.0;
const GUTTER_W: f32 = GUTTER_COLS * CHAR_W;

// ── actions ────────────────────────────────────────────────────────────────────

use crate::{
    Backspace, CloseSearch, Copy, Cut, Delete, DeleteWordLeft, DeleteWordRight, Enter, FindNext,
    FindPrev, MoveDocEnd, MoveDocStart, MoveDown, MoveLeft, MoveLineEnd, MoveLineStart,
    MovePageDown, MovePageUp, MoveRight, MoveUp, MoveWordLeft, MoveWordRight, OpenReplace,
    OpenSearch, Paste, Redo, ReplaceAll, ReplaceBackspace, ReplaceOne, Save, SearchBackspace,
    SelectAll, SelectDocEnd, SelectDocStart, SelectDown, SelectLeft, SelectLineEnd,
    SelectLineStart, SelectRight, SelectUp, SelectWordLeft, SelectWordRight, Tab, Undo,
};

// ── EditorView ─────────────────────────────────────────────────────────────────

pub struct EditorView {
    pub doc: Document,
    pub sel: Selection,
    pub history: History,
    pub focus_handle: FocusHandle,

    // Line display cache — rebuilt after every document mutation.
    pub line_cache: Vec<SharedString>,
    pub line_starts: Vec<usize>,

    pub scroll_handle: UniformListScrollHandle,
    last_scroll_line: usize,

    // Search / replace
    pub show_search: bool,
    pub show_replace: bool,
    pub search_handle: FocusHandle,
    pub replace_handle: FocusHandle,
    pub search_query: String,
    pub replace_query: String,
    pub matches: Vec<Range<usize>>,
    pub match_idx: usize,
}

impl EditorView {
    pub fn new(path: &str, cx: &mut Context<EditorView>) -> Self {
        let doc = Document::open(path).unwrap_or_else(|_| {
            let mut d = Document::open("/dev/null").expect("can't open /dev/null");
            d.path = std::path::PathBuf::from(path);
            d
        });
        let mut view = Self {
            doc,
            sel: Selection::default(),
            history: History::new(),
            focus_handle: cx.focus_handle(),
            line_cache: Vec::new(),
            line_starts: Vec::new(),
            scroll_handle: UniformListScrollHandle::new(),
            last_scroll_line: 0,
            show_search: false,
            show_replace: false,
            search_handle: cx.focus_handle(),
            replace_handle: cx.focus_handle(),
            search_query: String::new(),
            replace_query: String::new(),
            matches: Vec::new(),
            match_idx: 0,
        };
        view.rebuild_line_cache();
        view
    }

    // ── helpers ────────────────────────────────────────────────────────────────

    fn clamp_sel(&mut self) {
        let len = self.doc.len_chars();
        self.sel.head = self.sel.head.min(len);
        self.sel.anchor = self.sel.anchor.min(len);
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
        self.update_matches();
        cx.notify();
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
        self.update_matches();
        cx.notify();
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
        self.update_matches();
        cx.notify();
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
        self.update_matches();
        cx.notify();
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
        self.update_matches();
        cx.notify();
    }

    pub fn rebuild_line_cache(&mut self) {
        let rope = &self.doc.rope;
        let line_count = rope.len_lines();
        self.line_cache.clear();
        self.line_cache.reserve(line_count);
        self.line_starts.clear();
        self.line_starts.reserve(line_count);
        for i in 0..line_count {
            let char_start = rope.line_to_char(i);
            self.line_starts.push(char_start);
            let raw = rope.line(i).to_string();
            let content = raw.trim_end_matches(['\n', '\r']);
            self.line_cache.push(SharedString::from(format!("{:>4}  {}", i + 1, content)));
        }
    }

    fn update_matches(&mut self) {
        self.rebuild_line_cache();
        let q = Query::new(self.search_query.clone());
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
        self.update_matches();
        cx.notify();
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
        self.update_matches();
        cx.notify();
    }

    // ── action handlers ────────────────────────────────────────────────────────

    fn on_move_left(&mut self, _: &MoveLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_left(&self.doc.rope, self.sel, false);
        cx.notify();
    }
    fn on_move_right(&mut self, _: &MoveRight, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_right(&self.doc.rope, self.sel, false);
        cx.notify();
    }
    fn on_move_up(&mut self, _: &MoveUp, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_up(&self.doc.rope, self.sel, false);
        cx.notify();
    }
    fn on_move_down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_down(&self.doc.rope, self.sel, false);
        cx.notify();
    }
    fn on_move_word_left(&mut self, _: &MoveWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_word_left(&self.doc.rope, self.sel, false);
        cx.notify();
    }
    fn on_move_word_right(&mut self, _: &MoveWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_word_right(&self.doc.rope, self.sel, false);
        cx.notify();
    }
    fn on_move_line_start(&mut self, _: &MoveLineStart, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_home(&self.doc.rope, self.sel, false);
        cx.notify();
    }
    fn on_move_line_end(&mut self, _: &MoveLineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_end(&self.doc.rope, self.sel, false);
        cx.notify();
    }
    fn on_move_doc_start(&mut self, _: &MoveDocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_doc_start(self.sel, false);
        cx.notify();
    }
    fn on_move_doc_end(&mut self, _: &MoveDocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_doc_end(&self.doc.rope, self.sel, false);
        cx.notify();
    }
    fn on_move_page_up(&mut self, _: &MovePageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_page_up(&self.doc.rope, self.sel, false, 30);
        cx.notify();
    }
    fn on_move_page_down(&mut self, _: &MovePageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_page_down(&self.doc.rope, self.sel, false, 30);
        cx.notify();
    }

    fn on_select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.sel = cursor::move_left(&self.doc.rope, self.sel, true);
        cx.notify();
    }
    fn on_select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.sel = cursor::move_right(&self.doc.rope, self.sel, true);
        cx.notify();
    }
    fn on_select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.sel = cursor::move_up(&self.doc.rope, self.sel, true);
        cx.notify();
    }
    fn on_select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.sel = cursor::move_down(&self.doc.rope, self.sel, true);
        cx.notify();
    }
    fn on_select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.sel = cursor::move_word_left(&self.doc.rope, self.sel, true);
        cx.notify();
    }
    fn on_select_word_right(
        &mut self,
        _: &SelectWordRight,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sel = cursor::move_word_right(&self.doc.rope, self.sel, true);
        cx.notify();
    }
    fn on_select_line_start(
        &mut self,
        _: &SelectLineStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sel = cursor::move_home(&self.doc.rope, self.sel, true);
        cx.notify();
    }
    fn on_select_line_end(&mut self, _: &SelectLineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.sel = cursor::move_end(&self.doc.rope, self.sel, true);
        cx.notify();
    }
    fn on_select_doc_start(
        &mut self,
        _: &SelectDocStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sel = cursor::move_doc_start(self.sel, true);
        cx.notify();
    }
    fn on_select_doc_end(&mut self, _: &SelectDocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.sel = cursor::move_doc_end(&self.doc.rope, self.sel, true);
        cx.notify();
    }
    fn on_select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.sel = cursor::select_all(&self.doc.rope);
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
    fn on_tab(&mut self, _: &Tab, _: &mut Window, cx: &mut Context<Self>) {
        self.insert_text("\t", cx);
    }
    fn on_enter(&mut self, _: &Enter, _: &mut Window, cx: &mut Context<Self>) {
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
            self.update_matches();
            cx.notify();
        }
    }
    fn on_paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(item) = cx.read_from_clipboard() {
            if let Some(text) = item.text() {
                self.history.commit();
                self.insert_text(&text.clone(), cx);
            }
        }
    }

    fn on_undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(pos) = self.history.undo(&mut self.doc) {
            self.clamp_sel();
            self.sel = Selection::collapsed(pos.min(self.doc.len_chars()), &self.doc.rope);
            self.update_matches();
            cx.notify();
        }
    }
    fn on_redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(pos) = self.history.redo(&mut self.doc) {
            self.clamp_sel();
            self.sel = Selection::collapsed(pos.min(self.doc.len_chars()), &self.doc.rope);
            self.update_matches();
            cx.notify();
        }
    }

    fn on_save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        if let Ok(()) = save(&self.doc.rope, &self.doc.path) {
            self.doc.dirty = false;
            cx.notify();
        }
    }

    fn on_open_search(&mut self, _: &OpenSearch, window: &mut Window, cx: &mut Context<Self>) {
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
        self.search_query.pop();
        self.update_matches();
        cx.notify();
    }
    fn on_replace_backspace(
        &mut self,
        _: &ReplaceBackspace,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_query.pop();
        cx.notify();
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let Some(ref text) = ks.key_char else { return };
        if ks.modifiers.control || ks.modifiers.platform {
            return;
        }
        if text.chars().any(|c| c.is_control()) {
            return;
        }
        if self.show_replace && self.replace_handle.is_focused(window) {
            self.replace_query.push_str(text);
            cx.notify();
        } else if self.show_search && self.search_handle.is_focused(window) {
            self.search_query.push_str(text);
            self.update_matches();
            cx.notify();
        } else {
            self.insert_text(text, cx);
        }
    }

    // ── rendering helpers ──────────────────────────────────────────────────────

    /// Render one line. `t` is the theme cloned once at top of `render()`.
    fn render_line(&self, line_idx: usize, t: &RuntimeTheme) -> AnyElement {
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

        // ── Fast path: no decoration at all ───────────────────────────────────
        if !cursor_on_line && !has_sel && !has_match && !has_spans {
            return div()
                .h(px(LINE_H))
                .font_family(t.mono_family.clone())
                .text_size(px(t.font_size_code))
                .text_color(t.text)
                .child(self.line_cache[line_idx].clone())
                .into_any_element();
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

        let gutter = div()
            .flex_shrink_0()
            .w(px(GUTTER_W))
            .text_color(t.gutter)
            .child(format!("{:>4}", line_idx + 1));
        let gutter_spacer = div().flex_shrink_0().w(px(2.0 * CHAR_W)).child("  ");

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
                content_children.push(
                    div()
                        .flex_shrink_0()
                        .w(px(1.5))
                        .h(px(LINE_H))
                        .bg(t.cursor)
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
            let seg_div = div().flex_shrink_0().text_color(fg_color).child(seg_text);
            let seg_div = if let Some(color) = bg_color { seg_div.bg(color) } else { seg_div };
            content_children.push(seg_div.into_any());
        }
        if cursor_on_line && cursor_col == line_char_count {
            content_children.push(
                div().flex_shrink_0().w(px(1.5)).h(px(LINE_H)).bg(t.cursor).into_any(),
            );
        }

        let content = div().flex().flex_row().flex_1().children(content_children);
        div()
            .flex()
            .flex_row()
            .h(px(LINE_H))
            .font_family(t.mono_family.clone())
            .text_size(px(t.font_size_code))
            .child(gutter)
            .child(gutter_spacer)
            .child(content)
            .into_any_element()
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

    fn render_search_bar(&self, window: &Window, t: &RuntimeTheme) -> impl IntoElement {
        let match_info = if !self.search_query.is_empty() && !self.matches.is_empty() {
            format!("{}/{}", self.match_idx + 1, self.matches.len())
        } else if !self.search_query.is_empty() {
            "no matches".to_string()
        } else {
            String::new()
        };

        let search_focused = self.search_handle.is_focused(window);

        let search_row = div()
            .flex()
            .flex_row()
            .gap_2()
            .px_3()
            .py_1()
            .child(div().text_color(t.gutter).flex_shrink_0().child("Search:"))
            .child(
                div()
                    .flex_1()
                    .bg(if search_focused { t.line_highlight } else { t.bg_sunken })
                    .px_2()
                    .text_color(t.text)
                    .font_family(t.mono_family.clone())
                    .text_size(px(t.font_size_code))
                    .child(if self.search_query.is_empty() {
                        SharedString::from("⬜")
                    } else {
                        SharedString::from(format!("{}▏", self.search_query))
                    }),
            )
            .child(div().text_color(t.gutter).flex_shrink_0().child(match_info));

        if !self.show_replace {
            return div()
                .border_t_1()
                .border_color(t.separator)
                .bg(t.bg)
                .key_context("SearchBar")
                .track_focus(&self.search_handle)
                .child(search_row);
        }

        let replace_focused = self.replace_handle.is_focused(window);

        let replace_row = div()
            .flex()
            .flex_row()
            .gap_2()
            .px_3()
            .py_1()
            .child(div().text_color(t.gutter).flex_shrink_0().child("Replace:"))
            .child(
                div()
                    .flex_1()
                    .bg(if replace_focused { t.line_highlight } else { t.bg_sunken })
                    .px_2()
                    .text_color(t.text)
                    .font_family(t.mono_family.clone())
                    .text_size(px(t.font_size_code))
                    .child(if self.replace_query.is_empty() {
                        SharedString::from("⬜")
                    } else {
                        SharedString::from(format!("{}▏", self.replace_query))
                    }),
            )
            .child(div().text_color(t.gutter).flex_shrink_0().child("⏎ Replace · ⌘⏎ All"));

        div()
            .border_t_1()
            .border_color(t.separator)
            .bg(t.bg)
            .key_context("SearchBar")
            .track_focus(&self.search_handle)
            .child(search_row)
            .child(
                div()
                    .key_context("ReplaceBar")
                    .track_focus(&self.replace_handle)
                    .child(replace_row),
            )
    }
}

// ── GPUI impls ─────────────────────────────────────────────────────────────────

impl Focusable for EditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<RuntimeTheme>().clone();

        let file_name = self.doc.path.file_name().map_or_else(
            || "untitled".to_string(),
            |n| n.to_string_lossy().to_string(),
        );
        let title = if self.doc.dirty {
            format!("● {file_name}")
        } else {
            file_name
        };

        let title_color = if self.doc.dirty { t.dirty } else { t.text };
        let header = div()
            .flex()
            .flex_row()
            .justify_between()
            .px_4()
            .py_2()
            .bg(t.bg_elevated)
            .border_b_1()
            .border_color(t.separator)
            .child(
                div()
                    .font_family(t.ui_family.clone())
                    .text_size(px(t.font_size_body))
                    .text_color(title_color)
                    .child(title),
            )
            .child(
                div()
                    .font_family(t.ui_family.clone())
                    .text_size(px(t.font_size_caption))
                    .text_color(t.text_subtle)
                    .child(format!(
                        "{}L  {}N  ⌘S save · ⌘F find · ⌘⌥F replace",
                        self.doc.len_lines(),
                        node_count(&self.doc.tree),
                    )),
            );

        let line_count = self.doc.len_lines();
        let cursor_line = self.doc.rope.char_to_line(self.sel.head.min(self.doc.len_chars().saturating_sub(1)));
        if cursor_line != self.last_scroll_line {
            self.last_scroll_line = cursor_line;
            self.scroll_handle.scroll_to_item(cursor_line, ScrollStrategy::Center);
        }

        let entity = cx.entity();
        let t2 = t.clone();
        let content = uniform_list(
            "editor-lines",
            line_count,
            move |range: std::ops::Range<usize>, _window, cx| {
                let view = entity.read(cx);
                range.map(|i| view.render_line(i, &t2)).collect::<Vec<AnyElement>>()
            },
        )
        .flex_1()
        .px_2()
        .py_1()
        .bg(t.bg)
        .track_scroll(self.scroll_handle.clone());

        let root = div()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.bg)
            .key_context("Editor")
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
            .on_action(cx.listener(Self::on_tab))
            .on_action(cx.listener(Self::on_enter))
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_cut))
            .on_action(cx.listener(Self::on_paste))
            .on_action(cx.listener(Self::on_undo))
            .on_action(cx.listener(Self::on_redo))
            .on_action(cx.listener(Self::on_save))
            .on_action(cx.listener(Self::on_open_search))
            .on_action(cx.listener(Self::on_open_replace))
            .on_action(cx.listener(Self::on_close_search))
            .on_action(cx.listener(Self::on_find_next))
            .on_action(cx.listener(Self::on_find_prev))
            .on_action(cx.listener(Self::on_replace_one))
            .on_action(cx.listener(Self::on_replace_all))
            .on_action(cx.listener(Self::on_search_backspace))
            .on_action(cx.listener(Self::on_replace_backspace))
            .child(header)
            .child(content);

        if self.show_search {
            root.child(self.render_search_bar(window, &t))
        } else {
            root
        }
    }
}
