use std::path::Path;
use std::sync::Arc;

use faber_editor::markdown::OutlineEntry;
use gpui::{
    AnyElement, Context, Div, Entity, IntoElement, ListHorizontalSizingBehavior, MouseButton,
    SharedString, Stateful, div, img, prelude::*, px, svg, uniform_list,
};

use crate::file_icons;
use crate::theme::RuntimeTheme;
use crate::ui::{IconName, h_flex, v_flex};
use crate::workspace::Workspace;

pub const ACTIVITY_BAR_W: f32 = 44.0;
pub const SIDEBAR_PANEL_W: f32 = 240.0;
const TREE_ROW_H: f32 = 24.0;
const TREE_INDENT_W: f32 = 12.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SidebarItemKind {
    Explorer,
    Search,
    Outline,
}

/// One activity-bar icon. Adding an entry to `default_items` is the whole
/// contract for registering a new sidebar panel.
pub struct SidebarItem {
    pub kind: SidebarItemKind,
    pub icon: IconName,
    pub title: &'static str,
}

pub fn default_items() -> Vec<SidebarItem> {
    vec![
        SidebarItem { kind: SidebarItemKind::Explorer, icon: IconName::FileCopy, title: "Explorer" },
        SidebarItem { kind: SidebarItemKind::Search, icon: IconName::Search, title: "Search" },
        SidebarItem { kind: SidebarItemKind::Outline, icon: IconName::Toc, title: "Outline" },
    ]
}

pub struct SidebarState {
    pub open: bool,
    pub active: SidebarItemKind,
    pub width: f32,
}

impl Default for SidebarState {
    fn default() -> Self {
        Self { open: false, active: SidebarItemKind::Explorer, width: SIDEBAR_PANEL_W }
    }
}

