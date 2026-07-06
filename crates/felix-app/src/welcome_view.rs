use gpui::{IntoElement, prelude::*, px};

use crate::theme::RuntimeTheme;
use crate::ui::{Button, KeyHint, h_flex, v_flex};
use crate::{NewFile, OpenFile, OpenFolder};

/// Empty state shown in the tab area when no file is open.
pub fn render_welcome(t: &RuntimeTheme) -> impl IntoElement {
    let hint_row = |label: &'static str, keys: &'static str| {
        h_flex()
            .gap_2()
            .justify_between()
            .w_full()
            .child(
                v_flex()
                    .text_color(t.text_muted)
                    .text_size(px(t.font_size_caption))
                    .child(label),
            )
            .child(KeyHint::new(keys))
    };

    v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_2()
        .bg(t.bg)
        .font_family(t.ui_family.clone())
        .child(
            v_flex()
                .items_center()
                .gap_1()
                .child(
                    v_flex()
                        .text_color(t.text)
                        .text_size(px(t.font_size_heading * 1.6))
                        .child("Felix"),
                )
                .child(
                    v_flex()
                        .text_color(t.text_subtle)
                        .text_size(px(t.font_size_body))
                        .child("Lean, GPU-accelerated code editor"),
                ),
        )
        .child(
            v_flex()
                .mt_6()
                .gap_2()
                .w(px(280.))
                .child(Button::new("welcome-open-file", "Open File…").primary().on_click(
                    |_, window, cx| window.dispatch_action(Box::new(OpenFile), cx),
                ))
                .child(Button::new("welcome-open-folder", "Open Folder…").on_click(
                    |_, window, cx| window.dispatch_action(Box::new(OpenFolder), cx),
                ))
                .child(Button::new("welcome-new-file", "New File").on_click(
                    |_, window, cx| window.dispatch_action(Box::new(NewFile), cx),
                )),
        )
        .child(
            v_flex()
                .mt_6()
                .gap_1()
                .w(px(280.))
                .child(hint_row("Open file", "⌘O"))
                .child(hint_row("Open folder", "⇧⌘O"))
                .child(hint_row("New file", "⌘N")),
        )
}
