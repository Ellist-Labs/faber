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

/// Generic sticky ~26px bottom strip with left/right slot rows.
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

    #[allow(dead_code)]
    pub fn push_left(&mut self, item: AnyView) {
        self.left.push(item);
    }

    pub fn push_right(&mut self, item: AnyView) {
        self.right.push(item);
    }
}

impl Render for StatusBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<RuntimeTheme>().clone();

        let left_slot = h_flex().gap_2().children(self.left.iter().cloned());
        let right_slot = h_flex()
            .gap_2()
            .ml_auto()
            .children(self.right.iter().cloned());

        h_flex()
            .id("status-bar")
            .h(px(26.))
            .flex_shrink_0()
            .px_2()
            .bg(t.bg_elevated)
            .border_t_1()
            .border_color(t.separator)
            .text_size(px(t.font_size_caption))
            .font_family(t.ui_family.clone())
            .text_color(t.text_muted)
            .child(left_slot)
            .child(right_slot)
    }
}

// ── IndexingStatusItem ────────────────────────────────────────────────────────

/// Timing constants from the TDD.
/// Show the bar if the run is still active after this delay.
const APPEAR_DELAY_MS: u64 = 800;
/// Also show retroactively if the run completed after at least this long
/// (catches cold/verify scans that finish in 150ms–800ms). File-save
/// rescans complete in <20ms so they stay invisible.
const MIN_RUN_TO_SHOW_MS: u64 = 150;
const MIN_SHOWN_MS: u64 = 1000;
const LABEL_DWELL_MS: u64 = 500;
const POLL_MS: u64 = 100;

pub struct IndexingStatusItem {
    index_status: Entity<IndexStatus>,
    /// When the current run's Begin was received.
    run_started_at: Option<Instant>,
    /// When End was received (used to compute run duration for retroactive show).
    run_ended_at: Option<Instant>,
    /// When the item first became visible (for the 1s minimum shown rule).
    shown_at: Option<Instant>,
    visible: bool,
    /// Last time the displayed label changed.
    label_shown_at: Option<Instant>,
    label_phase: LabelPhase,
    // Keeps the 100ms poll task alive.
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

        // 100ms timer to re-evaluate visibility (appear delay + min dwell).
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
                // Begin may have been overwritten before the poll fired; synthesize it.
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
                // Run still active: show only after appear delay.
                if !self.visible
                    && now.duration_since(started) >= Duration::from_millis(APPEAR_DELAY_MS)
                {
                    self.visible = true;
                    self.shown_at = Some(now);
                }
                return;
            }

            // Run ended: also show retroactively if the run itself took long enough.
            // This catches cold/verify scans that complete in 150ms–800ms.
            // File-save rescans finish in <20ms and stay invisible.
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

        // Hide after minimum dwell.
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
                    .text_size(px(t.font_size_caption))
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

        // Fold across all statuses: Error > Downloading/Starting/Restarting > Running > Stopped
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
                ServerState::Running => {
                    if lsp.error_count > 0 {
                        t.error
                    } else if lsp.warning_count > 0 {
                        t.warning
                    } else {
                        t.success
                    }
                }
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
            .gap(px(3.))
            .px(px(4.))
            .h_full()
            .cursor_pointer()
            .child(
                svg()
                    .path(IconName::Code.path())
                    .size(px(13.))
                    .text_color(dot_color),
            );

        // Show a small download progress bar when a server is being fetched.
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

/// Status bar item showing error/warning counts; tapping opens the Problems tab.
pub struct DiagnosticsStatusItem {
    lsp_status: gpui::Entity<LspStatus>,
    focus_handle: gpui::FocusHandle,
}

impl DiagnosticsStatusItem {
    pub fn new(lsp_status: gpui::Entity<LspStatus>, cx: &mut Context<Self>) -> Self {
        cx.observe(&lsp_status, |_, _, cx| cx.notify()).detach();
        Self {
            lsp_status,
            focus_handle: cx.focus_handle(),
        }
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
        let fh = self.focus_handle.clone();

        div()
            .id("diagnostics-status-item")
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(4.))
            .h_full()
            .cursor_pointer()
            .child(
                div()
                    .text_size(px(t.font_size_caption))
                    .font_family(t.ui_family.clone())
                    .text_color(if error_count > 0 {
                        t.error
                    } else {
                        t.text_subtle
                    })
                    .child(format!("✕ {}", error_count)),
            )
            .child(
                div()
                    .text_size(px(t.font_size_caption))
                    .font_family(t.ui_family.clone())
                    .text_color(if warning_count > 0 {
                        t.warning
                    } else {
                        t.text_subtle
                    })
                    .child(format!("⚠ {}", warning_count)),
            )
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                fh.dispatch_action(&OpenProblems, window, cx);
            })
            .into_any_element()
    }
}

// ── ActiveDocInfo ─────────────────────────────────────────────────────────────

/// Tracks the active document's language; updated by Workspace on tab change / file open.
pub struct ActiveDocInfo {
    pub language: Option<Arc<Language>>,
}

impl ActiveDocInfo {
    pub fn new() -> Self {
        Self { language: None }
    }
}

// ── LanguageStatusItem ────────────────────────────────────────────────────────

pub struct LanguageStatusItem {
    active_doc: Entity<ActiveDocInfo>,
    focus_handle: gpui::FocusHandle,
}

impl LanguageStatusItem {
    pub fn new(
        active_doc: Entity<ActiveDocInfo>,
        _ws: gpui::WeakEntity<crate::workspace::Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&active_doc, |_, _, cx| cx.notify()).detach();
        Self {
            active_doc,
            focus_handle: cx.focus_handle(),
        }
    }
}

impl Render for LanguageStatusItem {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<RuntimeTheme>().clone();
        let lang_name = self
            .active_doc
            .read(cx)
            .language
            .as_ref()
            .map(|l| l.name.clone())
            .unwrap_or_else(|| t!("status_bar.plain_text").to_string());

        let fh = self.focus_handle.clone();
        div()
            .id("language-status-item")
            .flex()
            .items_center()
            .px(px(4.))
            .h_full()
            .cursor_pointer()
            .text_size(px(t.font_size_caption))
            .font_family(t.ui_family.clone())
            .text_color(t.text_muted)
            .child(lang_name)
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                fh.dispatch_action(&OpenLanguagePicker, window, cx);
            })
            .into_any_element()
    }
}
