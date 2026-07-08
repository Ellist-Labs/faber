use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use faber_editor::{ChangeSet, Transaction};
use faber_editor::project_search::{FileSearchResult, ProjectSearchQuery, run};
use ropey::Rope;

use gpui::{
    AnyElement, App, Bounds, Context, Entity, FocusHandle, Focusable, IntoElement,
    KeyDownEvent, ListHorizontalSizingBehavior, MouseButton, MouseMoveEvent,
    Render, SharedString, Task, TextRun, UniformListScrollHandle, WeakEntity, Window,
    canvas, div, fill, font, point, prelude::*, px, size, svg, uniform_list,
};
use rust_i18n::t;

use crate::editor_view::{EditorEvent, EditorView};
use crate::file_icons;
use crate::input_helpers::{
    delete_char_before, delete_char_range, insert_at, split_at_char, word_start_before,
};
use crate::settings_view::SettingsStore;
use crate::theme::RuntimeTheme;
use crate::ui::{IconName, ScrollbarDrag, h_flex, render_scrollbar, v_flex};
use crate::ui::scrollbar::{start_drag, update_drag};
use crate::workspace::Workspace;
use crate::{
    ProjectRoot,
    PsInputBackspace, PsInputMoveLeft, PsInputMoveRight, PsInputMoveStart, PsInputMoveEnd,
};

// ── result row model ──────────────────────────────────────────────────────────

#[derive(Clone)]
enum ResultRow {
    FileHeader { file_idx: usize },
    ExpandAbove { file_idx: usize },
    ContextLine { file_idx: usize, line_number: usize },
    Hit { file_idx: usize, hit_idx: usize },
    ExpandBelow { file_idx: usize },
}

// ── active input ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActiveInput {
    Query,
    Replace,
    Include,
    Exclude,
}

// ── view ──────────────────────────────────────────────────────────────────────

pub struct ProjectSearchView {
    pub focus_handle: FocusHandle,
    pub(crate) workspace: WeakEntity<Workspace>,

    // Hand-rolled inputs
    query: String,
    query_cursor: usize,
    pub(crate) query_handle: FocusHandle,
    replace: String,
    replace_cursor: usize,
    pub(crate) replace_handle: FocusHandle,
    include: String,
    include_cursor: usize,
    include_handle: FocusHandle,
    exclude: String,
    exclude_cursor: usize,
    exclude_handle: FocusHandle,

    // Flags
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
    include_ignored: bool,
    open_files_only: bool,
    filters_open: bool,
    replace_open: bool,

    // Results
    results: Vec<FileSearchResult>,
    collapsed: HashSet<PathBuf>,
    rows: Vec<ResultRow>,
    active_row: Option<usize>,
    total_matches: usize,
    limit_reached: bool,

    // Expand context state
    file_contents: HashMap<PathBuf, Vec<String>>,
    context_above: HashMap<PathBuf, usize>,
    context_below: HashMap<PathBuf, usize>,

    // Cached for relative path display in render (updated on each search)
    root_folder: Option<PathBuf>,

    // Search state
    search_task: Option<Task<()>>,
    search_generation: u64,
    is_searching: bool,
    has_searched: bool,

    // Scroll + scrollbar
    scroll: UniformListScrollHandle,
    scrollbar_drag: Option<ScrollbarDrag>,

    // Cursor blink
    cursor_blink_on: bool,
    cursor_blink_epoch: u64,
}

