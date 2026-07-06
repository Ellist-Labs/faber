//! Presentational input — styles existing text + caret. Not a text-input engine.
use gpui::{
    App, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div, px,
};

use crate::theme::ActiveTheme;

#[derive(IntoElement)]
pub struct Input {
    value: SharedString,
    placeholder: SharedString,
    focused: bool,
}

impl Input {
    pub fn new(value: impl Into<SharedString>) -> Self {
        Self {
            value: value.into(),
            placeholder: "".into(),
            focused: false,
        }
    }

    pub fn placeholder(mut self, text: impl Into<SharedString>) -> Self {
        self.placeholder = text.into();
        self
    }

    pub fn focused(mut self) -> Self {
        self.focused = true;
        self
    }
}

impl RenderOnce for Input {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let border = if self.focused { theme.border_focus } else { theme.border };
        let (text_color, display) = if self.value.is_empty() {
            (theme.text_subtle, self.placeholder)
        } else {
            (theme.text, self.value)
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .px_2()
            .h(px(28.0))
            .bg(theme.bg_sunken)
            .border_1()
            .border_color(border)
            .rounded(px(theme.radius_sm))
            .text_color(text_color)
            .text_size(px(theme.font_size_body))
            .font_family(theme.mono_family.clone())
            .child(display)
    }
}
