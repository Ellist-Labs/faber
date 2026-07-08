#![allow(dead_code)] // removed when Wave 2 adopts ui::Divider
use gpui::{App, IntoElement, RenderOnce, Styled, Window, div, px};

use crate::theme::ActiveTheme;

pub enum DividerDirection {
    Horizontal,
    Vertical,
}

#[derive(IntoElement)]
pub struct Divider {
    direction: DividerDirection,
}

impl Divider {
    pub fn horizontal() -> Self {
        Self {
            direction: DividerDirection::Horizontal,
        }
    }

    pub fn vertical() -> Self {
        Self {
            direction: DividerDirection::Vertical,
        }
    }
}

impl RenderOnce for Divider {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        match self.direction {
            DividerDirection::Horizontal => div().w_full().h(px(1.0)).bg(theme.separator),
            DividerDirection::Vertical => div().h_full().w(px(1.0)).bg(theme.separator),
        }
    }
}
