use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use faber_index::files::{FileIndexSnapshot, FinderQuery, filter};
use faber_settings::PreviewPosition;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Bounds, Context, FocusHandle, Focusable,
    IntoElement, KeyDownEvent, MouseButton, Render, ScrollHandle, SharedString, Task, TextRun,
    WeakEntity, Window, canvas, deferred, div, ease_in_out, fill, font, img, point, prelude::*, px,
    size, svg,
};
use rust_i18n::t;

use crate::file_icons;
use crate::file_preview::{FilePreview, PreviewContent, load_file, render_preview};
use crate::input_helpers::{
    delete_char_before, delete_char_range, insert_at, split_at_char, word_start_before,
};
use crate::settings_view::SettingsStore;
use crate::theme::RuntimeTheme;
use crate::ui::{IconName, KeyHint, h_flex, v_flex};
use crate::workspace::Workspace;
use crate::{
    FfBackspace, FfConfirm, FfDismiss, FfMoveEnd, FfMoveLeft, FfMoveRight, FfMoveStart,
    FfSelectNext, FfSelectPrev, FfToggleCase, FfToggleIgnored, FfTogglePreview, FfToggleRegex,
    FfToggleWholeWord,
};

const RESULT_LIMIT: usize = 100;
const FILTER_DEBOUNCE_MS: u64 = 30;
const PREVIEW_DEBOUNCE_MS: u64 = 100;

// ── modal geometry ─────────────────────────────────────────────────────────────
const BACKDROP_TOP: f32 = 56.;
const INPUT_ROW_H: f32 = 45.;
const MODAL_W_COLLAPSED: f32 = 640.;
const MODAL_BODY_H_COLLAPSED: f32 = 440.;
const MODAL_W_SIDE: f32 = 1060.;
const MODAL_BODY_H_SIDE: f32 = 480.;
const MODAL_W_BOTTOM: f32 = 720.;
const MODAL_BODY_H_BOTTOM: f32 = 564.;
const LIST_W_DEFAULT: f32 = 520.;
const LIST_H_DEFAULT: f32 = 260.;

/// One display row, resolved off-thread from a `FinderMatch`.
pub struct FinderRow {
    pub rel_path: SharedString,
    /// Byte offset where the file name starts in `rel_path`.
    pub name_off: usize,
    pub icon: &'static str,
    pub from_history: bool,
    pub is_ignored: bool,
    /// Char indices into `rel_path` of matched characters.
    pub positions: Vec<u32>,
}

pub struct FileFinderView {
    workspace: WeakEntity<Workspace>,
    pub focus_handle: FocusHandle,
    query: String,
    cursor: usize,
    case_sensitive: bool,
    whole_word: bool,
    regex_mode: bool,
    include_ignored: bool,
    mask: Option<String>,
    mask_open: bool,
    preview_on: bool,
    /// Bumped each time preview is toggled; used as animation ID seed so each
    /// toggle starts a fresh width+height animation.
    preview_toggle_count: usize,
    /// List panel width in the right/left split layout (px, user-resizable).
    list_width: f32,
    /// List panel height in the bottom split layout (px, user-resizable).
    list_height: f32,
    /// Active divider drag: (start_mouse_axis_pos, size_at_drag_start).
    divider_drag: Option<(f32, f32)>,
    selected: usize,
    rows: Vec<FinderRow>,
    filtering: bool,
    filter_generation: u64,
    filter_task: Option<Task<()>>,
    pub preview: FilePreview,
    preview_task: Option<Task<()>>,
    list_scroll: ScrollHandle,
    cursor_blink_on: bool,
    blink_epoch: u64,
    blink_task: Option<Task<()>>,
}

