use std::path::PathBuf;
use std::sync::Arc;

use gpui::{AnyElement, IntoElement, MouseButton, SharedString, UniformListScrollHandle, WeakEntity, div, img, prelude::*, px, uniform_list};
use faber_lsp::diagnostics::Severity;

use crate::theme::RuntimeTheme;
use crate::ui::h_flex;
use crate::workspace::Workspace;

#[derive(Clone)]
pub enum FileCounts {
    Refs(usize),
    Problems { errors: usize, warnings: usize },
}

#[derive(Clone)]
pub enum Row {
    FileHeader {
        filename: SharedString,
        dir: SharedString,
        file_icon: &'static str,
        counts: FileCounts,
    },
    RefEntry {
        path: Arc<PathBuf>,
        line: usize,
        col: usize,
        preview: SharedString,
    },
    DiagEntry {
        path: Arc<PathBuf>,
        lsp_line: u32,
        message: SharedString,
        severity: Severity,
    },
}

/// Splits a relative file path string into (filename, dir).
/// e.g. "src/foo/bar.rs" → ("bar.rs", "src/foo/")
pub fn split_path(rel: &str) -> (&str, &str) {
    match rel.rfind('/') {
        Some(slash) => (&rel[slash + 1..], &rel[..=slash]),
        None => (rel, ""),
    }
}

pub fn render_results_list(
    list_id: &'static str,
    rows: Vec<Row>,
    scroll: UniformListScrollHandle,
    workspace: Option<WeakEntity<Workspace>>,
) -> AnyElement {
    let count = rows.len();
    div()
        .flex()
        .flex_col()
        .size_full()
        .child(
            uniform_list(list_id, count, move |range, _window, cx| {
                let t = cx.global::<RuntimeTheme>().clone();
                range
                    .map(|i| render_row(&rows[i], &workspace, i, &t))
                    .collect()
            })
            .flex_1()
            .track_scroll(scroll),
        )
        .into_any_element()
}

fn render_row(
    row: &Row,
    workspace: &Option<WeakEntity<Workspace>>,
    i: usize,
    t: &RuntimeTheme,
) -> AnyElement {
    match row {
        Row::FileHeader { filename, dir, file_icon, counts } => {
            render_file_header(filename, dir, file_icon, counts, i, t)
        }
        Row::RefEntry { path, line, col, preview } => {
            render_ref_entry(path, *line, *col, preview, workspace, i, t)
        }
        Row::DiagEntry { path, lsp_line, message, severity } => {
            render_diag_entry(path, *lsp_line, message, *severity, workspace, i, t)
        }
    }
}

fn render_file_header(
    filename: &SharedString,
    dir: &SharedString,
    file_icon: &'static str,
    counts: &FileCounts,
    i: usize,
    t: &RuntimeTheme,
) -> AnyElement {
    let hover_bg = t.line_highlight;
    let count_el: AnyElement = match counts {
        FileCounts::Refs(n) => div()
            .font_family(t.ui_family.clone())
            .text_size(px(t.font_size_caption))
            .text_color(t.text_subtle)
            .flex_shrink_0()
            .child(format!("{n}"))
            .into_any_element(),
        FileCounts::Problems { errors, warnings } => h_flex()
            .gap_1()
            .flex_shrink_0()
            .font_family(t.ui_family.clone())
            .text_size(px(t.font_size_caption))
            .when(*errors > 0, {
                let e = *errors;
                let color = t.error;
                move |el| {
                    el.child(div().text_color(color).child(format!("{e}E")))
                }
            })
            .when(*warnings > 0, {
                let w = *warnings;
                let color = t.warning;
                move |el| {
                    el.child(div().text_color(color).child(format!("{w}W")))
                }
            })
            .into_any_element(),
    };

    div()
        .id(("results-header", i))
        .h(px(28.))
        .w_full()
        .flex()
        .items_center()
        .px_3()
        .gap_2()
        .font_family(t.ui_family.clone())
        .text_size(px(t.font_size_caption))
        .hover(move |s| s.bg(hover_bg))
        .child(img(file_icon).size(px(14.)).flex_shrink_0())
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(t.text)
                .flex_shrink_0()
                .child(filename.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .overflow_hidden()
                .text_ellipsis()
                .text_color(t.text_muted)
                .text_size(px(t.font_size_caption - 1.))
                .child(dir.to_string()),
        )
        .child(count_el)
        .into_any_element()
}

