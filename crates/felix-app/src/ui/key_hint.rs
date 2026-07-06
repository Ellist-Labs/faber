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
        div()
            .flex()
            .items_center()
            .justify_center()
            .px_1()
            .h(px(18.0))
            .bg(theme.bg_overlay)
            .border_1()
            .border_color(theme.border)
            .rounded(px(theme.radius_sm))
            .text_color(theme.text_muted)
            .text_size(px(theme.font_size_caption))
            .font_family(theme.mono_family.clone())
            .child(self.keys)
    }
}
