use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    App, Context, FocusHandle, Focusable, IntoElement, MouseButton, Render, SharedString,
    UniformListScrollHandle, WeakEntity, Window, div, prelude::*, px, uniform_list,
};
use rust_i18n::t;

use faber_lsp::diagnostics::{DiagnosticStore, Severity};

use crate::theme::RuntimeTheme;
use crate::workspace::Workspace;

#[derive(Clone)]
enum Row {
    FileHeader {
        path: SharedString,
        error_count: usize,
        warning_count: usize,
    },
    Entry {
        message: SharedString,
        file_path: Arc<PathBuf>,
        lsp_line: u32,
        severity: Severity,
    },
}

pub struct DiagnosticsPanel {
    pub focus_handle: FocusHandle,
    pub diagnostic_store: Option<Arc<DiagnosticStore>>,
    workspace: Option<WeakEntity<Workspace>>,
    rows: Vec<Row>,
    last_generation: Option<u64>,
    scroll: UniformListScrollHandle,
}

impl DiagnosticsPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            diagnostic_store: None,
            workspace: None,
            rows: Vec::new(),
            last_generation: None,
            scroll: UniformListScrollHandle::new(),
        }
    }

    pub fn set_store(&mut self, store: Arc<DiagnosticStore>, workspace: WeakEntity<Workspace>) {
        self.diagnostic_store = Some(store);
        self.workspace = Some(workspace);
        self.last_generation = None;
    }

    fn rebuild_rows(&mut self, root: Option<&Path>) {
        let Some(store) = &self.diagnostic_store else {
            self.rows.clear();
            return;
        };
        self.rows.clear();
        for (uri, entries) in store.get_all() {
            let file_path = Arc::new(
                uri.to_file_path()
                    .unwrap_or_else(|_| PathBuf::from(uri.path())),
            );
            let display = match root {
                Some(r) => file_path
                    .strip_prefix(r)
                    .unwrap_or(&file_path)
                    .to_string_lossy()
                    .into_owned(),
                None => file_path.to_string_lossy().into_owned(),
            };
            let error_count = entries
                .iter()
                .filter(|e| e.severity == Severity::Error)
                .count();
            let warning_count = entries
                .iter()
                .filter(|e| e.severity == Severity::Warning)
                .count();
            self.rows.push(Row::FileHeader {
                path: SharedString::from(display),
                error_count,
                warning_count,
            });
            for entry in entries {
                self.rows.push(Row::Entry {
                    message: SharedString::from(entry.message.clone()),
                    file_path: file_path.clone(),
                    lsp_line: entry.range.lsp_line,
                    severity: entry.severity,
                });
            }
        }
        self.last_generation = Some(
            self.diagnostic_store
                .as_ref()
                .map(|s| s.generation())
                .unwrap_or(0),
        );
    }
}

impl Focusable for DiagnosticsPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DiagnosticsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<RuntimeTheme>().clone();
        let root = cx
            .try_global::<crate::ProjectRoot>()
            .and_then(|r| r.0.clone());

        let current_gen = self
            .diagnostic_store
            .as_ref()
            .map(|s| s.generation())
            .unwrap_or(0);
        if Some(current_gen) != self.last_generation {
            self.rebuild_rows(root.as_deref());
        }

        if self.rows.is_empty() {
            return div()
                .flex()
                .flex_col()
                .size_full()
                .items_center()
                .justify_center()
                .font_family(t.ui_family.clone())
                .text_size(px(t.font_size_body))
                .text_color(t.text_muted)
                .child(t!("dashboard.problems_empty").to_string())
                .into_any_element();
        }

        let rows = self.rows.clone();
        let count = rows.len();
        let workspace = self.workspace.clone();

        uniform_list("diag-rows", count, move |range, _window, _cx| {
            let t2 = _cx.global::<RuntimeTheme>().clone();
            range
                .map(|i| match &rows[i] {
                    Row::FileHeader {
                        path,
                        error_count,
                        warning_count,
                    } => {
                        let label = match (*error_count, *warning_count) {
                            (0, 0) => path.to_string(),
                            (e, 0) => format!("{path}  {e}E"),
                            (0, w) => format!("{path}  {w}W"),
                            (e, w) => format!("{path}  {e}E {w}W"),
                        };
                        div()
                            .h(px(24.))
                            .w_full()
                            .flex()
                            .items_center()
                            .px(px(12.))
                            .font_family(t2.ui_family.clone())
                            .text_size(px(t2.font_size_caption))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(t2.text_muted)
                            .child(label)
                            .into_any_element()
                    }
                    Row::Entry {
                        message,
                        file_path,
                        lsp_line,
                        severity,
                    } => {
                        let color = match severity {
                            Severity::Error => t2.error,
                            Severity::Warning => t2.warning,
                            Severity::Information => t2.info,
                            Severity::Hint => t2.text_subtle,
                        };
                        let badge = match severity {
                            Severity::Error => "E",
                            Severity::Warning => "W",
                            Severity::Information => "I",
                            Severity::Hint => "H",
                        };
                        let line_label = format!(":{}", lsp_line + 1);
                        let msg = message.clone();
                        let path = file_path.clone();
                        let line = *lsp_line as usize;
                        let ws = workspace.clone();
                        div()
                            .id(("diag-entry", i))
                            .h(px(22.))
                            .w_full()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .px(px(24.))
                            .cursor_pointer()
                            .hover(|s| s.bg(t2.bg_elevated))
                            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                                if let Some(ws) = ws.as_ref().and_then(|w| w.upgrade()) {
                                    ws.update(cx, |workspace, cx| {
                                        workspace.navigate_to(&path, line, 0, window, cx);
                                    });
                                }
                            })
                            .child(
                                div()
                                    .font_family(t2.ui_family.clone())
                                    .text_size(px(t2.font_size_caption))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(color)
                                    .flex_shrink_0()
                                    .child(badge),
                            )
                            .child(
                                div()
                                    .font_family(t2.ui_family.clone())
                                    .text_size(px(t2.font_size_body))
                                    .text_color(t2.text)
                                    .flex_1()
                                    .overflow_hidden()
                                    .child(msg.to_string()),
                            )
                            .child(
                                div()
                                    .font_family(t2.mono_family.clone())
                                    .text_size(px(t2.font_size_caption))
                                    .text_color(t2.text_subtle)
                                    .flex_shrink_0()
                                    .child(line_label),
                            )
                            .into_any_element()
                    }
                })
                .collect()
        })
        .flex_1()
        .track_scroll(self.scroll.clone())
        .into_any_element()
    }
}
