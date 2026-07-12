use std::path::PathBuf;
use std::sync::Arc;

use faber_lsp::diagnostics::Severity;
use gpui::{
    AnyElement, FontWeight, IntoElement, MouseButton, Rgba, ScrollHandle, SharedString, WeakEntity,
    div, img, prelude::*, px, rgba,
};
use rust_i18n::t;

use crate::file_icons;
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
        /// Diagnostic source string (e.g. "rustc", "clippy"). May be empty.
        source: SharedString,
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

/// Scrollable column list — replaces the old uniform_list-based renderer.
/// Uses a plain div with overflow_y_scroll; suitable for bounded lists (hundreds of rows max).
pub fn render_results_list(
    list_id: &'static str,
    rows: Vec<Row>,
    scroll: ScrollHandle,
    workspace: Option<WeakEntity<Workspace>>,
) -> AnyElement {
    let children: Vec<AnyElement> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| render_row(row, &workspace, i))
        .collect();

    div()
        .id(list_id)
        .flex()
        .flex_col()
        .size_full()
        .overflow_y_scroll()
        .track_scroll(&scroll)
        .children(children)
        .into_any_element()
}

fn render_row(row: &Row, workspace: &Option<WeakEntity<Workspace>>, i: usize) -> AnyElement {
    match row {
        Row::FileHeader {
            filename,
            dir,
            file_icon,
            counts,
        } => render_file_header(filename, dir, *file_icon, counts, i),
        Row::RefEntry {
            path,
            line,
            col,
            preview,
        } => render_ref_entry(path, *line, *col, preview, workspace, i),
        Row::DiagEntry {
            path,
            lsp_line,
            message,
            severity,
            source,
        } => render_diag_entry(path, *lsp_line, message, *severity, source, workspace, i),
    }
}

fn render_file_header(
    filename: &SharedString,
    dir: &SharedString,
    _file_icon: &'static str,
    counts: &FileCounts,
    i: usize,
) -> AnyElement {
    // 7px language dot — color from file extension, fallback to text_muted rgba
    let dot_color: Rgba = file_icons::language_dot_color(filename.as_ref())
        .map(gpui::rgba)
        .unwrap_or_else(|| rgba(0x888888FFu32));

    let dot = div()
        .w(px(7.))
        .h(px(7.))
        .rounded_full()
        .bg(dot_color)
        .flex_shrink_0();

    // Count badge — bg_raised, border-radius 5, px 6 py 1, 10px
    let badge_el: AnyElement = match counts {
        FileCounts::Refs(n) => {
            let n = *n;
            div()
                .id(("hdr-badge", i))
                .px(px(6.))
                .py(px(1.))
                .rounded(px(5.))
                .bg(rgba(0x1A1A1AFFu32))
                .text_size(px(10.))
                .font_weight(FontWeight::NORMAL)
                .child(format!("{n}"))
                .into_any_element()
        }
        FileCounts::Problems { errors, warnings } => {
            let e = *errors;
            let w = *warnings;
            h_flex()
                .gap(px(4.))
                .flex_shrink_0()
                .when(e > 0, move |el| {
                    el.child(
                        div()
                            .px(px(6.))
                            .py(px(1.))
                            .rounded(px(5.))
                            .bg(rgba(0x1A1A1AFFu32))
                            .text_size(px(10.))
                            .text_color(rgba(0xFF453AFFu32))
                            .child(format!("{e}")),
                    )
                })
                .when(w > 0, move |el| {
                    el.child(
                        div()
                            .px(px(6.))
                            .py(px(1.))
                            .rounded(px(5.))
                            .bg(rgba(0x1A1A1AFFu32))
                            .text_size(px(10.))
                            .text_color(rgba(0xFF9F0AFFu32))
                            .child(format!("{w}")),
                    )
                })
                .into_any_element()
        }
    };

    // File header: padding 7px 8px, rounded 7, 12px weight 500 text_muted
    div()
        .id(("results-header", i))
        .w_full()
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(8.))
        .py(px(7.))
        .rounded(px(7.))
        .text_size(px(12.))
        .font_weight(FontWeight(500.0))
        .text_color(rgba(0x888888FFu32))
        .child(dot)
        .child(div().flex_shrink_0().child(filename.to_string()))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .overflow_hidden()
                .text_ellipsis()
                .text_size(px(11.))
                .text_color(rgba(0x555555FFu32))
                .child(dir.to_string()),
        )
        .child(badge_el)
        .into_any_element()
}

