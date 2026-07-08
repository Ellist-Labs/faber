use gpui::{Hsla, IntoElement, Pixels, RenderOnce, Styled, Svg, px, svg};

use crate::theme::ActiveTheme;

/// Monochrome UI icon (Material Symbols), tinted via text color.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IconName {
    Add,
    ChevronLeft,
    ChevronRight,
    Close,
    Code,
    ExpandMore,
    FileCopy,
    Filter,
    PanelBottom,
    PanelLeft,
    PanelRight,
    Refresh,
    Remove,
    Search,
    Settings,
    Toc,
    UnfoldLess,
    UnfoldMore,
    Visibility,
}

impl IconName {
    pub fn path(self) -> &'static str {
        match self {
            IconName::Add => "icons/ui/add.svg",
            IconName::ChevronLeft => "icons/ui/chevron_left.svg",
            IconName::ChevronRight => "icons/ui/chevron_right.svg",
            IconName::Close => "icons/ui/close.svg",
            IconName::Code => "icons/ui/code.svg",
            IconName::ExpandMore => "icons/ui/expand_more.svg",
            IconName::FileCopy => "icons/ui/file_copy.svg",
            IconName::Filter => "icons/ui/filter.svg",
            IconName::PanelBottom => "icons/ui/panel_bottom.svg",
            IconName::PanelLeft => "icons/ui/panel_left.svg",
            IconName::PanelRight => "icons/ui/panel_right.svg",
            IconName::Refresh => "icons/ui/refresh.svg",
            IconName::Remove => "icons/ui/remove.svg",
            IconName::Search => "icons/ui/search.svg",
            IconName::Settings => "icons/ui/settings.svg",
            IconName::Toc => "icons/ui/toc.svg",
            IconName::UnfoldLess => "icons/ui/unfold_less.svg",
            IconName::UnfoldMore => "icons/ui/unfold_more.svg",
            IconName::Visibility => "icons/ui/visibility.svg",
        }
    }
}

#[derive(IntoElement)]
pub struct Icon {
    name: IconName,
    size: Pixels,
    color: Option<Hsla>,
}

impl Icon {
    pub fn new(name: IconName) -> Self {
        Self { name, size: px(16.), color: None }
    }

    pub fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }

    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

impl RenderOnce for Icon {
    fn render(self, _window: &mut gpui::Window, cx: &mut gpui::App) -> impl IntoElement {
        let color = self.color.unwrap_or(cx.theme().text);
        let el: Svg = svg()
            .path(self.name.path())
            .size(self.size)
            .flex_shrink_0();
        el.text_color(color)
    }
}
