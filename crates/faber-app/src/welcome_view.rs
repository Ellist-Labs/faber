use std::path::PathBuf;

use gpui::{App, ElementId, Entity, FontWeight, IntoElement, Window, div, prelude::*, px};
use rust_i18n::t;

use crate::theme::RuntimeTheme;
use crate::ui::{KeyHint, h_flex, v_flex};
use crate::workspace::Workspace;
use crate::{NewFile, OpenFile, OpenFolder, OpenSettings};

/// Project name → 2-letter initials (first chars of first two words, or first two chars).
fn project_initials(name: &str) -> String {
    let mut words = name.split_whitespace().filter(|w| !w.is_empty());
    match (words.next(), words.next()) {
        (Some(a), Some(b)) => {
            let ca = a.chars().next().unwrap_or_default();
            let cb = b.chars().next().unwrap_or_default();
            format!("{}{}", ca, cb).to_uppercase()
        }
        (Some(a), None) => {
            let mut chars = a.chars();
            match (chars.next(), chars.next()) {
                (Some(c1), Some(c2)) => format!("{}{}", c1, c2).to_uppercase(),
                (Some(c1), None) => c1.to_uppercase().to_string(),
                _ => "??".to_string(),
            }
        }
        _ => "??".to_string(),
    }
}

/// Replace leading home-dir path with `~`.
fn abbreviate_path(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    if !home.is_empty() && path.starts_with(&home) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_string()
    }
}