impl FileFinderView {
    pub fn new(workspace: WeakEntity<Workspace>, preview_on: bool, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            workspace,
            focus_handle: cx.focus_handle(),
            query: String::new(),
            cursor: 0,
            case_sensitive: false,
            whole_word: false,
            regex_mode: false,
            include_ignored: false,
            mask: None,
            mask_open: false,
            preview_on,
            preview_toggle_count: 0,
            list_width: LIST_W_DEFAULT,
            list_height: LIST_H_DEFAULT,
            divider_drag: None,
            selected: 0,
            rows: Vec::new(),
            filtering: false,
            filter_generation: 0,
            filter_task: None,
            preview: FilePreview::new(),
            preview_task: None,
            list_scroll: ScrollHandle::new(),
            cursor_blink_on: true,
            blink_epoch: 0,
            blink_task: None,
        };
        // The view is constructed inside a Workspace update; reading the
        // workspace back (for the index snapshot) must wait until it returns.
        view.schedule_filter_deferred(cx);
        view.reset_blink(cx);
        view
    }

    fn schedule_filter_deferred(&self, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        cx.defer(move |cx| {
            this.update(cx, |view, cx| view.schedule_filter(cx)).ok();
        });
    }

    fn root(&self, cx: &App) -> Option<PathBuf> {
        self.workspace
            .upgrade()
            .and_then(|ws| ws.read(cx).root_folder().cloned())
    }

    fn snapshot(&self, cx: &App) -> Option<Arc<FileIndexSnapshot>> {
        self.workspace
            .upgrade()?
            .read(cx)
            .files_handle
            .as_ref()?
            .load()
    }

    fn history(&self, cx: &App) -> Vec<String> {
        match self.root(cx) {
            Some(root) => cx
                .global::<crate::AppStateStore>()
                .0
                .history_for(&root.to_string_lossy())
                .to_vec(),
            None => Vec::new(),
        }
    }

    // ── filtering ──────────────────────────────────────────────────────────────

    fn schedule_filter(&mut self, cx: &mut Context<Self>) {
        self.filter_generation += 1;
        let generation = self.filter_generation;

        let Some(snapshot) = self.snapshot(cx) else {
            self.rows.clear();
            self.filtering = self.root(cx).is_some();
            cx.notify();
            return;
        };
        let history = self.history(cx);
        let query = FinderQuery {
            text: self.query.clone(),
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            regex: self.regex_mode,
            mask: self.mask.clone(),
            ..Default::default()
        };
        self.filtering = true;
        cx.notify();

        self.filter_task = Some(cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(FILTER_DEBOUNCE_MS))
                .await;
            let valid = view
                .update(cx, |v, _| v.filter_generation == generation)
                .unwrap_or(false);
            if !valid {
                return;
            }
            let rows = cx
                .background_executor()
                .spawn(async move {
                    filter(&snapshot, &query, &history, RESULT_LIMIT)
                        .into_iter()
                        .map(|m| {
                            let e = &snapshot.entries[m.entry_ix as usize];
                            FinderRow {
                                rel_path: SharedString::from(e.rel_path.clone()),
                                name_off: e.name_off as usize,
                                icon: file_icons::icon_for_file(e.name()),
                                from_history: m.from_history,
                                is_ignored: e.is_ignored,
                                positions: m.positions,
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            view.update(cx, |view, cx| {
                if view.filter_generation != generation {
                    return;
                }
                view.rows = rows;
                view.filtering = false;
                view.selected = view.selected.min(view.rows.len().saturating_sub(1));
                view.list_scroll.scroll_to_item(view.selected);
                view.schedule_preview(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    // ── preview ────────────────────────────────────────────────────────────────

    fn schedule_preview(&mut self, cx: &mut Context<Self>) {
        if !self.preview_on {
            return;
        }
        self.preview.epoch += 1;
        let epoch = self.preview.epoch;
        let Some(root) = self.root(cx) else { return };
        let Some(row) = self.rows.get(self.selected) else {
            self.preview.content = PreviewContent::Empty;
            self.preview.path = None;
            cx.notify();
            return;
        };
        let abs = root.join(row.rel_path.as_ref());
        if self.preview.path.as_ref() == Some(&abs) {
            return;
        }
        if !matches!(self.preview.content, PreviewContent::Doc { .. }) {
            self.preview.content = PreviewContent::Loading;
            cx.notify();
        }
        self.preview_task = Some(cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(PREVIEW_DEBOUNCE_MS))
                .await;
            let valid = view
                .update(cx, |v, _| v.preview.epoch == epoch)
                .unwrap_or(false);
            if !valid {
                return;
            }
            let path = abs.clone();
            let loaded = cx
                .background_executor()
                .spawn(async move { load_file(&path) })
                .await;
            view.update(cx, |view, cx| {
                if view.preview.epoch != epoch {
                    return;
                }
                let registry = cx.global::<crate::Registry>().0.clone();
                view.preview.set_loaded(abs, loaded, &registry);
                cx.notify();
            })
            .ok();
        }));
    }

    // ── selection / confirm ────────────────────────────────────────────────────

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let len = self.rows.len() as isize;
        let next = (self.selected as isize + delta).rem_euclid(len);
        self.selected = next as usize;
        self.list_scroll.scroll_to_item(self.selected);
        self.schedule_preview(cx);
        cx.notify();
    }

    fn set_selection(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.rows.len() && self.selected != ix {
            self.selected = ix;
            self.schedule_preview(cx);
            cx.notify();
        }
    }

    fn confirm(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix) else { return };
        let Some(root) = self.root(cx) else { return };
        let rel = row.rel_path.to_string();
        let abs = root.join(&rel);

        let root_str = root.to_string_lossy().to_string();
        let mut state = cx.global::<crate::AppStateStore>().0.clone();
        state.record_finder_file(&root_str, &rel);
        let state_to_save = state.clone();
        cx.set_global(crate::AppStateStore(state));
        cx.background_executor()
            .spawn(async move {
                if let Err(err) = faber_settings::state::save(&state_to_save) {
                    eprintln!("faber: can't save state: {err}");
                }
            })
            .detach();

        if let Some(ws) = self.workspace.upgrade() {
            ws.update(cx, |ws, cx| {
                ws.close_file_finder(window, cx);
                ws.open_path(&abs, window, cx);
            });
        }
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ws) = self.workspace.upgrade() {
            ws.update(cx, |ws, cx| ws.close_file_finder(window, cx));
        }
    }

    // ── input ──────────────────────────────────────────────────────────────────

    fn set_query_changed(&mut self, cx: &mut Context<Self>) {
        self.selected = 0;
        self.reset_blink(cx);
        self.schedule_filter(cx);
    }

    fn reset_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_blink_on = true;
        self.blink_epoch += 1;
        let epoch = self.blink_epoch;
        self.blink_task = Some(cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(530))
                    .await;
                let cont = view
                    .update(cx, |this, cx| {
                        if this.blink_epoch != epoch {
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
        }));
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        // cmd-backspace clears; alt-backspace deletes the word before the cursor.
        // Both bypass GPUI dispatch on macOS (NSTextInputClient) — handle here.
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
        self.mask_open = false;
        self.set_query_changed(cx);
    }

    // ── action handlers ────────────────────────────────────────────────────────

    fn on_dismiss(&mut self, _: &FfDismiss, window: &mut Window, cx: &mut Context<Self>) {
        if self.mask_open {
            self.mask_open = false;
            cx.notify();
            return;
        }
        self.dismiss(window, cx);
    }

    fn on_confirm(&mut self, _: &FfConfirm, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm(self.selected, window, cx);
    }

    fn on_select_next(&mut self, _: &FfSelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(1, cx);
    }

    fn on_select_prev(&mut self, _: &FfSelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(-1, cx);
    }

    fn on_backspace(&mut self, _: &FfBackspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.cursor > 0 {
            self.query = delete_char_before(&self.query, self.cursor);
            self.cursor -= 1;
            self.set_query_changed(cx);
        }
    }

    fn on_move_left(&mut self, _: &FfMoveLeft, _: &mut Window, cx: &mut Context<Self>) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.reset_blink(cx);
            cx.notify();
        }
    }

    fn on_move_right(&mut self, _: &FfMoveRight, _: &mut Window, cx: &mut Context<Self>) {
        if self.cursor < self.query.chars().count() {
            self.cursor += 1;
            self.reset_blink(cx);
            cx.notify();
        }
    }

    fn on_move_start(&mut self, _: &FfMoveStart, _: &mut Window, cx: &mut Context<Self>) {
        self.cursor = 0;
        self.reset_blink(cx);
        cx.notify();
    }

    fn on_move_end(&mut self, _: &FfMoveEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.cursor = self.query.chars().count();
        self.reset_blink(cx);
        cx.notify();
    }

    fn on_toggle_case(&mut self, _: &FfToggleCase, _: &mut Window, cx: &mut Context<Self>) {
        self.case_sensitive = !self.case_sensitive;
        self.schedule_filter(cx);
    }

    fn on_toggle_word(&mut self, _: &FfToggleWholeWord, _: &mut Window, cx: &mut Context<Self>) {
        self.whole_word = !self.whole_word;
        self.schedule_filter(cx);
    }

    fn on_toggle_regex(&mut self, _: &FfToggleRegex, _: &mut Window, cx: &mut Context<Self>) {
        self.regex_mode = !self.regex_mode;
        self.schedule_filter(cx);
    }

    fn on_toggle_ignored(&mut self, _: &FfToggleIgnored, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_ignored(cx);
    }

    fn toggle_ignored(&mut self, cx: &mut Context<Self>) {
        self.include_ignored = !self.include_ignored;
        if self.include_ignored
            && let Some(ws) = self.workspace.upgrade()
        {
            ws.update(cx, |ws, cx| ws.ensure_full_index(cx));
        }
        self.schedule_filter(cx);
    }

    fn on_toggle_preview(&mut self, _: &FfTogglePreview, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_preview(cx);
    }

    fn toggle_preview(&mut self, cx: &mut Context<Self>) {
        self.preview_on = !self.preview_on;
        self.preview_toggle_count += 1;
        if self.preview_on {
            self.schedule_preview(cx);
        }
        cx.notify();
    }

    pub fn enable_preview(&mut self, cx: &mut Context<Self>) {
        if !self.preview_on {
            self.toggle_preview(cx);
        }
    }

    fn set_mask(&mut self, mask: Option<String>, cx: &mut Context<Self>) {
        self.mask = mask;
        self.mask_open = false;
        self.selected = 0;
        self.schedule_filter(cx);
    }

    // ── render pieces ──────────────────────────────────────────────────────────

    fn render_input_row(
        &self,
        t: &RuntimeTheme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focused = self.focus_handle.is_focused(window);
        let is_empty = self.query.is_empty();
        let caret_h = t.font_size_code + 4.;
        let radius = t.radius_sm;
        let hover_bg = t.line_highlight;

        // Same chip pattern as the search bars.
        let chip = |id: &'static str, label: &'static str, active: bool, t: &RuntimeTheme| {
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
                .when(active, {
                    let accent = t.accent;
                    let accent_hover = t.accent_hover;
                    move |el| el.bg(accent).hover(move |s| s.bg(accent_hover))
                })
                .when(!active, move |el| el.hover(move |s| s.bg(hover_bg)))
                .child(label)
        };

        let input: AnyElement = if !focused && is_empty {
            div()
                .flex_1()
                .h(px(caret_h))
                .flex()
                .items_center()
                .font_family(t.ui_family.clone())
                .text_size(px(t.font_size_caption))
                .text_color(t.text_subtle)
                .child(t!("file_finder.placeholder").to_string())
                .into_any()
        } else {
            let (before, after) = split_at_char(&self.query, self.cursor);
            let full_text = format!("{before}{after}");
            let caret_byte = before.len();
            let cur_on = focused && self.cursor_blink_on;
            let font_sz = px(t.font_size_code);
            let caret_h_px = px(caret_h);
            let mono = t.mono_family.clone();
            let text_col = t.text;
            let cursor_color = if cur_on {
                t.cursor
            } else {
                gpui::hsla(0., 0., 0., 0.)
            };
            canvas(
                move |_bounds, window, _cx| {
                    let runs = if full_text.is_empty() {
                        vec![]
                    } else {
                        vec![TextRun {
                            len: full_text.len(),
                            font: font(mono.clone()),
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
            .h(px(caret_h))
            .into_any()
        };

        // Include-ignored toggle — eye icon chip.
        let ignored_chip = div()
            .id("ff-ignored")
            .px_1()
            .py(px(2.))
            .rounded(px(radius))
            .cursor_pointer()
            .flex_shrink_0()
            .flex()
            .items_center()
            .when(self.include_ignored, {
                let accent = t.accent;
                let accent_hover = t.accent_hover;
                move |el| el.bg(accent).hover(move |s| s.bg(accent_hover))
            })
            .when(!self.include_ignored, move |el| {
                el.hover(move |s| s.bg(hover_bg))
            })
            .child(
                svg()
                    .path(IconName::Visibility.path())
                    .size(px(14.))
                    .text_color(if self.include_ignored {
                        t.text_on_accent
                    } else {
                        t.text_subtle
                    }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _, _, cx| view.toggle_ignored(cx)),
            );

        h_flex()
            .id("ff-input-row")
            .px_4()
            .py(px(10.))
            .gap_2()
            .border_b_1()
            .border_color(t.separator)
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
            .child(input)
            .child(
                h_flex()
                    .gap_1()
                    .flex_shrink_0()
                    .child(chip("ff-case", "Aa", self.case_sensitive, t).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, _, _, cx| {
                            view.case_sensitive = !view.case_sensitive;
                            view.schedule_filter(cx);
                        }),
                    ))
                    .child(chip("ff-word", "W", self.whole_word, t).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, _, _, cx| {
                            view.whole_word = !view.whole_word;
                            view.schedule_filter(cx);
                        }),
                    ))
                    .child(chip("ff-regex", ".*", self.regex_mode, t).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, _, _, cx| {
                            view.regex_mode = !view.regex_mode;
                            view.schedule_filter(cx);
                        }),
                    ))
                    .child(ignored_chip)
                    .child(self.render_mask_button(t, cx)),
            )
            .into_any()
    }

    fn render_mask_button(&self, t: &RuntimeTheme, cx: &mut Context<Self>) -> AnyElement {
        let label = match &self.mask {
            Some(ext) => format!("*.{ext}"),
            None => t!("file_finder.mask_all").to_string(),
        };
        let hover_bg = t.line_highlight;
        h_flex()
            .id("ff-mask")
            .px_2()
            .py(px(2.))
            .gap_1()
            .rounded(px(t.radius_sm))
            .cursor_pointer()
            .flex_shrink_0()
            .text_size(px(t.font_size_code - 1.))
            .font_family(t.mono_family.clone())
            .when(self.mask.is_some(), {
                let accent = t.accent;
                let accent_hover = t.accent_hover;
                move |el| el.bg(accent).hover(move |s| s.bg(accent_hover))
            })
            .when(self.mask.is_none(), move |el| {
                el.hover(move |s| s.bg(hover_bg))
            })
            .text_color(if self.mask.is_some() {
                t.text_on_accent
            } else {
                t.text_subtle
            })
            .child(label)
            .child(
                svg()
                    .path(IconName::ExpandMore.path())
                    .size(px(12.))
                    .text_color(if self.mask.is_some() {
                        t.text_on_accent
                    } else {
                        t.text_subtle
                    }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _, _, cx| {
                    view.mask_open = !view.mask_open;
                    cx.notify();
                }),
            )
            .into_any()
    }

    /// Extension picker panel content. Positioning is handled by the caller so
    /// the panel is rendered outside the `overflow_hidden` modal container.
    fn render_mask_panel(&self, t: &RuntimeTheme, cx: &mut Context<Self>) -> AnyElement {
        let extensions = self
            .snapshot(cx)
            .map(|s| s.extensions.clone())
            .unwrap_or_default();
        let mut items: Vec<AnyElement> = Vec::new();

        let row = |id: usize, label: String, count: Option<u32>, active: bool, t: &RuntimeTheme| {
            let hover_bg = t.line_highlight;
            h_flex()
                .id(("ff-mask-item", id))
                .px_3()
                .py(px(4.))
                .gap_2()
                .cursor_pointer()
                .font_family(t.mono_family.clone())
                .text_size(px(t.font_size_caption))
                .text_color(if active { t.accent } else { t.text })
                .hover(move |s| s.bg(hover_bg))
                .child(div().flex_1().child(label))
                .when_some(count, |el, n| {
                    el.child(div().text_color(t.text_subtle).child(format!("{n}")))
                })
        };

        items.push(
            row(
                0,
                t!("file_finder.mask_all").to_string(),
                None,
                self.mask.is_none(),
                t,
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _, _, cx| view.set_mask(None, cx)),
            )
            .into_any(),
        );
        for (i, (ext, count)) in extensions.into_iter().enumerate() {
            let active = self.mask.as_deref() == Some(ext.as_str());
            let ext_clone = ext.clone();
            items.push(
                row(i + 1, format!("*.{ext}"), Some(count), active, t)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |view, _, _, cx| {
                            view.set_mask(Some(ext_clone.clone()), cx)
                        }),
                    )
                    .into_any(),
            );
        }

        div()
            .id("ff-mask-dropdown")
            .w(px(200.))
            .max_h(px(320.))
            .overflow_y_scroll()
            .bg(t.bg_elevated)
            .border_1()
            .border_color(t.border)
            .rounded(px(t.radius_sm))
            .shadow_lg()
            .flex()
            .flex_col()
            .children(items)
            .into_any()
    }

    /// `fill_h`: when true the list stretches to its parent's height (used in
    /// side-by-side preview layout). When false, `max_h` caps the height.
    fn render_list(
        &self,
        fill_h: bool,
        max_h: f32,
        t: &RuntimeTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let empty_state = |msg: String, t: &RuntimeTheme| {
            div()
                .flex_1()
                .flex()
                .min_h(px(160.))
                .items_center()
                .justify_center()
                .py(px(24.))
                .font_family(t.ui_family.clone())
                .text_size(px(t.font_size_caption))
                .text_color(t.text_muted)
                .child(msg)
                .into_any()
        };

        if self.root(cx).is_none() {
            return empty_state(t!("file_finder.open_folder_hint").to_string(), t);
        }
        if self.rows.is_empty() {
            if self.filtering {
                return empty_state(t!("file_finder.indexing").to_string(), t);
            }
            let msg = if self.query.is_empty() {
                t!("file_finder.no_history").to_string()
            } else {
                t!("file_finder.no_matches").to_string()
            };
            return empty_state(msg, t);
        }

        let selected = self.selected;
        let entries: Vec<AnyElement> = self
            .rows
            .iter()
            .enumerate()
            .map(|(ix, row)| {
                let is_selected = ix == selected;
                let hover_bg = t.line_highlight;
                div()
                    .id(("ff-row", ix))
                    .flex()
                    .flex_row()
                    .items_center()
                    .px_3()
                    .py(px(5.))
                    .gap_2()
                    .font_family(t.ui_family.clone())
                    .text_size(px(t.font_size_caption))
                    .when(is_selected, |el| el.bg(t.line_highlight))
                    .cursor_pointer()
                    .hover(move |el| el.bg(hover_bg))
                    .on_mouse_move(cx.listener(move |view, _, _, cx| view.set_selection(ix, cx)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |view, _, window, cx| view.confirm(ix, window, cx)),
                    )
                    .child(img(row.icon).size(px(16.)).flex_shrink_0())
                    .child(render_row_text(row, t))
                    .when(row.from_history, |el| {
                        el.child(
                            svg()
                                .path(IconName::History.path())
                                .size(px(13.))
                                .text_color(t.text_subtle)
                                .flex_shrink_0(),
                        )
                    })
                    .into_any()
            })
            .collect();

        let list = div()
            .id("ff-list")
            .flex_col()
            .overflow_y_scroll()
            .track_scroll(&self.list_scroll);

        if fill_h {
            list.h_full().children(entries).into_any()
        } else {
            list.min_h(px(160.))
                .max_h(px(max_h))
                .children(entries)
                .into_any()
        }
    }

    /// Draggable divider between the list and preview panes.
    ///
    /// `row_divider`: `true` = horizontal line (bottom layout, drag up/down);
    ///               `false` = vertical line (right/left layout, drag left/right).
    fn render_divider(
        &self,
        row_divider: bool,
        t: &RuntimeTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_dragging = self.divider_drag.is_some();
        let sep = t.separator;
        let accent = t.accent;
        let accent_dim = gpui::hsla(accent.h, accent.s, accent.l, 0.35);
        let size = if row_divider {
            self.list_height
        } else {
            self.list_width
        };

        div()
            .id("ff-divider")
            .when(row_divider, |el| el.w_full().h(px(4.)).cursor_row_resize())
            .when(!row_divider, |el| el.h_full().w(px(4.)).cursor_col_resize())
            .bg(if is_dragging { accent_dim } else { sep })
            .hover(move |el| el.bg(accent_dim))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |view, ev: &gpui::MouseDownEvent, _, cx| {
                    let pos = if row_divider {
                        f32::from(ev.position.y)
                    } else {
                        f32::from(ev.position.x)
                    };
                    view.divider_drag = Some((pos, size));
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .into_any()
    }

    fn render_footer(&self, t: &RuntimeTheme) -> AnyElement {
        let hint = |keys: &'static str, label: String, t: &RuntimeTheme| {
            h_flex().gap_1().child(KeyHint::new(keys)).child(
                div()
                    .font_family(t.ui_family.clone())
                    .text_size(px(t.font_size_caption - 1.))
                    .text_color(t.text_muted)
                    .child(label),
            )
        };
        h_flex()
            .px_4()
            .py(px(6.))
            .gap_3()
            .border_t_1()
            .border_color(t.separator)
            .child(hint("↑↓", t!("file_finder.hint_navigate").to_string(), t))
            .child(hint("↵", t!("file_finder.hint_open").to_string(), t))
            .child(hint("⌥⌘P", t!("file_finder.hint_preview").to_string(), t))
            .child(hint("⎋", t!("file_finder.hint_dismiss").to_string(), t))
            .into_any()
    }
}

