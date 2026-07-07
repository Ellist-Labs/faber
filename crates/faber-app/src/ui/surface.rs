#![allow(dead_code)] // removed when Wave 2 adopts ui::Surface
use gpui::{App, IntoElement, ParentElement, RenderOnce, Styled, Window, div, px};

use crate::theme::ActiveTheme;

#[derive(IntoElement)]
pub struct Surface {
    children: gpui::AnyElement,
    elevated: bool,
    radius: Option<f32>,
}

impl Surface {
    pub fn new(child: impl IntoElement) -> Self {
        Self { children: child.into_any_element(), elevated: false, radius: None }
    }

    pub fn elevated(mut self) -> Self {
        self.elevated = true;
        self
    }

    pub fn radius(mut self, r: f32) -> Self {
        self.radius = Some(r);
        self
    }
}

impl RenderOnce for Surface {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let bg = if self.elevated { theme.bg_elevated } else { theme.bg };
        let radius = self.radius.unwrap_or(theme.radius_md);
        div()
            .bg(bg)
            .border_1()
            .border_color(theme.border)
            .rounded(px(radius))
            .child(self.children)
    }
}
