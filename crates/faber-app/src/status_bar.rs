use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyView, Context, Entity, IntoElement, MouseButton, Render, Window, div, prelude::*, px, svg,
};
use rust_i18n::t;

use crate::lsp_status::LspStatus;
use crate::theme::RuntimeTheme;
use crate::ui::h_flex;
use crate::workspace::IndexStatus;
use crate::{OpenLanguagePicker, OpenProblems};
use faber_lang::Language;
use faber_lsp::server::ServerState;

// ── StatusBar ─────────────────────────────────────────────────────────────────

pub struct StatusBar {
    left: Vec<AnyView>,
    right: Vec<AnyView>,
}

impl StatusBar {
    pub fn new() -> Self {
        Self {
            left: vec![],
            right: vec![],
        }
    }

    pub fn push_left(&mut self, item: AnyView) {
        self.left.push(item);
    }

    pub fn push_right(&mut self, item: AnyView) {
        self.right.push(item);
    }
}

fn item_sep(t: &RuntimeTheme) -> impl IntoElement {
    div().w(px(1.)).h(px(14.)).flex_shrink_0().bg(t.border)
}

impl Render for StatusBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<RuntimeTheme>().clone();

        // Interleave 1px dividers between items
        let mut left_children: Vec<gpui::AnyElement> = Vec::new();
        for (i, item) in self.left.iter().enumerate() {
            if i > 0 {
                left_children.push(item_sep(&t).into_any_element());
            }
            left_children.push(item.clone().into_any_element());
        }

        let mut right_children: Vec<gpui::AnyElement> = Vec::new();
        for (i, item) in self.right.iter().enumerate() {
            if i > 0 {
                right_children.push(item_sep(&t).into_any_element());
            }
            right_children.push(item.clone().into_any_element());
        }

        let left_slot = h_flex().children(left_children);
        let right_slot = h_flex().ml_auto().children(right_children);

        h_flex()
            .id("status-bar")
            .h(px(24.))
            .flex_shrink_0()
            .px_2()
            .bg(t.bg_elevated)
            .border_t_1()
            .border_color(t.border)
            .text_size(px(11.))
            .font_family(t.ui_family.clone())
            .text_color(t.text_muted)
            .child(left_slot)
            .child(right_slot)
    }
}

// ── Status dot helper ─────────────────────────────────────────────────────────

fn status_dot(color: gpui::Hsla) -> impl IntoElement {
    div().size(px(6.)).rounded_full().flex_shrink_0().bg(color)
}

// ── IndexingStatusItem ────────────────────────────────────────────────────────

const APPEAR_DELAY_MS: u64 = 800;
const MIN_RUN_TO_SHOW_MS: u64 = 150;
const MIN_SHOWN_MS: u64 = 1000;
const LABEL_DWELL_MS: u64 = 500;
const POLL_MS: u64 = 100;

pub struct IndexingStatusItem {
    index_status: Entity<IndexStatus>,
    run_started_at: Option<Instant>,
    run_ended_at: Option<Instant>,
    shown_at: Option<Instant>,
    visible: bool,
    label_shown_at: Option<Instant>,
    label_phase: LabelPhase,
    _poll_task: gpui::Task<()>,
}

#[derive(Clone, PartialEq)]
enum LabelPhase {
    Scanning,
    Indexing { done: usize, total: usize },
}

impl IndexingStatusItem {
    pub fn new(index_status: Entity<IndexStatus>, cx: &mut Context<Self>) -> Self {
        cx.observe(&index_status, |this, _, cx| {
            this.on_status_changed(cx);
        })
        .detach();

        let poll_task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(POLL_MS))
                    .await;
                let keep_going = this
                    .update(cx, |item, cx| {
                        item.tick(cx);
                        true
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        });