/// Name + dimmed directory, with fuzzy-matched chars in accent color.
fn render_row_text(row: &FinderRow, t: &RuntimeTheme) -> AnyElement {
    let rel = row.rel_path.as_ref();
    let name = &rel[row.name_off..];
    let dir = rel[..row.name_off].trim_end_matches('/');
    let name_off_chars = rel[..row.name_off].chars().count() as u32;

    let name_el = render_matched_text(name, &row.positions, name_off_chars, t.text, t);
    let dir_el = if dir.is_empty() {
        None
    } else {
        Some(render_matched_text(dir, &row.positions, 0, t.text_muted, t))
    };

    h_flex()
        .flex_1()
        .min_w(px(0.))
        .gap_2()
        .overflow_hidden()
        .when(row.is_ignored, |el| el.opacity(0.6))
        .child(name_el)
        .when_some(dir_el, |el, d| {
            el.child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(px(t.font_size_caption - 1.))
                    .child(d),
            )
        })
        .into_any()
}

/// Render `text` (a slice of rel_path starting at char offset `char_off`) as
/// span segments, painting chars listed in `positions` with the accent color.
fn render_matched_text(
    text: &str,
    positions: &[u32],
    char_off: u32,
    base_color: gpui::Hsla,
    t: &RuntimeTheme,
) -> AnyElement {
    let plain = || {
        div()
            .text_color(base_color)
            .overflow_hidden()
            .text_ellipsis()
            .child(SharedString::from(text.to_string()))
            .into_any()
    };
    let chars: Vec<char> = text.chars().collect();
    if positions.is_empty() || chars.is_empty() {
        return plain();
    }
    let end = char_off + chars.len() as u32;
    let local: Vec<usize> = positions
        .iter()
        .filter(|&&p| p >= char_off && p < end)
        .map(|&p| (p - char_off) as usize)
        .collect();
    if local.is_empty() {
        return plain();
    }

    let mut spans: Vec<AnyElement> = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let matched = local.binary_search(&i).is_ok();
        let start = i;
        while i < chars.len() && local.binary_search(&i).is_ok() == matched {
            i += 1;
        }
        let seg: String = chars[start..i].iter().collect();
        spans.push(
            div()
                .text_color(if matched { t.accent } else { base_color })
                .child(SharedString::from(seg))
                .into_any(),
        );
    }
    h_flex()
        .min_w(px(0.))
        .overflow_hidden()
        .children(spans)
        .into_any()
}