impl ProjectSearchView {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        prefill: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            focus_handle: cx.focus_handle(),
            workspace,
            query: String::new(),
            query_cursor: 0,
            query_handle: cx.focus_handle(),
            replace: String::new(),
            replace_cursor: 0,
            replace_handle: cx.focus_handle(),
            include: String::new(),
            include_cursor: 0,
            include_handle: cx.focus_handle(),
            exclude: String::new(),
            exclude_cursor: 0,
            exclude_handle: cx.focus_handle(),
            case_sensitive: false,
            whole_word: false,
            regex: false,
            include_ignored: false,
            open_files_only: false,
            filters_open: false,
            replace_open: false,
            results: Vec::new(),
            collapsed: HashSet::new(),
            rows: Vec::new(),
            active_row: None,
            total_matches: 0,
            limit_reached: false,
            file_contents: HashMap::new(),
            context_above: HashMap::new(),
            context_below: HashMap::new(),
            root_folder: cx.global::<ProjectRoot>().0.clone(),
            search_task: None,
            search_generation: 0,
            is_searching: false,
            has_searched: false,
            scroll: UniformListScrollHandle::new(),
            scrollbar_drag: None,
            cursor_blink_on: true,
            cursor_blink_epoch: 0,
        };
        if !prefill.is_empty() {
            view.query_cursor = prefill.chars().count();
            view.query = prefill;
            view.schedule_search(cx);
        }
        view
    }

    /// Replace the query text (called when re-activating an existing tab with a selection).
    pub fn set_query(&mut self, text: String, cx: &mut Context<Self>) {
        self.query_cursor = text.chars().count();
        self.query = text;
        self.search_generation += 1;
        self.schedule_search(cx);
        cx.notify();
    }

    // ── search execution ──────────────────────────────────────────────────────

    fn schedule_search(&mut self, cx: &mut Context<Self>) {
        if self.query.is_empty() {
            self.search_task = None;
            self.results.clear();
            self.rows.clear();
            self.total_matches = 0;
            self.is_searching = false;
            self.has_searched = false;
            cx.notify();
            return;
        }

        self.context_above.clear();
        self.context_below.clear();

        let root = cx.global::<ProjectRoot>().0.clone();
        self.root_folder = root.clone();
        let Some(root) = root else { return; };

        // Collect open-file paths before going async.
        let scope_paths: Option<Vec<PathBuf>> = if self.open_files_only {
            self.workspace.upgrade().map(|ws| {
                let ws = ws.read(cx);
                ws.tabs
                    .iter()
                    .filter_map(|t| {
                        t.editor().and_then(|e| {
                            let p = e.read(cx).doc.path.clone();
                            if p.as_os_str().is_empty() { None } else { Some(p) }
                        })
                    })
                    .collect()
            })
        } else {
            None
        };

        let mut query = ProjectSearchQuery::new(&self.query);
        query.case_sensitive = self.case_sensitive;
        query.whole_word = self.whole_word;
        query.regex = self.regex;
        query.include_ignored = self.include_ignored;
        query.includes = ProjectSearchQuery::parse_globs(&self.include);
        query.excludes = ProjectSearchQuery::parse_globs(&self.exclude);
        query.scope_paths = scope_paths;

        let generation = self.search_generation;
        self.is_searching = true;
        self.has_searched = false;
        self.results.clear();
        self.rows.clear();
        cx.notify();

        self.search_task = Some(cx.spawn(async move |view_entity, cx| {
            // Debounce
            cx.background_executor().timer(Duration::from_millis(150)).await;

            // Check still valid after debounce.
            let still_valid = view_entity
                .update(cx, |v, _| v.search_generation == generation)
                .unwrap_or(false);
            if !still_valid {
                return;
            }

            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut file_results = Vec::new();
                    let limit_reached = run(&query, &root, |r| {
                        file_results.push(r);
                        true
                    });
                    (file_results, limit_reached)
                })
                .await;

            view_entity
                .update(cx, |view, cx| {
                    if view.search_generation != generation {
                        return;
                    }
                    let (file_results, limit_reached) = result;
                    view.total_matches = file_results.iter().map(|r| r.hits.len()).sum();
                    view.results = file_results;
                    view.limit_reached = limit_reached;
                    view.has_searched = true;
                    view.rebuild_rows();
                    view.is_searching = false;
                    cx.notify();
                })
                .ok();
        }));
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        for (file_idx, result) in self.results.iter().enumerate() {
            self.rows.push(ResultRow::FileHeader { file_idx });
            if !self.collapsed.contains(&result.path) {
                let path = &result.path;
                let first_hit_line = result.hits.first().map(|h| h.line).unwrap_or(0);
                let last_hit_line = result.hits.last().map(|h| h.line).unwrap_or(0);
                let lines_above = self.context_above.get(path).copied().unwrap_or(0).min(first_hit_line);
                let file_len = self.file_contents.get(path).map(|v| v.len());
                let lines_below = self.context_below.get(path).copied().unwrap_or(0)
                    .min(file_len.map(|n| n.saturating_sub(last_hit_line + 1)).unwrap_or(0));

                if first_hit_line > lines_above {
                    self.rows.push(ResultRow::ExpandAbove { file_idx });
                }
                let context_start = first_hit_line.saturating_sub(lines_above);
                for ln in context_start..first_hit_line {
                    self.rows.push(ResultRow::ContextLine { file_idx, line_number: ln });
                }
                for hit_idx in 0..result.hits.len() {
                    self.rows.push(ResultRow::Hit { file_idx, hit_idx });
                }
                for ln in (last_hit_line + 1)..(last_hit_line + 1 + lines_below) {
                    self.rows.push(ResultRow::ContextLine { file_idx, line_number: ln });
                }
                let can_expand_below = match file_len {
                    Some(n) => last_hit_line + 1 + lines_below < n,
                    None => true,
                };
                if can_expand_below {
                    self.rows.push(ResultRow::ExpandBelow { file_idx });
                }
            }
        }
    }

    fn load_file_content(&mut self, path: &PathBuf) {
        if self.file_contents.contains_key(path) {
            return;
        }
        if let Ok(content) = std::fs::read_to_string(path) {
            let lines: Vec<String> = content.lines().map(String::from).collect();
            self.file_contents.insert(path.clone(), lines);
        }
    }

    fn expand_context_for_active(&mut self) {
        let file_idx = {
            let row_ref = self.active_row.and_then(|r| self.rows.get(r));
            match row_ref {
                Some(ResultRow::FileHeader { file_idx })
                | Some(ResultRow::ExpandAbove { file_idx })
                | Some(ResultRow::ExpandBelow { file_idx }) => *file_idx,
                Some(ResultRow::ContextLine { file_idx, .. })
                | Some(ResultRow::Hit { file_idx, .. }) => *file_idx,
                None => {
                    if self.results.is_empty() { return; }
                    0
                }
            }
        };
        let Some(path) = self.results.get(file_idx).map(|r| r.path.clone()) else { return; };
        self.load_file_content(&path);
        *self.context_above.entry(path.clone()).or_insert(0) += 3;
        *self.context_below.entry(path.clone()).or_insert(0) += 3;
        self.rebuild_rows();
    }

    fn toggle_file_collapsed(&mut self, file_idx: usize) {
        let path = self.results[file_idx].path.clone();
        if self.collapsed.contains(&path) {
            self.collapsed.remove(&path);
        } else {
            self.collapsed.insert(path);
        }
        self.rebuild_rows();
    }

    fn collapse_all_results(&mut self) {
        for r in &self.results {
            self.collapsed.insert(r.path.clone());
        }
        self.rebuild_rows();
    }

    fn expand_all_results(&mut self) {
        self.collapsed.clear();
        self.rebuild_rows();
    }

    fn all_collapsed(&self) -> bool {
        !self.results.is_empty() && self.results.iter().all(|r| self.collapsed.contains(&r.path))
    }

    // ── cursor blink ──────────────────────────────────────────────────────────

    fn reset_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_blink_on = true;
        self.cursor_blink_epoch += 1;
        let epoch = self.cursor_blink_epoch;
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(530)).await;
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

    // ── replace ───────────────────────────────────────────────────────────────

    fn do_replace_all(&mut self, cx: &mut Context<Self>) {
        if self.results.is_empty() || self.replace.is_empty() {
            return;
        }
        let Some(ws) = self.workspace.upgrade() else { return };
        let replacement = self.replace.clone();

        for file_result in &self.results {
            let path = &file_result.path;

            let editor_entity: Option<Entity<EditorView>> = ws
                .read(cx)
                .tabs
                .iter()
                .filter_map(|t| t.editor())
                .find(|e| e.read(cx).doc.path == *path)
                .cloned();

            if let Some(editor) = editor_entity {
                let rep = replacement.clone();
                let hits = file_result.hits.clone();
                editor.update(cx, |ev, cx| {
                    let len = ev.doc.rope.len_chars();
                    let mut edits: Vec<(usize, usize, String)> = hits
                        .iter()
                        .flat_map(|hit| {
                            hit.ranges.iter().map(|r| {
                                (
                                    hit.line_start_char + r.start,
                                    hit.line_start_char + r.end,
                                    rep.clone(),
                                )
                            })
                        })
                        .collect();
                    edits.sort_by_key(|(s, _, _)| *s);
                    if edits.is_empty() {
                        return;
                    }
                    let cs = ChangeSet::from_changes(len, edits);
                    let tx = Transaction::from_changeset(cs);
                    ev.doc.apply(tx);
                    cx.emit(EditorEvent::Edited);
                    cx.notify();
                });
            } else {
                let rep = replacement.clone();
                let hits = file_result.hits.clone();
                if let Ok(content) = std::fs::read_to_string(path) {
                    let mut rope = Rope::from_str(&content);
                    let len = rope.len_chars();
                    let mut edits: Vec<(usize, usize, String)> = hits
                        .iter()
                        .flat_map(|hit| {
                            hit.ranges.iter().map(|r| {
                                (
                                    hit.line_start_char + r.start,
                                    hit.line_start_char + r.end,
                                    rep.clone(),
                                )
                            })
                        })
                        .collect();
                    edits.sort_by_key(|(s, _, _)| *s);
                    let cs = ChangeSet::from_changes(len, edits);
                    cs.apply(&mut rope);
                    let _ = std::fs::write(path, rope.to_string());
                }
            }
        }

        self.search_generation += 1;
        self.schedule_search(cx);
    }

    fn do_replace_one(&mut self, cx: &mut Context<Self>) {
        let Some(active) = self.active_row else { return };
        let row = self.rows.get(active).cloned();
        let (file_idx, hit_idx) = match row {
            Some(ResultRow::Hit { file_idx, hit_idx }) => (file_idx, hit_idx),
            Some(ResultRow::FileHeader { file_idx }) => (file_idx, 0),
            _ => return,
        };
        let Some(file_result) = self.results.get(file_idx) else { return };
        let Some(hit) = file_result.hits.get(hit_idx) else { return };
        let path = file_result.path.clone();
        let hit = hit.clone();
        let replacement = self.replace.clone();

        let Some(ws) = self.workspace.upgrade() else { return };
        let editor_entity: Option<Entity<EditorView>> = ws
            .read(cx)
            .tabs
            .iter()
            .filter_map(|t| t.editor())
            .find(|e| e.read(cx).doc.path == path)
            .cloned();

        if let Some(editor) = editor_entity {
            let rep = replacement.clone();
            editor.update(cx, |ev, cx| {
                let len = ev.doc.rope.len_chars();
                let edits: Vec<(usize, usize, String)> = hit
                    .ranges
                    .iter()
                    .map(|r| {
                        (
                            hit.line_start_char + r.start,
                            hit.line_start_char + r.end,
                            rep.clone(),
                        )
                    })
                    .collect();
                if edits.is_empty() {
                    return;
                }
                let cs = ChangeSet::from_changes(len, edits);
                let tx = Transaction::from_changeset(cs);
                ev.doc.apply(tx);
                cx.emit(EditorEvent::Edited);
                cx.notify();
            });
        } else {
            let rep = replacement.clone();
            if let Ok(content) = std::fs::read_to_string(&path) {
                let mut rope = Rope::from_str(&content);
                let len = rope.len_chars();
                let edits: Vec<(usize, usize, String)> = hit
                    .ranges
                    .iter()
                    .map(|r| {
                        (
                            hit.line_start_char + r.start,
                            hit.line_start_char + r.end,
                            rep.clone(),
                        )
                    })
                    .collect();
                let cs = ChangeSet::from_changes(len, edits);
                cs.apply(&mut rope);
                let _ = std::fs::write(&path, rope.to_string());
            }
        }

        self.search_generation += 1;
        self.schedule_search(cx);
    }

    // ── key handling ──────────────────────────────────────────────────────────

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;

        // Determine which input (if any) is focused.
        let focused = if self.query_handle.is_focused(window) {
            Some(ActiveInput::Query)
        } else if self.replace_handle.is_focused(window) {
            Some(ActiveInput::Replace)
        } else if self.include_handle.is_focused(window) {
            Some(ActiveInput::Include)
        } else if self.exclude_handle.is_focused(window) {
            Some(ActiveInput::Exclude)
        } else {
            None
        };

        let Some(active) = focused else { return };

        // Handle special keys.
        let modifiers = &ks.modifiers;
        match ks.key.as_str() {
            "escape" => {
                window.focus(&self.focus_handle);
                cx.notify();
                return;
            }
            "enter" => {
                if ks.modifiers.shift {
                    // shift+enter: expand context around the active file's results
                    self.expand_context_for_active();
                    cx.notify();
                } else if active == ActiveInput::Query {
                    // Re-run search immediately (skip debounce).
                    self.search_generation += 1;
                    self.schedule_search(cx);
                }
                return;
            }
            "tab" => {
                // Cycle inputs: query → include → exclude → query.
                match active {
                    ActiveInput::Query if self.filters_open => {
                        window.focus(&self.include_handle);
                    }
                    ActiveInput::Include => {
                        window.focus(&self.exclude_handle);
                    }
                    ActiveInput::Exclude | ActiveInput::Query => {
                        window.focus(&self.query_handle);
                    }
                    ActiveInput::Replace => {
                        window.focus(&self.query_handle);
                    }
                }
                cx.notify();
                return;
            }
            _ => {}
        }

        // cmd+backspace → clear to line start; alt+backspace → delete word before cursor.
        if ks.key.as_str() == "backspace" && (modifiers.platform || modifiers.alt) {
            let (text_ref, cursor_ref) = self.active_input_fields(window);
            let new_pos = if modifiers.platform {
                0
            } else {
                word_start_before(text_ref, *cursor_ref)
            };
            if new_pos < *cursor_ref {
                *text_ref = delete_char_range(text_ref, new_pos, *cursor_ref);
                *cursor_ref = new_pos;
                self.search_generation += 1;
                self.schedule_search(cx);
                self.reset_blink(cx);
                cx.notify();
            }
            return;
        }

        // Cursor / backspace actions are handled via key bindings (PsInput*).
        // Only handle character insertion here.
        if modifiers.platform || modifiers.control {
            return;
        }

        let Some(ref raw_text) = ks.key_char else { return };
        if raw_text.chars().any(|c| c.is_control()) {
            return;
        }
        let text_buf;
        let text: &str = if window.capslock().on {
            text_buf = raw_text
                .chars()
                .map(|c| if c.is_ascii_alphabetic() {
                    if c.is_ascii_uppercase() { c.to_ascii_lowercase() } else { c.to_ascii_uppercase() }
                } else {
                    c
                })
                .collect::<String>();
            &text_buf
        } else {
            raw_text.as_str()
        };

        let (text_ref, cursor_ref) = match active {
            ActiveInput::Query => (&mut self.query, &mut self.query_cursor),
            ActiveInput::Replace => (&mut self.replace, &mut self.replace_cursor),
            ActiveInput::Include => (&mut self.include, &mut self.include_cursor),
            ActiveInput::Exclude => (&mut self.exclude, &mut self.exclude_cursor),
        };

        *text_ref = insert_at(text_ref, *cursor_ref, text);
        *cursor_ref += text.chars().count();

        // Trigger search on query / filter changes.
        if matches!(active, ActiveInput::Query | ActiveInput::Include | ActiveInput::Exclude) {
            self.search_generation += 1;
            self.schedule_search(cx);
        } else {
            cx.notify();
        }
        self.reset_blink(cx);
    }

    // ── action handlers (bound in render via on_action) ───────────────────────

    fn on_ps_backspace(
        &mut self,
        _: &PsInputBackspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (text_ref, cursor_ref) = self.active_input_fields(window);
        if *cursor_ref > 0 {
            *text_ref = delete_char_before(text_ref, *cursor_ref);
            *cursor_ref -= 1;
            self.search_generation += 1;
            self.schedule_search(cx);
            self.reset_blink(cx);
            cx.notify();
        }
    }

    fn on_ps_move_left(
        &mut self,
        _: &PsInputMoveLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (_, cursor_ref) = self.active_input_fields(window);
        *cursor_ref = cursor_ref.saturating_sub(1);
        self.reset_blink(cx);
        cx.notify();
    }

    fn on_ps_move_right(
        &mut self,
        _: &PsInputMoveRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (text_ref, cursor_ref) = self.active_input_fields(window);
        let max = text_ref.chars().count();
        *cursor_ref = (*cursor_ref + 1).min(max);
        self.reset_blink(cx);
        cx.notify();
    }

    fn on_ps_move_start(
        &mut self,
        _: &PsInputMoveStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (_, cursor_ref) = self.active_input_fields(window);
        *cursor_ref = 0;
        self.reset_blink(cx);
        cx.notify();
    }

    fn on_ps_move_end(
        &mut self,
        _: &PsInputMoveEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (text_ref, cursor_ref) = self.active_input_fields(window);
        *cursor_ref = text_ref.chars().count();
        self.reset_blink(cx);
        cx.notify();
    }

    /// Returns mutable refs to the focused input's text and cursor.
    /// Defaults to query if no input is focused.
    fn active_input_fields<'a>(
        &'a mut self,
        window: &Window,
    ) -> (&'a mut String, &'a mut usize) {
        if self.replace_handle.is_focused(window) {
            (&mut self.replace, &mut self.replace_cursor)
        } else if self.include_handle.is_focused(window) {
            (&mut self.include, &mut self.include_cursor)
        } else if self.exclude_handle.is_focused(window) {
            (&mut self.exclude, &mut self.exclude_cursor)
        } else {
            (&mut self.query, &mut self.query_cursor)
        }
    }
}