        Self {
            index_status,
            run_started_at: None,
            run_ended_at: None,
            shown_at: None,
            visible: false,
            label_shown_at: None,
            label_phase: LabelPhase::Scanning,
            _poll_task: poll_task,
        }
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let before = self.visible;
        self.recompute_visibility();
        if self.visible != before {
            cx.notify();
        }
    }

    fn on_status_changed(&mut self, cx: &mut Context<Self>) {
        use faber_index::progress::{Phase, ProgressEvent};

        let progress = self.index_status.read(cx).current_progress.clone();
        match progress {
            Some(ProgressEvent::Begin) => {
                self.run_started_at = Some(Instant::now());
                self.run_ended_at = None;
            }
            Some(ProgressEvent::Report { phase, done, total }) => {
                if self.run_started_at.is_none() {
                    self.run_started_at = Some(Instant::now());
                    self.run_ended_at = None;
                }
                let new_phase = match phase {
                    Phase::Scanning => LabelPhase::Scanning,
                    Phase::Indexing { .. } | Phase::Publishing => {
                        LabelPhase::Indexing { done, total }
                    }
                };
                let now = Instant::now();
                let dwell_elapsed = self
                    .label_shown_at
                    .map(|t| now.duration_since(t) >= Duration::from_millis(LABEL_DWELL_MS))
                    .unwrap_or(true);
                if dwell_elapsed && new_phase != self.label_phase {
                    self.label_phase = new_phase;
                    self.label_shown_at = Some(now);
                } else if self.label_shown_at.is_none() {
                    self.label_shown_at = Some(now);
                }
            }
            Some(ProgressEvent::End { .. }) => {
                self.run_ended_at = Some(Instant::now());
            }
            None => {}
        }
        self.recompute_visibility();
        cx.notify();
    }

    fn recompute_visibility(&mut self) {
        let now = Instant::now();

        if let Some(started) = self.run_started_at {
            if self.run_ended_at.is_none() {
                if !self.visible
                    && now.duration_since(started) >= Duration::from_millis(APPEAR_DELAY_MS)
                {
                    self.visible = true;
                    self.shown_at = Some(now);
                }
                return;
            }

            if !self.visible {
                let run_duration = self
                    .run_ended_at
                    .and_then(|e| e.checked_duration_since(started))
                    .unwrap_or_default();
                if run_duration >= Duration::from_millis(MIN_RUN_TO_SHOW_MS) {
                    self.visible = true;
                    self.shown_at = Some(now);
                }
            }
        }

        if self.visible {
            let dwell_done = self
                .shown_at
                .map(|t| now.duration_since(t) >= Duration::from_millis(MIN_SHOWN_MS))
                .unwrap_or(true);
            if dwell_done {
                self.visible = false;
                self.shown_at = None;
                self.run_started_at = None;
                self.run_ended_at = None;
            }
        }
    }
}

impl Render for IndexingStatusItem {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.visible {
            return div().into_any_element();
        }

        let t = cx.global::<RuntimeTheme>().clone();

        let label_text: String = match &self.label_phase {
            LabelPhase::Scanning => t!("status_bar.scanning").to_string(),
            LabelPhase::Indexing { done, total } => {
                format!("{} {} / {}", t!("status_bar.indexing"), done, total)
            }
        };

        let fraction: f32 = match &self.label_phase {
            LabelPhase::Scanning => 0.0,
            LabelPhase::Indexing { done, total } => {
                if *total > 0 {
                    (*done as f32 / *total as f32).clamp(0.0, 1.0)
                } else {
                    0.0
                }
            }
        };

        let bar_w = 70_f32;
        let filled_w = (bar_w * fraction).max(4.);
        let progress_bar = div()
            .w(px(bar_w))
            .h(px(3.))
            .rounded_full()
            .bg(t.separator)
            .relative()
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .h_full()
                    .w(px(filled_w))
                    .rounded_full()
                    .bg(t.accent),
            );

        h_flex()
            .gap_2()
            .pr_1()
            .child(progress_bar)
            .child(
                div()
                    .text_size(px(11.))
                    .font_family(t.ui_family.clone())
                    .text_color(t.text_muted)
                    .child(label_text),
            )
            .into_any_element()
    }
}

// ── LspStatusItem ─────────────────────────────────────────────────────────────

pub struct LspStatusItem {
    lsp_status: gpui::Entity<LspStatus>,
    ws: gpui::WeakEntity<crate::workspace::Workspace>,
}

impl LspStatusItem {
    pub fn new(
        lsp_status: gpui::Entity<LspStatus>,
        ws: gpui::WeakEntity<crate::workspace::Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&lsp_status, |_, _, cx| cx.notify()).detach();
        Self { lsp_status, ws }
    }
}

