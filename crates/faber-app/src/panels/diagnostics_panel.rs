use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    App, Context, FocusHandle, Focusable, IntoElement, Render, ScrollHandle, SharedString,
    WeakEntity, Window, div, prelude::*, px, rgba,
};
use rust_i18n::t;

use faber_lsp::diagnostics::{DiagnosticStore, Severity};

use crate::file_icons;
use crate::panels::results_list::{
    FileCounts, Row, render_problems_banner, render_results_list, split_path,
};
use crate::theme::RuntimeTheme;
use crate::workspace::Workspace;

pub struct DiagnosticsPanel {
    pub focus_handle: FocusHandle,
    pub diagnostic_store: Option<Arc<DiagnosticStore>>,
    workspace: Option<WeakEntity<Workspace>>,
    rows: Vec<Row>,
    last_generation: Option<u64>,
    scroll: ScrollHandle,
    /// Total error count across all files (kept in sync with rows).
    total_errors: usize,
    /// Total warning count across all files.
    total_warnings: usize,
}

impl DiagnosticsPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            diagnostic_store: None,
            workspace: None,
            rows: Vec::new(),
            last_generation: None,
            scroll: ScrollHandle::new(),
            total_errors: 0,
            total_warnings: 0,
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
            self.total_errors = 0;
            self.total_warnings = 0;
            return;
        };
        self.rows.clear();
        self.total_errors = 0;
        self.total_warnings = 0;

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
            let _file_icon = file_icons::icon_for_file(filename);

            let error_count = entries
                .iter()
                .filter(|e| e.severity == Severity::Error)
                .count();
            let warning_count = entries
                .iter()
                .filter(|e| e.severity == Severity::Warning)
                .count();

            self.total_errors += error_count;
            self.total_warnings += warning_count;

            self.rows.push(Row::FileHeader {
                filename: SharedString::from(filename.to_owned()),
                dir: SharedString::from(dir.to_owned()),
                file_icon: file_icons::icon_for_file(filename),
                counts: FileCounts::Problems {
                    errors: error_count,
                    warnings: warning_count,
                },
            });
            for entry in entries {
                self.rows.push(Row::DiagEntry {
                    path: file_path.clone(),
                    lsp_line: entry.range.lsp_line,
                    message: SharedString::from(entry.message.clone()),
                    severity: entry.severity,
                    source: SharedString::from(entry.source.as_ref().to_owned()),
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
                .child(render_problems_banner(0, 0))
                .items_center()
                .justify_center()
                .font_family(t.ui_family.clone())
                .text_size(px(t.font_size_body))
                .text_color(t.text_muted)
                .child(t!("dashboard.problems_empty").to_string())
                .into_any_element();
        }

        // Outer container: padding 8px 12px 4px around the file groups
        let list_content = div()
            .flex()
            .flex_col()
            .px(px(12.))
            .pt(px(8.))
            .pb(px(4.))
            .child(render_results_list(
                "diag-rows",
                self.rows.clone(),
                self.scroll.clone(),
                self.workspace.clone(),
            ));

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgba(0x000000FFu32))
            .child(render_problems_banner(
                self.total_errors,
                self.total_warnings,
            ))
            .child(list_content)
            .into_any_element()
    }
}