impl Focusable for ProjectSearchView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

// ── render ────────────────────────────────────────────────────────────────────

impl Render for ProjectSearchView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<RuntimeTheme>().clone();

        let settings = &cx.global::<SettingsStore>().0;
        let show_scrollbar = settings.show_scrollbar;

        let is_dragging = self.scrollbar_drag.is_some();
        let base_handle = self.scroll.0.borrow().base_handle.clone();

        // Build the root element with action/event listeners first (before any sub-renders
        // that mutably borrow cx, to avoid borrow-checker conflicts with let bindings).
        let replace_open = self.replace_open;
        let filters_open = self.filters_open;
        let rows_empty = self.rows.is_empty();

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.bg)
            .key_context("ProjectSearch")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(Self::on_ps_backspace))
            .on_action(cx.listener(Self::on_ps_move_left))
            .on_action(cx.listener(Self::on_ps_move_right))
            .on_action(cx.listener(Self::on_ps_move_start))
            .on_action(cx.listener(Self::on_ps_move_end))
            .when(is_dragging, |el| {
                el.on_mouse_move(cx.listener(|ps, ev: &MouseMoveEvent, _, cx| {
                    if let Some(ref drag) = ps.scrollbar_drag {
                        let handle = ps.scroll.0.borrow().base_handle.clone();
                        update_drag(drag, ev, &handle);
                        cx.notify();
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|ps, _, _, cx| {
                        ps.scrollbar_drag = None;
                        cx.notify();
                    }),
                )
            })
            // Sub-renders called inline so cx borrows are temporary and don't collide.
            .child(self.render_toolbar(window, &t, cx))
            .when(replace_open, |el| el.child(self.render_replace_row(window, &t, cx)))
            .when(filters_open, |el| el.child(self.render_filter_row(window, &t, cx)))
            .child({
                let scrollbar = render_scrollbar(
                    "ps-scrollbar",
                    "ps-scrollbar-thumb",
                    &base_handle,
                    show_scrollbar,
                    is_dragging,
                    cx.listener(|ps, ev, _, cx| {
                        let handle = ps.scroll.0.borrow().base_handle.clone();
                        ps.scrollbar_drag = Some(start_drag(ev, &handle));
                        cx.notify();
                    }),
                    &t,
                    None,
                );
                let body: AnyElement = if rows_empty {
                    self.render_empty_state(&t, cx)
                } else {
                    self.render_results(&t, cx)
                };
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_row()
                    .child(div().flex().flex_col().flex_1().min_w(px(0.)).min_h(px(0.)).child(body))
                    .child(scrollbar)
            })
    }
}

