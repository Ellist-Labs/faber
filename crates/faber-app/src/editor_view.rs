use std::{cell::Cell, ops::Range, rc::Rc, sync::Arc, time::Duration};

use faber_editor::{
    ChangeSet, LanguageRegistry, Selection, SyntaxToken, Transaction,
    buffer::Document,
    cursor,
    edit_history::History,
    highlight::char_col_to_byte_col,
    markdown::{
        edit::{EnterAction, enter_action, looks_like_url, smart_wrap, toggle_checkbox},
        parse_markdown,
    },
    outline::{Outline, OutlineItem},
    search::Query,
};
use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, CursorStyle, EventEmitter, FocusHandle,
    Focusable, IntoElement, KeyDownEvent, ListHorizontalSizingBehavior, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Render, ScrollStrategy, ScrollWheelEvent,
    SharedString, TextRun, UniformListScrollHandle, Window, anchored, canvas, deferred, div, fill,
    font, point, prelude::*, px, size, svg, uniform_list,
};

use crate::input_helpers::{
    delete_char_before, delete_char_range, insert_at, split_at_char, word_start_before,
};
use crate::markdown_preview::MarkdownPreviewView;
use crate::settings_view::SettingsStore;
use crate::theme::RuntimeTheme;
use crate::ui::scrollbar::{start_drag, update_drag};
use crate::ui::{
    IconName, ScrollbarDrag, modal_backdrop, modal_container, modal_footer, render_scrollbar,
};
use rust_i18n::t;

// ── layout ─────────────────────────────────────────────────────────────────────
// Line height and char width live on RuntimeTheme (settings-scaled).

const GUTTER_COLS: f32 = 6.0;

// ── outline overlay geometry ───────────────────────────────────────────────────
const OUTLINE_INPUT_ROW_H: f32 = 45.;
const OUTLINE_FOOTER_H: f32 = 30.;
const OUTLINE_MODAL_H: f32 = 480.;
const OUTLINE_BODY_H: f32 = OUTLINE_MODAL_H - OUTLINE_INPUT_ROW_H - OUTLINE_FOOTER_H;

// ── actions ────────────────────────────────────────────────────────────────────

use crate::{
    Backspace, BoldSelection, CloseSearch, Copy, Cut, Delete, DeleteLine, DeleteToLineEnd,
    DeleteToLineStart, DeleteWordLeft, DeleteWordRight, Enter, FindNext, FindPrev, InputMoveEnd,
    InputMoveLeft, InputMoveRight, InputMoveStart, ItalicSelection, MoveDocEnd, MoveDocStart,
    MoveDown, MoveLeft, MoveLineEnd, MoveLineStart, MovePageDown, MovePageUp, MoveRight, MoveUp,
    MoveWordLeft, MoveWordRight, OpenReplace, OpenSearch, Paste, ProjectRoot, Redo, ReplaceAll,
    ReplaceBackspace, ReplaceOne, SearchBackspace, SelectAll, SelectDocEnd, SelectDocStart,
    SelectDown, SelectLeft, SelectLineEnd, SelectLineStart, SelectRight, SelectUp, SelectWordLeft,
    SelectWordRight, Tab, ToggleCheckbox, TogglePreview, ToggleReplace, ToggleSearchCase,
    ToggleSearchRegex, ToggleSearchWholeWord, Undo,
};

// ── EditorView ─────────────────────────────────────────────────────────────────

/// Emitted after every document mutation — drives auto-save and future
/// subscribers (LSP didChange, etc.).
pub enum EditorEvent {
    Edited,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SelectMode {
    Character,
    Word,
    Line,
}

pub struct EditorView {
    pub doc: Document,
    pub sel: Selection,
    pub history: History,
    pub registry: Arc<LanguageRegistry>,
    pub focus_handle: FocusHandle,

    // Line display cache — rebuilt after every document mutation.
    pub line_cache: Vec<SharedString>,
    pub line_starts: Vec<usize>,
    pub widest_line: usize,

    pub scroll_handle: UniformListScrollHandle,
    last_scroll_line: usize,
    mouse_selecting: bool,
    // Paint-time anchor: (line_idx, origin_x, origin_y) of the last rendered line.
    // Hit-testing derives both X column and row line from this instead of
    // reconstructing from scroll-handle math, so clicks always agree with glyphs.
    text_origin: Rc<Cell<Option<(usize, f32, f32)>>>,
    select_mode: SelectMode,
    select_anchor: std::ops::Range<usize>,

    // Markdown preview
    pub preview: Option<gpui::Entity<MarkdownPreviewView>>,
    pub show_preview: bool,
    /// Current document outline (headings for markdown, symbols for code).
    /// Updated per-edit via `after_edit`; drives the breadcrumb + overlay.
    pub outline: Arc<Outline>,
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
    // Word-occurrence cache: all whole-word matches for the identifier under the caret.
    // Rebuilt only when the word changes or the document is edited; not per-frame.
    pub word_occ: Vec<Range<usize>>,
    word_occ_key: Option<String>,
    pub search_whole_word: bool,
    pub search_regex: bool,

    pub scrollbar_drag: Option<ScrollbarDrag>,

    // Outline overlay (heading navigator for markdown files)
    pub outline_open: bool,
    pub outline_query: String,
    pub outline_cursor: usize,        // caret position in the query input
    pub outline_hover: Option<usize>, // index into the filtered list
    pub outline_highlight: Option<(usize, usize)>, // (start_line, end_line) of hovered section
    pub outline_handle: FocusHandle,

    // Cursor blink
    pub cursor_blink_on: bool,
    cursor_blink_epoch: u64,

    // Flash highlight — set by navigate_to, cleared after ~800ms
    pub flash_line: Option<usize>,

    // LSP diagnostics — set by Workspace after LspManager is wired up.
    pub diagnostic_store: Option<std::sync::Arc<faber_lsp::diagnostics::DiagnosticStore>>,

    // LSP hover
    pub lsp_manager: Option<std::sync::Arc<faber_lsp::manager::LspManager>>,
    hover_timer: Option<gpui::Task<()>>,
    /// Pixel Y of the last mouse-over; anchors the popover vertically.
    hover_pixel_y: f32,
    /// Char offset in the rope at the last mouse-over position.
    hover_char_offset: Option<usize>,
    /// Markdown / plain-text content from the last successful hover response.
    pub hover_content: Option<String>,
}

impl EditorView {
    pub fn new(path: &str, cx: &mut Context<EditorView>) -> Self {
        let registry = cx.global::<crate::Registry>().0.clone();
        let doc = Document::open_with_registry(path, &registry).unwrap_or_else(|_| {
            let mut d =
                Document::open_with_registry("/dev/null", &registry).expect("can't open /dev/null");
            d.path = std::path::PathBuf::from(path);
            d
        });
        Self::from_doc(doc, registry, cx)
    }

    pub fn from_doc(
        doc: Document,
        registry: Arc<LanguageRegistry>,
        cx: &mut Context<EditorView>,
    ) -> Self {
        let mut view = Self {
            doc,
            sel: Selection::default(),
            history: History::new(),
            registry: registry.clone(),
            focus_handle: cx.focus_handle(),
            line_cache: Vec::new(),
            line_starts: Vec::new(),
            widest_line: 0,
            scroll_handle: UniformListScrollHandle::new(),
            last_scroll_line: 0,
            mouse_selecting: false,
            text_origin: Rc::new(Cell::new(None)),
            select_mode: SelectMode::Character,
            select_anchor: 0..0,
            preview: None,
            show_preview: false,
            outline: Arc::new(Outline::default()),
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
            word_occ: Vec::new(),
            word_occ_key: None,
            search_whole_word: false,
            search_regex: false,
            scrollbar_drag: None,
            outline_open: false,
            outline_query: String::new(),
            outline_cursor: 0,
            outline_hover: None,
            outline_highlight: None,
            outline_handle: cx.focus_handle(),
            cursor_blink_on: true,
            cursor_blink_epoch: 0,
            flash_line: None,
            diagnostic_store: None,
            lsp_manager: None,
            hover_timer: None,
            hover_pixel_y: 0.0,
            hover_char_offset: None,
            hover_content: None,
        };
        view.rebuild_line_cache();
        if view.is_markdown() {
            let source = view.doc.rope.to_string();
            let md = parse_markdown(&source, &view.doc.rope, &registry);
            view.outline = Arc::new(Outline { items: md.outline });
        } else {
            view.outline = Arc::clone(&view.doc.outline);
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
                cx.background_executor()
                    .timer(Duration::from_millis(530))
                    .await;
                let cont = view
                    .update(cx, |this, cx| {
                        if this.cursor_blink_epoch != epoch {
                            return false;
                        }
                        this.cursor_blink_on = !this.cursor_blink_on;
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !cont {
                    break;
                }
            }
        })
        .detach();
    }