impl Focusable for FileFinderView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for FileFinderView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<RuntimeTheme>().clone();
        let preview_pos = cx.global::<SettingsStore>().0.file_finder_preview_position;
        let show_preview = self.preview_on;
        let toggle_count = self.preview_toggle_count;

        // ── sizing ─────────────────────────────────────────────────────────────
        let is_bottom = matches!(preview_pos, PreviewPosition::Bottom);

        // Divider thickness (draggable handle).
        const DIV_PX: f32 = 4.;

        // expanded_w / expanded_body_h: dimensions when preview is ON.
        let expanded_w: f32 = if is_bottom {
            MODAL_W_BOTTOM
        } else {
            MODAL_W_SIDE
        };
        let expanded_body_h: f32 = if is_bottom {
            MODAL_BODY_H_BOTTOM
        } else {
            MODAL_BODY_H_SIDE
        };

        let final_w: f32 = if show_preview {
            expanded_w
        } else {
            MODAL_W_COLLAPSED
        };
        let final_body_h: f32 = if show_preview {
            expanded_body_h
        } else {
            MODAL_BODY_H_COLLAPSED
        };

        // Animation start values. On the very first render (count==0) from_* ==
        // final_* so the animation is a no-op and no flicker occurs.
        let (from_w, from_body_h) = if toggle_count == 0 {
            (final_w, final_body_h)
        } else if show_preview {
            (MODAL_W_COLLAPSED, MODAL_BODY_H_COLLAPSED) // just expanded → start from collapsed
        } else {
            (expanded_w, expanded_body_h) // just collapsed → start from expanded
        };

        // ── user-controlled sizes (clamped to reasonable bounds) ───────────────
        let list_w = self.list_width.clamp(180., expanded_w - 180. - DIV_PX);
        let list_h = self
            .list_height
            .clamp(100., expanded_body_h - 100. - DIV_PX);

        // ── list ───────────────────────────────────────────────────────────────
        // Right/left: list fills its fixed-height column (fill_h=true).
        // Bottom: list has an explicit h set by the container wrapper.
        // No-preview: normal max_h behaviour.
        let list_fill_h = show_preview && !is_bottom;
        let list = self.render_list(list_fill_h, 440., &t, cx);

        // ── body content ───────────────────────────────────────────────────────
        let body: AnyElement = if show_preview {
            let preview = render_preview(cx.entity(), &self.preview, &t);
            match preview_pos {
                PreviewPosition::Bottom => {
                    let divider = self.render_divider(true, &t, cx);
                    // v_flex with a fixed total height so preview's flex_1 works.
                    v_flex()
                        .h(px(expanded_body_h))
                        .child(
                            div()
                                .h(px(list_h))
                                .flex_shrink_0()
                                .flex()
                                .flex_col()
                                .overflow_hidden()
                                .child(list),
                        )
                        .child(divider)
                        .child(
                            div()
                                .flex_1()
                                .min_h(px(0.))
                                .flex()
                                .flex_col()
                                .child(preview),
                        )
                        .into_any()
                }
                PreviewPosition::Right => {
                    let divider = self.render_divider(false, &t, cx);
                    div()
                        .flex()
                        .flex_row()
                        .h(px(expanded_body_h))
                        .child(
                            div()
                                .w(px(list_w))
                                .flex_shrink_0()
                                .h_full()
                                .flex()
                                .flex_col()
                                .child(list),
                        )
                        .child(divider)
                        .child(preview)
                        .into_any()
                }
                PreviewPosition::Left => {
                    let divider = self.render_divider(false, &t, cx);
                    div()
                        .flex()
                        .flex_row()
                        .h(px(expanded_body_h))
                        .child(preview)
                        .child(divider)
                        .child(
                            div()
                                .w(px(list_w))
                                .flex_shrink_0()
                                .h_full()
                                .flex()
                                .flex_col()
                                .child(list),
                        )
                        .into_any()
                }
            }
        } else {
            list
        };

        // ── animated body container ────────────────────────────────────────────
        // overflow_hidden clips the body at the animated height, creating a
        // smooth reveal as the modal expands/collapses.
        let body_container = div()
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(body)
            .with_animation(
                ("ff-body-h", toggle_count),
                Animation::new(std::time::Duration::from_millis(220)).with_easing(ease_in_out),
                move |el, p| el.max_h(px(from_body_h + (final_body_h - from_body_h) * p)),
            );

        // ── modal ──────────────────────────────────────────────────────────────
        let modal = div()
            .id("ff-modal")
            .occlude()
            .relative()
            .bg(t.bg_elevated)
            .rounded_lg()
            .border_1()
            .border_color(t.border)
            .shadow_lg()
            .overflow_hidden()
            .flex()
            .flex_col()
            .key_context("FileFinder")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_dismiss))
            .on_action(cx.listener(Self::on_confirm))
            .on_action(cx.listener(Self::on_select_next))
            .on_action(cx.listener(Self::on_select_prev))
            .on_action(cx.listener(Self::on_backspace))
            .on_action(cx.listener(Self::on_move_left))
            .on_action(cx.listener(Self::on_move_right))
            .on_action(cx.listener(Self::on_move_start))
            .on_action(cx.listener(Self::on_move_end))
            .on_action(cx.listener(Self::on_toggle_case))
            .on_action(cx.listener(Self::on_toggle_word))
            .on_action(cx.listener(Self::on_toggle_regex))
            .on_action(cx.listener(Self::on_toggle_ignored))
            .on_action(cx.listener(Self::on_toggle_preview))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(self.render_input_row(&t, window, cx))
            .child(body_container)
            .child(self.render_footer(&t))
            // Animated width — interpolates from_w → final_w on each toggle.
            .with_animation(
                ("ff-modal-w", toggle_count),
                Animation::new(std::time::Duration::from_millis(220)).with_easing(ease_in_out),
                move |el, p| el.w(px(from_w + (final_w - from_w) * p)),
            );

        // ── mask dropdown (outside modal to escape overflow_hidden) ────────────
        let vw = window.viewport_size().width;
        // Modal right edge = vw/2 + final_w/2. Dropdown 200px, 12px inset.
        let dropdown_left = vw / 2.0 + px(final_w / 2.0 - 212.0);
        let dropdown_top = px(BACKDROP_TOP + INPUT_ROW_H);

        // ── drag cursor overlay ────────────────────────────────────────────────
        // While the user drags the divider, an invisible full-screen overlay
        // captures mouse-move and mouse-up events so fast cursor movements don't
        // "escape" the narrow divider hit-area.
        let is_dragging = self.divider_drag.is_some();
        let drag_is_bottom = is_bottom;

        deferred(
            div()
                .id("ff-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_start()
                .justify_center()
                .pt(px(BACKDROP_TOP))
                .bg(gpui::hsla(0., 0., 0., 0.35))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|view, _, window, cx| view.dismiss(window, cx)),
                )
                .child(modal)
                .when(self.mask_open, |el| {
                    el.child(
                        div()
                            .absolute()
                            .left(dropdown_left)
                            .top(dropdown_top)
                            .child(self.render_mask_panel(&t, cx)),
                    )
                })
                .when(is_dragging, |el| {
                    let list_w_min = 180.;
                    let list_w_max = expanded_w - 180. - DIV_PX;
                    let list_h_min = 100.;
                    let list_h_max = expanded_body_h - 100. - DIV_PX;
                    el.child(
                        div()
                            .id("ff-drag-overlay")
                            .absolute()
                            .inset_0()
                            .when(drag_is_bottom, |el| el.cursor_row_resize())
                            .when(!drag_is_bottom, |el| el.cursor_col_resize())
                            .on_mouse_move(cx.listener(
                                move |view, ev: &gpui::MouseMoveEvent, _, cx| {
                                    let Some((start_pos, start_size)) = view.divider_drag else {
                                        return;
                                    };
                                    if drag_is_bottom {
                                        let delta = f32::from(ev.position.y) - start_pos;
                                        view.list_height =
                                            (start_size + delta).clamp(list_h_min, list_h_max);
                                    } else {
                                        let delta = f32::from(ev.position.x) - start_pos;
                                        view.list_width =
                                            (start_size + delta).clamp(list_w_min, list_w_max);
                                    }
                                    cx.notify();
                                },
                            ))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|view, _, _, cx| {
                                    view.divider_drag = None;
                                    cx.notify();
                                }),
                            ),
                    )
                }),
        )
        .with_priority(2)
    }
}
