use felix_editor::{
    buffer::Document,
    cursor,
    Selection,
    edit_history::History,
    node_count,
    save::save,
    search::Query,
};
use gpui::{
    AnyElement, App, Application, Bounds, ClipboardItem, Context, FocusHandle, Focusable,
    KeyBinding, KeyDownEvent, SharedString, Window, WindowBounds, WindowOptions, actions, div,
    prelude::*, px, rgb, size,
};
use std::{env, ops::Range, time::Instant};

// ─── visual constants ────────────────────────────────────────────────────────

const LINE_H: f32 = 20.0;
/// Approximate char width for Menlo 14px (text_sm ≈ 0.875rem = 14px).
const CHAR_W: f32 = 8.4;
/// Gutter: "{:>4}  " = 4 digits + 2 spaces = 6 chars.
const GUTTER_COLS: f32 = 6.0;
const GUTTER_W: f32 = GUTTER_COLS * CHAR_W;

// Catppuccin Mocha palette.
const BG: u32 = 0x1e1e2e;
const TEXT: u32 = 0xcdd6f4;
const GUTTER: u32 = 0x6c7086;
const SEP: u32 = 0x313244;
const CURSOR: u32 = 0xcdd6f4;
const SEL_BG: u32 = 0x45475a;
const MATCH_BG: u32 = 0x4a4f6a;
const ACTIVE_MATCH_BG: u32 = 0x7f849c;
const DIRTY_INDICATOR: u32 = 0xf38ba8;

// ─── actions ─────────────────────────────────────────────────────────────────

actions!(
    editor,
    [
        MoveLeft, MoveRight, MoveUp, MoveDown,
        MoveWordLeft, MoveWordRight,
        MoveLineStart, MoveLineEnd,
        MoveDocStart, MoveDocEnd,
        MovePageUp, MovePageDown,
        SelectLeft, SelectRight, SelectUp, SelectDown,
        SelectWordLeft, SelectWordRight,
        SelectLineStart, SelectLineEnd,
        SelectDocStart, SelectDocEnd,
        SelectAll,
        Backspace, Delete,
        DeleteWordLeft, DeleteWordRight,
        Tab, Enter,
        Copy, Cut, Paste,
        Undo, Redo,
        Save,
        OpenSearch, OpenReplace, CloseSearch,
        FindNext, FindPrev,
        ReplaceOne, ReplaceAll,
        SearchBackspace, ReplaceBackspace,
    ]
);

// ─── EditorView ──────────────────────────────────────────────────────────────

struct EditorView {
    doc: Document,
    sel: Selection,
    history: History,
    focus_handle: FocusHandle,

    // Line display cache — rebuilt after every document mutation.
    // line_cache[i]: pre-formatted "{:>4}  {content}" SharedString.
    // line_starts[i]: char offset of line i's start in the rope (O(1) cursor/selection checks).
    line_cache: Vec<SharedString>,
    line_starts: Vec<usize>,

    // Search / replace
    show_search: bool,
    show_replace: bool,
    search_handle: FocusHandle,
    replace_handle: FocusHandle,
    search_query: String,
    replace_query: String,
    matches: Vec<Range<usize>>,
    match_idx: usize,
}

