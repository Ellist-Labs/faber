use std::path::Path;
use std::sync::Arc;

use faber_editor::outline::Outline;
use gpui::{
    AnyElement, App, Context, Div, Entity, IntoElement, ListHorizontalSizingBehavior, MouseButton,
    SharedString, Stateful, div, prelude::*, px, svg, uniform_list,
};

use crate::file_icons;
use crate::theme::RuntimeTheme;
use crate::ui::{Icon, IconName, glass_surface, h_flex, v_flex};
use crate::workspace::Workspace;

#[allow(dead_code)]
pub const ACTIVITY_BAR_W: f32 = 44.0;
#[allow(dead_code)]
pub const SIDEBAR_PANEL_W: f32 = 240.0;
const TREE_INDENT_W: f32 = 16.0;

#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SidebarItemKind {
    Explorer,
    Search,
}

pub struct SidebarState {
    pub open: bool,
    pub active: SidebarItemKind,
}

impl Default for SidebarState {
    fn default() -> Self {
        Self {
            open: false,
            active: SidebarItemKind::Explorer,
        }
    }
}

impl Workspace {
    /// Render the sidebar as a glass overlay (absolute, left-anchored, no layout shift).
    pub(crate) fn render_sidebar_panel(
        &self,
        t: &RuntimeTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let header = self.render_explorer_header(t, cx);
        let body = self.render_explorer(t, cx);
        let sidebar_fh = self.sidebar_focus.clone();

        glass_surface(t)
            .id("sidebar-overlay")
            .absolute()
            .left_0()
            .top_0()
            .bottom_0()
            .w(px(t.sidebar_w))
            .flex()
            .flex_col()
            // No rounded corners — full-height overlay
            .border_r_1()
            .border_color(t.border_focus)
            .key_context("Sidebar")
            .track_focus(&sidebar_fh)
            .child(header)
            .child(body)
    }

