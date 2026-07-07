use gpui::{
    AnyElement, App, ClickEvent, ElementId, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div,
    prelude::FluentBuilder as _, px,
};

use crate::theme::ActiveTheme;

pub enum ButtonVariant {
    #[allow(dead_code)] // reserved for primary CTAs; not yet used by any view
    Primary,
    Ghost,
    /// Transparent resting background with a hover highlight — for list-style
    /// rows and toggle chips that should not read as filled buttons.
    List,
}

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: SharedString,
    variant: ButtonVariant,
    disabled: bool,
    selected: bool,
    full_width: bool,
    caption: bool,
    content: Option<AnyElement>,
    on_click: Option<ClickHandler>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            variant: ButtonVariant::Ghost,
            disabled: false,
            selected: false,
            full_width: false,
            caption: false,
            content: None,
            on_click: None,
        }
    }

    /// Render the label at the caption font size — used by dense chip rows.
    pub fn caption(mut self) -> Self {
        self.caption = true;
        self
    }

    #[allow(dead_code)] // reserved for primary CTAs; not yet used by any view
    pub fn primary(mut self) -> Self {
        self.variant = ButtonVariant::Primary;
        self
    }

    pub fn list(mut self) -> Self {
        self.variant = ButtonVariant::List;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.disabled = true;
        self
    }

    /// Toggle-style selection: paints an `accent_muted` background when set.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Left-align content and stretch to the parent's full width. Used for
    /// list-style rows (icon + label + trailing hint).
    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }

    /// Replace the text label with a custom child (e.g. an icon + label row).
    pub fn content(mut self, content: impl IntoElement) -> Self {
        self.content = Some(content.into_any_element());
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let (mut bg, hover_bg, text) = match (self.disabled, &self.variant) {
            (true, ButtonVariant::List) => {
                (gpui::transparent_black(), gpui::transparent_black(), theme.text_disabled)
            }
            (true, _) => (theme.accent_muted, theme.accent_muted, theme.text_disabled),
            (false, ButtonVariant::Primary) => {
                (theme.accent, theme.accent_hover, theme.text_on_accent)
            }
            (false, ButtonVariant::Ghost) => (theme.bg_overlay, theme.line_highlight, theme.text),
            (false, ButtonVariant::List) => {
                (gpui::transparent_black(), theme.line_highlight, theme.text)
            }
        };
        if self.selected && !self.disabled {
            bg = theme.accent_muted;
        }
        let interactive = !self.disabled;
        let has_content = self.content.is_some();
        let font_size =
            if self.caption { theme.font_size_caption } else { theme.font_size_body };
        div()
            .id(self.id)
            .flex()
            .items_center()
            .when(self.full_width, |el| el.w_full().justify_start())
            .when(!self.full_width, |el| el.justify_center())
            .px_3()
            .py_1()
            .bg(bg)
            .text_color(text)
            .text_size(px(font_size))
            .font_family(theme.ui_family.clone())
            .rounded(px(theme.radius_md))
            .when(interactive, |el| {
                el.cursor_pointer().hover(move |s| s.bg(hover_bg))
            })
            .when_some(self.on_click.filter(|_| interactive), |el, handler| {
                el.on_click(move |ev, window, cx| handler(ev, window, cx))
            })
            .when_some(self.content, |el, content| el.child(content))
            .when(!has_content, |el| el.child(self.label))
    }
}