impl Render for LspStatusItem {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::ui::icon::IconName;
        let t = cx.global::<RuntimeTheme>().clone();
        let lsp = self.lsp_status.read(cx);

        let (dot_color, download_fraction) = if lsp.statuses.is_empty() {
            (t.text_subtle, None)
        } else {
            let worst = lsp.statuses.iter().max_by_key(|s| match &s.state {
                ServerState::Error(_) => 4,
                ServerState::Downloading
                | ServerState::Starting
                | ServerState::Initializing
                | ServerState::Restarting { .. } => 3,
                ServerState::Running => 2,
                ServerState::Stopped => 0,
            });
            let Some(status) = worst else {
                return div().into_any_element();
            };

            let dl_fraction = if matches!(status.state, ServerState::Downloading) {
                status.download_fraction
            } else {
                None
            };

            let color = match &status.state {
                ServerState::Running => t.success,
                ServerState::Downloading
                | ServerState::Starting
                | ServerState::Initializing
                | ServerState::Restarting { .. } => t.warning,
                ServerState::Error(_) => t.error,
                ServerState::Stopped => t.text_subtle,
            };
            (color, dl_fraction)
        };

        let ws = self.ws.clone();
        let mut row = div()
            .id("lsp-status-item")
            .flex()
            .items_center()
            .gap(px(4.))
            .px(px(7.))
            .py(px(2.))
            .rounded(px(6.))
            .h_full()
            .cursor_pointer()
            .hover(|s| s.bg(t.bg_raised))
            .child(status_dot(dot_color))
            .child(
                svg()
                    .path(IconName::Code.path())
                    .size(px(11.))
                    .text_color(dot_color),
            );

        if let Some(fraction) = download_fraction {
            let bar_w = 50_f32;
            let filled_w = (bar_w * fraction).max(3.);
            let progress_bar = div()
                .w(px(bar_w))
                .h(px(3.))
                .rounded_full()
                .bg(t.separator)
                .relative()
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .h_full()
                        .w(px(filled_w))
                        .rounded_full()
                        .bg(t.accent),
                );
            row = row.child(progress_bar);
        }

        row.on_mouse_down(MouseButton::Left, move |ev, _window, cx| {
            if let Some(ws) = ws.upgrade() {
                let pos = ev.position;
                ws.update(cx, |ws, cx| {
                    ws.lsp_overlay_open = !ws.lsp_overlay_open;
                    ws.lsp_overlay_pos = pos;
                    cx.notify();
                });
            }
        })
        .into_any_element()
    }
}

// ── DiagnosticsStatusItem ─────────────────────────────────────────────────────

pub struct DiagnosticsStatusItem {
    lsp_status: gpui::Entity<LspStatus>,
}

impl DiagnosticsStatusItem {
    pub fn new(lsp_status: gpui::Entity<LspStatus>, cx: &mut Context<Self>) -> Self {
        cx.observe(&lsp_status, |_, _, cx| cx.notify()).detach();
        Self { lsp_status }
    }
}

impl Render for DiagnosticsStatusItem {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<RuntimeTheme>().clone();
        let lsp = self.lsp_status.read(cx);

        let has_running = lsp
            .statuses
            .iter()
            .any(|s| matches!(s.state, ServerState::Running));
        if !has_running {
            return div().into_any_element();
        }

        let error_count = lsp.error_count;
        let warning_count = lsp.warning_count;

        h_flex()
            .id("diagnostics-status-item")
            .gap(px(4.))
            .h_full()
            .cursor_pointer()
            .child(
                // Errors
                h_flex()
                    .gap(px(4.))
                    .px(px(7.))
                    .py(px(2.))
                    .rounded(px(6.))
                    .hover(|s| s.bg(t.bg_raised))
                    .child(status_dot(t.error))
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_family(t.ui_family.clone())
                            .text_color(if error_count > 0 {
                                t.error
                            } else {
                                t.text_subtle
                            })
                            .child(format!("{}", error_count)),
                    ),
            )
            .child(
                // Warnings
                h_flex()
                    .gap(px(4.))
                    .px(px(7.))
                    .py(px(2.))
                    .rounded(px(6.))
                    .hover(|s| s.bg(t.bg_raised))
                    .child(status_dot(t.warning))
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_family(t.ui_family.clone())
                            .text_color(if warning_count > 0 {
                                t.warning
                            } else {
                                t.text_subtle
                            })
                            .child(format!("{}", warning_count)),
                    ),
            )
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                log::debug!("status_bar: diagnostics item clicked → OpenProblems");
                window.dispatch_action(Box::new(OpenProblems), cx);
            })
            .into_any_element()
    }
}