impl Workspace {
    pub(crate) fn render_activity_bar(
        &self,
        t: &RuntimeTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .w(px(ACTIVITY_BAR_W))
            .flex_shrink_0()
            .h_full()
            .items_center()
            .py_2()
            .gap_1()
            .bg(t.bg_elevated)
            .border_r_1()
            .border_color(t.separator)
            .children(self.sidebar_items.iter().map(|item| {
                let kind = item.kind;
                let is_active = self.sidebar.open && self.sidebar.active == kind;
                div()
                    .id(item.title)
                    .group("activity-item")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(34.0))
                    .rounded(px(t.radius_md))
                    .when(is_active, |el| el.bg(t.bg))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |ws, _, _, cx| ws.on_activity_click(kind, cx)),
                    )
                    .child(
                        svg()
                            .path(item.icon.path())
                            .size(px(21.0))
                            .text_color(if is_active { t.text } else { t.text_subtle })
                            .group_hover("activity-item", |s| s.text_color(t.text)),
                    )
            }))
    }

    pub(crate) fn render_sidebar_panel(
        &self,
        t: &RuntimeTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let title = match self.sidebar.active {
            SidebarItemKind::Explorer => rust_i18n::t!("sidebar.explorer").to_string(),
            SidebarItemKind::Search => rust_i18n::t!("sidebar.search").to_string(),
            SidebarItemKind::Outline => rust_i18n::t!("sidebar.outline").to_string(),
        };

        let header = h_flex()
            .px_3()
            .h(px(30.0))
            .flex_shrink_0()
            .text_size(px(t.font_size_caption))
            .text_color(t.text_muted)
            .font_family(t.ui_family.clone())
            .child(title);

        let body: AnyElement = match self.sidebar.active {
            SidebarItemKind::Explorer => self.render_explorer(t, cx),
            SidebarItemKind::Search => self.render_search_placeholder(t),
            SidebarItemKind::Outline => self.render_outline(t, cx),
        };

        v_flex()
            .w(px(self.sidebar.width))
            .flex_shrink_0()
            .h_full()
            .bg(t.bg_elevated)
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
        // Find the index of the widest-looking row so the list can self-size horizontally.
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
                .on_mouse_up(gpui::MouseButton::Left, cx.listener(|ws, _, _, cx| {
                    ws.tree_scrollbar_drag = None;
                    cx.notify();
                }))
            })
            .child(div().flex().flex_col().flex_1().min_w(px(0.)).min_h(px(0.)).child(tree_list))
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

        let icon = if row.is_dir {
            file_icons::icon_for_folder(&row.name, row.expanded)
        } else {
            file_icons::icon_for_file(&row.name)
        };

        // Vertical indent guide lines — one thin line per ancestor depth level.
        // Each line is absolutely positioned at the center of that depth's indent column.
        let guides: Vec<AnyElement> = if indent_guides && depth > 0 {
            (0..depth)
                .map(|d| {
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .w(px(1.0))
                        .left(px(8.0 + d as f32 * TREE_INDENT_W + TREE_INDENT_W * 0.5 - 0.5))
                        .bg(t.separator)
                        .into_any_element()
                })
                .collect()
        } else {
            vec![]
        };

        h_flex()
            .id(ix)
            .h(px(TREE_ROW_H))
            .relative()
            .pl(px(8.0 + depth as f32 * TREE_INDENT_W))
            .pr_2()
            .gap_1()
            .font_family(t.ui_family.clone())
            .text_size(px(t.font_size_caption))
            .text_color(if is_active || is_dir { t.text } else { t.text_muted })
            .cursor_pointer()
            .when(is_active, |el| el.bg(t.selection))
            .hover(|el| el.bg(t.line_highlight))
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
            .child(img(icon).size(px(16.0)).flex_shrink_0())
            .child(div().whitespace_nowrap().flex_shrink_0().child(row.name.clone()))
    }

    fn render_outline(&self, t: &RuntimeTheme, cx: &mut Context<Self>) -> AnyElement {
        let outline = self.active_outline(cx);
        let t2 = t.clone();
        let entity = cx.entity();

        if outline.is_empty() {
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

        uniform_list(
            "outline",
            outline.len(),
            move |range, _window, cx| {
                let ws = entity.read(cx);
                range
                    .map(|ix| ws.render_outline_row(ix, &outline, &entity, &t2).into_any_element())
                    .collect::<Vec<_>>()
            },
        )
        .flex_1()
        .into_any_element()
    }

    fn render_outline_row(
        &self,
        ix: usize,
        outline: &Arc<Vec<OutlineEntry>>,
        entity: &Entity<Workspace>,
        t: &RuntimeTheme,
    ) -> impl IntoElement {
        let entry = &outline[ix];
        let line = entry.source_line;
        let entity = entity.clone();
        let indent = (entry.level.saturating_sub(1)) as f32 * TREE_INDENT_W;

        h_flex()
            .id(ix)
            .h(px(TREE_ROW_H))
            .pl(px(8.0 + indent))
            .pr_2()
            .gap_1()
            .font_family(t.ui_family.clone())
            .text_size(px(t.font_size_caption))
            .text_color(if entry.level == 1 { t.text } else { t.text_muted })
            .cursor_pointer()
            .hover(|el| el.bg(t.line_highlight))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                entity.update(cx, |ws, cx| ws.outline_navigate(line, cx));
            })
            .child(
                div()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(SharedString::from(entry.text.clone())),
            )
    }

    /// Returns the outline of the active markdown editor, or empty.
    fn active_outline(&self, cx: &Context<Self>) -> Arc<Vec<OutlineEntry>> {
        let Some(i) = self.active else { return Arc::new(vec![]); };
        let Some(tab) = self.tabs.get(i) else { return Arc::new(vec![]); };
        let Some(editor) = tab.editor() else { return Arc::new(vec![]); };
        Arc::clone(&editor.read(cx).outline)
    }

    fn render_search_placeholder(&self, t: &RuntimeTheme) -> AnyElement {
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .px_3()
            .font_family(t.ui_family.clone())
            .text_size(px(t.font_size_caption))
            .text_color(t.text_muted)
            .child(rust_i18n::t!("sidebar.search_coming_soon").to_string())
            .into_any_element()
    }

    pub(crate) fn render_sidebar_resize_handle(
        &self,
        t: &RuntimeTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("sidebar-resize-handle")
            .w(px(4.0))
            .h_full()
            .flex_shrink_0()
            .bg(t.separator)
            .cursor_col_resize()
            .hover(|el| el.bg(t.accent))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|ws, _, _, cx| {
                    ws.sidebar_resizing = true;
                    cx.notify();
                }),
            )
    }

    fn on_activity_click(&mut self, kind: SidebarItemKind, cx: &mut Context<Self>) {
        if self.sidebar.open && self.sidebar.active == kind {
            self.sidebar.open = false;
        } else {
            self.sidebar.open = true;
            self.sidebar.active = kind;
        }
        cx.notify();
    }
}