    fn render_explorer(&self, t: &RuntimeTheme, cx: &mut Context<Self>) -> AnyElement {
        if self.tree.is_none() {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .px_3()
                .gap_2()
                .font_family(t.ui_family.clone())
                .text_size(px(t.font_size_caption))
                .text_color(t.text_muted)
                .child(rust_i18n::t!("sidebar.no_folder").to_string())
                .child(rust_i18n::t!("sidebar.open_folder_hint").to_string())
                .into_any_element();
        }

        let entity = cx.entity();
        let t2 = t.clone();
        let active_path = self.active_editor_path(cx);
        let char_w = t.char_w_code;
        let widest_idx = self
            .visible_rows
            .iter()
            .enumerate()
            .max_by_key(|(_, r)| {
                (r.depth as f32 * TREE_INDENT_W + r.name.len() as f32 * char_w) as usize
            })
            .map(|(i, _)| i)
            .unwrap_or(0);
        let settings = &cx.global::<crate::settings_view::SettingsStore>().0;
        let show_scrollbar = settings.show_scrollbar;
        let indent_guides = settings.indent_guides;
        let is_dragging = self.tree_scrollbar_drag.is_some();
        let tree_base_handle = self.tree_scroll.0.borrow().base_handle.clone();

        let scrollbar = crate::ui::render_scrollbar(
            "explorer-scrollbar",
            "explorer-scrollbar-thumb",
            &tree_base_handle,
            show_scrollbar,
            is_dragging,
            cx.listener(|ws, ev, _, cx| {
                let handle = ws.tree_scroll.0.borrow().base_handle.clone();
                ws.tree_scrollbar_drag = Some(crate::ui::scrollbar::start_drag(ev, &handle));
                cx.notify();
            }),
            t,
            None,
        );

        let tree_list = uniform_list(
            "file-tree",
            self.visible_rows.len(),
            move |range: std::ops::Range<usize>, _window, cx| {
                let entity2 = entity.clone();
                let ws = entity.read(cx);
                range
                    .map(|ix| {
                        ws.render_tree_row(ix, &entity2, active_path.as_deref(), &t2, indent_guides)
                            .into_any_element()
                    })
                    .collect::<Vec<AnyElement>>()
            },
        )
        .flex_1()
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .with_width_from_item(Some(widest_idx))
        .track_scroll(self.tree_scroll.clone());

        div()
            .flex()
            .flex_row()
            .flex_1()
            .min_h(px(0.))
            .when(is_dragging, |el| {
                el.on_mouse_move(cx.listener(|ws, ev: &gpui::MouseMoveEvent, _, cx| {
                    if let Some(ref drag) = ws.tree_scrollbar_drag {
                        let handle = ws.tree_scroll.0.borrow().base_handle.clone();
                        crate::ui::scrollbar::update_drag(drag, ev, &handle);
                        cx.notify();
                    }
                }))
                .on_mouse_up(
                    gpui::MouseButton::Left,
                    cx.listener(|ws, _, _, cx| {
                        ws.tree_scrollbar_drag = None;
                        cx.notify();
                    }),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .min_h(px(0.))
                    .child(tree_list),
            )
            .child(scrollbar)
            .into_any_element()
    }

    fn render_tree_row(
        &self,
        ix: usize,
        entity: &Entity<Workspace>,
        active_path: Option<&Path>,
        t: &RuntimeTheme,
        indent_guides: bool,
    ) -> Stateful<Div> {
        let row = &self.visible_rows[ix];
        let depth = row.depth;
        let is_active = active_path.is_some_and(|ap| ap == row.path);
        let path = row.path.clone();
        let is_dir = row.is_dir;
        let entity = entity.clone();

        let chevron: AnyElement = if row.is_dir {
            svg()
                .path(if row.expanded {
                    IconName::ExpandMore.path()
                } else {
                    IconName::ChevronRight.path()
                })
                .size(px(12.0))
                .text_color(t.text_subtle)
                .into_any_element()
        } else {
            div().size(px(12.0)).into_any_element()
        };

        // Language dot for files; folder chevron only for dirs (no dot)
        let file_indicator: AnyElement = if row.is_dir {
            div().size(px(7.)).into_any_element()
        } else {
            let dot_color = file_icons::language_dot_color(&row.name)
                .map(|v| {
                    let c: gpui::Hsla = gpui::rgba(v).into();
                    c
                })
                .unwrap_or(t.text_muted);
            div()
                .size(px(7.))
                .rounded_full()
                .bg(dot_color)
                .flex_shrink_0()
                .into_any_element()
        };

        // Vertical indent guides
        let guides: Vec<AnyElement> = if indent_guides && depth > 0 {
            (0..depth)
                .map(|d| {
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .w(px(1.0))
                        .left(px(
                            8.0 + d as f32 * TREE_INDENT_W + TREE_INDENT_W * 0.5 - 0.5
                        ))
                        .bg(t.separator)
                        .into_any_element()
                })
                .collect()
        } else {
            vec![]
        };

        h_flex()
            .id(ix)
            .h(px(t.tree_row_h))
            .mx(px(5.))
            .rounded(px(7.))
            .relative()
            .pl(px(8.0 + depth as f32 * TREE_INDENT_W))
            .pr_2()
            .gap_1()
            .font_family(t.ui_family.clone())
            .text_size(px(12.5))
            .text_color(if is_active {
                t.text
            } else if is_dir {
                t.text_subtle
            } else {
                t.text_muted
            })
            .when(is_dir, |el| el.font_weight(gpui::FontWeight::MEDIUM))
            .cursor_pointer()
            .when(is_active, |el| el.bg(t.accent_muted))
            .when(!is_active, |el| {
                el.hover(|s| s.bg(gpui::rgba(0xFFFFFF0F)).text_color(t.text))
            })
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                entity.update(cx, |ws, cx| {
                    if is_dir {
                        ws.toggle_tree_node(&path, cx);
                    } else {
                        ws.open_path(&path, window, cx);
                    }
                });
            })
            .children(guides)
            .child(div().flex_shrink_0().child(chevron))
            .child(file_indicator)
            .child(
                div()
                    .whitespace_nowrap()
                    .flex_shrink_0()
                    .child(row.name.clone()),
            )
    }

    fn render_outline(&self, t: &RuntimeTheme, cx: &mut Context<Self>) -> AnyElement {
        let outline = self.active_outline(cx);
        let t2 = t.clone();
        let entity = cx.entity();

        if outline.items.is_empty() {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .px_3()
                .font_family(t.ui_family.clone())
                .text_size(px(t.font_size_caption))
                .text_color(t.text_muted)
                .child(rust_i18n::t!("sidebar.no_headings").to_string())
                .into_any_element();
        }

        uniform_list("outline", outline.items.len(), move |range, _window, cx| {
            let ws = entity.read(cx);
            range
                .map(|ix| {
                    ws.render_outline_row(ix, &outline, &entity, &t2)
                        .into_any_element()
                })
                .collect::<Vec<_>>()
        })
        .flex_1()
        .into_any_element()
    }

