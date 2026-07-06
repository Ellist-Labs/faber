use gpui::{App, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div, px};

use crate::theme::ActiveTheme;

pub enum ButtonVariant {
    Primary,
    Ghost,
}

#[derive(IntoElement)]
pub struct Button {
    label: SharedString,
    variant: ButtonVariant,
    disabled: bool,
}

impl Button {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self { label: label.into(), variant: ButtonVariant::Ghost, disabled: false }
    }

    pub fn primary(mut self) -> Self {
        self.variant = ButtonVariant::Primary;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.disabled = true;
        self
    }
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let (bg, text) = match (self.disabled, &self.variant) {
            (true, _) => (theme.accent_muted, theme.text_disabled),
            (false, ButtonVariant::Primary) => (theme.accent, theme.text_on_accent),
            (false, ButtonVariant::Ghost) => (theme.bg_overlay, theme.text),
        };
        div()
            .flex()
            .items_center()
            .justify_center()
            .px_3()
            .py_1()
            .bg(bg)
            .text_color(text)
            .text_size(px(theme.font_size_body))
            .font_family(theme.ui_family.clone())
            .rounded(px(theme.radius_md))
            .child(self.label)
    }
}
