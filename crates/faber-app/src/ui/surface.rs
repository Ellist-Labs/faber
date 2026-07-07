use gpui::{App, IntoElement, ParentElement, RenderOnce, Styled, Window, div, px};

use crate::theme::ActiveTheme;

#[derive(IntoElement)]
pub struct Surface {
    children: gpui::AnyElement,
    elevated: bool,
    radius: f32,
}

impl Surface {
    pub fn new(child: impl IntoElement) -> Self {
        Self { children: child.into_any_element(), elevated: false, radius: 6.0 }
    }

    pub fn elevated(mut self) -> Self {
        self.elevated = true;
        self
    }

    pub fn radius(mut self, px: f32) -> Self {
        self.radius = px;
        self
    }
}

impl RenderOnce for Surface {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let bg = if self.elevated { theme.bg_elevated } else { theme.bg };
        div()
            .bg(bg)
            .border_1()
            .border_color(theme.border)
            .rounded(px(self.radius))
            .child(self.children)
    }
}
