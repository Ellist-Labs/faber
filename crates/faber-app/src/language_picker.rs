use std::sync::Arc;

use gpui::{
    Context, FocusHandle, Focusable, IntoElement, Render, Window, deferred, div, prelude::*, px,
};
use rust_i18n::t;

use faber_lang::Language;

use crate::{
    LpConfirm, LpDismiss, LpSelectNext, LpSelectPrev,
    theme::RuntimeTheme,
    ui::{h_flex, modal_backdrop_clear, modal_container, modal_footer, v_flex},
    workspace::Workspace,
};

pub struct LanguagePickerView {
    pub languages: Vec<Arc<Language>>,
    pub current_lang_id: Option<faber_lang::LanguageId>,
    pub cursor: usize,
    pub focus_handle: FocusHandle,
    pub ws: gpui::WeakEntity<Workspace>,
}

impl LanguagePickerView {
    pub fn new(
        languages: Vec<Arc<Language>>,
        current_lang_id: Option<faber_lang::LanguageId>,
        ws: gpui::WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let cursor = languages
            .iter()
            .position(|l| Some(&l.id) == current_lang_id.as_ref())
            .unwrap_or(0);
        Self {
            languages,
            current_lang_id,
            cursor,
            focus_handle: cx.focus_handle(),
            ws,
        }
    }

    fn on_dismiss(&mut self, _: &LpDismiss, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ws) = self.ws.upgrade() {
            ws.update(cx, |ws, cx| ws.close_language_picker(window, cx));
        }
    }

    fn on_confirm(&mut self, _: &LpConfirm, window: &mut Window, cx: &mut Context<Self>) {
        let lang = self.languages.get(self.cursor).cloned();
        if let Some(ws) = self.ws.upgrade() {
            ws.update(cx, |ws, cx| {
                ws.apply_language_override(lang, cx);
                ws.close_language_picker(window, cx);
            });
        }
    }

    fn on_select_next(&mut self, _: &LpSelectNext, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.languages.is_empty() {
            self.cursor = (self.cursor + 1) % self.languages.len();
            cx.notify();
        }
    }

    fn on_select_prev(&mut self, _: &LpSelectPrev, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.languages.is_empty() {
            self.cursor = self
                .cursor
                .checked_sub(1)
                .unwrap_or(self.languages.len() - 1);
            cx.notify();
        }
    }
}

impl Focusable for LanguagePickerView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for LanguagePickerView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<RuntimeTheme>().clone();

        let cursor = self.cursor;
        let current_lang_id = self.current_lang_id.clone();

        // Spec §5.5: rows rounded 8, mx 5, accent_muted selected, white 6% hover
        let hover_bg_row = gpui::rgba(0xFFFFFF0F);
        let rows: Vec<_> = self
            .languages
            .iter()
            .enumerate()
            .map(|(ix, lang)| {
                let is_selected = ix == cursor;
                let is_current = current_lang_id
                    .as_ref()
                    .map(|id| id == &lang.id)
                    .unwrap_or(false);
                let accent_muted = t.accent_muted;
                let name = lang.name.clone();
                h_flex()
                    .id(("lp-row", ix))
                    .mx(px(5.))
                    .px(px(t.sp5))
                    .py(px(t.sp2))
                    .gap(px(t.sp3))
                    .rounded(px(t.radius_md))
                    .when(is_selected, |el| el.bg(accent_muted))
                    .hover(move |el| {
                        if is_selected {
                            el
                        } else {
                            el.bg(gpui::Hsla::from(hover_bg_row))
                        }
                    })
                    .child(
                        div()
                            .w(px(16.))
                            .text_size(px(t.font_size_caption))
                            .font_family(t.ui_family.clone())
                            .text_color(t.accent)
                            .child(if is_current { "✓" } else { "" }),
                    )
                    .child(
                        div()
                            .text_size(px(t.font_size_caption))
                            .font_family(t.ui_family.clone())
                            .text_color(if is_selected { t.text } else { t.text_muted })
                            .child(name),
                    )
            })
            .collect();

        let list = v_flex()
            .id("lp-list")
            .overflow_y_scroll()
            .py(px(4.))
            .max_h(px(300.))
            .children(rows);

        let footer = modal_footer(
            &t,
            &[
                ("↑↓", t!("language_picker.hint_select").to_string()),
                ("esc", t!("language_picker.hint_dismiss").to_string()),
            ],
        );

        let modal = modal_container("lp-modal", &t)
            .key_context("LanguagePicker")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_dismiss))
            .on_action(cx.listener(Self::on_confirm))
            .on_action(cx.listener(Self::on_select_next))
            .on_action(cx.listener(Self::on_select_prev))
            .w(px(320.))
            .child(
                div()
                    .px(px(t.sp5))
                    .py(px(t.sp3))
                    .border_b_1()
                    .border_color(t.separator)
                    .text_size(px(t.font_size_caption))
                    .font_family(t.ui_family.clone())
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(t.text_muted)
                    .child(t!("language_picker.title").to_string()),
            )
            .child(list)
            .child(footer);

        const PAD_TOP: f32 = 132.;
        deferred(
            modal_backdrop_clear("lp-backdrop", PAD_TOP)
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|view, _, window, cx| {
                        view.on_dismiss(&LpDismiss, window, cx);
                    }),
                )
                .child(modal),
        )
    }
}