impl EditorView {
    fn new(path: &str, cx: &mut Context<EditorView>) -> Self {
        let doc = Document::open(path).unwrap_or_else(|_| {
            // If file can't be opened, open an empty buffer pointing to that path.
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

    // ── helpers ──────────────────────────────────────────────────────────────

    fn clamp_sel(&mut self) {
        let len = self.doc.len_chars();
        self.sel.head = self.sel.head.min(len);
        self.sel.anchor = self.sel.anchor.min(len);
    }

    fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.history.commit(); // break coalescing on non-trivial operations
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
            let word_start =
                cursor::move_word_left(&self.doc.rope, self.sel, false).head;
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
            let word_end =
                cursor::move_word_right(&self.doc.rope, self.sel, false).head;
            if word_end > self.sel.head {
                let edit = self.doc.delete(self.sel.head..word_end);
                self.history.push_other(edit);
                self.sel = Selection::collapsed(self.sel.head, &self.doc.rope);
            }
        }
        self.update_matches();
        cx.notify();
    }

    fn rebuild_line_cache(&mut self) {
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
        // Clamp match_idx.
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
        // Iterate matches in reverse to keep earlier char offsets valid.
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

    // ── action handlers ──────────────────────────────────────────────────────

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
    fn on_move_word_right(
        &mut self,
        _: &MoveWordRight,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.history.commit();
        self.sel = cursor::move_word_right(&self.doc.rope, self.sel, false);
        cx.notify();
    }
    fn on_move_line_start(
        &mut self,
        _: &MoveLineStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.history.commit();
        self.sel = cursor::move_home(&self.doc.rope, self.sel, false);
        cx.notify();
    }
    fn on_move_line_end(&mut self, _: &MoveLineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.history.commit();
        self.sel = cursor::move_end(&self.doc.rope, self.sel, false);
        cx.notify();
    }
    fn on_move_doc_start(
        &mut self,
        _: &MoveDocStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    // ── selection ────────────────────────────────────────────────────────────

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
    fn on_select_word_left(
        &mut self,
        _: &SelectWordLeft,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
    fn on_select_line_end(
        &mut self,
        _: &SelectLineEnd,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
    fn on_select_doc_end(
        &mut self,
        _: &SelectDocEnd,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sel = cursor::move_doc_end(&self.doc.rope, self.sel, true);
        cx.notify();
    }
    fn on_select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.sel = cursor::select_all(&self.doc.rope);
        cx.notify();
    }

    // ── editing ──────────────────────────────────────────────────────────────

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
        self.history.commit(); // newline always starts a new undo group
    }

    // ── clipboard ────────────────────────────────────────────────────────────

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

    // ── undo / redo ──────────────────────────────────────────────────────────

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

    // ── file ─────────────────────────────────────────────────────────────────

    fn on_save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        if let Ok(()) = save(&self.doc.rope, &self.doc.path) {
            self.doc.dirty = false;
            cx.notify();
        }
    }

    // ── search / replace ─────────────────────────────────────────────────────

    fn on_open_search(&mut self, _: &OpenSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.show_search = true;
        self.show_replace = false;
        self.update_matches();
        window.focus(&self.search_handle);
        cx.notify();
    }
    fn on_open_replace(
        &mut self,
        _: &OpenReplace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_search = true;
        self.show_replace = true;
        self.update_matches();
        window.focus(&self.search_handle);
        cx.notify();
    }
    fn on_close_search(
        &mut self,
        _: &CloseSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    // ── key_down (printable character input) ─────────────────────────────────

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let Some(ref text) = ks.key_char else { return };
        if ks.modifiers.control || ks.modifiers.platform {
            return;
        }
        // Skip control characters (Tab, Enter handled by actions).
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

    // ── rendering helpers ────────────────────────────────────────────────────

    /// Render one line of the document (0-indexed `line_idx`).
    fn render_line(&self, line_idx: usize) -> impl IntoElement {
        // O(1) line boundary lookups from the prebuilt cache.
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

        // ── Fast path: no special decoration on this line ─────────────────────
        // Single div + one SharedString child — same element count as the old
        // read-only viewer, keeping RSS at baseline for large files.
        if !cursor_on_line && !has_sel && !has_match {
            return div()
                .h(px(LINE_H))
                .font_family("Menlo")
                .text_sm()
                .text_color(rgb(TEXT))
                .child(self.line_cache[line_idx].clone());
        }

        // ── Slow path: cursor / selection / search-match on this line ─────────
        let rope = &self.doc.rope;
        let raw = rope.line(line_idx).to_string();
        let line_str = raw.trim_end_matches(['\n', '\r']).to_string();
        let line_chars: Vec<char> = line_str.chars().collect();
        let line_char_count = line_chars.len();
        let line_char_end = line_char_start + line_char_count;

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
            .text_color(rgb(GUTTER))
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
                    div().flex_shrink_0().w(px(1.5)).h(px(LINE_H)).bg(rgb(CURSOR)).into_any(),
                );
            }
            let in_sel = has_sel && seg_start >= line_sel_start && seg_end <= line_sel_end;
            let match_bg: Option<u32> = match_ranges_on_line
                .iter()
                .find(|(s, e, _)| seg_start >= *s && seg_end <= *e)
                .map(|(_, _, active)| if *active { ACTIVE_MATCH_BG } else { MATCH_BG });
            let bg = match_bg.or(if in_sel { Some(SEL_BG) } else { None });
            let seg_text: String = line_chars[seg_start..seg_end].iter().collect();
            let seg_div = div().flex_shrink_0().text_color(rgb(TEXT)).child(seg_text);
            let seg_div = if let Some(color) = bg { seg_div.bg(rgb(color)) } else { seg_div };
            content_children.push(seg_div.into_any());
        }
        if cursor_on_line && cursor_col == line_char_count {
            content_children.push(
                div().flex_shrink_0().w(px(1.5)).h(px(LINE_H)).bg(rgb(CURSOR)).into_any(),
            );
        }

        let content = div().flex().flex_row().flex_1().children(content_children);
        div()
            .flex()
            .flex_row()
            .h(px(LINE_H))
            .font_family("Menlo")
            .text_sm()
            .child(gutter)
            .child(gutter_spacer)
            .child(content)
    }

