use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    App, Context, FocusHandle, Focusable, IntoElement, Render, SharedString,
    UniformListScrollHandle, WeakEntity, Window, div, prelude::*, px,
};
use rust_i18n::t;

use crate::file_icons;
use crate::panels::results_list::{FileCounts, Row, render_results_list, split_path};
use crate::theme::RuntimeTheme;
use crate::workspace::Workspace;

pub struct ReferencesPanel {
    pub focus_handle: FocusHandle,
    workspace: Option<WeakEntity<Workspace>>,
    rows: Vec<Row>,
    scroll: UniformListScrollHandle,
}

impl ReferencesPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            workspace: None,
            rows: Vec::new(),
            scroll: UniformListScrollHandle::new(),
        }
    }

    pub fn set_workspace(&mut self, workspace: WeakEntity<Workspace>) {
        self.workspace = Some(workspace);
    }

    pub fn populate(
        &mut self,
        locations: Vec<(PathBuf, usize, usize)>,
        root: Option<&std::path::Path>,
    ) {
        self.rows.clear();

        let mut groups: Vec<(PathBuf, Vec<(usize, usize)>)> = Vec::new();
        for (path, line, col) in locations {
            if let Some(g) = groups.iter_mut().find(|(p, _)| p == &path) {
                g.1.push((line, col));
            } else {
                groups.push((path, vec![(line, col)]));
            }
        }

        for (path, entries) in groups {
            let rel = match root {
                Some(r) => path
                    .strip_prefix(r)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned(),
                None => path.to_string_lossy().into_owned(),
            };
            let rel_fwd = rel.replace('\\', "/");
            let (filename, dir) = split_path(&rel_fwd);
            let file_icon = file_icons::icon_for_file(filename);
            let count = entries.len();

            self.rows.push(Row::FileHeader {
                filename: SharedString::from(filename.to_owned()),
                dir: SharedString::from(dir.to_owned()),
                file_icon,
                counts: FileCounts::Refs(count),
            });

            let path = Arc::new(path);
            for (line, col) in entries {
                let preview = read_line_preview(&path, line);
                self.rows.push(Row::RefEntry {
                    path: path.clone(),
                    line,
                    col,
                    preview: SharedString::from(preview),
                });
            }
        }
    }
}

fn read_line_preview(path: &std::path::Path, line: usize) -> String {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.lines().nth(line).map(|l| l.trim().to_owned()))
        .unwrap_or_default()
}

impl Focusable for ReferencesPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ReferencesPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<RuntimeTheme>().clone();

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
                .child(t!("tab.references").to_string())
                .into_any_element();
        }

        render_results_list(
            "ref-rows",
            self.rows.clone(),
            self.scroll.clone(),
            self.workspace.clone(),
        )
    }
}