/// Last path component (project folder name).
fn project_name(path: &str) -> String {
    PathBuf::from(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

pub fn render_welcome(
    t: &RuntimeTheme,
    recent_projects: &[String],
    _recent_files: &[String],
    entity: &Entity<Workspace>,
) -> impl IntoElement {
    // White ~6% and ~5% for hover backgrounds per spec.
    let hover_bg_action = gpui::rgba(0xFFFFFF0F); // ~5.9%
    let hover_bg_project = gpui::rgba(0xFFFFFF0D); // ~5.1%

    // ── Left rail action row helper ──────────────────────────────────────────
    // Returns an interactive row: 30px tall, 0 8px padding, rounded 7, 12.5px text.
    let action_row =
        move |id: ElementId,
              label: String,
              hint: &'static str,
              handler: Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static>|
              -> gpui::AnyElement {
            div()
                .id(id)
                .flex()
                .flex_row()
                .items_center()
                .w_full()
                .h(px(30.))
                .px(px(8.))
                .rounded(px(7.))
                .text_color(t.text_muted)
                .text_size(px(12.5))
                .font_family(t.ui_family.clone())
                .cursor_pointer()
                .hover(move |s| s.bg(hover_bg_action).text_color(t.text))
                .on_click(move |ev, window, cx| handler(ev, window, cx))
                .child(div().flex_1().child(label))
                .child(KeyHint::new(hint))
                .into_any_element()
        };

    // ── Left rail ────────────────────────────────────────────────────────────
    let left_rail = div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .w(px(220.))
        .h_full()
        .bg(t.bg)
        .border_r_1()
        .border_color(t.border)
        .pt(px(28.))
        .pl(px(20.))
        .pr(px(20.))
        .pb(px(24.))
        // Brand block — pl(8) aligns with the action row text (which also has px(8) internal pad)
        .child(
            v_flex()
                .pl(px(8.))
                .child(
                    div()
                        .text_color(t.text)
                        .text_size(px(17.))
                        .font_weight(FontWeight::BOLD)
                        .font_family(t.ui_family.clone())
                        .child("Faber"),
                )
                .child(
                    div()
                        .mt(px(5.))
                        .text_color(t.text_subtle)
                        .text_size(px(10.5))
                        .font_family(t.mono_family.clone())
                        .child(format!("v{}", env!("CARGO_PKG_VERSION"))),
                ),
        )
        // Gap between brand and START section
        .child(div().h(px(28.)))
        // Section label: START
        .child(
            div()
                .px(px(8.))
                .mb(px(4.))
                .text_color(t.text_subtle)
                .text_size(px(10.))
                .font_weight(FontWeight::SEMIBOLD)
                .font_family(t.ui_family.clone())
                .child(t!("welcome.start").to_string()),
        )
        // Action: New File
        .child(action_row(
            "welcome-new-file".into(),
            t!("welcome.new_file").to_string(),
            "⌘N",
            Box::new(|_, window: &mut Window, cx: &mut App| {
                window.dispatch_action(Box::new(NewFile), cx)
            }),
        ))
        // Action: Open File
        .child(action_row(
            "welcome-open-file".into(),
            t!("welcome.open_file").to_string(),
            "⌘O",
            Box::new(|_, window: &mut Window, cx: &mut App| {
                window.dispatch_action(Box::new(OpenFile), cx)
            }),
        ))
        // Action: Open Folder
        .child(action_row(
            "welcome-open-folder".into(),
            t!("welcome.open_folder").to_string(),
            "⇧⌘O",
            Box::new(|_, window: &mut Window, cx: &mut App| {
                window.dispatch_action(Box::new(OpenFolder), cx)
            }),
        ))
        // Action: Open Settings
        .child(action_row(
            "welcome-settings".into(),
            t!("welcome.open_settings").to_string(),
            "⌘,",
            Box::new(|_, window: &mut Window, cx: &mut App| {
                window.dispatch_action(Box::new(OpenSettings), cx)
            }),
        ));

    // ── Right column — recent projects ───────────────────────────────────────
    let right_col = v_flex()
        .flex_1()
        .overflow_hidden()
        // Header strip: 36px, border-bottom, "RECENT" label
        .child(
            h_flex()
                .h(px(36.))
                .flex_shrink_0()
                .items_center()
                .px(px(20.))
                .border_b_1()
                .border_color(t.border)
                .child(
                    div()
                        .text_color(t.text_subtle)
                        .text_size(px(10.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .font_family(t.ui_family.clone())
                        .child(t!("welcome.recent").to_string()),
                ),
        )
        // Scrollable project list
        .child(
            div()
                .id("welcome-recent-list")
                .flex_1()
                .overflow_y_scroll()
                .px(px(10.))
                .py(px(8.))
                .when(recent_projects.is_empty(), |el| {
                    el.flex().items_center().justify_center().child(
                        div()
                            .text_color(t.text_muted)
                            .text_size(px(12.5))
                            .font_family(t.ui_family.clone())
                            .child(t!("welcome.no_recent").to_string()),
                    )
                })
                .when(!recent_projects.is_empty(), |el| {
                    el.children(recent_projects.iter().enumerate().map(|(i, path)| {
                        let ent = entity.clone();
                        let p = path.clone();
                        let name = project_name(path);
                        let abbrev = abbreviate_path(path);
                        let initials = project_initials(&name);
                        let bg_raised = t.bg_raised;
                        let text_col = t.text;
                        let text_muted_col = t.text_muted;
                        let text_subtle_col = t.text_subtle;
                        let ui_fam = t.ui_family.clone();
                        let mono_fam = t.mono_family.clone();

                        div()
                            .id(("recent-proj", i))
                            .flex()
                            .flex_row()
                            .items_center()
                            .w_full()
                            .h(px(46.))
                            .px(px(10.))
                            .rounded(px(9.))
                            .gap(px(12.))
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover_bg_project))
                            .on_click(move |_, _, cx| {
                                let folder = PathBuf::from(&p);
                                ent.update(cx, |ws, cx| ws.set_root_folder(folder, cx));
                            })
                            // Icon: 30×30, rounded 7, bg_raised, 2-letter initials
                            .child(
                                div()
                                    .flex()
                                    .flex_shrink_0()
                                    .items_center()
                                    .justify_center()
                                    .w(px(30.))
                                    .h(px(30.))
                                    .rounded(px(7.))
                                    .bg(bg_raised)
                                    .text_color(text_muted_col)
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .font_family(ui_fam.clone())
                                    .child(initials),
                            )
                            // Name + path column
                            .child(
                                v_flex()
                                    .flex_1()
                                    .overflow_hidden()
                                    .child(
                                        div()
                                            .overflow_hidden()
                                            .text_color(text_col)
                                            .text_size(px(13.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .font_family(ui_fam.clone())
                                            .child(name),
                                    )
                                    .child(
                                        div()
                                            .overflow_hidden()
                                            .text_color(text_subtle_col)
                                            .text_size(px(11.))
                                            .font_family(mono_fam)
                                            .child(abbrev),
                                    ),
                            )
                    }))
                }),
        );

    // ── Root: full size, two-column flex row ─────────────────────────────────
    div()
        .flex()
        .flex_row()
        .size_full()
        .bg(t.bg)
        .child(left_rail)
        .child(right_col)
}
