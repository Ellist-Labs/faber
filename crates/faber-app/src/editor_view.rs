use std::{cell::Cell, ops::Range, rc::Rc, sync::Arc, time::Duration};

use faber_editor::{
    ChangeSet, LanguageRegistry, Selection, Transaction,
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
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render, ScrollHandle, ScrollStrategy,
    ScrollWheelEvent, SharedString, TextRun, UniformListScrollHandle, Window, anchored, canvas,
    deferred, div, fill, font, point, prelude::*, px, size, svg, uniform_list,
};

use crate::input_helpers::{
    delete_char_before, delete_char_range, insert_at, split_at_char, word_start_before,
};
use crate::markdown_preview::MarkdownPreviewView;
use crate::settings_view::SettingsStore;
use crate::theme::RuntimeTheme;
use crate::ui::scrollbar::{start_drag, update_drag};
use crate::ui::{
    IconName, ScrollbarDrag, glass_surface, modal_backdrop, modal_container, modal_footer,
    render_scrollbar,
};
use rust_i18n::t;

// ── layout ─────────────────────────────────────────────────────────────────────
// Line height and char width live on RuntimeTheme (settings-scaled).

const GUTTER_COLS: f32 = 6.0;

// ── hover popover geometry + timing ───────────────────────────────────────────
const HOVER_DELAY_MS: u64 = 300;
const HOVER_HIDE_DELAY_MS: u64 = 300;
const HOVER_MAX_W: f32 = 560.;
const HOVER_MAX_H: f32 = 320.;

// ── outline overlay geometry ───────────────────────────────────────────────────
const OUTLINE_INPUT_ROW_H: f32 = 45.;
const OUTLINE_FOOTER_H: f32 = 30.;
const OUTLINE_MODAL_H: f32 = 480.;
const OUTLINE_BODY_H: f32 = OUTLINE_MODAL_H - OUTLINE_INPUT_ROW_H - OUTLINE_FOOTER_H;

// ── actions ────────────────────────────────────────────────────────────────────

use crate::{
    Backspace, BoldSelection, CancelCompletion, CloseSearch, CompletionFirst, CompletionLast,
    CompletionNext, CompletionPrev, ConfirmCompletion, Copy, Cut, Delete, DeleteLine,
    DeleteToLineEnd, DeleteToLineStart, DeleteWordLeft, DeleteWordRight, Enter, FindNext, FindPrev,
    FindReferences, GoToDefinition, InputMoveEnd, InputMoveLeft, InputMoveRight, InputMoveStart,
    ItalicSelection, MoveDocEnd, MoveDocStart, MoveDown, MoveLeft, MoveLineEnd, MoveLineStart,
    MovePageDown, MovePageUp, MoveRight, MoveUp, MoveWordLeft, MoveWordRight, OpenReplace,
    OpenSearch, Paste, ProjectRoot, Redo, ReplaceAll, ReplaceBackspace, ReplaceOne,
    SearchBackspace, SelectAll, SelectDocEnd, SelectDocStart, SelectDown, SelectLeft,
    SelectLineEnd, SelectLineStart, SelectRight, SelectUp, SelectWordLeft, SelectWordRight,
    ShowCompletions, Tab, ToggleCheckbox, TogglePreview, ToggleReplace, ToggleSearchCase,
    ToggleSearchRegex, ToggleSearchWholeWord, Undo,
};

// ── HoverState ─────────────────────────────────────────────────────────────────

/// All hover-popover state consolidated in one place, mirroring Zed's HoverState.
pub struct HoverState {
    /// Show timer + LSP fetch task (replaces `hover_timer`).
    pub info_task: Option<gpui::Task<()>>,
    /// 300ms grace-period task before hiding.
    pub hiding_task: Option<gpui::Task<()>>,
    /// Running min distance from cursor to popover rect; used for sticky logic.
    pub closest_distance: Option<Pixels>,
    /// Last-painted popover bounds; captured each frame for hit-testing.
    pub bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Parsed markdown document to render.
    pub content: Option<Arc<faber_editor::markdown::MarkdownDoc>>,
    /// Symbol range (char offsets) from the hover response; prevents re-fetch on micro-movement.
    pub symbol_range: Option<std::ops::Range<usize>>,
    /// Scroll handle for the scrollable popover body.
    pub scroll: ScrollHandle,
    /// Locked popover anchor (symbol start glyph), fixed at show time.
    pub anchor: Option<crate::hover_popover::HoverAnchor>,
    /// Cached content height (segments sum); computed once at show time, not per frame.
    pub estimated_height: f32,
    /// Char offset that triggered the last hover fetch.
    pub char_offset: Option<usize>,
    /// Flattened selectable content; rebuilt when `content` changes.
    pub segments: crate::hover_popover::SharedSegments,
    /// Active text selection inside the popover.
    pub selection: Option<crate::hover_popover::HoverSelection>,
    /// True while dragging a selection inside the popover.
    pub selecting: bool,
    /// Link pressed at mouse-down; opened at mouse-up on the same link.
    pub pressed_link: Option<(usize, usize)>,
    /// Active popover scrollbar thumb drag.
    pub scrollbar_drag: Option<crate::ui::ScrollbarDrag>,
    /// Repeating tick that scrolls the popover while a selection drag is past
    /// its edge, extending the selection (Zed-style drag auto-scroll).
    pub autoscroll_task: Option<gpui::Task<()>>,
}

impl Default for HoverState {
    fn default() -> Self {
        Self {
            info_task: None,
            hiding_task: None,
            closest_distance: None,
            bounds: Rc::new(Cell::new(None)),
            content: None,
            symbol_range: None,
            scroll: ScrollHandle::new(),
            anchor: None,
            estimated_height: 0.0,
            char_offset: None,
            segments: Rc::new(std::cell::RefCell::new(Vec::new())),
            selection: None,
            selecting: false,
            pressed_link: None,
            scrollbar_drag: None,
            autoscroll_task: None,
        }
    }
}

impl HoverState {
    /// A non-empty selection locks the popover: only an explicit click outside
    /// (or Escape / scroll / edit) dismisses it.
    fn selection_locked(&self) -> bool {
        self.selecting || self.selection.is_some_and(|s| !s.is_empty())
    }
}

// ── HoveredLink (cmd+hover go-to-definition preview) ──────────────────────────

/// Cached definition lookup for the symbol under the cmd-hovered mouse,
/// mirroring Zed's HoveredLinkState. `locations` empty = negative cache
/// (definitely not clickable; don't re-request until the mouse leaves range).
pub struct HoveredLink {
    pub symbol_range: std::ops::Range<usize>,
    pub locations: Vec<(std::path::PathBuf, usize, usize)>,
}

// ── CompletionMenu ─────────────────────────────────────────────────────────────

pub struct CompletionMenu {
    pub items: Vec<faber_lsp::completion::ParsedCompletion>,
    pub filtered: Vec<usize>,
    pub selected_ix: usize,
    pub word_start: usize,
    pub query: String,
    pub initial_query: String,
    pub is_incomplete: bool,
    pub scroll: ScrollHandle,
    pub request_task: Option<gpui::Task<()>>,
    pub doc_text: Option<String>,
    pub resolve_task: Option<gpui::Task<()>>,
    pub locked_anchor: Option<crate::hover_popover::HoverAnchor>,
    /// Persistent markdown segments for the doc panel — rebuilt when doc changes.
    pub doc_segments: crate::hover_popover::SharedSegments,
    /// Persistent scroll handle for the doc panel — preserved across re-filters.
    pub doc_scroll: ScrollHandle,
}

