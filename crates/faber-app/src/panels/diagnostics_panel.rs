use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{App, Context, FocusHandle, Focusable, IntoElement, Render, SharedString, UniformListScrollHandle, WeakEntity, Window, div, prelude::*, px};
use rust_i18n::t;

use faber_lsp::diagnostics::{DiagnosticStore, Severity};

use crate::file_icons;
use crate::panels::results_list::{FileCounts, Row, render_results_list, split_path};
use crate::theme::RuntimeTheme;
use crate::workspace::Workspace;

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
            let rel = match root {
                Some(r) => file_path
                    .strip_prefix(r)
                    .unwrap_or(&file_path)
                    .to_string_lossy()
                    .into_owned(),
                None => file_path.to_string_lossy().into_owned(),
            };
            let rel_fwd = rel.replace('\\', "/");
            let (filename, dir) = split_path(&rel_fwd);
            let file_icon = file_icons::icon_for_file(filename);

            let error_count = entries.iter().filter(|e| e.severity == Severity::Error).count();
            let warning_count = entries.iter().filter(|e| e.severity == Severity::Warning).count();

            self.rows.push(Row::FileHeader {
                filename: SharedString::from(filename.to_owned()),
                dir: SharedString::from(dir.to_owned()),
                file_icon,
                counts: FileCounts::Problems { errors: error_count, warnings: warning_count },
            });
            for entry in entries {
                self.rows.push(Row::DiagEntry {
                    path: file_path.clone(),
                    lsp_line: entry.range.lsp_line,
                    message: SharedString::from(entry.message.clone()),
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

        render_results_list(
            "diag-rows",
            self.rows.clone(),
            self.scroll.clone(),
            self.workspace.clone(),
        )
    }
}
