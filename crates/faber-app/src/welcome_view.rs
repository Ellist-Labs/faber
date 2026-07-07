use gpui::{App, IntoElement, StyleRefinement, Window, div, prelude::*, px};

use crate::theme::RuntimeTheme;
use crate::ui::{Icon, IconName, KeyHint, h_flex, v_flex};
use crate::{NewFile, OpenFile, OpenFolder, OpenSettings};

pub fn render_welcome(t: &RuntimeTheme) -> impl IntoElement {
    let bg = t.bg;
    let hover_bg = t.bg_overlay;
    let text = t.text;
    let text_muted = t.text_muted;
    let text_subtle = t.text_subtle;
    let separator = t.separator;
    let font_size_body = t.font_size_body;
    let font_size_caption = t.font_size_caption;
    let font_size_heading = t.font_size_heading;
    let ui_family = t.ui_family.clone();
    let radius_sm = t.radius_sm;

    let ui_fam = ui_family.clone();
    let section_label = move |label: &'static str| {
        h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .child(
                div()
                    .text_color(text_subtle)
                    .text_size(px(font_size_caption))
                    .font_family(ui_fam.clone())
                    .child(label),
            )
            .child(div().flex_1().h(px(1.0)).bg(separator))
    };

    v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .bg(bg)
        // ── Header ───────────────────────────────────────────────────────────
        .child(
            v_flex()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .text_color(text)
                        .text_size(px(font_size_heading * 2.0))
                        .font_family(ui_family.clone())
                        .child("Faber"),
                )
                .child(
                    div()
                        .text_color(text_subtle)
                        .text_size(px(font_size_body))
                        .font_family(ui_family.clone())
                        .child("Lean, GPU-accelerated code editor"),
                ),
        )
        // ── Action panel ─────────────────────────────────────────────────────
        .child(
            v_flex()
                .mt_8()
                .w(px(300.0))
                .gap_3()
                // GET STARTED section
                .child(
                    v_flex()
                        .gap_px()
                        .child(section_label("GET STARTED"))
                        .child(
                            div()
                                .id("welcome-new-file")
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .w_full()
                                .px_2()
                                .py_1()
                                .rounded(px(radius_sm))
                                .cursor_pointer()
                                .hover(move |s: StyleRefinement| s.bg(hover_bg))
                                .on_click(|_, window: &mut Window, cx: &mut App| {
                                    window.dispatch_action(Box::new(NewFile), cx)
                                })
                                .child(Icon::new(IconName::Add).size(16.0).color(text_muted))
                                .child(
                                    div()
                                        .flex_1()
                                        .text_color(text)
                                        .text_size(px(font_size_body))
                                        .font_family(ui_family.clone())
                                        .child("New File"),
                                )
                                .child(KeyHint::new("⌘N")),
                        )
                        .child(
                            div()
                                .id("welcome-open-file")
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .w_full()
                                .px_2()
                                .py_1()
                                .rounded(px(radius_sm))
                                .cursor_pointer()
                                .hover(move |s: StyleRefinement| s.bg(hover_bg))
                                .on_click(|_, window: &mut Window, cx: &mut App| {
                                    window.dispatch_action(Box::new(OpenFile), cx)
                                })
                                .child(
                                    Icon::new(IconName::FileCopy).size(16.0).color(text_muted),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .text_color(text)
                                        .text_size(px(font_size_body))
                                        .font_family(ui_family.clone())
                                        .child("Open File…"),
                                )
                                .child(KeyHint::new("⌘O")),
                        )
                        .child(
                            div()
                                .id("welcome-open-folder")
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .w_full()
                                .px_2()
                                .py_1()
                                .rounded(px(radius_sm))
                                .cursor_pointer()
                                .hover(move |s: StyleRefinement| s.bg(hover_bg))
                                .on_click(|_, window: &mut Window, cx: &mut App| {
                                    window.dispatch_action(Box::new(OpenFolder), cx)
                                })
                                .child(Icon::new(IconName::Toc).size(16.0).color(text_muted))
                                .child(
                                    div()
                                        .flex_1()
                                        .text_color(text)
                                        .text_size(px(font_size_body))
                                        .font_family(ui_family.clone())
                                        .child("Open Folder…"),
                                )
                                .child(KeyHint::new("⇧⌘O")),
                        ),
                )
                // CONFIGURE section
                .child(
                    v_flex()
                        .gap_px()
                        .child(section_label("CONFIGURE"))
                        .child(
                            div()
                                .id("welcome-settings")
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .w_full()
                                .px_2()
                                .py_1()
                                .rounded(px(radius_sm))
                                .cursor_pointer()
                                .hover(move |s: StyleRefinement| s.bg(hover_bg))
                                .on_click(|_, window: &mut Window, cx: &mut App| {
                                    window.dispatch_action(Box::new(OpenSettings), cx)
                                })
                                .child(Icon::new(IconName::Settings).size(16.0).color(text_muted))
                                .child(
                                    div()
                                        .flex_1()
                                        .text_color(text)
                                        .text_size(px(font_size_body))
                                        .font_family(ui_family.clone())
                                        .child("Open Settings"),
                                )
                                .child(KeyHint::new("⌘,")),
                        ),
                ),
        )
}
