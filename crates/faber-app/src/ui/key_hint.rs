use gpui::{App, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div, px};

use crate::theme::ActiveTheme;

/// Renders a keyboard shortcut badge, e.g. `⌘S` or `⎋`.
#[derive(IntoElement)]
pub struct KeyHint {
    keys: SharedString,
}

impl KeyHint {
    pub fn new(keys: impl Into<SharedString>) -> Self {
        Self { keys: keys.into() }
    }
}

impl RenderOnce for KeyHint {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        // Keybinding chip (spec §5.11): bg_raised, hairline border, 10px label.
        div()
            .flex()
            .items_center()
            .justify_center()
            .px(px(5.))
            .h(px(17.0))
            .bg(theme.bg_raised)
            .border_1()
            .border_color(theme.border_focus)
            .rounded(px(5.))
            .text_color(theme.text_muted)
            .text_size(px(10.))
            .font_family(theme.ui_family.clone())
            .child(self.keys)
    }
}