fn render_ref_entry(
    path: &Arc<PathBuf>,
    line: usize,
    col: usize,
    preview: &SharedString,
    workspace: &Option<WeakEntity<Workspace>>,
    i: usize,
) -> AnyElement {
    // location label: line:col right-aligned, 11px mono text_subtle
    let loc_label = format!("{}:{}", line + 1, col + 1);
    let preview_text = preview.clone();
    let p = path.clone();
    let ws = workspace.clone();

    // 16px file icon using existing icon_for_file
    let file_icon = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(file_icons::icon_for_file)
        .unwrap_or("");

    div()
        .id(("ref-entry", i))
        .w_full()
        .flex()
        .items_center()
        .gap(px(6.))
        .pl(px(24.))
        .pr(px(8.))
        .py(px(7.))
        .rounded(px(8.))
        .cursor_pointer()
        .hover(|s| s.bg(rgba(0xFFFFFF0Du32)))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            if let Some(ws) = ws.as_ref().and_then(|w| w.upgrade()) {
                ws.update(cx, |workspace, cx| {
                    workspace.navigate_to(&p, line, col, window, cx);
                });
            }
        })
        .child(img(file_icon).size(px(16.)).flex_shrink_0())
        .child(
            div()
                .font_family(gpui::SharedString::from("monospace"))
                .text_size(px(12.))
                .text_color(rgba(0xFFFFFFFFu32))
                .flex_1()
                .overflow_hidden()
                .text_ellipsis()
                .child(preview_text.to_string()),
        )
        .child(
            div()
                .font_family(gpui::SharedString::from("monospace"))
                .text_size(px(11.))
                .text_color(rgba(0x555555FFu32))
                .flex_shrink_0()
                .child(loc_label),
        )
        .into_any_element()
}

fn render_diag_entry(
    path: &Arc<PathBuf>,
    lsp_line: u32,
    message: &SharedString,
    severity: Severity,
    source: &SharedString,
    workspace: &Option<WeakEntity<Workspace>>,
    i: usize,
) -> AnyElement {
    // Severity pill styling per §5.8
    let (pill_text, pill_bg, pill_fg) = match severity {
        Severity::Error => ("ERROR", rgba(0xFF453A24u32), rgba(0xFF453AFFu32)),
        Severity::Warning => ("WARNING", rgba(0xFF9F0A21u32), rgba(0xFF9F0AFFu32)),
        Severity::Information => ("INFO", rgba(0x5E5CE629u32), rgba(0x5E5CE6FFu32)),
        Severity::Hint => ("HINT", rgba(0x55555529u32), rgba(0x555555FFu32)),
    };

    // Location line: "line {n}" · {source} (when source non-empty)
    let line_num = lsp_line + 1;
    let loc_str = if source.is_empty() {
        t!("dashboard.location_line", line = line_num).to_string()
    } else {
        format!(
            "{} \u{00B7} {}",
            t!("dashboard.location_line", line = line_num),
            source
        )
    };

    let msg = message.clone();
    let p = path.clone();
    let line = lsp_line as usize;
    let ws = workspace.clone();

    div()
        .id(("diag-entry", i))
        .w_full()
        .flex()
        .gap(px(8.))
        .pl(px(24.))
        .pr(px(8.))
        .py(px(7.))
        .rounded(px(8.))
        .cursor_pointer()
        .hover(|s| s.bg(rgba(0xFFFFFF0Du32)))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            if let Some(ws) = ws.as_ref().and_then(|w| w.upgrade()) {
                ws.update(cx, |workspace, cx| {
                    workspace.navigate_to(&p, line, 0, window, cx);
                });
            }
        })
        // Severity pill (top-left, aligned to first text line)
        .child(
            div()
                .flex_shrink_0()
                .mt(px(2.))
                .px(px(6.))
                .py(px(2.))
                .rounded(px(5.))
                .bg(pill_bg)
                .text_color(pill_fg)
                .text_size(px(9.5))
                .font_weight(FontWeight::BOLD)
                .child(pill_text),
        )
        // Text column: message + location
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .flex_1()
                .min_w(px(0.))
                // Message: 12.5px text
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(rgba(0xFFFFFFFFu32))
                        .child(msg.to_string()),
                )
                // Location: 11px mono text_subtle
                .child(
                    div()
                        .font_family(gpui::SharedString::from("monospace"))
                        .text_size(px(11.))
                        .text_color(rgba(0x555555FFu32))
                        .child(loc_str),
                ),
        )
        .into_any_element()
}