impl CompletionMenu {
    fn selected_item(&self) -> Option<&faber_lsp::completion::ParsedCompletion> {
        let ix = *self.filtered.get(self.selected_ix)?;
        self.items.get(ix)
    }
}

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

    // LSP — set by Workspace after push_editor_tab wires the handles.
    pub lsp_manager: Option<std::sync::Arc<faber_lsp::manager::LspManager>>,
    pub ws_handle: Option<gpui::WeakEntity<crate::workspace::Workspace>>,
    pub hover: HoverState,

    pub completion: Option<CompletionMenu>,
    completion_suppress_once: bool,

    // True while the Cmd/Super key is held; drives hand cursor on editor content.
    pub cmd_held: bool,
    // Cmd+hover link preview (Zed's hovered_link): definition cache for the
    // symbol under the mouse. Hand cursor shows only when `locations` is non-empty.
    pub hovered_link: Option<HoveredLink>,
    link_task: Option<gpui::Task<()>>,
    /// Offset that triggered the in-flight/last link request (dedupe).
    link_trigger: Option<usize>,
    /// Last mouse position over the editor, for modifier-change re-evaluation.
    last_mouse_pos: Option<gpui::Point<Pixels>>,
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
            ws_handle: None,
            hover: HoverState::default(),
            completion: None,
            completion_suppress_once: false,
            cmd_held: false,
            hovered_link: None,
            link_task: None,
            link_trigger: None,
            last_mouse_pos: None,
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
        // Document changed under the popover / link preview — both are stale.
        self.dismiss_hover(cx);
        self.clear_hovered_link(cx);
        if self.completion.is_some() {
            self.refresh_completion_filter_or_dismiss(cx);
        }
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
                (GUTTER_COLS * t.char_w_code).max(54.0)
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

    /// Like `offset_at`, but only for positions over actual glyphs — Zed's
    /// `PointForPosition::as_valid`. Past-EOL, gutter, and below-document
    /// positions return None so they never trigger hover or link previews.
    fn hover_offset_at(
        &self,
        p: gpui::Point<gpui::Pixels>,
        t: &RuntimeTheme,
        window: &mut Window,
    ) -> Option<usize> {
        let (anchor_line, origin_x, origin_y) = self.text_origin.get()?;
        let px_y = f32::from(p.y);
        let px_x = f32::from(p.x);
        let line_f = anchor_line as f32 + ((px_y - origin_y) / t.line_height_code).floor();
        if line_f < 0.0 {
            return None;
        }
        let line = line_f as usize;
        if line >= self.line_starts.len() {
            return None;
        }
        let rel_x = px_x - origin_x;
        if rel_x < 0.0 {
            return None;
        }
        let shaped = self.shape_editor_line(line, t, window, &[]);
        if rel_x > f32::from(shaped.width) + t.char_w_code * 0.5 {
            return None;
        }
        let byte_off = shaped.closest_index_for_x(px(rel_x));
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
        // Any click on the editor (the popover stops propagation for clicks
        // inside itself) dismisses the hover popover, selection included.
        self.dismiss_hover(cx);
        self.dismiss_completion(cx);
        let t = cx.global::<RuntimeTheme>().clone();
        let show_line_numbers = cx.global::<SettingsStore>().0.line_numbers;
        let Some(offset) = self.offset_at(ev.position, &t, show_line_numbers, window) else {
            return;
        };

        // Cmd+click → go-to-definition (takes precedence over selection).
        if ev.click_count == 1 && ev.modifiers.platform && !ev.modifiers.shift {
            self.sel = Selection::collapsed(offset, &self.doc.rope);
            cx.notify();
            // Use the cmd+hover cache when it already resolved this symbol.
            if let Some(link) = &self.hovered_link
                && link.symbol_range.contains(&offset)
            {
                if let Some((path, line, ch)) = link.locations.first().cloned() {
                    self.goto_location(path, line, ch, window, cx);
                }
                return;
            }
            self.trigger_go_to_definition(window, cx);
            return;
        }

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
        self.last_mouse_pos = Some(ev.position);

        // Keep cmd_held in sync; drives the hand cursor on editor content.
        let cmd = ev.modifiers.platform;
        if self.cmd_held != cmd {
            self.cmd_held = cmd;
            cx.notify();
        }

        // A popover scrollbar drag continues even when the pointer leaves it.
        if let Some(drag) = self.hover.scrollbar_drag {
            update_drag(&drag, ev, &self.hover.scroll);
            cx.notify();
            return;
        }

        // A selection drag that started inside the popover continues even when
        // the pointer leaves it — extend to the nearest segment position and
        // auto-scroll while past the popover edge.
        if self.hover.selecting {
            if let Some(hit) = crate::hover_popover::hit_test(&self.hover.segments, ev.position)
                && let Some(sel) = &mut self.hover.selection
            {
                sel.end = hit;
                cx.notify();
            }
            self.update_hover_autoscroll(ev.position, cx);
            return;
        }

        // Track position for hover when not dragging; compute offset while Window is available.
        if !ev.dragging() {
            let t = cx.global::<RuntimeTheme>().clone();
            let offset = self.hover_offset_at(ev.position, &t, window);
            self.update_hovered_link(offset, cmd, cx);
            self.hover_at(ev.position, offset, window, cx);
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
        // Finish popover scrollbar / selection drags that ended outside it.
        if self.hover.scrollbar_drag.take().is_some() {
            cx.notify();
        }
        if self.hover.selecting {
            self.hover.selecting = false;
            self.hover.autoscroll_task = None;
            if self.hover.selection.is_some_and(|s| s.is_empty()) {
                self.hover.selection = None;
            }
            cx.notify();
        }
        if self.mouse_selecting {
            self.mouse_selecting = false;
            cx.notify();
        }
    }

    // ── LSP hover ─────────────────────────────────────────────────────────────
    //
    // Mirrors Zed's hover_popover.rs `hover_at`/`show_hover`/`hide_hover`:
    // - Valid text position: keep popover while inside the symbol range,
    //   otherwise hide and arm a fresh show timer for the new position.
    // - Off-text position: sticky grace — keep the popover while the mouse
    //   approaches it; hide HOVER_HIDE_DELAY_MS after it moves away.
    // - The popover itself stops mouse propagation, so the editor never sees
    //   moves while the pointer is inside it.

    fn hover_at(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        offset: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // An active selection locks the popover: only click/Escape/scroll dismiss.
        if self.hover.selection_locked() {
            return;
        }
        match offset {
            Some(off) => {
                self.hover.hiding_task = None;
                self.hover.closest_distance = None;
                self.show_hover(off, window, cx);
            }
            None => {
                if self.hover.content.is_none() {
                    self.hover.info_task = None;
                    self.hover.char_offset = None;
                    return;
                }
                // Sticky: moving toward the popover keeps it; once the mouse
                // moves away a single grace timer counts down to dismissal.
                let getting_closer = self.is_mouse_getting_closer(position);
                if !getting_closer && self.hover.hiding_task.is_some() {
                    return;
                }
                self.hover.hiding_task = Some(cx.spawn(async move |view, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(HOVER_HIDE_DELAY_MS))
                        .await;
                    view.update(cx, |ev, cx| {
                        log::debug!("hover: grace timer elapsed → dismiss");
                        ev.dismiss_hover(cx);
                    })
                    .ok();
                }));
            }
        }
    }

    fn show_hover(&mut self, offset: usize, window: &mut Window, cx: &mut Context<Self>) {
        // Still over the current popover's symbol (inclusive, like Zed's
        // same_info_hover) — nothing to do.
        if self.hover.content.is_some()
            && let Some(range) = &self.hover.symbol_range
            && offset >= range.start
            && offset <= range.end
        {
            return;
        }
        // Same trigger position with a timer already pending — let it run.
        if Some(offset) == self.hover.char_offset && self.hover.info_task.is_some() {
            return;
        }
        // Moved to a new symbol: the old popover hides immediately (Zed parity).
        if self.hover.content.is_some() {
            log::debug!("hover: left symbol range → hide (offset {offset})");
            self.dismiss_hover(cx);
        }
        self.hover.char_offset = Some(offset);
        self.hover.info_task = Some(cx.spawn_in(window, async move |view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(HOVER_DELAY_MS))
                .await;
            view.update_in(cx, |ev, window, cx| ev.trigger_hover(window, cx))
                .ok();
        }));
    }

    fn trigger_hover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(offset) = self.hover.char_offset else {
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
        let rope = self.doc.rope.clone();
        let registry = Arc::clone(&self.registry);
        // Bare ``` fences in hover responses inherit the document's language.
        let fallback_ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_string());
        let lsp_pos = faber_lsp::position::to_lsp_position(&self.doc.rope, offset, encoding);
        let params_json = serde_json::json!({
            "textDocument": { "uri": uri.as_str() },
            "position": { "line": lsp_pos.line, "character": lsp_pos.character }
        });
        let Some(rx) = mgr.request_for_document(&uri, "textDocument/hover", params_json) else {
            log::debug!("hover: no running server for {uri} — request skipped");
            return;
        };
        log::debug!(
            "hover: request offset={offset} pos={}:{}",
            lsp_pos.line,
            lsp_pos.character
        );
        cx.spawn_in(window, async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { rx.recv_timeout(std::time::Duration::from_secs(5)) })
                .await;
            let parsed = match result {
                Ok(Ok(ref val)) => {
                    let text = faber_lsp::hover::extract_hover_text(val);
                    let lsp_range = faber_lsp::hover::extract_hover_range(val);
                    log::debug!(
                        "hover: response text_len={:?} range={lsp_range:?}",
                        text.as_ref().map(|t| t.len())
                    );
                    if let Some(t) = &text {
                        log::debug!(
                            "hover: markdown head: {:?}",
                            t.chars().take(300).collect::<String>()
                        );
                        // Debug builds: dump full content so we can inspect what
                        // rust-analyzer actually sends (lists, images, code fences, etc).
                        #[cfg(debug_assertions)]
                        {
                            let _ = std::fs::write("/tmp/faber_hover_last.md", t.as_bytes());
                        }
                    }
                    let char_range = lsp_range.and_then(|r| {
                        let start =
                            faber_lsp::position::from_lsp_position(&rope, r.start, encoding)?;
                        let end = faber_lsp::position::from_lsp_position(&rope, r.end, encoding)?;
                        Some(start..end)
                    });
                    text.map(|t| {
                        let hover_rope = ropey::Rope::from_str(&t);
                        let md = faber_editor::markdown::parse_markdown_with_fallback(
                            &t,
                            &hover_rope,
                            &registry,
                            fallback_ext.as_deref(),
                        );
                        log::debug!(
                            "hover: parsed {} blocks: {}",
                            md.blocks.len(),
                            md.blocks
                                .iter()
                                .map(|b| match &b.kind {
                                    faber_editor::markdown::BlockKind::Heading {
                                        level, ..
                                    } => format!("H{level}"),
                                    faber_editor::markdown::BlockKind::Paragraph { .. } =>
                                        "P".into(),
                                    faber_editor::markdown::BlockKind::CodeBlock {
                                        lang, ..
                                    } => format!("Code({})", lang.as_deref().unwrap_or("?")),
                                    faber_editor::markdown::BlockKind::Blockquote { .. } =>
                                        "Quote".into(),
                                    faber_editor::markdown::BlockKind::List { ordered, .. } =>
                                        if *ordered { "OL" } else { "UL" }.into(),
                                    faber_editor::markdown::BlockKind::Table { .. } =>
                                        "Table".into(),
                                    faber_editor::markdown::BlockKind::Rule => "HR".into(),
                                    faber_editor::markdown::BlockKind::HtmlBlock { .. } =>
                                        "HTML".into(),
                                })
                                .collect::<Vec<_>>()
                                .join(" ")
                        );
                        (Arc::new(md), char_range)
                    })
                }
                other => {
                    log::debug!("hover: request failed or timed out: {other:?}");
                    None
                }
            };
            view.update_in(cx, |ev, window, cx| {
                if let Some((md, range)) = parsed {
                    // No server range → fall back to the word under the cursor so
                    // the popover still anchors and dismisses sensibly.
                    let range = range.unwrap_or_else(|| {
                        let word = cursor::word_at(&ev.doc.rope, offset);
                        word.range()
                    });
                    // Freshness: accept if the mouse is still at the trigger
                    // offset OR anywhere inside the response's own symbol range
                    // (drifting within the same word must not drop the popover).
                    let fresh = match ev.hover.char_offset {
                        Some(cur) => cur == offset || (cur >= range.start && cur <= range.end),
                        None => false,
                    };
                    if !fresh {
                        log::debug!("hover: stale response for offset {offset} — dropped");
                        return;
                    }
                    // New content supersedes any pending hide from the previous
                    // symbol AND any pending re-show for an offset inside this
                    // range (it would only duplicate this request).
                    ev.hover.hiding_task = None;
                    ev.hover.info_task = None;
                    let t = cx.global::<RuntimeTheme>().clone();
                    ev.hover.anchor = ev.hover_anchor_for_offset(range.start, &t, window);
                    crate::hover_popover::rebuild_segments(&ev.hover.segments, &md, &t);
                    ev.hover.estimated_height =
                        crate::hover_popover::estimate_height(&ev.hover.segments.borrow(), &t);
                    ev.hover.content = Some(md);
                    ev.hover.symbol_range = Some(range);
                    ev.hover.closest_distance = None;
                    ev.hover.selection = None;
                    ev.hover.selecting = false;
                    ev.hover.pressed_link = None;
                    ev.hover.bounds.set(None);
                    log::debug!(
                        "hover: show range={:?} anchor={:?}",
                        ev.hover.symbol_range,
                        ev.hover.anchor
                    );
                } else {
                    // Empty/failed response: only clear if the mouse is still at
                    // the trigger offset — otherwise a newer hover owns the state.
                    if ev.hover.char_offset == Some(offset) {
                        ev.dismiss_hover(cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Window-space anchor of the glyph at `offset` — the fixed point the
    /// popover attaches to. None if geometry hasn't been painted yet.
    fn hover_anchor_for_offset(
        &self,
        offset: usize,
        t: &RuntimeTheme,
        window: &mut Window,
    ) -> Option<crate::hover_popover::HoverAnchor> {
        let (anchor_line, origin_x, origin_y) = self.text_origin.get()?;
        let offset = offset.min(self.doc.len_chars().saturating_sub(1));
        let line = self.doc.rope.char_to_line(offset);
        let line_start = self.line_starts.get(line).copied()?;
        let line_str: &str = self.line_cache.get(line)?;
        let char_col = offset.saturating_sub(line_start);
        let byte_col = char_col_to_byte_col(line_str, char_col);
        let shaped = self.shape_editor_line(line, t, window, &[]);
        let x = origin_x + f32::from(shaped.x_for_index(byte_col));
        let line_top = origin_y + (line as f32 - anchor_line as f32) * t.line_height_code;
        Some(crate::hover_popover::HoverAnchor {
            x,
            line_top,
            line_bottom: line_top + t.line_height_code,
        })
    }

    pub fn dismiss_hover(&mut self, cx: &mut Context<Self>) {
        self.hover.info_task = None;
        self.hover.hiding_task = None;
        // Also invalidates any in-flight response (stale-offset check).
        self.hover.char_offset = None;
        let had_content = self.hover.content.is_some();
        self.hover.content = None;
        self.hover.symbol_range = None;
        self.hover.closest_distance = None;
        self.hover.anchor = None;
        self.hover.bounds.set(None);
        self.hover.selection = None;
        self.hover.selecting = false;
        self.hover.pressed_link = None;
        self.hover.scrollbar_drag = None;
        self.hover.autoscroll_task = None;
        self.hover.segments.borrow_mut().clear();
        if had_content {
            log::debug!("hover: dismissed");
            cx.notify();
        }
    }

    /// While a selection drag sits past the popover's top/bottom edge, run a
    /// repeating tick that scrolls the content and extends the selection to the
    /// edge — dragging to the end selects everything (Zed parity).
    fn update_hover_autoscroll(&mut self, pos: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let Some(bounds) = self.hover.bounds.get() else {
            return;
        };
        let outside = pos.y < bounds.top() || pos.y > bounds.bottom();
        if !outside {
            self.hover.autoscroll_task = None;
            return;
        }
        if self.hover.autoscroll_task.is_some() {
            return;
        }
        self.hover.autoscroll_task = Some(cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(30))
                    .await;
                let keep_going = view
                    .update(cx, |ev, cx| {
                        if !ev.hover.selecting {
                            ev.hover.autoscroll_task = None;
                            return false;
                        }
                        let (Some(bounds), Some(pos)) = (ev.hover.bounds.get(), ev.last_mouse_pos)
                        else {
                            return false;
                        };
                        let overshoot = if pos.y < bounds.top() {
                            f32::from(pos.y - bounds.top())
                        } else if pos.y > bounds.bottom() {
                            f32::from(pos.y - bounds.bottom())
                        } else {
                            0.0
                        };
                        if overshoot == 0.0 {
                            ev.hover.autoscroll_task = None;
                            return false;
                        }
                        // Speed scales with how far past the edge the drag sits.
                        let step = (overshoot.abs() * 0.3).clamp(2.0, 40.0) * overshoot.signum();
                        let off = ev.hover.scroll.offset();
                        let max = f32::from(ev.hover.scroll.max_offset().height);
                        let new_y = (f32::from(off.y) - step).clamp(-max.max(0.0), 0.0);
                        ev.hover.scroll.set_offset(point(off.x, px(new_y)));
                        // Extend the selection to the content at the popover edge.
                        let edge_y = if overshoot < 0.0 {
                            bounds.top() + px(4.)
                        } else {
                            bounds.bottom() - px(4.)
                        };
                        if let Some(hit) =
                            crate::hover_popover::hit_test(&ev.hover.segments, point(pos.x, edge_y))
                            && let Some(sel) = &mut ev.hover.selection
                        {
                            sel.end = hit;
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        }));
    }

    /// Returns the Euclidean distance from `pos` to the nearest edge of the hover
    /// popover bounds (0 if `pos` is inside the bounds).
    fn distance_to_hover_bounds(&self, pos: gpui::Point<Pixels>) -> Pixels {
        let Some(bounds) = self.hover.bounds.get() else {
            return px(f32::MAX);
        };
        if bounds.contains(&pos) {
            return px(0.);
        }
        let cx = bounds.center().x;
        let cy = bounds.center().y;
        let hw = bounds.size.width / 2.;
        let hh = bounds.size.height / 2.;
        let dx = (pos.x - cx).abs() - hw;
        let dy = (pos.y - cy).abs() - hh;
        let dx = if dx > px(0.) { dx } else { px(0.) };
        let dy = if dy > px(0.) { dy } else { px(0.) };
        px((f32::from(dx) * f32::from(dx) + f32::from(dy) * f32::from(dy)).sqrt())
    }

    /// Updates `closest_distance` and returns `true` while the mouse keeps
    /// approaching the popover. Zed semantics: a 4px tolerance absorbs jitter,
    /// and the stored distance is the monotonic minimum seen so far.
    fn is_mouse_getting_closer(&mut self, pos: gpui::Point<Pixels>) -> bool {
        if self.hover.content.is_none() {
            return false;
        }
        let dist = self.distance_to_hover_bounds(pos);
        if let Some(closest) = self.hover.closest_distance
            && dist > closest + px(4.)
        {
            return false;
        }
        let min = self
            .hover
            .closest_distance
            .map_or(dist, |closest| dist.min(closest));
        self.hover.closest_distance = Some(min);
        true
    }

    // ── Cmd+hover link preview ────────────────────────────────────────────────
    //
    // Mirrors Zed's hover_links.rs: while cmd is held, resolve the definition
    // under the mouse (cached per symbol range, including the negative result)
    // and underline it. The hand cursor shows only when a definition exists, so
    // keywords like `impl`/`struct` never advertise a fake link.

    fn update_hovered_link(&mut self, offset: Option<usize>, cmd: bool, cx: &mut Context<Self>) {
        let (Some(off), true) = (offset, cmd) else {
            self.clear_hovered_link(cx);
            return;
        };
        if self.mouse_selecting {
            self.clear_hovered_link(cx);
            return;
        }
        // Cached (positive or negative) for the symbol under the mouse.
        if let Some(link) = &self.hovered_link
            && link.symbol_range.contains(&off)
        {
            return;
        }
        // Same trigger point already in flight.
        if self.link_trigger == Some(off) && self.link_task.is_some() {
            return;
        }
        let word = cursor::word_at(&self.doc.rope, off);
        let word_range = word.range();
        if word_range.is_empty() {
            self.clear_hovered_link(cx);
            return;
        }
        // Whitespace / punctuation-only "words" are never links.
        let word_text: String = self.doc.rope.slice(word_range.clone()).to_string();
        if !word_text.chars().any(|c| c.is_alphanumeric() || c == '_') {
            self.clear_hovered_link(cx);
            return;
        }

        let Some(mgr) = self.lsp_manager.clone() else {
            return;
        };
        let Ok(uri) = url::Url::from_file_path(&self.doc.path) else {
            return;
        };
        let encoding = mgr.position_encoding_for_uri(&uri);
        let rope = self.doc.rope.clone();
        let lsp_pos = faber_lsp::position::to_lsp_position(&self.doc.rope, off, encoding);
        let params_json = serde_json::json!({
            "textDocument": { "uri": uri.as_str() },
            "position": { "line": lsp_pos.line, "character": lsp_pos.character }
        });
        let Some(rx) = mgr.request_for_document(&uri, "textDocument/definition", params_json)
        else {
            return;
        };
        log::debug!("link: definition probe offset={off} word={word_text:?}");
        self.link_trigger = Some(off);
        self.link_task = Some(cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { rx.recv_timeout(std::time::Duration::from_secs(5)) })
                .await;
            let (locations, origin) = match result {
                Ok(Ok(ref val)) => (
                    parse_definition_locations(val),
                    faber_lsp::hover::extract_origin_selection_range(val).and_then(|r| {
                        let start =
                            faber_lsp::position::from_lsp_position(&rope, r.start, encoding)?;
                        let end = faber_lsp::position::from_lsp_position(&rope, r.end, encoding)?;
                        Some(start..end)
                    }),
                ),
                _ => (Vec::new(), None),
            };
            view.update(cx, |ev, cx| {
                // Stale: the mouse moved to another word while resolving.
                if ev.link_trigger != Some(off) {
                    return;
                }
                let symbol_range = origin.unwrap_or(word_range);
                log::debug!(
                    "link: resolved {} location(s) range={symbol_range:?}",
                    locations.len()
                );
                ev.hovered_link = Some(HoveredLink {
                    symbol_range,
                    locations,
                });
                cx.notify();
            })
            .ok();
        }));
    }

    fn clear_hovered_link(&mut self, cx: &mut Context<Self>) {
        self.link_task = None;
        self.link_trigger = None;
        if self.hovered_link.take().is_some() {
            cx.notify();
        }
    }

    /// True when the symbol under the mouse resolved to at least one definition —
    /// drives the hand cursor and the underline.
    fn link_preview_active(&self) -> bool {
        self.cmd_held
            && self
                .hovered_link
                .as_ref()
                .is_some_and(|l| !l.locations.is_empty())
    }

    // ── Go-to-Definition ──────────────────────────────────────────────────────

    fn on_go_to_definition(
        &mut self,
        _: &GoToDefinition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.trigger_go_to_definition(window, cx);
    }

    // ── Find References ───────────────────────────────────────────────────────

    fn on_find_references(
        &mut self,
        _: &FindReferences,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.trigger_find_references(window, cx);
    }

    fn trigger_find_references(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mgr) = self.lsp_manager.clone() else {
            return;
        };
        let path = self.doc.path.clone();
        let uri = match url::Url::from_file_path(&path) {
            Ok(u) => u,
            Err(_) => return,
        };
        let offset = self.sel.head;
        let encoding = mgr.position_encoding_for_uri(&uri);
        let lsp_pos = faber_lsp::position::to_lsp_position(&self.doc.rope, offset, encoding);
        let params_json = serde_json::json!({
            "textDocument": { "uri": uri.as_str() },
            "position": { "line": lsp_pos.line, "character": lsp_pos.character },
            "context": { "includeDeclaration": true }
        });
        let Some(rx) = mgr.request_for_document(&uri, "textDocument/references", params_json)
        else {
            return;
        };
        cx.spawn_in(window, async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { rx.recv_timeout(std::time::Duration::from_secs(5)) })
                .await;
            let locations = match result {
                Ok(Ok(val)) => parse_definition_locations(&val),
                _ => return,
            };
            if locations.is_empty() {
                return;
            }
            log::debug!("find_references: {} location(s)", locations.len());
            let _ = view.update_in(cx, |ev, window, cx| {
                if let Some(ws) = ev.ws_handle.clone().and_then(|h| h.upgrade()) {
                    window.defer(cx, move |window, cx| {
                        ws.update(cx, |ws, cx| {
                            ws.open_references(locations, window, cx);
                        });
                    });
                }
            });
        })
        .detach();
    }

    fn trigger_go_to_definition(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mgr) = self.lsp_manager.clone() else {
            return;
        };
        let path = self.doc.path.clone();
        let uri = match url::Url::from_file_path(&path) {
            Ok(u) => u,
            Err(_) => return,
        };
        let offset = self.sel.head;
        let encoding = mgr.position_encoding_for_uri(&uri);
        let lsp_pos = faber_lsp::position::to_lsp_position(&self.doc.rope, offset, encoding);
        let params_json = serde_json::json!({
            "textDocument": { "uri": uri.as_str() },
            "position": { "line": lsp_pos.line, "character": lsp_pos.character }
        });
        let Some(rx) = mgr.request_for_document(&uri, "textDocument/definition", params_json)
        else {
            return;
        };
        cx.spawn_in(window, async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { rx.recv_timeout(std::time::Duration::from_secs(5)) })
                .await;
            let locations = match result {
                Ok(Ok(val)) => parse_definition_locations(&val),
                _ => return,
            };
            log::debug!("goto_def: {} location(s)", locations.len());
            let Some(first) = locations.first() else {
                return;
            };
            let (def_path, def_line, def_char) = first.clone();
            let _ = view.update_in(cx, |ev, window, cx| {
                ev.goto_location(def_path, def_line, def_char, window, cx);
            });
        })
        .detach();
    }

    /// Jump to a definition location — in-place for the current document,
    /// through the workspace for other files.
    fn goto_location(
        &mut self,
        def_path: std::path::PathBuf,
        def_line: usize,
        def_char: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        log::debug!(
            "goto_location: {}:{def_line}:{def_char}",
            def_path.display()
        );
        if self.doc.path == def_path {
            let char_idx = self
                .line_starts
                .get(def_line)
                .map(|&ls| ls + def_char)
                .unwrap_or(self.doc.rope.len_chars().saturating_sub(1));
            self.sel.head = char_idx;
            self.sel.anchor = char_idx;
            self.scroll_handle
                .scroll_to_item(def_line, gpui::ScrollStrategy::Center);
            self.flash_line = Some(def_line);
            let epoch = self.cursor_blink_epoch;
            cx.spawn(async move |view, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(800))
                    .await;
                view.update(cx, |ev, cx| {
                    if ev.cursor_blink_epoch == epoch {
                        ev.flash_line = None;
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
            cx.notify();
        } else if let Some(ws) = self.ws_handle.clone().and_then(|h| h.upgrade()) {
            // Defer: navigate_to reads pane editors — including this one, which
            // is mid-update when goto_location runs from a mouse/task handler.
            // A synchronous ws.update here re-enters the entity map and panics.
            window.defer(cx, move |window, cx| {
                ws.update(cx, |ws, cx| {
                    ws.navigate_to(&def_path, def_line, def_char, window, cx);
                });
            });
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

    fn on_backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        // A manual delete consumes the one-shot post-accept suppression — otherwise
        // editing a just-accepted word (delete a letter to fix it) never re-opens
        // completion until the whole word is erased.
        self.completion_suppress_once = false;
        self.do_backspace(cx);
        // Re-trigger completion if menu was dismissed (e.g. after accepting, or after
        // deleting a non-word char like `::`) and there is now a word prefix at the cursor.
        if self.completion.is_none() {
            let head = self.sel.head;
            if let Some((word_start, query)) =
                crate::completion_logic::compute_word_prefix(&self.doc.rope, head)
            {
                if !query.is_empty() {
                    eprintln!("[completion] backspace re-trigger query={:?}", query);
                    self.do_trigger_completion(word_start, query, None, window, cx);
                }
            }
        }
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
        // A selection inside the hover popover takes precedence.
        if let Some(sel) = &self.hover.selection
            && !sel.is_empty()
        {
            let text = crate::hover_popover::selected_text(&self.hover.segments.borrow(), sel);
            if !text.is_empty() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                return;
            }
        }
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
        // Escape dismisses the hover popover before anything else (Zed parity).
        if ks.key.as_str() == "escape" && self.hover.content.is_some() {
            self.dismiss_hover(cx);
            return;
        }
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
            if !self.completion_suppress_once {
                self.schedule_completion_on_input(text, window, cx);
            } else {
                self.completion_suppress_once = false;
            }
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
            let gutter_w = (GUTTER_COLS * t.char_w_code).max(54.0);
            row.child(
                div()
                    .flex_shrink_0()
                    .w(px(gutter_w))
                    .h_full()
                    .bg(t.bg_sunken)
                    .border_r_1()
                    .border_color(t.border)
                    .pr(px(13.))
                    .flex()
                    .items_center()
                    .justify_end()
                    .text_size(px(t.font_size_gutter))
                    .text_color(if cursor_on_line {
                        t.gutter_active
                    } else {
                        t.gutter
                    })
                    .child(format!("{}", line_idx + 1)),
            )
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
        let mut runs = crate::buffer_view::build_text_runs(
            &text,
            self.doc.highlight_spans(line_idx),
            t,
            &line_diags,
        );
        // Cmd+hover link preview: underline the symbol that resolved to a definition.
        if self.link_preview_active()
            && let Some(link) = &self.hovered_link
            && let Some(&line_start) = self.line_starts.get(line_idx)
        {
            let line_str: &str = &text;
            let line_char_count = line_str.chars().count();
            let line_end = line_start + line_char_count;
            if link.symbol_range.start < line_end && link.symbol_range.end > line_start {
                let s_char = link
                    .symbol_range
                    .start
                    .saturating_sub(line_start)
                    .min(line_char_count);
                let e_char = link
                    .symbol_range
                    .end
                    .saturating_sub(line_start)
                    .min(line_char_count);
                let s_byte = char_col_to_byte_col(line_str, s_char);
                let e_byte = char_col_to_byte_col(line_str, e_char);
                let accent = t.accent;
                crate::hover_popover::style_run_range(&mut runs, s_byte..e_byte, |run| {
                    run.color = accent;
                    run.underline = Some(gpui::UnderlineStyle {
                        thickness: px(1.),
                        color: Some(accent),
                        wavy: false,
                    });
                });
            }
        }
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
        let hover_bg = t.bg_raised;
        let sep_color = t.separator;
        let radius = t.radius_sm;
        let radius_md = t.radius_md;
        let border_focus = t.border_focus;

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
                    .text_color(t.text_muted)
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
                .bg(t.bg_sunken)
                .px_2()
                .flex()
                .items_center()
                .rounded(px(radius_md))
                .border_1()
                .border_color(if focused { border_focus } else { t.bg_sunken })
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
            .bg(t.bg_raised)
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
            .bg(t.bg_raised)
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
            .border_color(t.border)
            .bg(t.bg_elevated)
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
                let seg_color = crate::editor_logic::context_to_capture(item.context.as_deref())
                    .and_then(|name| t.highlight_id(name))
                    .and_then(|id| t.syntax_style(id))
                    .map(|s| s.color)
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
                let comment_id = t.highlight_id("comment");
                let in_comment = self.doc.highlight_spans(cursor_line).iter().any(|s| {
                    if comment_id != Some(s.highlight_id) {
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
            .on_mouse_move(cx.listener(Self::on_mouse_move_editor))
            .on_scroll_wheel(cx.listener(|view, _ev: &ScrollWheelEvent, _, cx| {
                // Scrolling detaches the popover from its anchor — hide it (Zed parity).
                view.dismiss_hover(cx);
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
                        .cursor(if self.link_preview_active() {
                            // Only when the symbol under the mouse has a definition.
                            CursorStyle::PointingHand
                        } else {
                            CursorStyle::IBeam
                        })
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
                .on_action(cx.listener(Self::on_show_completions))
                .on_action(cx.listener(Self::on_completion_next))
                .on_action(cx.listener(Self::on_completion_prev))
                .on_action(cx.listener(Self::on_completion_first))
                .on_action(cx.listener(Self::on_completion_last))
                .on_action(cx.listener(Self::on_confirm_completion))
                .on_action(cx.listener(Self::on_cancel_completion))
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
            let key_ctx: &str = if self.outline_open {
                "OutlineOverlay"
            } else if self.completion.is_some() && is_md {
                "Editor markdown showing_completions"
            } else if self.completion.is_some() {
                "Editor showing_completions"
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

        let key_ctx: &str = if self.outline_open {
            "OutlineOverlay"
        } else if self.completion.is_some() && is_md {
            "Editor markdown showing_completions"
        } else if self.completion.is_some() {
            "Editor showing_completions"
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
            .on_action(cx.listener(Self::on_go_to_definition))
            .on_action(cx.listener(Self::on_find_references))
            .on_action(cx.listener(Self::on_show_completions))
            .on_action(cx.listener(Self::on_completion_next))
            .on_action(cx.listener(Self::on_completion_prev))
            .on_action(cx.listener(Self::on_completion_first))
            .on_action(cx.listener(Self::on_completion_last))
            .on_action(cx.listener(Self::on_confirm_completion))
            .on_action(cx.listener(Self::on_cancel_completion))
            .on_modifiers_changed(cx.listener(
                |view, ev: &gpui::ModifiersChangedEvent, window, cx| {
                    let cmd = ev.modifiers.platform;
                    if view.cmd_held == cmd {
                        return;
                    }
                    view.cmd_held = cmd;
                    if cmd {
                        // Re-evaluate the link preview at the current mouse position.
                        if let Some(pos) = view.last_mouse_pos {
                            let t = cx.global::<RuntimeTheme>().clone();
                            let offset = view.hover_offset_at(pos, &t, window);
                            view.update_hovered_link(offset, true, cx);
                        }
                    } else {
                        view.clear_hovered_link(cx);
                    }
                    cx.notify();
                },
            ))
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
        let root = if self.hover.content.is_some() {
            match self.render_hover_popover(&t, cx) {
                Some(popover) => root.child(popover),
                None => root,
            }
        } else {
            root
        };
        let root = if self.completion.is_some() {
            match self.render_completion_overlay(&t, window, cx) {
                Some(el) => root.child(el),
                None => root,
            }
        } else {
            root
        };
        root.into_any()
    }
}

// ── Hover popover ──────────────────────────────────────────────────────────────

impl EditorView {
    /// Render the hover popover, LOCKED to the symbol anchor computed at show
    /// time (Zed parity — the popover never follows the mouse). Content is
    /// selectable; links open on click; a selection keeps the popover alive
    /// until the user clicks outside it.
    fn render_hover_popover(
        &mut self,
        t: &RuntimeTheme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        use crate::hover_popover::{SegmentKind, hit_test, link_at, segment_styled_text};

        let anchor = self.hover.anchor?;
        let bounds_cell = Rc::clone(&self.hover.bounds);
        let scroll = self.hover.scroll.clone();
        let selection = self.hover.selection;
        let sel_color = t.selection;

        // ── content: block-structured rendering ───────────────────────────────
        // Consecutive segments group into their block containers: code lines →
        // sunken panel, quote lines → bordered callout, images → badge row,
        // table cells → bordered grid. Everything else renders standalone.
        #[derive(Clone, PartialEq)]
        enum Bucket {
            Plain,
            Code,
            Quote,
            Images,
            Table,
        }

        // Builds the selectable text wrapper for one segment. `gap_override`
        // suppresses the segment's own top gap when a group container owns it.
        let seg_wrapper = |seg: &mut crate::hover_popover::Segment,
                           sel_range: Option<std::ops::Range<usize>>,
                           gap_override: Option<f32>,
                           t: &RuntimeTheme|
         -> AnyElement {
            let styled = segment_styled_text(seg, sel_range, sel_color);
            // Retain the layout handle: selection hit-testing reads it on
            // mouse events between frames.
            seg.layout = styled.layout().clone();
            let seg_bounds = Rc::clone(&seg.bounds);
            seg.bounds.set(None);
            let is_code = seg.kind == crate::hover_popover::SegmentKind::Code;
            let gap = gap_override.unwrap_or(seg.top_gap);
            div()
                .relative()
                .w_full()
                .mt(px(gap))
                .ml(px(seg.indent))
                .min_h(px(if is_code {
                    t.line_height_code
                } else {
                    seg.text_size + 4.
                }))
                .when(is_code, |el| el.font_family(t.mono_family.clone()))
                .when(!is_code, |el| {
                    el.font_family(t.ui_family.clone()).pb(px(2.))
                })
                .text_size(px(seg.text_size))
                .text_color(if is_code { t.text } else { t.text_muted })
                .child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, _, _| seg_bounds.set(Some(bounds)),
                    )
                    .absolute()
                    .size_full(),
                )
                .child(styled)
                .into_any_element()
        };

        let mut content_children: Vec<AnyElement> = Vec::new();
        {
            let mut segments = self.hover.segments.borrow_mut();
            let seg_count = segments.len();

            let mut cur: Option<Bucket> = None;
            let mut group: Vec<AnyElement> = Vec::new();
            let mut group_gap = 0.0f32;
            let mut first_in_group = false;
            let mut table_rows: Vec<Vec<AnyElement>> = Vec::new();

            let flush = |bucket: &Option<Bucket>,
                         group: &mut Vec<AnyElement>,
                         table_rows: &mut Vec<Vec<AnyElement>>,
                         out: &mut Vec<AnyElement>,
                         gap: f32,
                         t: &RuntimeTheme| {
                match bucket {
                    Some(Bucket::Code) if !group.is_empty() => out.push(
                        div()
                            .w_full()
                            .mt(px(gap))
                            .bg(t.bg_sunken)
                            .rounded(px(t.radius_xs))
                            .px(px(t.sp3))
                            .py(px(t.sp2))
                            .children(std::mem::take(group))
                            .into_any_element(),
                    ),
                    Some(Bucket::Quote) if !group.is_empty() => out.push(
                        div()
                            .w_full()
                            .mt(px(gap))
                            .border_l_2()
                            .border_color(t.accent_muted)
                            .pl(px(t.sp3))
                            .children(std::mem::take(group))
                            .into_any_element(),
                    ),
                    Some(Bucket::Images) if !group.is_empty() => out.push(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .items_center()
                            .gap(px(6.))
                            .mt(px(gap))
                            .children(std::mem::take(group))
                            .into_any_element(),
                    ),
                    Some(Bucket::Table) if !table_rows.is_empty() => {
                        let n_rows = table_rows.len();
                        let rows: Vec<AnyElement> = std::mem::take(table_rows)
                            .into_iter()
                            .enumerate()
                            .map(|(r, cells)| {
                                div()
                                    .flex()
                                    .flex_row()
                                    .when(r == 0, |el| el.bg(t.bg_elevated))
                                    .when(r + 1 < n_rows, |el| {
                                        el.border_b_1().border_color(t.separator)
                                    })
                                    .children(cells)
                                    .into_any_element()
                            })
                            .collect();
                        out.push(
                            div()
                                .w_full()
                                .mt(px(gap))
                                .border_1()
                                .border_color(t.separator)
                                .rounded(px(t.radius_sm))
                                .overflow_hidden()
                                .children(rows)
                                .into_any_element(),
                        );
                    }
                    _ => {}
                }
            };

            for ix in 0..seg_count {
                let bucket = {
                    let seg = &segments[ix];
                    if seg.table_cell.is_some() {
                        Bucket::Table
                    } else if matches!(seg.kind, SegmentKind::Image { .. }) {
                        Bucket::Images
                    } else if seg.quote {
                        Bucket::Quote
                    } else if seg.kind == SegmentKind::Code {
                        Bucket::Code
                    } else {
                        Bucket::Plain
                    }
                };
                if cur.as_ref() != Some(&bucket) {
                    flush(
                        &cur,
                        &mut group,
                        &mut table_rows,
                        &mut content_children,
                        group_gap,
                        t,
                    );
                    group_gap = segments[ix].top_gap;
                    first_in_group = true;
                    cur = Some(bucket.clone());
                }

                let sel_range = {
                    let seg = &segments[ix];
                    selection.and_then(|s| s.range_in_segment(ix, seg.text.len()))
                };
                match bucket {
                    Bucket::Plain => {
                        let seg = &mut segments[ix];
                        if seg.kind == SegmentKind::Rule {
                            content_children
                                .push(div().h(px(1.)).my(px(6.)).bg(t.border).into_any_element());
                        } else {
                            content_children.push(seg_wrapper(seg, sel_range, None, t));
                        }
                    }
                    Bucket::Code => {
                        let seg = &mut segments[ix];
                        // The panel owns the block gap; lines stack flush.
                        group.push(seg_wrapper(seg, sel_range, Some(0.0), t));
                    }
                    Bucket::Quote => {
                        let seg = &mut segments[ix];
                        let gap = if first_in_group { 0.0 } else { seg.top_gap };
                        group.push(seg_wrapper(seg, sel_range, Some(gap), t));
                    }
                    Bucket::Images => {
                        let seg = &segments[ix];
                        if let SegmentKind::Image { url, link } = &seg.kind {
                            let image = gpui::img(url.clone())
                                .h(px(18.))
                                .object_fit(gpui::ObjectFit::ScaleDown)
                                .into_any_element();
                            let el = match link {
                                Some(target) => {
                                    let target = target.clone();
                                    div()
                                        .id(("hover-img", ix))
                                        .cursor_pointer()
                                        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                            cx.stop_propagation();
                                            cx.open_url(&target);
                                        })
                                        .child(image)
                                        .into_any_element()
                                }
                                None => div().child(image).into_any_element(),
                            };
                            group.push(el);
                        }
                    }
                    Bucket::Table => {
                        let (_, col, _) = segments[ix].table_cell.unwrap_or((0, 0, 1));
                        if col == 0 {
                            table_rows.push(Vec::new());
                        }
                        let seg = &mut segments[ix];
                        let cell = div()
                            .flex_1()
                            .min_w(px(60.))
                            .px(px(t.sp2))
                            .py(px(2.))
                            .child(seg_wrapper(seg, sel_range, Some(0.0), t))
                            .into_any_element();
                        if let Some(row) = table_rows.last_mut() {
                            row.push(cell);
                        }
                    }
                }
                first_in_group = false;
            }
            flush(
                &cur,
                &mut group,
                &mut table_rows,
                &mut content_children,
                group_gap,
                t,
            );
        }

        // ── placement: prefer above the symbol line, flip below near the top ──
        // FLUSH against the line (Zed parity): any gap would overlap adjacent
        // lines, and crossing another symbol on the way to the popover would
        // dismiss it.
        let est_h = self.hover.estimated_height.min(HOVER_MAX_H);
        let above = anchor.line_top - est_h >= 8.0;
        let (pos, corner) = if above {
            (
                point(px(anchor.x), px(anchor.line_top)),
                gpui::Corner::BottomLeft,
            )
        } else {
            (
                point(px(anchor.x), px(anchor.line_bottom)),
                gpui::Corner::TopLeft,
            )
        };

        let popover = glass_surface(t)
            .id("hover-popover")
            .occlude()
            .cursor(CursorStyle::IBeam)
            .rounded(px(t.radius_lg))
            .on_mouse_move(cx.listener(|ev, e: &MouseMoveEvent, _, cx| {
                // Inside the popover: never hide; extend an active selection.
                ev.last_mouse_pos = Some(e.position);
                ev.hover.closest_distance = Some(px(0.));
                ev.hover.hiding_task = None;
                // Back inside — stop any edge auto-scroll.
                ev.hover.autoscroll_task = None;
                if let Some(drag) = ev.hover.scrollbar_drag {
                    update_drag(&drag, e, &ev.hover.scroll);
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                if ev.hover.selecting
                    && let Some(hit) = hit_test(&ev.hover.segments, e.position)
                    && let Some(sel) = &mut ev.hover.selection
                {
                    sel.end = hit;
                    cx.notify();
                }
                cx.stop_propagation();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|ev, e: &MouseDownEvent, _, cx| {
                    ev.hover.pressed_link = link_at(&ev.hover.segments, e.position);
                    if let Some(hit) = hit_test(&ev.hover.segments, e.position) {
                        ev.hover.selection = Some(crate::hover_popover::HoverSelection {
                            start: hit,
                            end: hit,
                        });
                        ev.hover.selecting = true;
                    }
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|ev, e: &MouseUpEvent, _, cx| {
                    ev.hover.scrollbar_drag = None;
                    ev.hover.autoscroll_task = None;
                    ev.hover.selecting = false;
                    let click_no_drag = ev.hover.selection.is_some_and(|s| s.is_empty());
                    if click_no_drag {
                        ev.hover.selection = None;
                    }
                    // Press + release on the same link without dragging → open it.
                    if click_no_drag
                        && let Some(pressed) = ev.hover.pressed_link
                        && link_at(&ev.hover.segments, e.position) == Some(pressed)
                    {
                        let url = ev
                            .hover
                            .segments
                            .borrow()
                            .get(pressed.0)
                            .and_then(|s| s.links.get(pressed.1).map(|(_, u)| u.clone()));
                        if let Some(url) = url {
                            log::debug!("hover: opening link {url}");
                            cx.open_url(&url);
                        }
                    }
                    ev.hover.pressed_link = None;
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(
                // Transparent overlay that records the popover's painted bounds
                // for sticky hit-testing.
                canvas(
                    |_, _, _| (),
                    move |bounds, _, _, _| {
                        bounds_cell.set(Some(bounds));
                    },
                )
                .absolute()
                .size_full(),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .max_h(px(HOVER_MAX_H))
                    .child(
                        // Block-level layout (no flex) so overflow_y_scroll can
                        // measure true content height and activate scrolling.
                        div()
                            .id("hover-scroll")
                            .overflow_y_scroll()
                            .max_w(px(HOVER_MAX_W))
                            .max_h(px(HOVER_MAX_H))
                            .track_scroll(&scroll)
                            .py(px(13.))
                            .px(px(15.))
                            .children(content_children),
                    )
                    .child(render_scrollbar(
                        "hover-scrollbar",
                        "hover-scrollbar-thumb",
                        &scroll,
                        true,
                        self.hover.scrollbar_drag.is_some(),
                        cx.listener(|ev, e: &MouseDownEvent, _, cx| {
                            ev.hover.scrollbar_drag = Some(start_drag(e, &ev.hover.scroll));
                            cx.stop_propagation();
                            cx.notify();
                        }),
                        t,
                        None,
                    )),
            );

        Some(
            deferred(
                anchored()
                    .position(pos)
                    .anchor(corner)
                    .snap_to_window_with_margin(px(8.))
                    .child(popover),
            )
            .with_priority(3)
            .into_any_element(),
        )
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
                        .mx(px(5.))
                        .my(px(1.))
                        .px(px(8. + indent))
                        .h(px(34.))
                        .gap_2()
                        .rounded(px(t.radius_md))
                        .font_family(t.ui_family.clone())
                        .text_size(px(13.))
                        .text_color(t.text)
                        .when(is_hovered, |el| el.bg(t.accent_muted))
                        .cursor_pointer()
                        .hover(|el| el.bg(t_clone.bg_raised))
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
                                    .text_size(px(11.))
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

/// Parse a `textDocument/definition` response into (path, line, char) tuples.
/// Handles Location, LocationLink, and arrays of either.
fn parse_definition_locations(val: &serde_json::Value) -> Vec<(std::path::PathBuf, usize, usize)> {
    let items: Vec<&serde_json::Value> = if val.is_array() {
        val.as_array().unwrap().iter().collect()
    } else if val.is_object() {
        vec![val]
    } else {
        return Vec::new();
    };

    items
        .into_iter()
        .filter_map(|item| {
            // LocationLink has targetUri/targetRange; Location has uri/range.
            let uri_str = item
                .get("targetUri")
                .or_else(|| item.get("uri"))
                .and_then(|v| v.as_str())?;
            let range = item
                .get("targetSelectionRange")
                .or_else(|| item.get("targetRange"))
                .or_else(|| item.get("range"))
                .and_then(|r| r.as_object())?;
            let start = range.get("start").and_then(|s| s.as_object())?;
            let line = start.get("line").and_then(|l| l.as_u64())? as usize;
            let character = start.get("character").and_then(|c| c.as_u64())? as usize;
            let url = url::Url::parse(uri_str).ok()?;
            let path = url.to_file_path().ok()?;
            Some((path, line, character))
        })
        .collect()
}

// ── Completion ─────────────────────────────────────────────────────────────────

const COMPLETION_ITEM_H: f32 = 24.0;

impl EditorView {
    fn schedule_completion_on_input(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mgr) = self.lsp_manager.clone() else {
            return;
        };
        let path = self.doc.path.clone();
        let Ok(uri) = url::Url::from_file_path(&path) else {
            return;
        };
        let trigger_chars = mgr.completion_trigger_chars_for_uri(&uri);
        let is_trigger_char = trigger_chars.iter().any(|c| c.as_str() == text);
        let is_word_char = text.chars().all(|c| c.is_alphanumeric() || c == '_');

        if is_trigger_char {
            let head = self.sel.head;
            eprintln!("[completion] trigger char {:?} at {head}", text);
            self.do_trigger_completion(head, String::new(), Some(text.to_string()), window, cx);
            return;
        }

        if !is_word_char {
            if self.completion.is_some() {
                eprintln!("[completion] non-word char {:?} — dismiss", text);
                self.dismiss_completion(cx);
            }
            return;
        }

        let head = self.sel.head;
        let Some((word_start, query)) =
            crate::completion_logic::compute_word_prefix(&self.doc.rope, head)
        else {
            eprintln!("[completion] word char but no prefix — dismiss");
            self.dismiss_completion(cx);
            return;
        };

        if let Some(ref menu) = self.completion {
            if !menu.is_incomplete {
                eprintln!("[completion] word char — local refilter handles it");
                return;
            }
            // isIncomplete=true: never re-request while we have a cached item set.
            // Zed keeps the full original item set and re-filters locally; empty local
            // matches hide the overlay rather than wiping items. Re-requesting would
            // replace items with Vec::new() while the task is in flight, breaking delete-back.
            if !menu.items.is_empty() {
                eprintln!(
                    "[completion] isIncomplete=true but {} cached items — local refilter only",
                    menu.items.len()
                );
                return;
            }
            eprintln!(
                "[completion] isIncomplete=true, no cached items — re-request query={:?}",
                query
            );
        } else {
            eprintln!(
                "[completion] new trigger word_start={word_start} query={:?}",
                query
            );
        }

        self.do_trigger_completion(word_start, query, None, window, cx);
    }

    fn do_show_completions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let head = self.sel.head;
        let (word_start, query) =
            crate::completion_logic::compute_word_prefix(&self.doc.rope, head)
                .unwrap_or((head, String::new()));
        eprintln!(
            "[completion] manual trigger word_start={word_start} query={:?}",
            query
        );
        self.do_trigger_completion(word_start, query, None, window, cx);
    }

    fn do_trigger_completion(
        &mut self,
        word_start: usize,
        query: String,
        trigger_char: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mgr) = self.lsp_manager.clone() else {
            eprintln!("[completion] no lsp_manager");
            return;
        };
        let path = self.doc.path.clone();
        let Ok(uri) = url::Url::from_file_path(&path) else {
            return;
        };
        let encoding = mgr.position_encoding_for_uri(&uri);
        let head = self.sel.head;
        let lsp_pos = faber_lsp::position::to_lsp_position(&self.doc.rope, head, encoding);
        let trigger_kind: u8 = if trigger_char.is_some() { 2 } else { 1 };
        let context_json = if let Some(ref ch) = trigger_char {
            serde_json::json!({ "triggerKind": trigger_kind, "triggerCharacter": ch })
        } else {
            serde_json::json!({ "triggerKind": trigger_kind })
        };
        let params_json = serde_json::json!({
            "textDocument": { "uri": uri.as_str() },
            "position": { "line": lsp_pos.line, "character": lsp_pos.character },
            "context": context_json,
        });
        let Some(rx) = mgr.request_for_document(&uri, "textDocument/completion", params_json)
        else {
            eprintln!("[completion] no running server");
            return;
        };
        eprintln!(
            "[completion] LSP request head={head} pos={}:{} query={:?}",
            lsp_pos.line, lsp_pos.character, query
        );

        let initial_query = query.clone();
        let registry = Arc::clone(&self.registry);
        let ext = self
            .doc
            .path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_string());
        let task =
            cx.spawn_in(window, async move |view, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move { rx.recv_timeout(std::time::Duration::from_secs(5)) })
                    .await;
                match result {
                    Ok(Ok(val)) => {
                        let list = faber_lsp::completion::parse_completion_response(&val);
                        eprintln!(
                            "[completion] response {} items is_incomplete={}",
                            list.items.len(),
                            list.is_incomplete
                        );
                        view.update_in(cx, |ev, window, cx| {
                        let current_head = ev.sel.head;
                        if current_head < word_start {
                            eprintln!("[completion] stale: cursor before word_start");
                            return;
                        }
                        let current_query: String = ev
                            .doc
                            .rope
                            .chars_at(word_start)
                            .take(current_head.saturating_sub(word_start))
                            .collect();
                        eprintln!(
                            "[completion] applying initial={:?} current={:?}",
                            initial_query, current_query
                        );

                        let filtered =
                            crate::completion_logic::fuzzy_filter(&list.items, &current_query);
                        let filtered_indices: Vec<usize> =
                            filtered.iter().map(|m| m.item_ix).collect();
                        eprintln!("[completion] filtered {} items", filtered_indices.len());

                        if filtered_indices.is_empty() {
                            // Keep existing local-filtered items if we still have them —
                            // server may return empty for an incomplete/throttled query.
                            if ev.completion.as_ref().is_some_and(|m| !m.filtered.is_empty()) {
                                eprintln!(
                                    "[completion] server empty but {} local items remain — keep",
                                    ev.completion.as_ref().unwrap().filtered.len()
                                );
                                if let Some(ref mut menu) = ev.completion {
                                    menu.is_incomplete = false;
                                }
                                cx.notify();
                                return;
                            }
                            let action = crate::completion_logic::resolve_empty_filter(
                                list.is_incomplete,
                                &initial_query,
                                &current_query,
                            );
                            eprintln!("[completion] empty filter action={:?}", action);
                            match action {
                                crate::completion_logic::EmptyFilterAction::Dismiss => {
                                    ev.completion = None;
                                }
                                crate::completion_logic::EmptyFilterAction::Rerequest => {
                                    ev.do_trigger_completion(
                                        word_start,
                                        current_query,
                                        None,
                                        window,
                                        cx,
                                    );
                                    return;
                                }
                            }
                            cx.notify();
                            return;
                        }

                        let old_label = ev
                            .completion
                            .as_ref()
                            .and_then(|m| m.selected_item())
                            .map(|i| i.label.clone());
                        let selected_ix = old_label
                            .and_then(|lbl| {
                                filtered_indices.iter().position(|&ix| {
                                    list.items.get(ix).is_some_and(|it| it.label == lbl)
                                })
                            })
                            .unwrap_or(0);

                        let old_parts =
                            ev.completion.take().map(|m| (m.scroll, m.doc_segments, m.doc_scroll));
                        let (old_scroll, new_doc_segments, new_doc_scroll) = match old_parts {
                            Some((s, ds, dsc)) => (Some(s), ds, dsc),
                            None => (
                                None,
                                std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
                                ScrollHandle::new(),
                            ),
                        };

                        // Rebuild doc segments for the initially selected item if it has docs.
                        {
                            let doc_text = filtered_indices
                                .get(selected_ix)
                                .and_then(|&ix| list.items.get(ix))
                                .and_then(|it| {
                                    it.documentation.as_ref().or(it.detail.as_ref())
                                })
                                .cloned();
                            if let Some(text) = doc_text {
                                let t = cx.global::<RuntimeTheme>().clone();
                                let rope = ropey::Rope::from_str(&text);
                                let md = std::sync::Arc::new(
                                    faber_editor::markdown::parse_markdown_with_fallback(
                                        &text,
                                        &rope,
                                        &registry,
                                        ext.as_deref(),
                                    ),
                                );
                                crate::hover_popover::rebuild_segments(&new_doc_segments, &md, &t);
                            }
                        }

                        ev.completion = Some(CompletionMenu {
                            items: list.items,
                            filtered: filtered_indices,
                            selected_ix,
                            word_start,
                            query: current_query,
                            initial_query,
                            is_incomplete: list.is_incomplete,
                            scroll: old_scroll.unwrap_or_else(ScrollHandle::new),
                            request_task: None,
                            doc_text: None,
                            resolve_task: None,
                            locked_anchor: None,
                            doc_segments: new_doc_segments,
                            doc_scroll: new_doc_scroll,
                        });
                        cx.notify();
                    })
                    .ok();
                    }
                    Ok(Err(e)) => eprintln!("[completion] LSP error: {e:?}"),
                    Err(_) => eprintln!("[completion] timed out"),
                }
            });

        let old_parts = self
            .completion
            .take()
            .map(|m| (m.scroll, m.doc_segments, m.doc_scroll, m.locked_anchor));
        let (old_scroll, stub_doc_segments, stub_doc_scroll, old_anchor) = match old_parts {
            Some((s, ds, dsc, anc)) => (Some(s), ds, dsc, anc),
            None => (
                None,
                std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
                ScrollHandle::new(),
                None,
            ),
        };
        self.completion = Some(CompletionMenu {
            items: Vec::new(),
            filtered: Vec::new(),
            selected_ix: 0,
            word_start,
            query: query.clone(),
            initial_query: query,
            is_incomplete: false,
            scroll: old_scroll.unwrap_or_else(ScrollHandle::new),
            request_task: Some(task),
            doc_text: None,
            resolve_task: None,
            locked_anchor: old_anchor,
            doc_segments: stub_doc_segments,
            doc_scroll: stub_doc_scroll,
        });
    }

    fn refresh_completion_filter_or_dismiss(&mut self, cx: &mut Context<Self>) {
        let head = self.sel.head;
        let Some(ref menu) = self.completion else {
            return;
        };
        let word_start = menu.word_start;

        if head < word_start {
            eprintln!("[completion] cursor before word_start — dismiss");
            self.completion = None;
            cx.notify();
            return;
        }

        let current_query: String = self
            .doc
            .rope
            .chars_at(word_start)
            .take(head.saturating_sub(word_start))
            .collect();

        if current_query
            .chars()
            .any(|c| !c.is_alphanumeric() && c != '_')
        {
            eprintln!("[completion] non-word in query — dismiss");
            self.completion = None;
            cx.notify();
            return;
        }

        eprintln!("[completion] refilter query={:?}", current_query);

        let filtered = crate::completion_logic::fuzzy_filter(&menu.items, &current_query);
        let filtered_indices: Vec<usize> = filtered.iter().map(|m| m.item_ix).collect();

        if filtered_indices.is_empty() {
            // Zed: never dismiss on empty local filter — keep menu alive so delete-back
            // restores matches against the retained full item set. Overlay hides automatically
            // when filtered is empty (render fn returns None). Only real dismissal triggers
            // are: empty query, cursor leaving word, escape, confirm.
            eprintln!("[completion] refilter empty — hide overlay, keep menu alive");
            if let Some(ref mut menu) = self.completion {
                menu.query = current_query;
                menu.filtered = vec![];
                menu.selected_ix = 0;
            }
            cx.notify();
            return;
        }

        if let Some(ref mut menu) = self.completion {
            menu.query = current_query;
            menu.filtered = filtered_indices;
            menu.selected_ix = menu.selected_ix.min(menu.filtered.len().saturating_sub(1));
            eprintln!(
                "[completion] refilter done {} items selected={}",
                menu.filtered.len(),
                menu.selected_ix
            );
        }
        cx.notify();
    }

    pub fn dismiss_completion(&mut self, cx: &mut Context<Self>) {
        if self.completion.is_some() {
            eprintln!("[completion] dismiss");
            self.completion = None;
            cx.notify();
        }
    }

    fn accept_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.completion.take() else {
            eprintln!("[completion] accept: no menu");
            return;
        };
        let Some(&item_ix) = menu.filtered.get(menu.selected_ix) else {
            eprintln!("[completion] accept: no item at {}", menu.selected_ix);
            cx.notify();
            return;
        };
        let item = &menu.items[item_ix];
        eprintln!("[completion] accept ix={item_ix} label={:?}", item.label);

        let Some(mgr) = self.lsp_manager.clone() else {
            return;
        };
        let path = self.doc.path.clone();
        let Ok(uri) = url::Url::from_file_path(&path) else {
            return;
        };
        let encoding = mgr.position_encoding_for_uri(&uri);

        let (insert_range, text) = if let Some(edit) = &item.text_edit {
            let start =
                faber_lsp::position::from_lsp_position(&self.doc.rope, edit.range.start, encoding)
                    .unwrap_or(menu.word_start);
            let end =
                faber_lsp::position::from_lsp_position(&self.doc.rope, edit.range.end, encoding)
                    .unwrap_or(self.sel.head);
            let t = if item.is_snippet {
                faber_lsp::completion::strip_snippet(&edit.new_text)
            } else {
                edit.new_text.clone()
            };
            eprintln!("[completion] text_edit {start}..{end} text={:?}", t);
            (start..end, t)
        } else {
            let raw = item.insert_text.as_deref().unwrap_or(&item.label);
            let t = if item.is_snippet {
                faber_lsp::completion::strip_snippet(raw)
            } else {
                raw.to_owned()
            };
            eprintln!(
                "[completion] insert {}.{} text={:?}",
                menu.word_start, self.sel.head, t
            );
            (menu.word_start..self.sel.head, t)
        };

        self.history.commit();
        let char_count = text.chars().count();
        let tx =
            faber_editor::Transaction::replace(&self.doc.rope, insert_range.clone(), text.clone());
        let inverse = self.doc.apply(tx);
        self.history.push_change(inverse);
        let new_pos = (insert_range.start + char_count).min(self.doc.len_chars());
        self.sel = faber_editor::Selection::collapsed(new_pos, &self.doc.rope);
        self.completion_suppress_once = true;
        self.after_edit(cx);
        let _ = window;
    }

    fn scroll_completion_to_selected(&self) {
        if let Some(ref menu) = self.completion {
            const VISIBLE_H: f32 = 10.0 * COMPLETION_ITEM_H;
            let item_top = menu.selected_ix as f32 * COMPLETION_ITEM_H;
            let item_bottom = item_top + COMPLETION_ITEM_H;
            // GPUI scroll offsets are negative (scrolled-down content has negative y).
            // Viewport shows content from (-offset_y) to (-offset_y + VISIBLE_H).
            let offset_y = f32::from(menu.scroll.offset().y);
            let viewport_top = -offset_y;
            let viewport_bottom = viewport_top + VISIBLE_H;
            let new_offset_y = if item_top < viewport_top {
                // Item above viewport — scroll up to show at top.
                -item_top
            } else if item_bottom > viewport_bottom {
                // Item below viewport — scroll down to show at bottom.
                -(item_bottom - VISIBLE_H)
            } else {
                return;
            };
            menu.scroll
                .set_offset(gpui::point(gpui::px(0.), gpui::px(new_offset_y)));
        }
    }

    fn on_show_completions(
        &mut self,
        _: &ShowCompletions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        self.do_show_completions(window, cx);
    }

    fn on_completion_next(
        &mut self,
        _: &CompletionNext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        if let Some(ref mut menu) = self.completion {
            let n = menu.filtered.len();
            if n == 0 {
                return;
            }
            menu.selected_ix = (menu.selected_ix + 1) % n;
            eprintln!("[completion] next → {}", menu.selected_ix);
        }
        self.scroll_completion_to_selected();
        self.schedule_completion_resolve(window, cx);
        cx.notify();
    }

    fn on_completion_prev(
        &mut self,
        _: &CompletionPrev,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        if let Some(ref mut menu) = self.completion {
            let n = menu.filtered.len();
            if n == 0 {
                return;
            }
            menu.selected_ix = if menu.selected_ix == 0 {
                n - 1
            } else {
                menu.selected_ix - 1
            };
            eprintln!("[completion] prev → {}", menu.selected_ix);
        }
        self.scroll_completion_to_selected();
        self.schedule_completion_resolve(window, cx);
        cx.notify();
    }

    fn on_completion_first(
        &mut self,
        _: &CompletionFirst,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        if let Some(ref mut menu) = self.completion {
            menu.selected_ix = 0;
        }
        self.scroll_completion_to_selected();
        self.schedule_completion_resolve(window, cx);
        cx.notify();
    }

    fn on_completion_last(
        &mut self,
        _: &CompletionLast,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        if let Some(ref mut menu) = self.completion {
            let n = menu.filtered.len();
            if n > 0 {
                menu.selected_ix = n - 1;
            }
        }
        self.scroll_completion_to_selected();
        self.schedule_completion_resolve(window, cx);
        cx.notify();
    }

    /// Fire a `completionItem/resolve` request for the currently selected item if it
    /// has no inline documentation yet. Stores result in `menu.doc_text` and rebuilds
    /// markdown segments for the doc panel.
    fn schedule_completion_resolve(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mgr) = self.lsp_manager.clone() else {
            return;
        };
        let path = self.doc.path.clone();
        let Ok(uri) = url::Url::from_file_path(&path) else {
            return;
        };
        let Some(ref mut menu) = self.completion else {
            return;
        };

        // If the selected item already has inline documentation, rebuild segments and show.
        if let Some(&item_ix) = menu.filtered.get(menu.selected_ix) {
            if let Some(inline_doc) = menu
                .items
                .get(item_ix)
                .and_then(|it| it.documentation.as_ref())
            {
                let text = inline_doc.clone();
                let t = cx.global::<RuntimeTheme>().clone();
                let rope = ropey::Rope::from_str(&text);
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|s| s.to_string());
                let md = std::sync::Arc::new(faber_editor::markdown::parse_markdown_with_fallback(
                    &text,
                    &rope,
                    &self.registry,
                    ext.as_deref(),
                ));
                crate::hover_popover::rebuild_segments(&menu.doc_segments, &md, &t);
                return;
            }
        }

        let Some(&item_ix) = menu.filtered.get(menu.selected_ix) else {
            return;
        };
        // Build the completionItem JSON for the resolve request.
        // Protocol requires at minimum `label`; servers use `data` for resolution context.
        let resolve_json = {
            let it = &menu.items[item_ix];
            let mut obj = serde_json::Map::new();
            obj.insert("label".into(), serde_json::json!(it.label));
            if let Some(k) = it.kind {
                obj.insert("kind".into(), serde_json::json!(k));
            }
            if let Some(ref d) = it.detail {
                obj.insert("detail".into(), serde_json::json!(d));
            }
            if !it.data.is_null() {
                obj.insert("data".into(), it.data.clone());
            }
            serde_json::Value::Object(obj)
        };
        let Some(rx) = mgr.resolve_completion_item(&uri, resolve_json) else {
            return;
        };
        // Clear stale doc from previous selection.
        menu.doc_text = None;
        menu.doc_segments.borrow_mut().clear();
        let resolve_registry = Arc::clone(&self.registry);
        let resolve_ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_string());
        let task = cx.spawn_in(window, async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { rx.recv_timeout(std::time::Duration::from_secs(3)) })
                .await;
            if let Ok(Ok(val)) = result {
                let doc = faber_lsp::completion::extract_doc_text(val.get("documentation"));
                view.update(cx, |ev, cx| {
                    if let Some(ref mut m) = ev.completion {
                        m.doc_text = doc;
                        // Rebuild markdown segments for the doc panel.
                        if let Some(ref text) = m.doc_text {
                            let t = cx.global::<RuntimeTheme>().clone();
                            let rope = ropey::Rope::from_str(text);
                            let md = std::sync::Arc::new(
                                faber_editor::markdown::parse_markdown_with_fallback(
                                    text,
                                    &rope,
                                    &resolve_registry,
                                    resolve_ext.as_deref(),
                                ),
                            );
                            crate::hover_popover::rebuild_segments(&m.doc_segments, &md, &t);
                        }
                        cx.notify();
                    }
                })
                .ok();
            }
        });
        if let Some(ref mut menu) = self.completion {
            menu.resolve_task = Some(task);
        }
    }

    fn on_confirm_completion(
        &mut self,
        _: &ConfirmCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        eprintln!(
            "[completion] confirm selected={}",
            self.completion.as_ref().map(|m| m.selected_ix).unwrap_or(0)
        );
        self.accept_completion(window, cx);
    }

    fn on_cancel_completion(
        &mut self,
        _: &CancelCompletion,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        eprintln!("[completion] cancel");
        self.dismiss_completion(cx);
    }

    fn render_completion_overlay(
        &mut self,
        t: &RuntimeTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.completion.as_ref()?.filtered.is_empty() {
            return None;
        }

        let word_start = self.completion.as_ref()?.word_start;

        // Recompute the word's CURRENT window position every frame. `deferred` paints
        // this overlay outside the scroll transform, so painting at the live position
        // keeps the overlay glued to the word — it travels with the text as the buffer
        // scrolls, instead of parking at a fixed window spot.
        let anchor = self.hover_anchor_for_offset(word_start, t, window)?;

        // Hide (but keep state) when the typed word scrolls off-screen.
        {
            let word_line = self
                .doc
                .rope
                .char_to_line(word_start.min(self.doc.len_chars().saturating_sub(1)));
            let top_line = self.top_visible_line(t);
            let viewport_h = {
                let st = self.scroll_handle.0.borrow();
                f32::from(st.base_handle.bounds().size.height)
            };
            let visible_lines = (viewport_h / t.line_height_code).ceil() as usize;
            let bottom_line = top_line + visible_lines;
            if word_line < top_line || word_line >= bottom_line {
                eprintln!(
                    "[completion] word_line={word_line} outside [{top_line},{bottom_line}) — hidden"
                );
                return None;
            }
        }

        let selected_ix = self.completion.as_ref()?.selected_ix;
        let scroll = self.completion.as_ref()?.scroll.clone();

        const MIN_VISIBLE: usize = 3;
        const MAX_VISIBLE: usize = 12;
        const DROPDOWN_W: f32 = 320.0;
        const DOC_MIN_W: f32 = 320.0;
        const DOC_MAX_W: f32 = 500.0;

        // Viewport bounds (window space) — size the menu to the space available
        // below the anchor, flipping above only when the space below is cramped
        // (Zed parity: more rows shown when there is room).
        let (vp_top, vp_bottom) = {
            let st = self.scroll_handle.0.borrow();
            let b = st.base_handle.bounds();
            let top = f32::from(b.origin.y);
            (top, top + f32::from(b.size.height))
        };
        let space_below = (vp_bottom - anchor.line_bottom).max(0.0);
        let space_above = (anchor.line_top - vp_top).max(0.0);
        let min_menu_h = MIN_VISIBLE as f32 * COMPLETION_ITEM_H;
        let place_above = space_below < min_menu_h && space_above > space_below;
        let avail = if place_above {
            space_above
        } else {
            space_below
        };

        let item_h_px = gpui::px(COMPLETION_ITEM_H);
        let caption_sz = gpui::px(t.font_size_caption);
        let code_sz = gpui::px(t.font_size_code);
        let selected_bg = t.accent_muted;

        // Kind → theme syntax color, reusing the editor's own token palette.
        let syntax_color = |name: &str| {
            t.highlight_id(name)
                .map(|id| t.syntax_color(id))
                .unwrap_or(t.text)
        };
        let c_function = syntax_color("function");
        let c_variable = syntax_color("variable");
        let c_type = syntax_color("type");
        let c_keyword = syntax_color("keyword");
        let c_constant = syntax_color("constant");
        let c_namespace = syntax_color("namespace");
        // Color + compact glyph per LSP CompletionItemKind, resolved in one pass.
        let kind_info = |kind: Option<i32>| -> (gpui::Hsla, &'static str) {
            match kind {
                Some(1) => (t.text, "ab"),               // Text
                Some(2) | Some(3) => (c_function, "fn"), // Method, Function
                Some(4) => (t.text, "c"),                // Constructor
                Some(5) => (c_variable, "f"),            // Field
                Some(6) => (c_variable, "v"),            // Variable
                Some(7) => (c_type, "C"),                // Class
                Some(8) => (c_type, "I"),                // Interface
                Some(9) => (c_namespace, "ns"),          // Module
                Some(10) => (c_variable, "p"),           // Property
                Some(11) => (t.text, "u"),               // Unit
                Some(12) => (c_constant, "="),           // Value
                Some(13) => (c_type, "E"),               // Enum
                Some(14) => (c_keyword, "kw"),           // Keyword
                Some(15) => (t.text, "⋯"),               // Snippet
                Some(20) => (t.text, "e"),               // EnumMember
                Some(21) => (c_constant, "K"),           // Constant
                Some(22) => (c_type, "T"),               // Struct
                Some(23) => (t.text, "!"),               // Event
                Some(24) => (t.text, "op"),              // Operator
                Some(25) => (c_type, "T"),               // TypeParameter
                _ => (t.text, "·"),
            }
        };

        // Extract item display data (label, kind, detail) for row rendering.
        let items: Vec<(String, Option<i32>, Option<String>)> = {
            let m = self.completion.as_ref()?;
            m.filtered
                .iter()
                .map(|&ix| {
                    let item = &m.items[ix];
                    (item.label.clone(), item.kind, item.detail.clone())
                })
                .collect()
        };

        let item_count = items.len();
        let fit_rows = (avail / COMPLETION_ITEM_H).floor() as usize;
        let visible_rows = fit_rows
            .clamp(MIN_VISIBLE, MAX_VISIBLE)
            .min(item_count)
            .max(1);
        let list_h = visible_rows as f32 * COMPLETION_ITEM_H;

        let rows: Vec<AnyElement> = items
            .into_iter()
            .enumerate()
            .map(|(list_ix, (label, kind, detail))| {
                let is_selected = list_ix == selected_ix;
                let (kc, glyph) = kind_info(kind);
                // Colored by kind; unselected rows dimmed via alpha so the
                // selection highlight still reads clearly.
                let label_color = if is_selected {
                    kc
                } else {
                    gpui::Hsla {
                        a: kc.a * 0.7,
                        ..kc
                    }
                };
                let mut row = div()
                    .id(("completion-item", list_ix))
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(item_h_px)
                    .px(px(t.sp2))
                    .gap(px(t.sp2))
                    .cursor_pointer()
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |view, _, window, cx| {
                            if let Some(ref mut m) = view.completion {
                                m.selected_ix = list_ix;
                            }
                            view.accept_completion(window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        // Fixed-size, flex-centered cell so the glyph shares the
                        // row's vertical center with the label (no baseline drift).
                        div()
                            .w(px(18.))
                            .h(item_h_px)
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(caption_sz)
                            .text_color(label_color)
                            .child(glyph.to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .text_size(code_sz)
                            .text_color(label_color)
                            .child(label),
                    );
                if let Some(d) = detail {
                    row = row.child(
                        div()
                            .text_size(caption_sz)
                            .text_color(t.text_subtle)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .max_w(px(120.))
                            .text_ellipsis()
                            .child(d),
                    );
                }
                if is_selected {
                    row = row.bg(selected_bg).rounded(px(t.radius_sm));
                }
                row.into_any()
            })
            .collect();

        // Dropdown list using glass popover surface.
        let dropdown = crate::ui::popover_container("completion-dropdown", t)
            .w(px(DROPDOWN_W))
            .h(px(list_h))
            .on_scroll_wheel(cx.listener(|_, _: &ScrollWheelEvent, _, cx| {
                cx.stop_propagation();
            }))
            .child(
                div()
                    .id("completion-list")
                    .flex_col()
                    .h_full()
                    .overflow_y_scroll()
                    .track_scroll(&scroll)
                    .p(px(t.sp1))
                    .children(rows),
            );

        // Documentation panel — uses persistent markdown segments (rebuilt when doc changes).
        // Responsive: grows to fit content within a width range, capped in height
        // to the available viewport space (scrolls past the cap). The scroll child
        // stays block-level (no flex) so overflow_y_scroll can measure true content
        // height and never squeezes wrapped lines on top of each other.
        let doc_panel: Option<AnyElement> = self.completion.as_ref().and_then(|m| {
            if m.doc_segments.borrow().is_empty() {
                return None;
            }
            let doc_scroll = m.doc_scroll.clone();
            let doc_segments = m.doc_segments.clone();
            let doc_max_h = avail.clamp(160.0, 480.0);
            let mut segs = m.doc_segments.borrow_mut();
            let content = crate::hover_popover::render_doc_content(&mut segs, t);
            drop(segs);
            Some(
                crate::ui::popover_container("completion-doc", t)
                    .ml(px(t.sp2))
                    .min_w(px(DOC_MIN_W))
                    .max_w(px(DOC_MAX_W))
                    .max_h(px(doc_max_h))
                    .on_scroll_wheel(cx.listener(|_, _: &ScrollWheelEvent, _, cx| {
                        cx.stop_propagation();
                    }))
                    // Clickable links; the panel occludes the editor so clicks
                    // never fall through to the buffer behind it.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |_, e: &MouseUpEvent, _, cx| {
                            if let Some((seg_ix, link_ix)) =
                                crate::hover_popover::link_at(&doc_segments, e.position)
                            {
                                let url = doc_segments
                                    .borrow()
                                    .get(seg_ix)
                                    .and_then(|s| s.links.get(link_ix).map(|(_, u)| u.clone()));
                                if let Some(url) = url {
                                    cx.open_url(&url);
                                }
                            }
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        div()
                            .id("completion-doc-scroll")
                            .overflow_y_scroll()
                            .max_h(px(doc_max_h))
                            .track_scroll(&doc_scroll)
                            .p(px(t.sp3))
                            .children(content),
                    )
                    .into_any(),
            )
        });

        // `occlude` + swallowing mouse events over the whole overlay bounds so a
        // click/hover that lands on the overlay never reaches the buffer behind it.
        let container = div()
            .id("completion-overlay")
            .occlude()
            .flex()
            .flex_row()
            .items_start()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_move(|_, _, cx| cx.stop_propagation())
            .child(dropdown)
            .when_some(doc_panel, |el, p| el.child(p));

        // Position at the word's live window position (recomputed each frame above),
        // so the overlay follows the text as it scrolls. Flip above only when cramped
        // below.
        let (pos, corner) = if place_above {
            (
                gpui::point(px(anchor.x), px(anchor.line_top)),
                gpui::Corner::BottomLeft,
            )
        } else {
            (
                gpui::point(px(anchor.x), px(anchor.line_bottom)),
                gpui::Corner::TopLeft,
            )
        };
        Some(
            deferred(
                anchored()
                    .position(pos)
                    .anchor(corner)
                    .snap_to_window_with_margin(px(8.))
                    .child(container),
            )
            .with_priority(10)
            .into_any(),
        )
    }
}
