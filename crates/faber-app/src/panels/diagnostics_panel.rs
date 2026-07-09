use std::sync::Arc;

use gpui::{
    App, Context, FocusHandle, Focusable, IntoElement, Render, Window, div, prelude::*, px,
};
use rust_i18n::t;

use faber_lsp::diagnostics::{DiagnosticEntry, DiagnosticStore, Severity};

use crate::theme::RuntimeTheme;

pub struct DiagnosticsPanel {
    pub focus_handle: FocusHandle,
    pub diagnostic_store: Option<Arc<DiagnosticStore>>,
}

#[allow(dead_code)]
impl DiagnosticsPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            diagnostic_store: None,
        }
    }

    pub fn set_store(&mut self, store: Arc<DiagnosticStore>) {
        self.diagnostic_store = Some(store);
    }

    fn all_entries(&self) -> Vec<DiagnosticEntry> {
        self.diagnostic_store
            .as_ref()
            .map(|s| {
                // Collect all entries across all URIs by iterating counts
                // For v1: just show aggregated counts
                let _ = s;
                vec![]
            })
            .unwrap_or_default()
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
        let (errors, warnings, total) = self
            .diagnostic_store
            .as_ref()
            .map(|store| {
                let e = store.count_by_severity(Severity::Error);
                let w = store.count_by_severity(Severity::Warning);
                (e, w, store.total_count())
            })
            .unwrap_or((0, 0, 0));

        div()
            .flex()
            .flex_col()
            .size_full()
            .p(px(16.))
            .font_family(t.ui_family.clone())
            .text_size(px(t.font_size_body))
            .text_color(t.text)
            .when(total == 0, |el| {
                el.child(
                    div()
                        .text_color(t.text_muted)
                        .child(t!("dashboard.problems_empty").to_string()),
                )
            })
            .when(total > 0, |el| {
                el.child(div().child(format!(
                    "{} {}, {} {}",
                    errors,
                    t!("status_bar.lsp_errors"),
                    warnings,
                    t!("status_bar.lsp_warnings")
                )))
            })
    }
}