/// Renders the Problems banner (§5.8): title + error/warning pills.
/// `error_count` and `warning_count` are totals across all files.
pub fn render_problems_banner(error_count: usize, warning_count: usize) -> AnyElement {
    // Banner: padding 16px 20px 13px, border_b
    div()
        .w_full()
        .flex()
        .items_center()
        .gap(px(10.))
        .pt(px(16.))
        .pb(px(13.))
        .px(px(20.))
        .border_b_1()
        .border_color(rgba(0xFFFFFF12u32))
        // Title
        .child(
            div()
                .text_size(px(13.))
                .font_weight(FontWeight(600.0))
                .text_color(rgba(0xFFFFFFFFu32))
                .flex_shrink_0()
                .child(t!("dashboard.problems_title").to_string()),
        )
        // Error pill (hidden when count is 0)
        .when(error_count > 0, move |el| {
            el.child(render_summary_pill(
                error_count,
                rgba(0xFF453A1Fu32),
                rgba(0xFF453AFFu32),
            ))
        })
        // Warning pill (hidden when count is 0)
        .when(warning_count > 0, move |el| {
            el.child(render_summary_pill(
                warning_count,
                rgba(0xFF9F0A1Au32),
                rgba(0xFF9F0AFFu32),
            ))
        })
        .into_any_element()
}

/// Renders the References banner (§5.9): title + result count.
pub fn render_references_banner(result_count: usize) -> AnyElement {
    div()
        .w_full()
        .flex()
        .items_center()
        .gap(px(8.))
        .pt(px(16.))
        .pb(px(13.))
        .px(px(20.))
        .border_b_1()
        .border_color(rgba(0xFFFFFF12u32))
        .child(
            div()
                .text_size(px(13.))
                .font_weight(FontWeight(600.0))
                .text_color(rgba(0xFFFFFFFFu32))
                .flex_shrink_0()
                .child(t!("tab.references").to_string()),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(rgba(0x888888FFu32))
                .child(t!("references.results", n = result_count).to_string()),
        )
        .into_any_element()
}

/// A colored summary pill: 5px dot + count text.
/// Pill: rounded 20px, px 9 py 3, 11px weight 500.
fn render_summary_pill(count: usize, bg_color: Rgba, text_color: Rgba) -> AnyElement {
    h_flex()
        .rounded(px(20.))
        .px(px(9.))
        .py(px(3.))
        .bg(bg_color)
        .gap(px(5.))
        .items_center()
        .flex_shrink_0()
        // 5px colored dot
        .child(
            div()
                .w(px(5.))
                .h(px(5.))
                .rounded_full()
                .bg(text_color)
                .flex_shrink_0(),
        )
        // Count label: 11px weight 500
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight(500.0))
                .text_color(text_color)
                .child(format!("{count}")),
        )
        .into_any_element()
}