fn render_ref_entry(
    path: &Arc<PathBuf>,
    line: usize,
    col: usize,
    preview: &SharedString,
    workspace: &Option<WeakEntity<Workspace>>,
    i: usize,
    t: &RuntimeTheme,
) -> AnyElement {
    let hover_bg = t.line_highlight;
    let line_label = format!(":{}", line + 1);
    let preview_text = preview.clone();
    let p = path.clone();
    let ws = workspace.clone();

    div()
        .id(("ref-entry", i))
        .h(px(24.))
        .w_full()
        .flex()
        .items_center()
        .gap(px(6.))
        .pl(px(28.))
        .pr_3()
        .cursor_pointer()
        .hover(move |s| s.bg(hover_bg))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            if let Some(ws) = ws.as_ref().and_then(|w| w.upgrade()) {
                ws.update(cx, |workspace, cx| {
                    workspace.navigate_to(&p, line, col, window, cx);
                });
            }
        })
        .child(
            div()
                .font_family(t.mono_family.clone())
                .text_size(px(t.font_size_code))
                .text_color(t.text)
                .flex_1()
                .overflow_hidden()
                .text_ellipsis()
                .child(preview_text.to_string()),
        )
        .child(
            div()
                .font_family(t.mono_family.clone())
                .text_size(px(t.font_size_caption))
                .text_color(t.text_subtle)
                .flex_shrink_0()
                .child(line_label),
        )
        .into_any_element()
}

fn render_diag_entry(
    path: &Arc<PathBuf>,
    lsp_line: u32,
    message: &SharedString,
    severity: Severity,
    workspace: &Option<WeakEntity<Workspace>>,
    i: usize,
    t: &RuntimeTheme,
) -> AnyElement {
    let hover_bg = t.line_highlight;
    let severity_color = match severity {
        Severity::Error => t.error,
        Severity::Warning => t.warning,
        Severity::Information => t.info,
        Severity::Hint => t.text_subtle,
    };
    let badge = match severity {
        Severity::Error => "E",
        Severity::Warning => "W",
        Severity::Information => "I",
        Severity::Hint => "H",
    };
    let line_label = format!(":{}", lsp_line + 1);
    let msg = message.clone();
    let p = path.clone();
    let line = lsp_line as usize;
    let ws = workspace.clone();

    div()
        .id(("diag-entry", i))
        .h(px(24.))
        .w_full()
        .flex()
        .items_center()
        .gap(px(8.))
        .cursor_pointer()
        .hover(move |s| s.bg(hover_bg))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            if let Some(ws) = ws.as_ref().and_then(|w| w.upgrade()) {
                ws.update(cx, |workspace, cx| {
                    workspace.navigate_to(&p, line, 0, window, cx);
                });
            }
        })
        // Zed-style severity stripe: 3px colored left border
        .child(div().w(px(3.)).h_full().bg(severity_color).flex_shrink_0())
        // Severity badge
        .child(
            div()
                .font_family(t.ui_family.clone())
                .text_size(px(t.font_size_caption))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(severity_color)
                .flex_shrink_0()
                .child(badge),
        )
        // Message
        .child(
            div()
                .font_family(t.ui_family.clone())
                .text_size(px(t.font_size_body))
                .text_color(t.text)
                .flex_1()
                .overflow_hidden()
                .text_ellipsis()
                .child(msg.to_string()),
        )
        // Line number
        .child(
            div()
                .font_family(t.mono_family.clone())
                .text_size(px(t.font_size_caption))
                .text_color(t.text_subtle)
                .flex_shrink_0()
                .pr_3()
                .child(line_label),
        )
        .into_any_element()
}