    fn render_outline_row(
        &self,
        ix: usize,
        outline: &Arc<Outline>,
        entity: &Entity<Workspace>,
        t: &RuntimeTheme,
    ) -> impl IntoElement {
        let entry = &outline.items[ix];
        let line = entry.source_line;
        let entity = entity.clone();
        let indent = (entry.depth as f32) * TREE_INDENT_W;

        h_flex()
            .id(ix)
            .h(px(t.tree_row_h))
            .pl(px(8.0 + indent))
            .pr_2()
            .gap_1()
            .font_family(t.ui_family.clone())
            .text_size(px(t.font_size_caption))
            .text_color(if entry.depth == 0 {
                t.text
            } else {
                t.text_muted
            })
            .cursor_pointer()
            .hover(|el| el.bg(t.line_highlight))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                entity.update(cx, |ws, cx| ws.outline_navigate(line, cx));
            })
            .child(
                div()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(SharedString::from(entry.name.clone())),
            )
    }

    /// Returns the outline of the active editor, or empty.
    fn active_outline(&self, cx: &Context<Self>) -> Arc<Outline> {
        let pane = self.pane().read(cx);
        let Some(tab) = pane.active_tab() else {
            return Arc::new(Outline::default());
        };
        let Some(editor) = tab.editor() else {
            return Arc::new(Outline::default());
        };
        Arc::clone(&editor.read(cx).outline)
    }

    pub(crate) fn active_is_markdown(&self, cx: &App) -> bool {
        let pane = self.pane().read(cx);
        pane.active_tab()
            .and_then(|t| t.editor())
            .is_some_and(|e| e.read(cx).is_markdown())
    }

    pub(crate) fn render_right_panel(
        &self,
        t: &RuntimeTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let body: AnyElement = if self.active_is_markdown(cx) {
            self.render_outline(t, cx)
        } else {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .px_3()
                .font_family(t.ui_family.clone())
                .text_size(px(t.font_size_caption))
                .text_color(t.text_muted)
                .child(rust_i18n::t!("sidebar.no_headings").to_string())
                .into_any_element()
        };

        v_flex()
            .w(px(240.))
            .flex_shrink_0()
            .h_full()
            .bg(t.bg_elevated)
            .border_l_1()
            .border_color(t.separator)
            .child(
                div()
                    .px_3()
                    .h(px(30.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .text_size(px(t.font_size_caption))
                    .text_color(t.text_muted)
                    .font_family(t.ui_family.clone())
                    .child(rust_i18n::t!("panel.headings").to_string()),
            )
            .child(body)
    }

    fn render_explorer_header(&self, t: &RuntimeTheme, cx: &mut Context<Self>) -> AnyElement {
        let icon_btn = |id: &'static str, _icon: IconName, t: &RuntimeTheme| {
            div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .size(px(22.0))
                .rounded(px(t.radius_sm))
                .cursor_pointer()
                .hover(|s| s.bg(t.line_highlight))
        };

        h_flex()
            .px(px(14.))
            .h(px(30.0))
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .font_family(t.ui_family.clone())
                    .text_size(px(10.5))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(t.text_muted)
                    .child(rust_i18n::t!("sidebar.explorer").to_string().to_uppercase()),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        icon_btn("explorer-refresh", IconName::Refresh, t)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|ws, _, _, cx| {
                                    ws.refresh_tree(cx);
                                }),
                            )
                            .child(
                                Icon::new(IconName::Refresh)
                                    .size(px(14.0))
                                    .color(t.text_subtle),
                            ),
                    )
                    .child(
                        icon_btn("explorer-collapse", IconName::UnfoldLess, t)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|ws, _, _, cx| {
                                    ws.collapse_tree_all(cx);
                                }),
                            )
                            .child(
                                Icon::new(IconName::UnfoldLess)
                                    .size(px(14.0))
                                    .color(t.text_subtle),
                            ),
                    )
                    .child(
                        icon_btn("explorer-expand", IconName::UnfoldMore, t)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|ws, _, _, cx| {
                                    ws.expand_tree_all(cx);
                                }),
                            )
                            .child(
                                Icon::new(IconName::UnfoldMore)
                                    .size(px(14.0))
                                    .color(t.text_subtle),
                            ),
                    ),
            )
            .into_any_element()
    }
}
