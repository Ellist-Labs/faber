//! Project-wide symbol picker (cmd-t). Queries the persisted LMDB symbol index
//! via `project_symbols`, then lets the user fuzzy-navigate to any indexed
//! function, struct, or other outline item.

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, Context, FocusHandle, Focusable, IntoElement, KeyDownEvent, Render, ScrollHandle,
    SharedString, Task, WeakEntity, Window, div, prelude::*, px,
};
use rust_i18n::t;

use crate::input_helpers::{
    delete_char_before, delete_char_range, insert_at, split_at_char, word_start_before,
};
use crate::theme::RuntimeTheme;
use crate::ui::{h_flex, v_flex};
use crate::workspace::Workspace;
use crate::{
    SfBackspace, SfConfirm, SfDismiss, SfMoveEnd, SfMoveLeft, SfMoveRight, SfMoveStart,
    SfSelectNext, SfSelectPrev,
};

const RESULT_LIMIT: usize = 100;
const FILTER_DEBOUNCE_MS: u64 = 30;

const MODAL_W: f32 = 640.;
const MODAL_H: f32 = 440.;

pub struct SymbolFinderView {
    workspace: WeakEntity<Workspace>,
    pub focus_handle: FocusHandle,
    query: String,
    cursor: usize,
    selected: usize,
    rows: Vec<SymbolRow>,
    filtering: bool,
    filter_generation: u64,
    filter_task: Option<Task<()>>,
    list_scroll: ScrollHandle,
    cursor_blink_on: bool,
    blink_epoch: u64,
    blink_task: Option<Task<()>>,
}

struct SymbolRow {
    name: SharedString,
    rel_path: SharedString,
    source_line: usize,
}

impl SymbolRow {
    fn from_match(m: faber_index::SymbolMatch) -> Self {
        Self {
            name: m.name.into(),
            rel_path: m.rel_path.into(),
            source_line: m.source_line,
        }
    }
}

