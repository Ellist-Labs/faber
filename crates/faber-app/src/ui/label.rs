use gpui::{App, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div, px};

use crate::theme::ActiveTheme;

pub enum LabelSize {
    Caption,
    Body,
    Heading,
}

enum LabelColor {
    Default,
    Muted,
    Subtle,
}

#[derive(IntoElement)]
pub struct Label {
    text: SharedString,
    size: LabelSize,
    color: LabelColor,
}

impl Label {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            size: LabelSize::Body,
            color: LabelColor::Default,
        }
    }

    pub fn caption(mut self) -> Self {
        self.size = LabelSize::Caption;
        self
    }

    pub fn heading(mut self) -> Self {
        self.size = LabelSize::Heading;
        self
    }

    pub fn muted(mut self) -> Self {
        self.color = LabelColor::Muted;
        self
    }

    /// Dimmest text tone (theme `text_subtle`) — section labels, taglines.
    pub fn subtle(mut self) -> Self {
        self.color = LabelColor::Subtle;
        self
    }
}

impl RenderOnce for Label {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let color = match self.color {
            LabelColor::Default => theme.text,
            LabelColor::Muted => theme.text_muted,
            LabelColor::Subtle => theme.text_subtle,
        };
        let size = match self.size {
            LabelSize::Caption => theme.font_size_caption,
            LabelSize::Body => theme.font_size_body,
            LabelSize::Heading => theme.font_size_heading,
        };
        div()
            .text_color(color)
            .text_size(px(size))
            .font_family(theme.ui_family.clone())
            .child(self.text)
    }
}