    fn render_search_bar(&self, window: &Window, _cx: &Context<Self>) -> impl IntoElement {
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
            .child(
                div()
                    .text_color(rgb(GUTTER))
                    .flex_shrink_0()
                    .child("Search:"),
            )
            .child(
                div()
                    .flex_1()
                    .bg(if search_focused { rgb(0x313244) } else { rgb(0x181825) })
                    .px_2()
                    .text_color(rgb(TEXT))
                    .font_family("Menlo")
                    .text_sm()
                    .child(if self.search_query.is_empty() {
                        SharedString::from("⬜")
                    } else {
                        SharedString::from(format!("{}▏", self.search_query))
                    }),
            )
            .child(div().text_color(rgb(GUTTER)).flex_shrink_0().child(match_info));

        if !self.show_replace {
            return div()
                .border_t_1()
                .border_color(rgb(SEP))
                .bg(rgb(BG))
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
            .child(
                div().text_color(rgb(GUTTER)).flex_shrink_0().child("Replace:"),
            )
            .child(
                div()
                    .flex_1()
                    .bg(if replace_focused { rgb(0x313244) } else { rgb(0x181825) })
                    .px_2()
                    .text_color(rgb(TEXT))
                    .font_family("Menlo")
                    .text_sm()
                    .child(if self.replace_query.is_empty() {
                        SharedString::from("⬜")
                    } else {
                        SharedString::from(format!("{}▏", self.replace_query))
                    }),
            )
            .child(
                div()
                    .text_color(rgb(GUTTER))
                    .flex_shrink_0()
                    .child("⏎ Replace · ⌘⏎ All"),
            );

        div()
            .border_t_1()
            .border_color(rgb(SEP))
            .bg(rgb(BG))
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

// ── GPUI impls ────────────────────────────────────────────────────────────────

impl Focusable for EditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let file_name = self.doc.path.file_name().map_or_else(
            || "untitled".to_string(),
            |n| n.to_string_lossy().to_string(),
        );
        let title = if self.doc.dirty {
            format!("● {file_name}")
        } else {
            file_name
        };

        let header = div()
            .flex()
            .flex_row()
            .justify_between()
            .px_4()
            .py_2()
            .bg(rgb(BG))
            .border_b_1()
            .border_color(rgb(SEP))
            .child(
                div().text_color(rgb(if self.doc.dirty { DIRTY_INDICATOR } else { TEXT })).child(title),
            )
            .child(
                div().text_color(rgb(GUTTER)).child(format!(
                    "{}L  {}N  ⌘S save · ⌘F find · ⌘⌥F replace",
                    self.doc.len_lines(),
                    node_count(&self.doc.tree),
                )),
            );

        let line_count = self.doc.len_lines();
        let content = div()
            .flex_1()
            .id("editor-scroll")
            .overflow_scroll()
            .px_2()
            .py_1()
            .bg(rgb(BG))
            .children((0..line_count).map(|i| self.render_line(i)));

        let root = div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(BG))
            .key_context("Editor")
            .track_focus(&self.focus_handle(cx))
            .on_key_down(cx.listener(Self::on_key_down))
            // Movement
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
            // Selection
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
            // Editing
            .on_action(cx.listener(Self::on_backspace))
            .on_action(cx.listener(Self::on_delete))
            .on_action(cx.listener(Self::on_delete_word_left))
            .on_action(cx.listener(Self::on_delete_word_right))
            .on_action(cx.listener(Self::on_tab))
            .on_action(cx.listener(Self::on_enter))
            // Clipboard
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_cut))
            .on_action(cx.listener(Self::on_paste))
            // Undo/redo
            .on_action(cx.listener(Self::on_undo))
            .on_action(cx.listener(Self::on_redo))
            // File
            .on_action(cx.listener(Self::on_save))
            // Search
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
            root.child(self.render_search_bar(window, cx))
        } else {
            root
        }
    }
}

// ─── main ─────────────────────────────────────────────────────────────────────