impl Focusable for SymbolFinderView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SymbolFinderView {
    pub fn new(workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            workspace,
            focus_handle: cx.focus_handle(),
            query: String::new(),
            cursor: 0,
            selected: 0,
            rows: Vec::new(),
            filtering: false,
            filter_generation: 0,
            filter_task: None,
            list_scroll: ScrollHandle::new(),
            cursor_blink_on: true,
            blink_epoch: 0,
            blink_task: None,
        };
        view.schedule_filter_deferred(cx);
        view.reset_blink(cx);
        view
    }

    fn store_arc(&self, cx: &App) -> Option<Arc<faber_index::store::IndexStore>> {
        self.workspace.upgrade()?.read(cx).index_store_arc()
    }

    fn root(&self, cx: &App) -> Option<std::path::PathBuf> {
        self.workspace
            .upgrade()
            .and_then(|ws| ws.read(cx).root_folder().cloned())
    }

    fn schedule_filter_deferred(&self, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        cx.defer(move |cx| {
            this.update(cx, |view, cx| view.schedule_filter(cx)).ok();
        });
    }

    fn set_query_changed(&mut self, cx: &mut Context<Self>) {
        self.selected = 0;
        self.reset_blink(cx);
        self.schedule_filter(cx);
    }

    fn schedule_filter(&mut self, cx: &mut Context<Self>) {
        self.filter_generation += 1;
        let generation = self.filter_generation;

        let Some(store) = self.store_arc(cx) else {
            self.rows.clear();
            self.filtering = false;
            cx.notify();
            return;
        };

        let query = self.query.clone();
        self.filtering = true;
        cx.notify();

        self.filter_task = Some(cx.spawn(async move |view_entity, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(FILTER_DEBOUNCE_MS))
                .await;

            let still_valid = view_entity
                .update(cx, |v, _| v.filter_generation == generation)
                .unwrap_or(false);
            if !still_valid {
                return;
            }

            let matches = cx
                .background_executor()
                .spawn(async move {
                    faber_index::project_symbols(&store, &query, RESULT_LIMIT).unwrap_or_default()
                })
                .await;

            view_entity
                .update(cx, |view, cx| {
                    if view.filter_generation != generation {
                        return;
                    }
                    view.rows = matches.into_iter().map(SymbolRow::from_match).collect();
                    view.selected = view.selected.min(view.rows.len().saturating_sub(1));
                    view.filtering = false;
                    cx.notify();
                })
                .ok();
        }));
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        let rel_path = row.rel_path.to_string();
        let source_line = row.source_line;

        let Some(root) = self.root(cx) else {
            return;
        };
        let abs_path = root.join(&rel_path);

        let Some(ws) = self.workspace.upgrade() else {
            return;
        };
        ws.update(cx, |workspace, cx| {
            workspace.close_symbol_finder(window, cx);
            workspace.navigate_to(&abs_path, source_line, 0, window, cx);
        });
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ws) = self.workspace.upgrade() {
            ws.update(cx, |w, cx| w.close_symbol_finder(window, cx));
        }
    }

    fn move_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let n = self.rows.len() as i32;
        self.selected = ((self.selected as i32 + delta).rem_euclid(n)) as usize;
        self.list_scroll.scroll_to_item(self.selected);
        cx.notify();
    }

    fn reset_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_blink_on = true;
        self.blink_epoch += 1;
        let epoch = self.blink_epoch;
        self.blink_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(530))
                    .await;
                let cont = this
                    .update(cx, |v, cx| {
                        if v.blink_epoch != epoch {
                            return false;
                        }
                        v.cursor_blink_on = !v.cursor_blink_on;
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !cont {
                    break;
                }
            }
        }));
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        if ks.modifiers.platform
            && !ks.modifiers.control
            && !ks.modifiers.alt
            && ks.key.as_str() == "backspace"
        {
            self.query.clear();
            self.cursor = 0;
            self.set_query_changed(cx);
            return;
        }
        if ks.modifiers.alt
            && !ks.modifiers.platform
            && !ks.modifiers.control
            && !ks.modifiers.shift
            && ks.key.as_str() == "backspace"
        {
            let ws = word_start_before(&self.query, self.cursor);
            if ws < self.cursor {
                self.query = delete_char_range(&self.query, ws, self.cursor);
                self.cursor = ws;
                self.set_query_changed(cx);
            }
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
        self.query = insert_at(&self.query, self.cursor, raw_text);
        self.cursor += raw_text.chars().count();
        self.set_query_changed(cx);
    }

    fn on_sf_backspace(&mut self, _: &SfBackspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.cursor > 0 {
            self.query = delete_char_before(&self.query, self.cursor);
            self.cursor -= 1;
            self.set_query_changed(cx);
        }
    }

    fn on_sf_confirm(&mut self, _: &SfConfirm, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm(window, cx);
    }

    fn on_sf_dismiss(&mut self, _: &SfDismiss, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss(window, cx);
    }

    fn on_sf_select_next(&mut self, _: &SfSelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(1, cx);
    }

    fn on_sf_select_prev(&mut self, _: &SfSelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(-1, cx);
    }

    fn on_sf_move_left(&mut self, _: &SfMoveLeft, _: &mut Window, cx: &mut Context<Self>) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.reset_blink(cx);
            cx.notify();
        }
    }

    fn on_sf_move_right(&mut self, _: &SfMoveRight, _: &mut Window, cx: &mut Context<Self>) {
        if self.cursor < self.query.chars().count() {
            self.cursor += 1;
            self.reset_blink(cx);
            cx.notify();
        }
    }

    fn on_sf_move_start(&mut self, _: &SfMoveStart, _: &mut Window, cx: &mut Context<Self>) {
        self.cursor = 0;
        self.reset_blink(cx);
        cx.notify();
    }

    fn on_sf_move_end(&mut self, _: &SfMoveEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.cursor = self.query.chars().count();
        self.reset_blink(cx);
        cx.notify();
    }
}

