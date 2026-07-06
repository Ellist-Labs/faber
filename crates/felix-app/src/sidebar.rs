use gpui::{
    AnyElement, Context, Div, Entity, IntoElement, MouseButton, Stateful, div, prelude::*, px,
    uniform_list,
};

use crate::theme::RuntimeTheme;
use crate::ui::{h_flex, v_flex};
use crate::workspace::Workspace;

pub const ACTIVITY_BAR_W: f32 = 40.0;
pub const SIDEBAR_PANEL_W: f32 = 240.0;
const TREE_ROW_H: f32 = 24.0;
const TREE_INDENT_W: f32 = 12.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SidebarItemKind {
    Explorer,
    Search,
}

/// One activity-bar icon. Adding an entry to `default_items` is the whole
/// contract for registering a new sidebar panel.
pub struct SidebarItem {
    pub kind: SidebarItemKind,
    pub icon: &'static str,
    pub title: &'static str,
}

pub fn default_items() -> Vec<SidebarItem> {
    vec![
        SidebarItem { kind: SidebarItemKind::Explorer, icon: "▤", title: "Explorer" },
        SidebarItem { kind: SidebarItemKind::Search, icon: "⌕", title: "Search" },
    ]
}

pub struct SidebarState {
    pub open: bool,
    pub active: SidebarItemKind,
}

impl Default for SidebarState {
    fn default() -> Self {
        Self { open: false, active: SidebarItemKind::Explorer }
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
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(30.0))
                    .rounded(px(t.radius_md))
                    .text_size(px(t.font_size_heading))
                    .text_color(if is_active { t.text } else { t.text_subtle })
                    .when(is_active, |el| el.bg(t.bg))
                    .cursor_pointer()
                    .hover(|el| el.text_color(t.text))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |ws, _, _, cx| ws.on_activity_click(kind, cx)),
                    )
                    .child(item.icon)
            }))
    }

    pub(crate) fn render_sidebar_panel(
        &self,
        t: &RuntimeTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let title = self
            .sidebar_items
            .iter()
            .find(|i| i.kind == self.sidebar.active)
            .map_or("", |i| i.title);

        let header = h_flex()
            .px_3()
            .h(px(30.0))
            .flex_shrink_0()
            .text_size(px(t.font_size_caption))
            .text_color(t.text_muted)
            .font_family(t.ui_family.clone())
            .child(title.to_uppercase());

        let body: AnyElement = match self.sidebar.active {
            SidebarItemKind::Explorer => self.render_explorer(t, cx),
            SidebarItemKind::Search => self.render_search_placeholder(t),
        };

        v_flex()
            .w(px(SIDEBAR_PANEL_W))
            .flex_shrink_0()
            .h_full()
            .bg(t.bg_elevated)
            .border_r_1()
            .border_color(t.separator)
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
                .child("No folder open")
                .child("Use File › Open Folder…")
                .into_any_element();
        }

        let entity = cx.entity();
        let t2 = t.clone();
        uniform_list(
            "file-tree",
            self.visible_rows.len(),
            move |range: std::ops::Range<usize>, _window, cx| {
                let entity2 = entity.clone();
                let ws = entity.read(cx);
                range
                    .map(|ix| ws.render_tree_row(ix, &entity2, &t2).into_any_element())
                    .collect::<Vec<AnyElement>>()
            },
        )
        .flex_1()
        .into_any_element()
    }

    fn render_tree_row(
        &self,
        ix: usize,
        entity: &Entity<Workspace>,
        t: &RuntimeTheme,
    ) -> Stateful<Div> {
        let row = &self.visible_rows[ix];
        let path = row.path.clone();
        let is_dir = row.is_dir;
        let entity = entity.clone();

        let chevron = if row.is_dir {
            if row.expanded { "▾" } else { "▸" }
        } else {
            " "
        };

        h_flex()
            .id(ix)
            .h(px(TREE_ROW_H))
            .pl(px(8.0 + row.depth as f32 * TREE_INDENT_W))
            .pr_2()
            .gap_1()
            .font_family(t.ui_family.clone())
            .text_size(px(t.font_size_caption))
            .text_color(if is_dir { t.text } else { t.text_muted })
            .cursor_pointer()
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
            .child(div().w(px(12.0)).flex_shrink_0().text_color(t.text_subtle).child(chevron))
            .child(div().overflow_hidden().text_ellipsis().child(row.name.clone()))
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
            .child("Project search coming soon")
            .into_any_element()
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
