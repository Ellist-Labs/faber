use gpui::{App, IntoElement, Window, div, prelude::*, px};
use rust_i18n::t;

use crate::theme::RuntimeTheme;
use crate::ui::{Button, Icon, IconName, KeyHint, Label, h_flex, v_flex};
use crate::{NewFile, OpenFile, OpenFolder, OpenSettings};

pub fn render_welcome(t: &RuntimeTheme) -> impl IntoElement {
    let separator = t.separator;

    let section_label = move |label: String| {
        h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .child(Label::new(label).caption().subtle())
            .child(div().flex_1().h(px(1.0)).bg(separator))
    };

    let row = |id: &'static str, icon: IconName, label: String| {
        h_flex()
            .gap_2()
            .w_full()
            .child(Icon::new(icon).size(px(16.)).color(t.text_muted))
            .child(div().flex_1().child(Label::new(label)))
            .child(KeyHint::new(id))
    };

    v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .bg(t.bg)
        // ── Header ───────────────────────────────────────────────────────────
        .child(
            v_flex()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .text_color(t.text)
                        .text_size(px(t.font_size_heading * 2.0))
                        .font_family(t.ui_family.clone())
                        .child("Faber"),
                )
                .child(Label::new(t!("welcome.tagline").to_string()).subtle()),
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
                        .child(section_label(t!("welcome.get_started").to_string()))
                        .child(
                            Button::new("welcome-new-file", "")
                                .list()
                                .full_width()
                                .content(row(
                                    "⌘N",
                                    IconName::Add,
                                    t!("welcome.new_file").to_string(),
                                ))
                                .on_click(|_, window: &mut Window, cx: &mut App| {
                                    window.dispatch_action(Box::new(NewFile), cx)
                                }),
                        )
                        .child(
                            Button::new("welcome-open-file", "")
                                .list()
                                .full_width()
                                .content(row(
                                    "⌘O",
                                    IconName::FileCopy,
                                    t!("welcome.open_file").to_string(),
                                ))
                                .on_click(|_, window: &mut Window, cx: &mut App| {
                                    window.dispatch_action(Box::new(OpenFile), cx)
                                }),
                        )
                        .child(
                            Button::new("welcome-open-folder", "")
                                .list()
                                .full_width()
                                .content(row(
                                    "⇧⌘O",
                                    IconName::Toc,
                                    t!("welcome.open_folder").to_string(),
                                ))
                                .on_click(|_, window: &mut Window, cx: &mut App| {
                                    window.dispatch_action(Box::new(OpenFolder), cx)
                                }),
                        ),
                )
                // CONFIGURE section
                .child(
                    v_flex()
                        .gap_px()
                        .child(section_label(t!("welcome.configure").to_string()))
                        .child(
                            Button::new("welcome-settings", "")
                                .list()
                                .full_width()
                                .content(row(
                                    "⌘,",
                                    IconName::Settings,
                                    t!("welcome.open_settings").to_string(),
                                ))
                                .on_click(|_, window: &mut Window, cx: &mut App| {
                                    window.dispatch_action(Box::new(OpenSettings), cx)
                                }),
                        ),
                ),
        )
}