impl Render for SymbolFinderView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<RuntimeTheme>().clone();
        let store_ready = self.store_arc(cx).is_some();
        let is_indexing = self.filtering && self.rows.is_empty();
        let no_results = !self.filtering && self.rows.is_empty();
        let has_root = self.root(cx).is_some();

        // ── empty-state helper ────────────────────────────────────────────────
        let empty = |msg: &str, t: &RuntimeTheme| {
            div()
                .flex_1()
                .flex()
                .min_h(px(120.))
                .items_center()
                .justify_center()
                .py(px(20.))
                .font_family(t.ui_family.clone())
                .text_size(px(t.font_size_caption))
                .text_color(t.text_muted)
                .child(msg.to_string())
                .into_any()
        };

        // ── input row ─────────────────────────────────────────────────────────
        let (before, after) = split_at_char(&self.query, self.cursor);
        let caret_h = t.font_size_body + 4.;
        let cur_on = self.cursor_blink_on;

        let input = if self.query.is_empty() {
            h_flex()
                .px_3()
                .h(px(caret_h + 12.))
                .items_center()
                .border_b_1()
                .border_color(t.separator)
                .font_family(t.ui_family.clone())
                .text_size(px(t.font_size_body))
                .text_color(t.text_subtle)
                .child(t!("symbol_finder.placeholder").to_string())
                .into_any()
        } else {
            h_flex()
                .px_3()
                .h(px(caret_h + 12.))
                .items_center()
                .gap_0()
                .border_b_1()
                .border_color(t.separator)
                .child(
                    div()
                        .font_family(t.ui_family.clone())
                        .text_size(px(t.font_size_body))
                        .text_color(t.text)
                        .child(before),
                )
                .child(div().h(px(caret_h)).w(px(1.5)).bg(if cur_on {
                    t.cursor
                } else {
                    gpui::hsla(0., 0., 0., 0.)
                }))
                .child(
                    div()
                        .font_family(t.ui_family.clone())
                        .text_size(px(t.font_size_body))
                        .text_color(t.text)
                        .child(after),
                )
                .into_any()
        };

        // ── body ──────────────────────────────────────────────────────────────
        let body: gpui::AnyElement = if !has_root {
            empty(&t!("symbol_finder.open_folder_hint"), &t)
        } else if !store_ready || is_indexing {
            empty(&t!("symbol_finder.indexing"), &t)
        } else if no_results && !self.query.is_empty() {
            empty(&t!("symbol_finder.no_matches"), &t)
        } else {
            let selected = self.selected;
            let entries: Vec<gpui::AnyElement> = self
                .rows
                .iter()
                .enumerate()
                .map(|(i, row)| {
                    let is_selected = i == selected;
                    let hover_bg = t.line_highlight;
                    let name = row.name.to_string();
                    let path_and_line = format!("{}:{}", row.rel_path, row.source_line + 1);
                    div()
                        .id(("sf-row", i))
                        .px_3()
                        .py(px(4.))
                        .cursor_pointer()
                        .when(is_selected, |el| el.bg(t.line_highlight))
                        .hover(move |el| el.bg(hover_bg))
                        .on_mouse_move(cx.listener(move |view, _, _, cx| {
                            if view.selected != i {
                                view.selected = i;
                                cx.notify();
                            }
                        }))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |view, _, window, cx| {
                                view.selected = i;
                                view.confirm(window, cx);
                            }),
                        )
                        .child(
                            v_flex()
                                .gap_0()
                                .child(
                                    div()
                                        .font_family(t.ui_family.clone())
                                        .text_size(px(t.font_size_caption))
                                        .text_color(t.text)
                                        .child(name),
                                )
                                .child(
                                    div()
                                        .font_family(t.ui_family.clone())
                                        .text_size(px(t.font_size_caption - 1.))
                                        .text_color(t.text_subtle)
                                        .child(path_and_line),
                                ),
                        )
                        .into_any()
                })
                .collect();

            div()
                .id("sf-list")
                .flex_col()
                .min_h(px(120.))
                .max_h(px(MODAL_H - 60.))
                .overflow_y_scroll()
                .track_scroll(&self.list_scroll)
                .children(entries)
                .into_any()
        };

        // ── modal shell ────────────────────────────────────────────────────────
        let modal = v_flex()
            .w(px(MODAL_W))
            .max_h(px(MODAL_H))
            .bg(t.bg_elevated)
            .border_1()
            .border_color(t.separator)
            .rounded(px(t.radius_md))
            .key_context("SymbolFinder")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_sf_dismiss))
            .on_action(cx.listener(Self::on_sf_confirm))
            .on_action(cx.listener(Self::on_sf_select_next))
            .on_action(cx.listener(Self::on_sf_select_prev))
            .on_action(cx.listener(Self::on_sf_backspace))
            .on_action(cx.listener(Self::on_sf_move_left))
            .on_action(cx.listener(Self::on_sf_move_right))
            .on_action(cx.listener(Self::on_sf_move_start))
            .on_action(cx.listener(Self::on_sf_move_end))
            .on_key_down(cx.listener(Self::on_key_down))
            .child(input)
            .child(body);

        div()
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .pt(px(72.))
            .child(modal)
    }
}