fn main() {
    let start = Instant::now();
    let path = env::args().nth(1).unwrap_or_else(|| "src/main.rs".into());

    Application::new().run(move |cx: &mut App| {
        register_keybindings(cx);

        let bounds = Bounds::centered(None, size(px(1024.), px(768.)), cx);

        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| EditorView::new(&path, cx)),
            )
            .unwrap();

        window
            .update(cx, |view, window, cx| {
                window.focus(&view.focus_handle);
                cx.activate(true);
            })
            .unwrap();

        println!("FELIX_READY startup_ms={}", start.elapsed().as_millis());
    });
}

fn register_keybindings(cx: &mut App) {
    cx.bind_keys([
        // Movement
        KeyBinding::new("left", MoveLeft, Some("Editor")),
        KeyBinding::new("right", MoveRight, Some("Editor")),
        KeyBinding::new("up", MoveUp, Some("Editor")),
        KeyBinding::new("down", MoveDown, Some("Editor")),
        KeyBinding::new("alt-left", MoveWordLeft, Some("Editor")),
        KeyBinding::new("alt-right", MoveWordRight, Some("Editor")),
        KeyBinding::new("cmd-left", MoveLineStart, Some("Editor")),
        KeyBinding::new("cmd-right", MoveLineEnd, Some("Editor")),
        KeyBinding::new("home", MoveLineStart, Some("Editor")),
        KeyBinding::new("end", MoveLineEnd, Some("Editor")),
        KeyBinding::new("cmd-up", MoveDocStart, Some("Editor")),
        KeyBinding::new("cmd-down", MoveDocEnd, Some("Editor")),
        KeyBinding::new("pageup", MovePageUp, Some("Editor")),
        KeyBinding::new("pagedown", MovePageDown, Some("Editor")),
        // Selection
        KeyBinding::new("shift-left", SelectLeft, Some("Editor")),
        KeyBinding::new("shift-right", SelectRight, Some("Editor")),
        KeyBinding::new("shift-up", SelectUp, Some("Editor")),
        KeyBinding::new("shift-down", SelectDown, Some("Editor")),
        KeyBinding::new("shift-alt-left", SelectWordLeft, Some("Editor")),
        KeyBinding::new("shift-alt-right", SelectWordRight, Some("Editor")),
        KeyBinding::new("shift-cmd-left", SelectLineStart, Some("Editor")),
        KeyBinding::new("shift-cmd-right", SelectLineEnd, Some("Editor")),
        KeyBinding::new("shift-home", SelectLineStart, Some("Editor")),
        KeyBinding::new("shift-end", SelectLineEnd, Some("Editor")),
        KeyBinding::new("shift-cmd-up", SelectDocStart, Some("Editor")),
        KeyBinding::new("shift-cmd-down", SelectDocEnd, Some("Editor")),
        KeyBinding::new("cmd-a", SelectAll, Some("Editor")),
        // Editing
        KeyBinding::new("backspace", Backspace, Some("Editor")),
        KeyBinding::new("delete", Delete, Some("Editor")),
        KeyBinding::new("alt-backspace", DeleteWordLeft, Some("Editor")),
        KeyBinding::new("alt-delete", DeleteWordRight, Some("Editor")),
        KeyBinding::new("tab", Tab, Some("Editor")),
        KeyBinding::new("enter", Enter, Some("Editor")),
        // Clipboard
        KeyBinding::new("cmd-c", Copy, Some("Editor")),
        KeyBinding::new("cmd-x", Cut, Some("Editor")),
        KeyBinding::new("cmd-v", Paste, Some("Editor")),
        // Undo/redo
        KeyBinding::new("cmd-z", Undo, Some("Editor")),
        KeyBinding::new("cmd-shift-z", Redo, Some("Editor")),
        // File
        KeyBinding::new("cmd-s", Save, Some("Editor")),
        // Search
        KeyBinding::new("cmd-f", OpenSearch, Some("Editor")),
        KeyBinding::new("cmd-alt-f", OpenReplace, Some("Editor")),
        KeyBinding::new("cmd-g", FindNext, None),
        KeyBinding::new("cmd-shift-g", FindPrev, None),
        // Search bar
        KeyBinding::new("escape", CloseSearch, Some("SearchBar")),
        KeyBinding::new("enter", FindNext, Some("SearchBar")),
        KeyBinding::new("shift-enter", FindPrev, Some("SearchBar")),
        KeyBinding::new("backspace", SearchBackspace, Some("SearchBar")),
        // Replace bar
        KeyBinding::new("escape", CloseSearch, Some("ReplaceBar")),
        KeyBinding::new("enter", ReplaceOne, Some("ReplaceBar")),
        KeyBinding::new("cmd-enter", ReplaceAll, Some("ReplaceBar")),
        KeyBinding::new("backspace", ReplaceBackspace, Some("ReplaceBar")),
    ]);
}