    fn clamp_sel(&mut self) {
        let len = self.doc.len_chars();
        self.sel.head = self.sel.head.min(len);
        self.sel.anchor = self.sel.anchor.min(len);
    }

    /// Scroll to `line`.
    fn scroll_to_line(&self, line: usize) {
        self.scroll_handle
            .scroll_to_item(line, ScrollStrategy::Center);
    }

    /// Top visible logical line.
    fn top_visible_line(&self, t: &RuntimeTheme) -> usize {
        let off = self.scroll_handle.0.borrow().base_handle.offset();
        let y = f32::from(off.y);
        // offset.y is ≤ 0 when scrolled down; negate to get pixels scrolled
        (-y / t.line_height_code).floor().max(0.0) as usize
    }

    fn clear_word_occ(&mut self) {
        self.word_occ.clear();
        self.word_occ_key = None;
    }

    /// Post-mutation bookkeeping — every document edit funnels through here.
    fn after_edit(&mut self, cx: &mut Context<Self>) {
        self.clear_word_occ();
        self.update_matches();
        if self.is_markdown() {
            self.schedule_markdown_update(cx);
        } else {
            // Code outline is recomputed synchronously inside Document::apply;
            // pull the fresh Arc here (cheap clone).
            self.outline = Arc::clone(&self.doc.outline);
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
        let registry = self.registry.clone();
        let update_preview = self.show_preview && self.preview.is_some();
        cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(75))
                .await;
            let source = rope.to_string();
            let md = Arc::new(parse_markdown(&source, &rope, &registry));
            let outline = Arc::new(Outline {
                items: md.outline.clone(),
            });
            let _ = view.update(cx, |this, cx| {
                if this.outline_gen != current_gen {
                    return;
                }
                this.outline = outline;
                if update_preview && let Some(ref preview) = this.preview {
                    let md2 = Arc::clone(&md);
                    preview.update(cx, |pv, _cx| pv.apply_md(md2));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.history.commit();
        if !self.sel.is_empty() {
            let inverse = self.doc.delete(self.sel.range());
            self.history.push_change(inverse);
            let pos = self.sel.start();
            self.sel = Selection::collapsed(pos, &self.doc.rope);
        }
        let pos = self.sel.head;
        let inverse = self.doc.insert(pos, text);
        let end = pos + text.chars().count();
        self.history.push_insert(inverse);
        self.sel = Selection::collapsed(end, &self.doc.rope);
        self.after_edit(cx);
    }

    fn do_backspace(&mut self, cx: &mut Context<Self>) {
        self.history.commit();
        if !self.sel.is_empty() {
            let edit = self.doc.delete(self.sel.range());
            self.history.push_change(edit);
            let pos = self.sel.start();
            self.sel = Selection::collapsed(pos, &self.doc.rope);
        } else if self.sel.head > 0 {
            let pos = self.sel.head - 1;
            let edit = self.doc.delete(pos..self.sel.head);
            self.history.push_change(edit);
            self.sel = Selection::collapsed(pos, &self.doc.rope);
        }
        self.after_edit(cx);
    }

    fn do_delete_fwd(&mut self, cx: &mut Context<Self>) {
        self.history.commit();
        if !self.sel.is_empty() {
            let edit = self.doc.delete(self.sel.range());
            self.history.push_change(edit);
            let pos = self.sel.start();
            self.sel = Selection::collapsed(pos, &self.doc.rope);
        } else if self.sel.head < self.doc.len_chars() {
            let edit = self.doc.delete(self.sel.head..self.sel.head + 1);
            self.history.push_change(edit);
            self.sel = Selection::collapsed(self.sel.head, &self.doc.rope);
        }
        self.after_edit(cx);
    }

    fn do_delete_word_left(&mut self, cx: &mut Context<Self>) {
        self.history.commit();
        if !self.sel.is_empty() {
            let edit = self.doc.delete(self.sel.range());
            self.history.push_change(edit);
            let pos = self.sel.start();
            self.sel = Selection::collapsed(pos, &self.doc.rope);
        } else {
            let word_start = cursor::move_word_left(&self.doc.rope, self.sel, false).head;
            if word_start < self.sel.head {
                let edit = self.doc.delete(word_start..self.sel.head);
                self.history.push_change(edit);
                self.sel = Selection::collapsed(word_start, &self.doc.rope);
            }
        }
        self.after_edit(cx);
    }

    fn do_delete_word_right(&mut self, cx: &mut Context<Self>) {
        self.history.commit();
        if !self.sel.is_empty() {
            let edit = self.doc.delete(self.sel.range());
            self.history.push_change(edit);
            let pos = self.sel.start();
            self.sel = Selection::collapsed(pos, &self.doc.rope);
        } else {
            let word_end = cursor::move_word_right(&self.doc.rope, self.sel, false).head;
            if word_end > self.sel.head {
                let edit = self.doc.delete(self.sel.head..word_end);
                self.history.push_change(edit);
                self.sel = Selection::collapsed(self.sel.head, &self.doc.rope);
            }
        }
        self.after_edit(cx);
    }

    fn do_delete_to_line_start(&mut self, cx: &mut Context<Self>) {
        self.history.commit();
        let head = self.sel.head;
        let line_idx = self
            .doc
            .rope
            .char_to_line(head.min(self.doc.len_chars().saturating_sub(1)));
        let line_start = self.doc.rope.line_to_char(line_idx);
        if !self.sel.is_empty() {
            let edit = self.doc.delete(self.sel.range());
            self.history.push_change(edit);
            let pos = self.sel.start();
            self.sel = Selection::collapsed(pos, &self.doc.rope);
        } else if line_start < head {
            let edit = self.doc.delete(line_start..head);
            self.history.push_change(edit);
            self.sel = Selection::collapsed(line_start, &self.doc.rope);
        }
        self.after_edit(cx);
    }

    fn do_delete_to_line_end(&mut self, cx: &mut Context<Self>) {
        self.history.commit();
        if !self.sel.is_empty() {
            let edit = self.doc.delete(self.sel.range());
            self.history.push_change(edit);
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
                self.history.push_change(edit);
                self.sel = Selection::collapsed(head, &self.doc.rope);
            }
        }
        self.after_edit(cx);
    }

    fn do_delete_line(&mut self, cx: &mut Context<Self>) {
        self.history.commit();
        let len = self.doc.len_chars();
        if len == 0 {
            return;
        }
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
            self.history.push_change(edit);
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
            self.line_cache
                .push(SharedString::from(content.to_string()));
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
        window: &mut Window,
    ) -> Option<usize> {
        let st = self.scroll_handle.0.borrow();
        let vb = st.base_handle.bounds();
        let off = st.base_handle.offset();
        drop(st);
        if vb.size.width == gpui::Pixels::ZERO {
            return None;
        }
        let px_y = f32::from(p.y);
        let px_x = f32::from(p.x);
        let (line, rel_x) = if let Some((anchor_line, origin_x, origin_y)) = self.text_origin.get()
        {
            // Measured at paint time — identical geometry to glyphs for both axes.
            let line_delta = ((px_y - origin_y) / t.line_height_code).floor();
            let line = (anchor_line as f32 + line_delta).max(0.0) as usize;
            (line, px_x - origin_x)
        } else {
            // Pre-first-paint fallback (nothing visible yet, click is inert).
            let gutter_px = if show_line_numbers {
                (GUTTER_COLS + 2.0) * t.char_w_code
            } else {
                0.0
            };
            let vb_y = f32::from(vb.origin.y);
            let vb_x = f32::from(vb.origin.x);
            let off_y = f32::from(off.y);
            let off_x = f32::from(off.x);
            let rel_y = (px_y - vb_y) - off_y;
            let line = (rel_y / t.line_height_code).floor().max(0.0) as usize;
            (line, (px_x - vb_x) - 8.0 - gutter_px - off_x)
        };
        let line = line.min(self.line_starts.len().saturating_sub(1));
        let shaped = self.shape_editor_line(line, t, window, &[]);
        let byte_off = shaped.closest_index_for_x(px(rel_x.max(0.0)));
        let line_str: &str = &self.line_cache[line];
        let char_col = faber_editor::highlight::byte_col_to_char_col(line_str, byte_off as u32);
        Some(self.line_starts[line] + char_col)
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
        let Some(offset) = self.offset_at(ev.position, &t, show_line_numbers, window) else {
            return;
        };

        match ev.click_count {
            2 => {
                let sel = cursor::word_at(&self.doc.rope, offset);
                self.select_anchor = sel.anchor..sel.head;
                self.select_mode = SelectMode::Word;
                self.sel = sel;
            }
            count if count >= 3 => {
                let sel = cursor::line_selection(&self.doc.rope, offset);
                self.select_anchor = sel.anchor..sel.head;
                self.select_mode = SelectMode::Line;
                self.sel = sel;
            }
            _ => {
                if ev.modifiers.shift {
                    let goal_col = cursor::col_of(&self.doc.rope, offset);
                    self.sel = Selection {
                        anchor: self.sel.anchor,
                        head: offset,
                        goal_col,
                    };
                } else {
                    self.sel = Selection::collapsed(offset, &self.doc.rope);
                    self.select_anchor = offset..offset;
                }
                self.select_mode = SelectMode::Character;
            }
        }
        self.mouse_selecting = true;
        self.reset_blink(cx);
        self.last_scroll_line = self
            .doc
            .rope
            .char_to_line(offset.min(self.doc.len_chars().saturating_sub(1)));
        cx.notify();
    }

    fn on_mouse_move_editor(
        &mut self,
        ev: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Track position for hover when not dragging; compute offset while Window is available.
        if !ev.dragging() {
            let t = cx.global::<RuntimeTheme>().clone();
            let show_ln = cx.global::<SettingsStore>().0.line_numbers;
            let offset = self.offset_at(ev.position, &t, show_ln, window);
            self.schedule_hover(ev.position, offset, cx);
        }

        if !ev.dragging() || !self.mouse_selecting {
            return;
        }
        let t = cx.global::<RuntimeTheme>().clone();
        let show_line_numbers = cx.global::<SettingsStore>().0.line_numbers;
        let Some(offset) = self.offset_at(ev.position, &t, show_line_numbers, window) else {
            return;
        };

        let new_sel = match self.select_mode {
            SelectMode::Character => {
                let goal_col = cursor::col_of(&self.doc.rope, offset);
                Selection {
                    anchor: self.sel.anchor,
                    head: offset,
                    goal_col,
                }
            }
            SelectMode::Word => {
                let word = cursor::word_at(&self.doc.rope, offset);
                let anchor_range = &self.select_anchor;
                if offset <= anchor_range.start {
                    Selection {
                        anchor: anchor_range.end,
                        head: word.anchor,
                        goal_col: cursor::col_of(&self.doc.rope, word.anchor),
                    }
                } else {
                    Selection {
                        anchor: anchor_range.start,
                        head: word.head,
                        goal_col: cursor::col_of(&self.doc.rope, word.head),
                    }
                }
            }
            SelectMode::Line => {
                let line_sel = cursor::line_selection(&self.doc.rope, offset);
                let anchor_range = &self.select_anchor;
                if offset <= anchor_range.start {
                    Selection {
                        anchor: anchor_range.end,
                        head: line_sel.anchor,
                        goal_col: cursor::col_of(&self.doc.rope, line_sel.anchor),
                    }
                } else {
                    Selection {
                        anchor: anchor_range.start,
                        head: line_sel.head,
                        goal_col: cursor::col_of(&self.doc.rope, line_sel.head),
                    }
                }
            }
        };
        self.sel = new_sel;
        cx.notify();
    }

    fn on_mouse_up_editor(
        &mut self,
        _ev: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mouse_selecting {
            self.mouse_selecting = false;
            cx.notify();
        }
    }

    // ── LSP hover ─────────────────────────────────────────────────────────────

    fn schedule_hover(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        offset: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        if self.hover_content.is_some() {
            self.hover_content = None;
            cx.notify();
        }
        self.hover_pixel_y = f32::from(position.y);
        self.hover_char_offset = offset;
        self.hover_timer = Some(cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            view.update(cx, |ev, cx| ev.trigger_hover(cx)).ok();
        }));
    }