// ── render helpers ────────────────────────────────────────────────────────────

impl ProjectSearchView {
    fn render_toolbar(
        &self,
        window: &Window,
        t: &RuntimeTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hover_bg = t.line_highlight;
        let radius = t.radius_sm;
        let sep_color = t.separator;

        let icon_btn = move |id: &'static str| {
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

        let vsep = move || div().w(px(1.)).h(px(14.)).bg(sep_color).flex_shrink_0();

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

        // Query input
        let query_focused = self.query_handle.is_focused(window);
        let caret_visible = self.cursor_blink_on;
        let (q_before, q_after) = split_at_char(&self.query, self.query_cursor);
        let cursor_h = t.font_size_code + 2.;

        let query_input = h_flex()
            .flex_1()
            .min_w(px(0.))
            .px_2()
            .py(px(t.sp2))
            .rounded(px(t.radius_sm))
            .bg(t.bg_sunken)
            .border_1()
            .border_color(if query_focused { t.border_focus } else { t.border })
            .cursor_text()
            .key_context("ProjectSearch")
            .track_focus(&self.query_handle)
            .on_mouse_down(MouseButton::Left, cx.listener(|ps, _, window, cx| {
                window.focus(&ps.query_handle);
                cx.notify();
            }))
            .font_family(t.ui_family.clone())
            .text_size(px(t.font_size_caption))
            .child(if self.query.is_empty() && !query_focused {
                div().text_color(t.text_subtle).child(t!("project_search.placeholder").to_string()).into_any_element()
            } else {
                {
                    let cur_on = query_focused && caret_visible;
                    let full_text = format!("{}{}", q_before, q_after);
                    let caret_byte = q_before.len();
                    let font_sz = px(t.font_size_caption);
                    let line_h = px(cursor_h);
                    let ui_family = t.ui_family.clone();
                    let text_col = t.text;
                    let cursor_color = if cur_on { t.cursor } else { gpui::hsla(0., 0., 0., 0.) };
                    canvas(
                        move |_bounds, window, _cx| {
                            let runs = if full_text.is_empty() { vec![] } else {
                                vec![TextRun { len: full_text.len(), font: font(ui_family.clone()), color: text_col, background_color: None, underline: None, strikethrough: None }]
                            };
                            window.text_system().shape_line(SharedString::from(full_text), font_sz, &runs, None)
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
                    .into_any_element()
                }
            });

        // Match count
        let match_info = if self.is_searching {
            t!("project_search.searching").to_string()
        } else if self.has_searched && !self.query.is_empty() {
            if self.total_matches == 0 {
                t!("project_search.no_results").to_string()
            } else if self.limit_reached {
                t!(
                    "project_search.limit_reached",
                    matches = format!("{}", self.total_matches)
                ).to_string()
            } else {
                format!("{}/{}", self.total_matches, self.results.len())
            }
        } else {
            String::new()
        };

        // Expand/collapse all toggle
        let all_collapsed = self.all_collapsed();
        let toggle_icon = if all_collapsed { IconName::UnfoldMore } else { IconName::UnfoldLess };
        let collapse_toggle = icon_btn("ps-collapse-toggle")
            .when(!self.results.is_empty(), |el| {
                el.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |ps, _, _, cx| {
                        if all_collapsed { ps.expand_all_results(); } else { ps.collapse_all_results(); }
                        cx.notify();
                    }),
                )
            })
            .child(svg().path(toggle_icon.path()).size(px(16.)).text_color(t.text_subtle).flex_shrink_0());

        // Filter toggle
        let filter_active = self.filters_open;
        let filter_btn = icon_btn("ps-filter-toggle")
            .when(filter_active, |el| el.bg(t.line_highlight))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|ps, _, _, cx| {
                    ps.filters_open = !ps.filters_open;
                    cx.notify();
                }),
            )
            .child(svg().path(IconName::Filter.path()).size(px(14.)).text_color(
                if filter_active { t.text } else { t.text_subtle },
            ));

        // Replace toggle
        let replace_active = self.replace_open;
        let replace_toggle_icon = if replace_active { IconName::Remove } else { IconName::Add };
        let replace_toggle_btn = icon_btn("ps-replace-toggle")
            .when(replace_active, |el| el.bg(t.line_highlight))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|ps, _, window, cx| {
                    ps.replace_open = !ps.replace_open;
                    if ps.replace_open {
                        window.focus(&ps.replace_handle);
                    }
                    cx.notify();
                }),
            )
            .child(svg().path(replace_toggle_icon.path()).size(px(14.)).text_color(
                if replace_active { t.accent } else { t.text_subtle },
            ));

        // Option chips
        let case_chip = chip("ps-case", "Aa", self.case_sensitive, t).on_mouse_down(
            MouseButton::Left,
            cx.listener(|ps, _, _, cx| {
                ps.case_sensitive = !ps.case_sensitive;
                ps.search_generation += 1;
                ps.schedule_search(cx);
            }),
        );
        let word_chip = chip("ps-word", "W", self.whole_word, t).on_mouse_down(
            MouseButton::Left,
            cx.listener(|ps, _, _, cx| {
                ps.whole_word = !ps.whole_word;
                ps.search_generation += 1;
                ps.schedule_search(cx);
            }),
        );
        let regex_chip = chip("ps-regex", ".*", self.regex, t).on_mouse_down(
            MouseButton::Left,
            cx.listener(|ps, _, _, cx| {
                ps.regex = !ps.regex;
                ps.search_generation += 1;
                ps.schedule_search(cx);
            }),
        );

        h_flex()
            .h(px(38.))
            .px(px(t.sp4))
            .gap_2()
            .flex_shrink_0()
            .items_center()
            .border_b_1()
            .border_color(t.separator)
            .bg(t.bg_elevated)
            .child(collapse_toggle)
            .child(vsep())
            .child(query_input)
            .child(
                h_flex()
                    .items_center()
                    .gap_1()
                    .flex_shrink_0()
                    .w(px(300.))
                    .child(case_chip)
                    .child(word_chip)
                    .child(regex_chip)
                    .child(vsep())
                    .child(filter_btn)
                    .child(replace_toggle_btn)
                    .child(vsep())
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .text_size(px(t.font_size_caption))
                            .text_color(t.text_subtle)
                            .font_family(t.ui_family.clone())
                            .child(match_info),
                    ),
            )
    }

    fn render_replace_row(
        &self,
        window: &Window,
        t: &RuntimeTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let replace_focused = self.replace_handle.is_focused(window);
        let caret_visible = self.cursor_blink_on;
        let (r_before, r_after) = split_at_char(&self.replace, self.replace_cursor);
        let cursor_h = t.font_size_code + 2.;

        let replace_input = h_flex()
            .flex_1()
            .min_w(px(0.))
            .px_2()
            .py(px(t.sp2))
            .rounded(px(t.radius_sm))
            .bg(t.bg_sunken)
            .border_1()
            .border_color(if replace_focused { t.border_focus } else { t.border })
            .cursor_text()
            .key_context("ProjectSearch")
            .track_focus(&self.replace_handle)
            .on_mouse_down(MouseButton::Left, cx.listener(|ps, _, window, cx| {
                window.focus(&ps.replace_handle);
                cx.notify();
            }))
            .font_family(t.ui_family.clone())
            .text_size(px(t.font_size_caption))
            .child(if self.replace.is_empty() && !replace_focused {
                div()
                    .text_color(t.text_subtle)
                    .child(t!("project_search.replace_placeholder").to_string())
                    .into_any_element()
            } else {
                {
                    let cur_on = replace_focused && caret_visible;
                    let full_text = format!("{}{}", r_before, r_after);
                    let caret_byte = r_before.len();
                    let font_sz = px(t.font_size_caption);
                    let line_h = px(cursor_h);
                    let ui_family = t.ui_family.clone();
                    let text_col = t.text;
                    let cursor_color = if cur_on { t.cursor } else { gpui::hsla(0., 0., 0., 0.) };
                    canvas(
                        move |_bounds, window, _cx| {
                            let runs = if full_text.is_empty() { vec![] } else {
                                vec![TextRun { len: full_text.len(), font: font(ui_family.clone()), color: text_col, background_color: None, underline: None, strikethrough: None }]
                            };
                            window.text_system().shape_line(SharedString::from(full_text), font_sz, &runs, None)
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
                    .into_any_element()
                }
            });

        let has_results = !self.results.is_empty();
        let replace_btn = div()
            .id("ps-replace-one")
            .px(px(t.sp4))
            .py(px(t.sp2))
            .rounded(px(t.radius_sm))
            .text_size(px(t.font_size_caption))
            .font_family(t.ui_family.clone())
            .text_color(if has_results { t.text } else { t.text_disabled })
            .when(has_results, |el| {
                el.cursor_pointer().hover(|s| s.bg(t.line_highlight)).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|ps, _, _, cx| ps.do_replace_one(cx)),
                )
            })
            .child(t!("project_search.replace").to_string());

        let replace_all_btn = div()
            .id("ps-replace-all")
            .px(px(t.sp4))
            .py(px(t.sp2))
            .rounded(px(t.radius_sm))
            .text_size(px(t.font_size_caption))
            .font_family(t.ui_family.clone())
            .text_color(if has_results { t.text } else { t.text_disabled })
            .when(has_results, |el| {
                el.cursor_pointer().hover(|s| s.bg(t.line_highlight)).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|ps, _, _, cx| ps.do_replace_all(cx)),
                )
            })
            .child(t!("project_search.replace_all").to_string());

        h_flex()
            .h(px(38.))
            .px(px(t.sp4))
            .gap_2()
            .flex_shrink_0()
            .items_center()
            .border_b_1()
            .border_color(t.separator)
            .bg(t.bg_elevated)
            .child(div().w(px(24.)).flex_shrink_0())
            .child(div().w(px(1.)).h(px(14.)).bg(t.separator).flex_shrink_0())
            .child(replace_input)
            .child(
                h_flex()
                    .items_center()
                    .gap_1()
                    .flex_shrink_0()
                    .w(px(300.))
                    .child(replace_btn)
                    .child(replace_all_btn),
            )
    }

    fn render_filter_row(
        &self,
        window: &Window,
        t: &RuntimeTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hover_bg = t.line_highlight;
        let radius = t.radius_sm;

        let mk_input = |id: &'static str, text: &str, cursor: usize, placeholder: &str,
                        focused: bool, caret: bool, t: &RuntimeTheme| {
            let cursor_h = t.font_size_code + 2.;
            let (before, after) = split_at_char(text, cursor);
            let placeholder = placeholder.to_string();
            div()
                .flex_1()
                .min_w(px(0.))
                .px_2()
                .py(px(t.sp2))
                .rounded(px(radius))
                .bg(t.bg_sunken)
                .border_1()
                .border_color(if focused { t.border_focus } else { t.border })
                .cursor_text()
                .font_family(t.ui_family.clone())
                .text_size(px(t.font_size_caption))
                .id(id)
                .child(if text.is_empty() && !focused {
                    div().text_color(t.text_subtle).child(placeholder).into_any_element()
                } else {
                    let cur_on = focused && caret;
                    let full_text = format!("{}{}", before, after);
                    let caret_byte_pos = before.len();
                    let font_sz = px(t.font_size_caption);
                    let line_h = px(cursor_h);
                    let ui_family = t.ui_family.clone();
                    let text_col = t.text;
                    let cursor_color = if cur_on { t.cursor } else { gpui::hsla(0., 0., 0., 0.) };
                    canvas(
                        move |_bounds, window, _cx| {
                            let runs = if full_text.is_empty() { vec![] } else {
                                vec![TextRun { len: full_text.len(), font: font(ui_family.clone()), color: text_col, background_color: None, underline: None, strikethrough: None }]
                            };
                            window.text_system().shape_line(SharedString::from(full_text), font_sz, &runs, None)
                        },
                        move |bounds, shaped, window, cx| {
                            let origin = bounds.origin;
                            let _ = shaped.paint(origin, line_h, window, cx);
                            let cx_x = origin.x + shaped.x_for_index(caret_byte_pos);
                            window.paint_quad(fill(
                                Bounds::new(point(cx_x, origin.y), size(px(2.0), line_h)),
                                cursor_color,
                            ));
                        },
                    )
                    .flex_1()
                    .h(line_h)
                    .into_any_element()
                })
        };

        let inc_focused = self.include_handle.is_focused(window);
        let exc_focused = self.exclude_handle.is_focused(window);
        let caret = self.cursor_blink_on;

        let inc_input = mk_input(
            "ps-include",
            &self.include,
            self.include_cursor,
            &t!("project_search.include_placeholder"),
            inc_focused,
            caret,
            t,
        )
        .key_context("ProjectSearch")
        .track_focus(&self.include_handle)
        .on_mouse_down(MouseButton::Left, cx.listener(|ps, _, window, cx| {
            window.focus(&ps.include_handle);
            cx.notify();
        }));

        let exc_input = mk_input(
            "ps-exclude",
            &self.exclude,
            self.exclude_cursor,
            &t!("project_search.exclude_placeholder"),
            exc_focused,
            caret,
            t,
        )
        .key_context("ProjectSearch")
        .track_focus(&self.exclude_handle)
        .on_mouse_down(MouseButton::Left, cx.listener(|ps, _, window, cx| {
            window.focus(&ps.exclude_handle);
            cx.notify();
        }));

        let toggle_chip = |id: &'static str, label: String, active: bool, t: &RuntimeTheme| {
            div()
                .id(id)
                .px_2()
                .py(px(t.sp1))
                .rounded(px(radius))
                .cursor_pointer()
                .flex_shrink_0()
                .text_size(px(t.font_size_caption))
                .font_family(t.ui_family.clone())
                .text_color(if active { t.text_on_accent } else { t.text_subtle })
                .when(active, move |el| el.bg(t.accent))
                .when(!active, move |el| el.hover(move |s| s.bg(hover_bg)))
                .child(label)
        };

        let open_files_chip = toggle_chip(
            "ps-open-files",
            t!("project_search.open_files_only").to_string(),
            self.open_files_only,
            t,
        )
        .on_mouse_down(MouseButton::Left, cx.listener(|ps, _, _, cx| {
            ps.open_files_only = !ps.open_files_only;
            ps.search_generation += 1;
            ps.schedule_search(cx);
        }));

        let ignored_chip = toggle_chip(
            "ps-include-ignored",
            t!("project_search.include_ignored").to_string(),
            self.include_ignored,
            t,
        )
        .on_mouse_down(MouseButton::Left, cx.listener(|ps, _, _, cx| {
            ps.include_ignored = !ps.include_ignored;
            ps.search_generation += 1;
            ps.schedule_search(cx);
        }));

        h_flex()
            .h(px(38.))
            .px(px(t.sp4))
            .gap_2()
            .flex_shrink_0()
            .items_center()
            .border_b_1()
            .border_color(t.separator)
            .bg(t.bg_elevated)
            .child(div().w(px(24.)).flex_shrink_0())
            .child(div().w(px(1.)).h(px(14.)).bg(t.separator).flex_shrink_0())
            .child(inc_input)
            .child(exc_input)
            .child(
                h_flex()
                    .items_center()
                    .gap_1()
                    .flex_shrink_0()
                    .w(px(300.))
                    .child(open_files_chip)
                    .child(ignored_chip),
            )
    }

    fn render_empty_state(&self, t: &RuntimeTheme, cx: &mut Context<Self>) -> AnyElement {
        if self.is_searching {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .font_family(t.ui_family.clone())
                .child(
                    div()
                        .text_color(t.text_subtle)
                        .text_size(px(t.font_size_caption))
                        .child(t!("project_search.searching").to_string()),
                )
                .into_any_element();
        }

        if self.has_searched {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(t.sp2))
                .font_family(t.ui_family.clone())
                .child(svg().path(IconName::Search.path()).size(px(28.)).text_color(t.text_disabled))
                .child(
                    div()
                        .text_color(t.text)
                        .text_size(px(t.font_size_body))
                        .child(t!("project_search.no_results_title").to_string()),
                )
                .child(
                    div()
                        .text_color(t.text_subtle)
                        .text_size(px(t.font_size_caption))
                        .child(t!("project_search.no_results_hint").to_string()),
                )
                .into_any_element();
        }

        // Pre-search empty state: interactive tips
        let hover_bg = t.line_highlight;
        let radius = t.radius_sm;
        let case_active = self.case_sensitive;
        let word_active = self.whole_word;
        let regex_active = self.regex;
        let filter_active = self.filters_open;
        let replace_active = self.replace_open;

        let mk_chip = |label: &'static str, active: bool, t: &RuntimeTheme| {
            div()
                .w(px(34.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(radius))
                .bg(if active { t.accent } else { t.bg_sunken })
                .text_color(if active { t.text_on_accent } else { t.text_subtle })
                .font_family(t.mono_family.clone())
                .text_size(px(t.font_size_code - 1.))
                .child(label)
        };
        let mk_icon = |icon: IconName, active: bool, t: &RuntimeTheme| {
            div()
                .w(px(34.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(radius))
                .bg(if active { t.accent } else { t.bg_sunken })
                .child(
                    svg()
                        .path(icon.path())
                        .size(px(12.))
                        .text_color(if active { t.text_on_accent } else { t.text_subtle }),
                )
        };
        let desc = |text: String, t: &RuntimeTheme| {
            div()
                .text_color(t.text_subtle)
                .text_size(px(t.font_size_caption))
                .font_family(t.ui_family.clone())
                .child(text)
        };

        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap(px(t.sp4))
            .child(
                v_flex()
                    .items_center()
                    .gap(px(t.sp1))
                    .child(
                        div()
                            .text_color(t.text)
                            .text_size(px(t.font_size_body))
                            .font_family(t.ui_family.clone())
                            .child(t!("project_search.empty_title").to_string()),
                    )
                    .child(
                        div()
                            .text_color(t.text_subtle)
                            .text_size(px(t.font_size_caption))
                            .font_family(t.ui_family.clone())
                            .child(t!("project_search.empty_hint").to_string()),
                    ),
            )
            .child(div().w(px(240.)).h(px(1.)).bg(t.separator))
            .child(
                v_flex()
                    .gap(px(t.sp1))
                    .child(
                        h_flex()
                            .id("ps-tip-case")
                            .gap_2()
                            .px_2()
                            .py(px(t.sp1))
                            .rounded(px(radius))
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover_bg))
                            .on_mouse_down(MouseButton::Left, cx.listener(|ps, _, _, cx| {
                                ps.case_sensitive = !ps.case_sensitive;
                                if !ps.query.is_empty() {
                                    ps.search_generation += 1;
                                    ps.schedule_search(cx);
                                }
                                cx.notify();
                            }))
                            .child(mk_chip("Aa", case_active, t))
                            .child(desc(t!("project_search.empty_tip_case").to_string(), t)),
                    )
                    .child(
                        h_flex()
                            .id("ps-tip-word")
                            .gap_2()
                            .px_2()
                            .py(px(t.sp1))
                            .rounded(px(radius))
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover_bg))
                            .on_mouse_down(MouseButton::Left, cx.listener(|ps, _, _, cx| {
                                ps.whole_word = !ps.whole_word;
                                if !ps.query.is_empty() {
                                    ps.search_generation += 1;
                                    ps.schedule_search(cx);
                                }
                                cx.notify();
                            }))
                            .child(mk_chip("W", word_active, t))
                            .child(desc(t!("project_search.empty_tip_word").to_string(), t)),
                    )
                    .child(
                        h_flex()
                            .id("ps-tip-regex")
                            .gap_2()
                            .px_2()
                            .py(px(t.sp1))
                            .rounded(px(radius))
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover_bg))
                            .on_mouse_down(MouseButton::Left, cx.listener(|ps, _, _, cx| {
                                ps.regex = !ps.regex;
                                if !ps.query.is_empty() {
                                    ps.search_generation += 1;
                                    ps.schedule_search(cx);
                                }
                                cx.notify();
                            }))
                            .child(mk_chip(".*", regex_active, t))
                            .child(desc(t!("project_search.empty_tip_regex").to_string(), t)),
                    )
                    .child(
                        h_flex()
                            .id("ps-tip-filter")
                            .gap_2()
                            .px_2()
                            .py(px(t.sp1))
                            .rounded(px(radius))
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover_bg))
                            .on_mouse_down(MouseButton::Left, cx.listener(|ps, _, _, cx| {
                                ps.filters_open = !ps.filters_open;
                                cx.notify();
                            }))
                            .child(mk_icon(IconName::Filter, filter_active, t))
                            .child(desc(t!("project_search.empty_tip_filter").to_string(), t)),
                    )
                    .child(
                        h_flex()
                            .id("ps-tip-replace")
                            .gap_2()
                            .px_2()
                            .py(px(t.sp1))
                            .rounded(px(radius))
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover_bg))
                            .on_mouse_down(MouseButton::Left, cx.listener(|ps, _, window, cx| {
                                ps.replace_open = !ps.replace_open;
                                if ps.replace_open {
                                    window.focus(&ps.replace_handle);
                                }
                                cx.notify();
                            }))
                            .child(mk_icon(
                                if replace_active { IconName::Remove } else { IconName::Add },
                                replace_active,
                                t,
                            ))
                            .child(desc(t!("project_search.empty_tip_replace").to_string(), t)),
                    ),
            )
            .into_any_element()
    }

    fn render_results(
        &self,
        t: &RuntimeTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity();
        let t2 = t.clone();
        let row_count = self.rows.len();

        uniform_list(
            "ps-results",
            row_count,
            move |range, _window, cx| {
                let ps = entity.read(cx);
                range
                    .map(|ix| ps.render_result_row(ix, &entity, &t2).into_any_element())
                    .collect::<Vec<AnyElement>>()
            },
        )
        .flex_1()
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .track_scroll(self.scroll.clone())
        .into_any_element()
    }

    fn render_result_row(
        &self,
        ix: usize,
        entity: &Entity<ProjectSearchView>,
        t: &RuntimeTheme,
    ) -> impl IntoElement {
        let row = self.rows[ix].clone();
        let is_active = self.active_row == Some(ix);
        let row_h = 24.0_f32;

        match row {
            ResultRow::FileHeader { file_idx } => {
                let file_result = &self.results[file_idx];
                let path = file_result.path.clone();
                let is_collapsed = self.collapsed.contains(&path);
                let hit_count = file_result.hits.len();
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let rel_path = self
                    .root_folder
                    .as_ref()
                    .and_then(|root| path.strip_prefix(root).ok())
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string_lossy().to_string());
                let icon = file_icons::icon_for_file(&name);
                let entity = entity.clone();
                let entity2 = entity.clone();
                let open_path = path.clone();
                let first_hit = file_result.hits.first().map(|h| (h.line, h.col)).unwrap_or((0, 0));
                let open_hl_color = t.line_highlight;
                let open_radius = t.radius_sm;

                h_flex()
                    .id(ix)
                    .h(px(28.))
                    .w_full()
                    .pl(px(t.sp4))
                    .pr_2()
                    .gap_1()
                    .items_center()
                    .font_family(t.ui_family.clone())
                    .text_size(px(t.font_size_caption))
                    .text_color(t.text)
                    .cursor_pointer()
                    .bg(t.bg_elevated)
                    .border_t_1()
                    .border_color(t.separator)
                    .hover(|s| s.bg(t.line_highlight))
                    .when(is_active, |el| el.bg(t.selection))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        entity.update(cx, |ps, _| {
                            ps.toggle_file_collapsed(file_idx);
                        });
                    })
                    .child(
                        svg()
                            .path(if is_collapsed {
                                IconName::ChevronRight.path()
                            } else {
                                IconName::ExpandMore.path()
                            })
                            .size(px(12.0))
                            .text_color(t.text_subtle),
                    )
                    .child(gpui::img(icon).size(px(14.0)).flex_shrink_0())
                    .child(div().text_color(t.text).child(name))
                    .child(div().text_color(t.text_subtle).flex_1().child(format!(" — {}", rel_path)))
                    .child(
                        div()
                            .px_2()
                            .rounded_full()
                            .bg(t.accent_muted)
                            .text_color(t.text_on_accent)
                            .text_size(px(t.font_size_caption - 1.))
                            .child(format!("{}", hit_count)),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("ps-open-file-{}", ix)))
                            .px_2()
                            .py(px(t.sp1))
                            .rounded(px(open_radius))
                            .flex_shrink_0()
                            .cursor_pointer()
                            .text_size(px(t.font_size_caption - 1.))
                            .text_color(t.text_subtle)
                            .hover(move |s| s.bg(open_hl_color))
                            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                                cx.stop_propagation();
                                entity2.update(cx, |ps, cx| {
                                    if let Some(ws) = ps.workspace.upgrade() {
                                        ws.update(cx, |ws, cx| {
                                            ws.navigate_to(&open_path, first_hit.0, first_hit.1, window, cx);
                                        });
                                    }
                                });
                            })
                            .child("Open File"),
                    )
            }
            ResultRow::ExpandAbove { file_idx } => {
                let file_result = &self.results[file_idx];
                let path = file_result.path.clone();
                let entity = entity.clone();
                h_flex()
                    .id(ix)
                    .h(px(row_h))
                    .w_full()
                    .pl(px(t.sp4 + 8.))
                    .pr_2()
                    .items_center()
                    .gap_1()
                    .cursor_pointer()
                    .hover(|s| s.bg(t.line_highlight))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        entity.update(cx, |ps, _| {
                            ps.load_file_content(&path);
                            let count = ps.context_above.entry(path.clone()).or_insert(0);
                            *count += 3;
                            ps.rebuild_rows();
                        });
                    })
                    .child(svg().path(IconName::UnfoldMore.path()).size(px(12.)).text_color(t.text_subtle).flex_shrink_0())
                    .child(div().text_size(px(t.font_size_caption - 1.)).text_color(t.text_subtle).font_family(t.ui_family.clone()).child("more above"))
            }
            ResultRow::ContextLine { file_idx, line_number } => {
                let file_result = &self.results[file_idx];
                let path = file_result.path.clone();
                let line_text = self.file_contents
                    .get(&file_result.path)
                    .and_then(|lines| lines.get(line_number))
                    .cloned()
                    .unwrap_or_default();
                let entity = entity.clone();
                h_flex()
                    .id(ix)
                    .h(px(row_h))
                    .w_full()
                    .pl(px(t.sp4 + 28.))
                    .pr_2()
                    .gap_2()
                    .items_center()
                    .font_family(t.mono_family.clone())
                    .text_size(px(t.font_size_caption))
                    .cursor_pointer()
                    .hover(|s| s.bg(t.line_highlight))
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        entity.update(cx, |ps, cx| {
                            if let Some(ws) = ps.workspace.upgrade() {
                                ws.update(cx, |ws, cx| {
                                    ws.navigate_to(&path, line_number, 0, window, cx);
                                });
                            }
                        });
                    })
                    .child(div().min_w(px(32.)).flex_shrink_0().text_color(t.text_subtle).child(format!("{}", line_number + 1)))
                    .child(div().flex_1().min_w(px(0.)).overflow_hidden().text_color(t.text_subtle).child(SharedString::from(line_text)))
            }
            ResultRow::ExpandBelow { file_idx } => {
                let file_result = &self.results[file_idx];
                let path = file_result.path.clone();
                let entity = entity.clone();
                h_flex()
                    .id(ix)
                    .h(px(row_h))
                    .w_full()
                    .pl(px(t.sp4 + 8.))
                    .pr_2()
                    .items_center()
                    .gap_1()
                    .cursor_pointer()
                    .hover(|s| s.bg(t.line_highlight))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        entity.update(cx, |ps, _| {
                            ps.load_file_content(&path);
                            let count = ps.context_below.entry(path.clone()).or_insert(0);
                            *count += 3;
                            ps.rebuild_rows();
                        });
                    })
                    .child(svg().path(IconName::UnfoldLess.path()).size(px(12.)).text_color(t.text_subtle).flex_shrink_0())
                    .child(div().text_size(px(t.font_size_caption - 1.)).text_color(t.text_subtle).font_family(t.ui_family.clone()).child("more below"))
            }
            ResultRow::Hit { file_idx, hit_idx } => {
                let file_result = &self.results[file_idx];
                let hit = &file_result.hits[hit_idx];
                let path = file_result.path.clone();
                let line = hit.line;
                let col = hit.col;
                let preview = hit.preview.clone();
                let ranges = hit.ranges.clone();
                let entity = entity.clone();

                // Build highlighted preview spans: before/match/after alternating.
                let preview_el = build_preview_spans(&preview, &ranges, t);

                h_flex()
                    .id(ix)
                    .h(px(row_h))
                    .w_full()
                    .pl(px(t.sp4 + 28.0)) // indent under file header
                    .pr_2()
                    .gap_2()
                    .items_center()
                    .font_family(t.mono_family.clone())
                    .text_size(px(t.font_size_caption))
                    .cursor_pointer()
                    .hover(|s| s.bg(t.line_highlight))
                    .when(is_active, |el| el.bg(t.selection))
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        entity.update(cx, |ps, cx| {
                            if let Some(ws) = ps.workspace.upgrade() {
                                ws.update(cx, |ws, cx| {
                                    ws.navigate_to(&path, line, col, window, cx);
                                });
                            }
                        });
                    })
                    // Line number
                    .child(
                        div()
                            .min_w(px(32.))
                            .flex_shrink_0()
                            .text_color(t.text_subtle)
                            .child(format!("{}", line + 1)),
                    )
                    .child(preview_el)
            }
        }
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Build a row of styled spans for a preview line with highlighted match ranges.
fn build_preview_spans(
    preview: &str,
    ranges: &[std::ops::Range<usize>],
    t: &RuntimeTheme,
) -> AnyElement {
    if ranges.is_empty() {
        return div()
            .overflow_hidden()
            .text_ellipsis()
            .text_color(t.text_muted)
            .child(SharedString::from(preview.to_string()))
            .into_any_element();
    }

    let chars: Vec<char> = preview.chars().collect();
    let mut spans: Vec<AnyElement> = Vec::new();
    let mut pos = 0usize;

    for r in ranges {
        let start = r.start.min(chars.len());
        let end = r.end.min(chars.len());

        if pos < start {
            let text: String = chars[pos..start].iter().collect();
            spans.push(div().text_color(t.text_muted).child(SharedString::from(text)).into_any_element());
        }
        if start < end {
            let text: String = chars[start..end].iter().collect();
            spans.push(
                div()
                    .text_color(t.text)
                    .bg(t.match_bg)
                    .rounded(px(2.))
                    .child(SharedString::from(text))
                    .into_any_element(),
            );
        }
        pos = end;
    }

    if pos < chars.len() {
        let text: String = chars[pos..].iter().collect();
        spans.push(div().text_color(t.text_muted).child(SharedString::from(text)).into_any_element());
    }

    h_flex()
        .flex_1()
        .min_w(px(0.))
        .overflow_hidden()
        .children(spans)
        .into_any_element()
}