// ── ActiveDocInfo ─────────────────────────────────────────────────────────────

pub struct ActiveDocInfo {
    pub language: Option<Arc<Language>>,
    /// (line 1-based, col 1-based) of the primary caret; None when no editor active.
    pub cursor: Option<(usize, usize)>,
    pub has_editor: bool,
}

impl ActiveDocInfo {
    pub fn new() -> Self {
        Self {
            language: None,
            cursor: None,
            has_editor: false,
        }
    }
}

// ── LanguageStatusItem ────────────────────────────────────────────────────────

pub struct LanguageStatusItem {
    active_doc: Entity<ActiveDocInfo>,
}

impl LanguageStatusItem {
    pub fn new(
        active_doc: Entity<ActiveDocInfo>,
        _ws: gpui::WeakEntity<crate::workspace::Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&active_doc, |_, _, cx| cx.notify()).detach();
        Self { active_doc }
    }
}

impl Render for LanguageStatusItem {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<RuntimeTheme>().clone();
        let info = self.active_doc.read(cx);
        let lang_name = info
            .language
            .as_ref()
            .map(|l| l.name.clone())
            .unwrap_or_else(|| t!("status_bar.plain_text").to_string());

        let dot_color = t.accent;

        div()
            .id("language-status-item")
            .flex()
            .items_center()
            .gap(px(4.))
            .px(px(7.))
            .py(px(2.))
            .rounded(px(6.))
            .h_full()
            .cursor_pointer()
            .text_size(px(11.))
            .font_family(t.ui_family.clone())
            .text_color(t.text_muted)
            .hover(|s| s.bg(t.bg_raised).text_color(t.text))
            .child(status_dot(dot_color))
            .child(lang_name)
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                window.dispatch_action(Box::new(OpenLanguagePicker), cx);
            })
            .into_any_element()
    }
}

// ── CursorStatusItem ─────────────────────────────────────────────────────────

pub struct CursorStatusItem {
    active_doc: Entity<ActiveDocInfo>,
}

impl CursorStatusItem {
    pub fn new(active_doc: Entity<ActiveDocInfo>, cx: &mut Context<Self>) -> Self {
        cx.observe(&active_doc, |_, _, cx| cx.notify()).detach();
        Self { active_doc }
    }
}

impl Render for CursorStatusItem {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<RuntimeTheme>().clone();
        let info = self.active_doc.read(cx);

        let Some((line, col)) = info.cursor else {
            return div().into_any_element();
        };

        if !info.has_editor {
            return div().into_any_element();
        }

        div()
            .id("cursor-status-item")
            .flex()
            .items_center()
            .px(px(7.))
            .py(px(2.))
            .rounded(px(6.))
            .h_full()
            .text_size(px(11.))
            .font_family(t.ui_family.clone())
            .text_color(t.text_muted)
            .hover(|s| s.bg(t.bg_raised))
            .child(t!("status_bar.ln_col", line = line, col = col).to_string())
            .into_any_element()
    }
}

// ── EncodingStatusItem ────────────────────────────────────────────────────────

pub struct EncodingStatusItem;

impl Render for EncodingStatusItem {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<RuntimeTheme>().clone();

        div()
            .id("encoding-status-item")
            .flex()
            .items_center()
            .px(px(7.))
            .py(px(2.))
            .rounded(px(6.))
            .h_full()
            .text_size(px(11.))
            .font_family(t.ui_family.clone())
            .text_color(t.text_muted)
            .hover(|s| s.bg(t.bg_raised))
            .child(t!("status_bar.encoding").to_string())
            .into_any_element()
    }
}