    fn trigger_hover(&mut self, cx: &mut Context<Self>) {
        let Some(offset) = self.hover_char_offset else {
            return;
        };
        let Some(mgr) = self.lsp_manager.clone() else {
            return;
        };
        let path = self.doc.path.clone();
        let uri = match url::Url::from_file_path(&path) {
            Ok(u) => u,
            Err(_) => return,
        };
        let encoding = mgr.position_encoding_for_uri(&uri);
        let lsp_pos = faber_lsp::position::to_lsp_position(&self.doc.rope, offset, encoding);
        let params_json = serde_json::json!({
            "textDocument": { "uri": uri.as_str() },
            "position": { "line": lsp_pos.line, "character": lsp_pos.character }
        });
        let Some(rx) = mgr.request_for_document(&uri, "textDocument/hover", params_json) else {
            return;
        };
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { rx.recv_timeout(std::time::Duration::from_secs(5)) })
                .await;
            let content = match result {
                Ok(Ok(val)) => faber_lsp::hover::extract_hover_text(&val),
                _ => None,
            };
            view.update(cx, |ev, cx| {
                ev.hover_content = content;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn dismiss_hover(&mut self, cx: &mut Context<Self>) {
        self.hover_timer = None;
        if self.hover_content.is_some() {
            self.hover_content = None;
            cx.notify();
        }
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
        self.sel = Selection {
            anchor: m.start,
            head: m.end,
            goal_col: 0,
        };
        cx.notify();
    }

    fn do_find_prev(&mut self, cx: &mut Context<Self>) {
        if self.matches.is_empty() {
            return;
        }
        self.match_idx = if self.match_idx == 0 {
            self.matches.len() - 1
        } else {
            self.match_idx - 1
        };
        let m = self.matches[self.match_idx].clone();
        self.sel = Selection {
            anchor: m.start,
            head: m.end,
            goal_col: 0,
        };
        cx.notify();
    }

    fn do_replace_one(&mut self, cx: &mut Context<Self>) {
        if self.matches.is_empty() {
            return;
        }
        let m = self.matches[self.match_idx].clone();
        let replacement = self.replace_query.clone();
        self.history.commit();
        let tx = Transaction::replace(&self.doc.rope, m.clone(), replacement.clone());
        let inverse = self.doc.apply(tx);
        self.history.push_change(inverse);
        let new_pos = m.start + replacement.chars().count();
        self.sel = Selection::collapsed(new_pos, &self.doc.rope);
        self.after_edit(cx);
    }

    fn do_replace_all(&mut self, cx: &mut Context<Self>) {
        if self.matches.is_empty() {
            return;
        }
        self.history.commit();
        let doc_len = self.doc.rope.len_chars();
        let changeset = ChangeSet::from_changes(
            doc_len,
            self.matches
                .iter()
                .map(|r| (r.start, r.end, self.replace_query.clone())),
        );
        let tx = Transaction::from_changeset(changeset);
        let inverse = self.doc.apply(tx);
        self.history.push_change(inverse);
        self.matches.clear();
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
    fn on_select_doc_start(&mut self, _: &SelectDocStart, _: &mut Window, cx: &mut Context<Self>) {
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
    fn on_delete_word_left(&mut self, _: &DeleteWordLeft, _: &mut Window, cx: &mut Context<Self>) {
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
                    let char_start =
                        line_char_start + line_str[..delete_cols.start].chars().count();
                    let char_end = line_char_start + line_str[..delete_cols.end].chars().count();
                    self.history.commit();
                    let edit = self.doc.delete(char_start..char_end);
                    self.history.push_change(edit);
                    self.sel = Selection::collapsed(char_start, &self.doc.rope);
                    let edit = self.doc.insert(char_start, "\n");
                    let end = char_start + 1;
                    self.history.push_insert(edit);
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
            self.history.push_change(edit);
            let pos = self.sel.start();
            self.sel = Selection::collapsed(pos, &self.doc.rope);
            self.after_edit(cx);
        }
    }
    fn on_paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(item) = cx.read_from_clipboard()
            && let Some(text) = item.text()
        {
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

    fn on_bold_selection(&mut self, _: &BoldSelection, _: &mut Window, cx: &mut Context<Self>) {
        if self.sel.is_empty() {
            return;
        }
        let selected: String = self.doc.rope.slice(self.sel.range()).to_string();
        let wrapped = smart_wrap(&selected, "**");
        self.history.commit();
        self.insert_text(&wrapped, cx);
        self.history.commit();
    }

    fn on_italic_selection(&mut self, _: &ItalicSelection, _: &mut Window, cx: &mut Context<Self>) {
        if self.sel.is_empty() {
            return;
        }
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
            let char_end = line_char_start + line_str[..byte_range.end].chars().count();
            self.history.commit();
            let edit = self.doc.delete(char_start..char_end);
            self.history.push_change(edit);
            let edit = self.doc.insert(char_start, replacement);
            self.history.push_insert(edit);
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
    fn on_search_backspace(&mut self, _: &SearchBackspace, _: &mut Window, cx: &mut Context<Self>) {
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
    fn on_input_move_left(
        &mut self,
        _: &InputMoveLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.search_handle.is_focused(window) {
            self.search_cursor = self.search_cursor.saturating_sub(1);
        } else if self.replace_handle.is_focused(window) {
            self.replace_cursor = self.replace_cursor.saturating_sub(1);
        }
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_input_move_right(
        &mut self,
        _: &InputMoveRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.search_handle.is_focused(window) {
            self.search_cursor = (self.search_cursor + 1).min(self.search_query.chars().count());
        } else if self.replace_handle.is_focused(window) {
            self.replace_cursor = (self.replace_cursor + 1).min(self.replace_query.chars().count());
        }
        self.reset_blink(cx);
        cx.notify();
    }
    fn on_input_move_start(
        &mut self,
        _: &InputMoveStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                ("backspace", false) => {
                    self.do_delete_to_line_start(cx);
                    return;
                }
                ("delete", false) => {
                    self.do_delete_to_line_end(cx);
                    return;
                }
                ("k", true) => {
                    self.do_delete_line(cx);
                    return;
                }
                _ => {}
            }
        }
        // opt+backspace: delete word backward in search / replace / outline inputs
        if ks.modifiers.alt
            && !ks.modifiers.platform
            && !ks.modifiers.control
            && !ks.modifiers.shift
            && ks.key.as_str() == "backspace"
        {
            if self.outline_open && self.outline_handle.is_focused(window) {
                let ws = word_start_before(&self.outline_query, self.outline_cursor);
                if ws < self.outline_cursor {
                    self.outline_query =
                        delete_char_range(&self.outline_query, ws, self.outline_cursor);
                    self.outline_cursor = ws;
                    self.outline_hover = None;
                    cx.notify();
                }
                return;
            }
            if self.show_search && self.search_handle.is_focused(window) {
                let ws = word_start_before(&self.search_query, self.search_cursor);
                if ws < self.search_cursor {
                    self.search_query =
                        delete_char_range(&self.search_query, ws, self.search_cursor);
                    self.search_cursor = ws;
                    self.update_matches();
                    self.reset_blink(cx);
                    cx.notify();
                }
                return;
            }
            if self.show_replace && self.replace_handle.is_focused(window) {
                let ws = word_start_before(&self.replace_query, self.replace_cursor);
                if ws < self.replace_cursor {
                    self.replace_query =
                        delete_char_range(&self.replace_query, ws, self.replace_cursor);
                    self.replace_cursor = ws;
                    self.reset_blink(cx);
                    cx.notify();
                }
                return;
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
                            self.outline_query =
                                delete_char_before(&self.outline_query, self.outline_cursor);
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
                        let first_line = self
                            .outline
                            .items
                            .iter()
                            .find(|e| q.is_empty() || e.name.to_lowercase().contains(&q))
                            .map(|e| e.source_line);
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
            if ks.modifiers.control || ks.modifiers.platform {
                return;
            }
            if raw_text.chars().any(|c| c.is_control()) {
                return;
            }
            self.outline_query = insert_at(&self.outline_query, self.outline_cursor, raw_text);
            self.outline_cursor += raw_text.chars().count();
            self.outline_hover = None;
            cx.notify();
            return;
        }

        let Some(ref raw_text) = ks.key_char else {
            return;
        };
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
                .map(|c| {
                    if c.is_ascii_alphabetic() {
                        if c.is_ascii_uppercase() {
                            c.to_ascii_lowercase()
                        } else {
                            c.to_ascii_uppercase()
                        }
                    } else {
                        c
                    }
                })
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
    /// - `outline_hl`: if this line falls within the range, apply a subtle section highlight.
    #[allow(clippy::too_many_arguments)]
    fn render_line(
        &self,
        line_idx: usize,
        t: &RuntimeTheme,
        show_line_numbers: bool,
        cursor_visible: bool,
        outline_hl: Option<(usize, usize)>,
        flash_line: Option<usize>,
        bracket_hl: Option<(usize, usize)>,
        window: &mut Window,
        file_diags: &[faber_lsp::diagnostics::DiagnosticEntry],
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

        let hl_this_line = outline_hl.is_some_and(|(s, e)| line_idx >= s && line_idx < e);
        let is_flash = flash_line == Some(line_idx);

        // Single render path: every line is shaped identically whether or not it
        // carries a caret/selection, so placing the caret never re-flows glyphs.
        let line_text = self.line_cache[line_idx].clone();
        let line_str: &str = &line_text;
        let line_char_count = line_str.chars().count();
        let line_char_end = line_char_start + line_char_count;

        let cursor_col = if cursor_on_line {
            head - line_char_start
        } else {
            0
        };

        let sel_start = self.sel.start();
        let sel_end = self.sel.end();
        let line_sel_start = sel_start.saturating_sub(line_char_start);
        let line_sel_end = if sel_end < line_char_end {
            sel_end.saturating_sub(line_char_start)
        } else {
            line_char_count
        };

        let match_ranges_on_line: Vec<(usize, usize, bool)> = self
            .matches
            .iter()
            .enumerate()
            .filter(|(_, m)| m.start <= line_char_end && m.end >= line_char_start)
            .map(|(i, m)| {
                let s = m.start.saturating_sub(line_char_start);
                let e = if m.end < line_char_end {
                    m.end - line_char_start
                } else {
                    line_char_count
                };
                (s, e, i == self.match_idx)
            })
            .collect();

        let make_row =
            |hl_this_line: bool, is_flash: bool, cursor_on_line: bool, t: &RuntimeTheme| {
                let line_h = t.line_height_code;
                let hl_color = t.line_highlight;
                let flash_color = t.match_bg;
                let base = div()
                    .flex()
                    .flex_row()
                    .relative()
                    .h(px(line_h))
                    .when(hl_this_line, |r| r.bg(hl_color))
                    .when(is_flash, |r| r.bg(flash_color))
                    .font_family(t.mono_family.clone())
                    .text_size(px(t.font_size_code));
                // Full-width current-line band: absolute div extends far left/right so the
                // highlight covers gutter + content + padding, clipped by the scroll viewport.
                if cursor_on_line && !is_flash {
                    base.child(
                        div()
                            .absolute()
                            .left(px(-9999.))
                            .w(px(19999.))
                            .top(px(0.))
                            .h(px(line_h))
                            .bg(hl_color),
                    )
                } else {
                    base
                }
            };

        // ── Non-wrap: shaped line + overlay quads (highlights under text, caret on top) ──
        let shaped = self.shape_editor_line(line_idx, t, window, file_diags);

        let mut hl_rects: Vec<(usize, usize, gpui::Hsla)> = Vec::new();
        if has_sel && line_sel_start < line_sel_end {
            let s_byte = char_col_to_byte_col(line_str, line_sel_start);
            let e_byte = char_col_to_byte_col(line_str, line_sel_end);
            if s_byte < e_byte {
                hl_rects.push((s_byte, e_byte, t.selection));
            }
        } else {
            // All occurrences of the word under the caret (no active selection).
            for occ in &self.word_occ {
                if occ.start <= line_char_end && occ.end > line_char_start {
                    let local_s = occ
                        .start
                        .saturating_sub(line_char_start)
                        .min(line_char_count);
                    let local_e = occ.end.saturating_sub(line_char_start).min(line_char_count);
                    if local_s < local_e {
                        let s_byte = char_col_to_byte_col(line_str, local_s);
                        let e_byte = char_col_to_byte_col(line_str, local_e);
                        if s_byte < e_byte {
                            hl_rects.push((s_byte, e_byte, t.word_highlight));
                        }
                    }
                }
            }
            // Innermost enclosing bracket pair — one highlight per bracket char.
            if let Some((open_off, close_off)) = bracket_hl {
                for bracket_char in [open_off, close_off] {
                    if bracket_char >= line_char_start && bracket_char < line_char_end {
                        let local = bracket_char - line_char_start;
                        let s_byte = char_col_to_byte_col(line_str, local);
                        // bracket chars may be multi-byte; advance to next char boundary
                        let e_byte = char_col_to_byte_col(line_str, local + 1);
                        if s_byte < e_byte {
                            hl_rects.push((s_byte, e_byte, t.word_highlight));
                        }
                    }
                }
            }
        }
        for (s, e, active) in &match_ranges_on_line {
            let s_byte = char_col_to_byte_col(line_str, *s);
            let e_byte = char_col_to_byte_col(line_str, *e);
            if s_byte < e_byte {
                hl_rects.push((
                    s_byte,
                    e_byte,
                    if *active { t.match_active } else { t.match_bg },
                ));
            }
        }
        // Selection continuing past EOL owns the newline: paint a stub so
        // multi-line selections have no gaps on short/empty lines.
        let sel_stub = has_sel && sel_start <= line_char_end && sel_end > line_char_end;

        let caret_byte: Option<usize> =
            (cursor_on_line && cursor_visible).then(|| char_col_to_byte_col(line_str, cursor_col));

        let line_h = px(t.line_height_code);
        let cursor_color = t.cursor;
        let sel_color = t.selection;
        let stub_w = px(t.char_w_code * 0.5);
        let content_w = shaped.width;
        let anchor_cell = self.text_origin.clone();

        let content = canvas(
            move |_bounds, _window, _cx| shaped,
            move |bounds, shaped, window, cx| {
                let origin = bounds.origin;
                anchor_cell.set(Some((line_idx, f32::from(origin.x), f32::from(origin.y))));
                for (s_byte, e_byte, color) in &hl_rects {
                    let x1 = origin.x + shaped.x_for_index(*s_byte);
                    let x2 = origin.x + shaped.x_for_index(*e_byte);
                    if x2 > x1 {
                        window.paint_quad(fill(
                            Bounds::new(point(x1, origin.y), size(x2 - x1, line_h)),
                            *color,
                        ));
                    }
                }
                if sel_stub {
                    window.paint_quad(fill(
                        Bounds::new(
                            point(origin.x + shaped.width, origin.y),
                            size(stub_w, line_h),
                        ),
                        sel_color,
                    ));
                }
                let _ = shaped.paint(origin, line_h, window, cx);
                if let Some(caret_byte) = caret_byte {
                    let x = origin.x + shaped.x_for_index(caret_byte);
                    window.paint_quad(fill(
                        Bounds::new(point(x, origin.y), size(px(2.0), line_h)),
                        cursor_color,
                    ));
                }
            },
        )
        .w(content_w)
        .h(line_h)
        .flex_shrink_0();

        let row = make_row(hl_this_line, is_flash, cursor_on_line, t);
        let row = if show_line_numbers {
            row.child(
                div()
                    .flex_shrink_0()
                    .w(px(GUTTER_COLS * t.char_w_code))
                    .text_size(px(t.font_size_gutter))
                    .text_color(if cursor_on_line {
                        t.gutter_active
                    } else {
                        t.gutter
                    })
                    .child(format!("{:>4}", line_idx + 1)),
            )
            .child(div().flex_shrink_0().w(px(2.0 * t.char_w_code)))
        } else {
            row
        };
        row.child(content).into_any_element()
    }

    /// Shape one logical line with its syntax runs. Single source of truth for
    /// painting AND mouse hit-testing — identical runs → identical glyph
    /// geometry → clicks land exactly where glyphs are painted. GPUI's line
    /// layout cache makes repeat shaping of unchanged lines free.
    fn shape_editor_line(
        &self,
        line_idx: usize,
        t: &RuntimeTheme,
        window: &mut Window,
        file_diags: &[faber_lsp::diagnostics::DiagnosticEntry],
    ) -> gpui::ShapedLine {
        let text = self.line_cache[line_idx].clone();
        let line_diags: Vec<faber_lsp::diagnostics::DiagnosticEntry> = file_diags
            .iter()
            .filter(|e| e.range.lsp_line as usize == line_idx)
            .cloned()
            .collect();
        let runs = crate::buffer_view::build_text_runs(
            &text,
            self.doc.highlight_spans(line_idx),
            t,
            &line_diags,
        );
        window
            .text_system()
            .shape_line(text, px(t.font_size_code), &runs, None)
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
                .text_color(if active {
                    t.text_on_accent
                } else {
                    t.text_subtle
                })
                .when(active, move |el| {
                    el.bg(t.accent).hover(move |s| s.bg(t.accent_hover))
                })
                .when(!active, move |el| el.hover(move |s| s.bg(hover_bg)))
                .child(label)
        };

        // ── replace-toggle (leftmost, Add = show replace, Remove = hide) ───────
        let toggle_icon = if show_replace {
            IconName::Remove
        } else {
            IconName::Add
        };
        let toggle_color = if show_replace {
            t.accent
        } else {
            t.text_subtle
        };
        let replace_toggle = icon_btn_base("toggle-replace")
            .when(show_replace, |el| el.bg(t.line_highlight))
            .child(
                svg()
                    .path(toggle_icon.path())
                    .size(px(14.))
                    .text_color(toggle_color),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, _, window, cx| v.on_toggle_replace(&ToggleReplace, window, cx)),
            );

        // ── navigation group: [◄ prev] [count] [► next] ───────────────────────
        let prev_btn = icon_btn_base("search-prev")
            .child(
                svg()
                    .path(IconName::ChevronLeft.path())
                    .size(px(14.))
                    .text_color(t.text_subtle),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, _, window, cx| v.on_find_prev(&FindPrev, window, cx)),
            );

        let next_btn = icon_btn_base("search-next")
            .child(
                svg()
                    .path(IconName::ChevronRight.path())
                    .size(px(14.))
                    .text_color(t.text_subtle),
            )
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
        let case_chip = chip("toggle-case", "Aa", self.search_case_sensitive, t).on_mouse_down(
            MouseButton::Left,
            cx.listener(|v, _, window, cx| v.on_toggle_search_case(&ToggleSearchCase, window, cx)),
        );

        let word_chip = chip("toggle-word", "W", self.search_whole_word, t).on_mouse_down(
            MouseButton::Left,
            cx.listener(|v, _, window, cx| {
                v.on_toggle_search_whole_word(&ToggleSearchWholeWord, window, cx)
            }),
        );

        let regex_chip = chip("toggle-regex", ".*", self.search_regex, t).on_mouse_down(
            MouseButton::Left,
            cx.listener(|v, _, window, cx| {
                v.on_toggle_search_regex(&ToggleSearchRegex, window, cx)
            }),
        );

        // ── close button (rightmost) ───────────────────────────────────────────
        let close_btn = icon_btn_base("search-close")
            .child(
                svg()
                    .path(IconName::Close.path())
                    .size(px(13.))
                    .text_color(t.text_subtle),
            )
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
                .bg(if focused {
                    t.line_highlight
                } else {
                    t.bg_sunken
                })
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
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|v, _, window, cx| {
                            window.focus(&v.search_handle);
                            v.search_cursor = v.search_query.chars().count();
                            v.reset_blink(cx);
                        }),
                    )
                    .child(if !search_focused && self.search_query.is_empty() {
                        div()
                            .text_color(t.text_subtle)
                            .child(t!("search.placeholder").to_string())
                            .into_any()
                    } else {
                        let cur_on = search_focused && caret_visible;
                        let s_before_owned = s_before.clone();
                        let s_after_owned = s_after.clone();
                        let font_sz = px(t.font_size_code);
                        let line_h = px(cursor_h);
                        let ui_family = t.ui_family.clone();
                        let text_col = t.text;
                        let cursor_col_val = t.cursor;
                        let cursor_color = if cur_on {
                            cursor_col_val
                        } else {
                            gpui::hsla(0., 0., 0., 0.)
                        };
                        let full_text = format!("{}{}", s_before_owned, s_after_owned);
                        let caret_byte = s_before_owned.len();
                        canvas(
                            move |_bounds, window, _cx| {
                                let runs = if full_text.is_empty() {
                                    vec![]
                                } else {
                                    vec![TextRun {
                                        len: full_text.len(),
                                        font: font(ui_family.clone()),
                                        color: text_col,
                                        background_color: None,
                                        underline: None,
                                        strikethrough: None,
                                    }]
                                };
                                window.text_system().shape_line(
                                    SharedString::from(full_text),
                                    font_sz,
                                    &runs,
                                    None,
                                )
                            },
                            move |bounds, shaped, window, cx| {
                                let origin = bounds.origin;
                                let _ = shaped.paint(origin, line_h, window, cx);
                                let cx_x = origin.x + shaped.x_for_index(caret_byte);
                                window.paint_quad(fill(
                                    Bounds::new(point(cx_x, origin.y), size(px(2.0), line_h)),
                                    cursor_color,
                                ));
                            },
                        )
                        .flex_1()
                        .h(line_h)
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
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|v, _, window, cx| {
                            window.focus(&v.replace_handle);
                            v.replace_cursor = v.replace_query.chars().count();
                            v.reset_blink(cx);
                        }),
                    )
                    .child(if !replace_focused && self.replace_query.is_empty() {
                        div()
                            .text_color(t.text_subtle)
                            .child(t!("search.replace_placeholder").to_string())
                            .into_any()
                    } else {
                        let cur_on = replace_focused && caret_visible;
                        let r_before_owned = r_before.clone();
                        let r_after_owned = r_after.clone();
                        let font_sz = px(t.font_size_code);
                        let line_h = px(cursor_h);
                        let ui_family2 = t.ui_family.clone();
                        let text_col = t.text;
                        let cursor_col_val = t.cursor;
                        let cursor_color = if cur_on {
                            cursor_col_val
                        } else {
                            gpui::hsla(0., 0., 0., 0.)
                        };
                        let full_text = format!("{}{}", r_before_owned, r_after_owned);
                        let caret_byte = r_before_owned.len();
                        canvas(
                            move |_bounds, window, _cx| {
                                let runs = if full_text.is_empty() {
                                    vec![]
                                } else {
                                    vec![TextRun {
                                        len: full_text.len(),
                                        font: font(ui_family2.clone()),
                                        color: text_col,
                                        background_color: None,
                                        underline: None,
                                        strikethrough: None,
                                    }]
                                };
                                window.text_system().shape_line(
                                    SharedString::from(full_text),
                                    font_sz,
                                    &runs,
                                    None,
                                )
                            },
                            move |bounds, shaped, window, cx| {
                                let origin = bounds.origin;
                                let _ = shaped.paint(origin, line_h, window, cx);
                                let cx_x = origin.x + shaped.x_for_index(caret_byte);
                                window.paint_quad(fill(
                                    Bounds::new(point(cx_x, origin.y), size(px(2.0), line_h)),
                                    cursor_color,
                                ));
                            },
                        )
                        .flex_1()
                        .h(line_h)
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
        self.doc
            .language
            .as_ref()
            .is_some_and(|l| l.id.0 == "markdown")
    }

    fn on_toggle_preview(
        &mut self,
        _: &TogglePreview,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_markdown() {
            return;
        }
        let entering_preview = !self.show_preview;
        self.show_preview = entering_preview;

        if entering_preview {
            let source_line = self
                .doc
                .rope
                .char_to_line(self.sel.head.min(self.doc.len_chars().saturating_sub(1)));
            let registry = self.registry.clone();
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
            self.scroll_handle
                .scroll_to_item(source_line, ScrollStrategy::Top);
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
            let icon = if show_preview {
                IconName::Code
            } else {
                IconName::Visibility
            };
            let color = if show_preview {
                t.accent
            } else {
                t.text_subtle
            };
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
                .child(
                    gpui::svg()
                        .path(icon.path())
                        .size(px(14.))
                        .text_color(color),
                )
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

        // Breadcrumb: compute the symbol/heading stack at the current top visible line.
        let top_line = self.top_visible_line(&t);
        let crumb_stack = if !self.outline.is_empty() {
            crate::editor_logic::breadcrumb_stack(&self.outline, top_line)
        } else {
            vec![]
        };

        let can_open_outline = !self.outline.is_empty();
        let crumb_hover_bg = t.line_highlight;
        let outline_active = self.outline_open;
        let sep_color = t.text_subtle;
        let path_color = if can_open_outline {
            t.text
        } else {
            t.text_subtle
        };

        // Build breadcrumb content as individual colored elements so each segment
        // can carry syntax-appropriate color (fn→function, struct→type, mod→namespace).
        let crumb_content = {
            let mut inner = div()
                .flex()
                .flex_row()
                .items_center()
                .overflow_hidden()
                .whitespace_nowrap()
                .gap(px(2.));
            // File path segment
            inner = inner.child(
                div()
                    .text_color(path_color)
                    .text_ellipsis()
                    .overflow_hidden()
                    .child(path_label.clone()),
            );
            for item in &crumb_stack {
                let seg_color = crate::editor_logic::context_to_token(item.context.as_deref())
                    .map(|tok| crate::buffer_view::token_color(tok, &t))
                    .unwrap_or(t.text);
                inner = inner
                    .child(div().text_color(sep_color).flex_shrink_0().child(" ›"))
                    .child(
                        div()
                            .text_color(seg_color)
                            .text_ellipsis()
                            .overflow_hidden()
                            .child(item.name.clone()),
                    );
            }
            inner
        };

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
            .when(outline_active, |el| el.bg(crumb_hover_bg))
            .when(can_open_outline, |el| {
                el.cursor_pointer().hover(move |s| s.bg(crumb_hover_bg))
            })
            .when(can_open_outline, |el| {
                el.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|v, _, window, cx| {
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
                    }),
                )
            })
            .child(crumb_content);

        let search_icon_color = if self.show_search {
            t.accent
        } else {
            t.text_subtle
        };
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
            .child(
                svg()
                    .path(IconName::Search.path())
                    .size(px(14.))
                    .text_color(search_icon_color),
            );

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
        let cursor_line = self
            .doc
            .rope
            .char_to_line(self.sel.head.min(self.doc.len_chars().saturating_sub(1)));
        if cursor_line != self.last_scroll_line {
            self.last_scroll_line = cursor_line;
            self.scroll_to_line(cursor_line);
        }

        let settings = &cx.global::<SettingsStore>().0;
        let show_line_numbers = settings.line_numbers;
        let show_scrollbar = settings.show_scrollbar;

        let is_dragging = self.scrollbar_drag.is_some();
        let outline_hl = self.outline_highlight;

        // Update word-occurrence cache when the identifier under the caret changes.
        // `Query::all_matches` is O(document); the key guard ensures it only runs on word change.
        // Update word-occurrence cache when the identifier under the caret changes.
        // `Query::all_matches` is O(document); the key guard ensures it only runs on word change.
        let active_word: Option<(usize, usize)> = if self.sel.is_empty() {
            let w = cursor::word_at(&self.doc.rope, self.sel.head);
            if !w.is_empty()
                && cursor::default_word_classifier(
                    self.doc
                        .rope
                        .char(self.sel.head.min(self.doc.len_chars().saturating_sub(1))),
                )
            {
                let head_char_col = self
                    .sel
                    .head
                    .saturating_sub(self.doc.rope.line_to_char(cursor_line));
                let line_str = self
                    .line_cache
                    .get(cursor_line)
                    .map(|s| s.as_ref())
                    .unwrap_or("");
                let head_byte_col = char_col_to_byte_col(line_str, head_char_col);
                let in_comment = self.doc.highlight_spans(cursor_line).iter().any(|s| {
                    if s.token != SyntaxToken::Comment {
                        return false;
                    }
                    let start = s.start_byte_col as usize;
                    let end = if s.end_byte_col == u32::MAX {
                        line_str.len()
                    } else {
                        s.end_byte_col as usize
                    };
                    head_byte_col >= start && head_byte_col < end
                });
                if in_comment {
                    None
                } else {
                    Some((w.start(), w.end()))
                }
            } else {
                None
            }
        } else {
            None
        };

        match active_word {
            Some((ws, we)) => {
                // Compare rope chars against cached key before allocating a String.
                let same = self
                    .word_occ_key
                    .as_deref()
                    .is_some_and(|k| self.doc.rope.slice(ws..we).chars().eq(k.chars()));
                if !same {
                    let word_text = self.doc.rope.slice(ws..we).to_string();
                    self.word_occ = Query::new(word_text.clone())
                        .case_sensitive(true)
                        .whole_word(true)
                        .all_matches(&self.doc.rope);
                    self.word_occ_key = Some(word_text);
                }
            }
            None => self.clear_word_occ(),
        }

        // Innermost enclosing bracket pair (O(tree depth), safe per frame).
        let bracket_hl: Option<(usize, usize)> = if self.sel.is_empty() {
            self.doc.enclosing_brackets(self.sel.head)
        } else {
            None
        };

        // Caret position as a [0,1] fraction of the document for the scrollbar marker.
        let caret_pos_frac: Option<f32> = if line_count > 1 {
            Some(cursor_line as f32 / (line_count - 1) as f32)
        } else {
            Some(0.0)
        };

        let editor_pane = {
            // ── Virtualized horizontal-scroll path ──
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
                caret_pos_frac,
            );
            let entity = cx.entity();
            let t2 = t.clone();
            let widest = self.widest_line;
            let flash = self.flash_line;
            // Pre-fetch once per render so shape_editor_line doesn't acquire the store lock per line.
            let file_diags: Vec<faber_lsp::diagnostics::DiagnosticEntry> = self
                .diagnostic_store
                .as_ref()
                .and_then(|store| {
                    url::Url::from_file_path(&self.doc.path)
                        .ok()
                        .map(|uri| store.get_for_uri(&uri))
                })
                .unwrap_or_default();
            const TRAILING_BLANK_LINES: usize = 6;
            let content = uniform_list(
                "editor-lines",
                line_count + TRAILING_BLANK_LINES,
                move |range: std::ops::Range<usize>, window, cx| {
                    let view = entity.read(cx);
                    range
                        .map(|i| {
                            if i < line_count {
                                view.render_line(
                                    i,
                                    &t2,
                                    show_line_numbers,
                                    cursor_visible,
                                    outline_hl,
                                    flash,
                                    bracket_hl,
                                    window,
                                    &file_diags,
                                )
                            } else {
                                div().h(px(t2.line_height_code)).into_any_element()
                            }
                        })
                        .collect::<Vec<AnyElement>>()
                },
            )
            .flex_1()
            .px_2()
            .bg(t.bg)
            .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
            .with_width_from_item(Some(widest))
            .track_scroll(self.scroll_handle.clone())
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down_editor))
            .on_scroll_wheel(cx.listener(|_view, _ev: &ScrollWheelEvent, _, cx| {
                // Trigger re-render so the breadcrumb heading stack refreshes.
                cx.notify();
            }));
            div()
                .flex()
                .flex_row()
                .flex_1()
                .min_w(px(0.))
                .min_h(px(0.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.))
                        .min_h(px(0.))
                        .cursor(CursorStyle::IBeam)
                        .child(content),
                )
                .child(editor_scrollbar)
        };

