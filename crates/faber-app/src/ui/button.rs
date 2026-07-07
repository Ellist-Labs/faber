use gpui::{
    App, ClickEvent, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder as _,
    px,
};

use crate::theme::ActiveTheme;

pub enum ButtonVariant {
    Primary,
    Ghost,
}

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: SharedString,
    variant: ButtonVariant,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            variant: ButtonVariant::Ghost,
            disabled: false,
            on_click: None,
        }
    }

    pub fn primary(mut self) -> Self {
        self.variant = ButtonVariant::Primary;
        self
    }

    #[allow(dead_code)]
    pub fn disabled(mut self) -> Self {
        self.disabled = true;
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
        let (bg, hover_bg, text) = match (self.disabled, &self.variant) {
            (true, _) => (theme.accent_muted, theme.accent_muted, theme.text_disabled),
            (false, ButtonVariant::Primary) => (theme.accent, theme.accent_hover, theme.text_on_accent),
            (false, ButtonVariant::Ghost) => (theme.bg_overlay, theme.line_highlight, theme.text),
        };
        let interactive = !self.disabled;
        div()
            .id(self.id)
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
            .when(interactive, |el| {
                el.cursor_pointer().hover(move |s| s.bg(hover_bg))
            })
            .when_some(self.on_click.filter(|_| interactive), |el, handler| {
                el.on_click(move |ev, window, cx| handler(ev, window, cx))
            })
            .child(self.label)
    }
}