        // In preview mode: side-by-side split (editor left, preview right).
        if show_preview && let Some(ref preview_entity) = self.preview {
            let preview = preview_entity.clone();
            let split = div()
                .flex()
                .flex_row()
                .flex_1()
                .min_h(px(0.))
                .child(editor_pane)
                .child(div().w(px(1.)).bg(t.separator).flex_shrink_0())
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.))
                        .min_h(px(0.))
                        .child(preview),
                );
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
                .when(is_dragging || self.mouse_selecting, |el| {
                    el.on_mouse_move(cx.listener(|view, ev: &MouseMoveEvent, window, cx| {
                        if let Some(ref drag) = view.scrollbar_drag {
                            let handle = view.scroll_handle.0.borrow().base_handle.clone();
                            update_drag(drag, ev, &handle);
                            cx.notify();
                        } else if view.mouse_selecting {
                            view.on_mouse_move_editor(ev, window, cx);
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|view, ev: &MouseUpEvent, window, cx| {
                            view.scrollbar_drag = None;
                            view.on_mouse_up_editor(ev, window, cx);
                            cx.notify();
                        }),
                    )
                })
                .child(header);
            let key_ctx = if self.outline_open {
                "OutlineOverlay"
            } else if is_md {
                "Editor markdown"
            } else {
                "Editor"
            };
            let root = root.key_context(key_ctx);
            let root = if self.show_search {
                root.child(self.render_search_bar(window, &t, cx))
            } else {
                root
            };
            let root = root.child(split);
            let root = if self.outline_open {
                root.child(self.render_outline_overlay(&t, window, cx))
            } else {
                root
            };
            return root.into_any();
        }

        let key_ctx = if self.outline_open {
            "OutlineOverlay"
        } else if is_md {
            "Editor markdown"
        } else {
            "Editor"
        };

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
            .when(is_dragging || self.mouse_selecting, |el| {
                el.on_mouse_move(cx.listener(|view, ev: &MouseMoveEvent, window, cx| {
                    if let Some(ref drag) = view.scrollbar_drag {
                        let handle = view.scroll_handle.0.borrow().base_handle.clone();
                        update_drag(drag, ev, &handle);
                        cx.notify();
                    } else if view.mouse_selecting {
                        view.on_mouse_move_editor(ev, window, cx);
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|view, ev: &MouseUpEvent, window, cx| {
                        view.scrollbar_drag = None;
                        view.on_mouse_up_editor(ev, window, cx);
                        cx.notify();
                    }),
                )
            })
            .child(header);

        let root = if self.show_search {
            root.child(self.render_search_bar(window, &t, cx))
        } else {
            root
        };
        let root = root.child(editor_pane);
        let root = if self.outline_open {
            root.child(self.render_outline_overlay(&t, window, cx))
        } else {
            root
        };
        let root = if let Some(content) = self.hover_content.clone() {
            root.child(self.render_hover_popover(&t, content, cx))
        } else {
            root
        };
        root.into_any()
    }
}

// ── Hover popover ──────────────────────────────────────────────────────────────

impl EditorView {
    fn render_hover_popover(
        &self,
        t: &RuntimeTheme,
        content: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pixel_y = self.hover_pixel_y;
        let t2 = t.clone();
        deferred(
            anchored()
                .position(point(px(120.), px(pixel_y + 24.)))
                .snap_to_window()
                .child(
                    div()
                        .id("hover-popover")
                        .bg(t.bg_elevated)
                        .border_1()
                        .border_color(t.border)
                        .rounded(px(t.radius_md))
                        .p(px(10.))
                        .max_w(px(520.))
                        .font_family(t.ui_family.clone())
                        .text_size(px(t.font_size_body))
                        .text_color(t.text)
                        .on_mouse_down(gpui::MouseButton::Left, cx.listener(|_, _, _, _| {}))
                        .child(
                            div()
                                .font_family(t2.mono_family.clone())
                                .text_size(px(t2.font_size_code))
                                .child(content),
                        ),
                ),
        )
        .with_priority(3)
        .into_any_element()
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

        // Filter items by query substring.
        let filtered: Vec<(usize, &OutlineItem)> = outline
            .items
            .iter()
            .enumerate()
            .filter(|(_, e)| query.is_empty() || e.name.to_lowercase().contains(&query))
            .collect();

        // ── search input ──────────────────────────────────────────────────────
        let (q_before, q_after) = split_at_char(&self.outline_query, self.outline_cursor);
        let is_query_empty = self.outline_query.is_empty();
        // Input uses code font so typed text is readable; height gives comfortable touch target.
        let caret_h = t.font_size_code + 4.;

        let search_input_base = div()
            .id("outline-search-input")
            .flex()
            .flex_row()
            .items_center()
            .h(px(OUTLINE_INPUT_ROW_H))
            .px_4()
            .py(px(10.))
            .border_b_1()
            .border_color(t.separator)
            .track_focus(&self.outline_handle)
            .font_family(t.mono_family.clone())
            .text_size(px(t.font_size_code))
            .text_color(t.text)
            .child(
                svg()
                    .path(IconName::Search.path())
                    .size(px(15.))
                    .text_color(t.text_muted)
                    .flex_shrink_0(),
            )
            .child(div().w(px(8.)).flex_shrink_0());

        // Mirror search bar: show placeholder only when NOT focused and query is empty.
        // Placeholder uses caption size and a more subdued color to distinguish from typed text.
        let search_input: gpui::AnyElement = if !outline_focused && is_query_empty {
            search_input_base
                .child(
                    div()
                        .flex_1()
                        .h(px(caret_h))
                        .flex()
                        .items_center()
                        .font_family(t.ui_family.clone())
                        .text_size(px(t.font_size_caption))
                        .text_color(t.text_subtle)
                        .child(t!("outline_overlay.placeholder").to_string()),
                )
                .into_any()
        } else {
            let cur_on = outline_focused && caret_visible;
            {
                let q_before_owned = q_before.clone();
                let q_after_owned = q_after.clone();
                let font_sz = px(t.font_size_code);
                let caret_h_px = px(caret_h);
                let mono_family = t.mono_family.clone();
                let text_col = t.text;
                let cursor_col_val = t.cursor;
                let cursor_color = if cur_on {
                    cursor_col_val
                } else {
                    gpui::hsla(0., 0., 0., 0.)
                };
                let full_text = format!("{}{}", q_before_owned, q_after_owned);
                let caret_byte = q_before_owned.len();
                search_input_base
                    .child(
                        canvas(
                            move |_bounds, window, _cx| {
                                let runs = if full_text.is_empty() {
                                    vec![]
                                } else {
                                    vec![TextRun {
                                        len: full_text.len(),
                                        font: font(mono_family.clone()),
                                        color: text_col,
                                        background_color: None,
                                        underline: None,
                                        strikethrough: None,
                                    }]
                                };
                                window.text_system().shape_line(
                                    SharedString::from(full_text),
                                    font_sz,
                                    &runs,
                                    None,
                                )
                            },
                            move |bounds, shaped, window, cx| {
                                let origin = bounds.origin;
                                let _ = shaped.paint(origin, caret_h_px, window, cx);
                                let cx_x = origin.x + shaped.x_for_index(caret_byte);
                                window.paint_quad(fill(
                                    Bounds::new(point(cx_x, origin.y), size(px(2.0), caret_h_px)),
                                    cursor_color,
                                ));
                            },
                        )
                        .flex_1()
                        .h(caret_h_px),
                    )
                    .into_any()
            }
        };

        // ── symbol list ───────────────────────────────────────────────────────
        let hover_idx = self.outline_hover;

        let list_body: AnyElement = if outline.items.is_empty() {
            div()
                .h(px(OUTLINE_BODY_H))
                .flex()
                .items_center()
                .justify_center()
                .font_family(t.ui_family.clone())
                .text_size(px(t.font_size_caption))
                .text_color(t.text_muted)
                .child(t!("outline_overlay.no_headings").to_string())
                .into_any()
        } else if filtered.is_empty() {
            div()
                .h(px(OUTLINE_BODY_H))
                .flex()
                .items_center()
                .justify_center()
                .font_family(t.ui_family.clone())
                .text_size(px(t.font_size_caption))
                .text_color(t.text_muted)
                .child(t!("outline_overlay.no_matches").to_string())
                .into_any()
        } else {
            let entries: Vec<AnyElement> = filtered
                .iter()
                .enumerate()
                .map(|(list_idx, (_orig_idx, entry))| {
                    let depth = entry.depth;
                    let source_line = entry.source_line;
                    let end_line = entry.end_line;
                    let is_markdown = entry.block_ix.is_some();
                    let is_hovered = hover_idx == Some(list_idx);
                    let indent = (depth as f32) * 14.0;

                    // Highlight range:
                    // • Code items: use the exact node body (source_line..=end_line).
                    // • Markdown items: extend to the next heading at same/higher level
                    //   in the FULL outline (not just the filtered view) so the section
                    //   highlight is correct even when a search filter is active.
                    let hl_end = if is_markdown {
                        outline
                            .items
                            .iter()
                            .skip_while(|e| e.source_line <= source_line)
                            .find(|e| e.depth <= depth)
                            .map(|e| e.source_line)
                            .unwrap_or(usize::MAX)
                    } else {
                        end_line + 1
                    };

                    let t_clone = t.clone();
                    div()
                        .id(("outline-entry", list_idx))
                        .flex()
                        .flex_row()
                        .items_center()
                        .px(px(12. + indent))
                        .py(px(6.))
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
                                view.outline_highlight = Some((source_line, hl_end));
                                view.scroll_to_line(source_line);
                                cx.notify();
                            }
                        }))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |view, _, window, cx| {
                                view.scroll_to_line(source_line);
                                view.outline_open = false;
                                view.outline_hover = None;
                                view.outline_highlight = None;
                                window.focus(&view.focus_handle);
                                cx.notify();
                            }),
                        )
                        .when_some(entry.context.clone(), |el, ctx| {
                            el.child(
                                div()
                                    .text_color(t.text_muted)
                                    .text_size(px(t.font_size_caption - 1.))
                                    .child(ctx)
                                    .flex_shrink_0(),
                            )
                        })
                        .child(
                            div()
                                .flex_1()
                                .overflow_hidden()
                                .child(SharedString::from(entry.name.clone())),
                        )
                        .into_any()
                })
                .collect();

            div()
                .h(px(OUTLINE_BODY_H))
                .overflow_hidden()
                .flex()
                .flex_col()
                .child(
                    div()
                        .id("outline-list")
                        .flex_col()
                        .h_full()
                        .overflow_y_scroll()
                        .children(entries),
                )
                .into_any()
        };

        // ── modal container — elevation: modal_container helper (shadow_lg + rounded_lg) ──
        let modal = modal_container("outline-modal", t)
            .h(px(OUTLINE_MODAL_H))
            .w(px(600.))
            .child(search_input)
            .child(list_body)
            .child(modal_footer(
                t,
                &[
                    ("↑↓", t!("outline_overlay.hint_navigate").to_string()),
                    ("↵", t!("outline_overlay.hint_jump").to_string()),
                    ("⎋", t!("outline_overlay.hint_dismiss").to_string()),
                ],
            ));

        // ── centered backdrop: absolute+inset_0 fills the editor pane ────────
        deferred(
            modal_backdrop("outline-backdrop", t)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|view, _, window, cx| {
                        view.outline_open = false;
                        view.outline_hover = None;
                        view.outline_highlight = None;
                        window.focus(&view.focus_handle);
                        cx.notify();
                    }),
                )
                .child(modal),
        )
        .with_priority(2)
        .into_any()
    }
}

