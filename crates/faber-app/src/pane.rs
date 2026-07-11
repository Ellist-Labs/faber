use std::path::Path;

use gpui::{AnyView, App, Context, Entity, FocusHandle, Render, Window, div, prelude::*};

use faber_core::pane_tree::PaneId;

use crate::editor_view::EditorView;
use crate::panels::diagnostics_panel::DiagnosticsPanel;
use crate::project_search_view::ProjectSearchView;
use crate::settings_view::SettingsView;

// ── Tab types ────────────────────────────────────────────────────────────────

/// Discriminant used to identify tab kind without exposing concrete types.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TabKind {
    Editor,
    Settings,
    ProjectSearch,
    Problems,
}

/// Type-erased tab content: renderable view + focus handle + title closure.
/// Adding a new panel type only requires a new `TabItem::from_*` constructor
/// — no existing match arms need updating.
pub struct TabItem {
    pub focus_handle: FocusHandle,
    pub view: AnyView,
    pub(crate) kind: TabKind,
    #[allow(clippy::type_complexity)]
    title_fn: Box<dyn Fn(&App) -> (String, bool) + Send + Sync>,
}

impl TabItem {
    pub(crate) fn title(&self, cx: &App) -> (String, bool) {
        (self.title_fn)(cx)
    }

    pub(crate) fn from_editor(entity: Entity<EditorView>, cx: &App) -> Self {
        let focus = entity.read(cx).focus_handle.clone();
        let view = AnyView::from(entity.clone());
        TabItem {
            focus_handle: focus,
            view,
            kind: TabKind::Editor,
            title_fn: Box::new(move |cx| {
                let doc = &entity.read(cx).doc;
                let name = doc.path.file_name().map_or_else(
                    || rust_i18n::t!("editor.untitled").to_string(),
                    |n| n.to_string_lossy().to_string(),
                );
                (name, doc.dirty)
            }),
        }
    }

    pub(crate) fn from_settings(entity: Entity<SettingsView>, cx: &App) -> Self {
        let focus = entity.read(cx).focus_handle.clone();
        let view = AnyView::from(entity);
        TabItem {
            focus_handle: focus,
            view,
            kind: TabKind::Settings,
            title_fn: Box::new(|_cx| (rust_i18n::t!("tab.settings").to_string(), false)),
        }
    }

    pub(crate) fn from_project_search(entity: Entity<ProjectSearchView>, cx: &App) -> Self {
        let focus = entity.read(cx).focus_handle.clone();
        let view = AnyView::from(entity);
        TabItem {
            focus_handle: focus,
            view,
            kind: TabKind::ProjectSearch,
            title_fn: Box::new(|_cx| (rust_i18n::t!("tab.search").to_string(), false)),
        }
    }

    pub(crate) fn from_problems(entity: Entity<DiagnosticsPanel>, cx: &App) -> Self {
        let focus = entity.read(cx).focus_handle.clone();
        let view = AnyView::from(entity);
        TabItem {
            focus_handle: focus,
            view,
            kind: TabKind::Problems,
            title_fn: Box::new(|_cx| (rust_i18n::t!("tab.problems").to_string(), false)),
        }
    }
}

pub(crate) struct TabMenu {
    pub tab_id: usize,
    pub pos: gpui::Point<gpui::Pixels>,
}

pub struct Tab {
    pub id: usize,
    pub content: TabItem,
    pub(crate) editor: Option<Entity<EditorView>>,
    pub(crate) project_search: Option<Entity<ProjectSearchView>>,
    pub(crate) problems: Option<Entity<DiagnosticsPanel>>,
}

impl Tab {
    pub(crate) fn editor(&self) -> Option<&Entity<EditorView>> {
        self.editor.as_ref()
    }

    pub(crate) fn title(&self, cx: &App) -> (String, bool) {
        self.content.title(cx)
    }

    pub(crate) fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.content.focus_handle.clone()
    }
}

// ── Pane entity ───────────────────────────────────────────────────────────────

#[allow(dead_code)]
pub struct Pane {
    pub id: PaneId,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active: Option<usize>,
    pub(crate) next_tab_id: usize,
    pub(crate) tab_menu: Option<TabMenu>,
    pub(crate) focus_handle: FocusHandle,
}

impl Pane {
    pub fn new(id: PaneId, cx: &mut Context<Self>) -> Self {
        Self {
            id,
            tabs: Vec::new(),
            active: None,
            next_tab_id: 0,
            tab_menu: None,
            focus_handle: cx.focus_handle(),
        }
    }

    // ── Tab data operations ───────────────────────────────────────────────────

    pub(crate) fn push_editor_tab(&mut self, entity: Entity<EditorView>, cx: &App) -> usize {
        let id = self.next_tab_id;
        let content = TabItem::from_editor(entity.clone(), cx);
        self.tabs.push(Tab {
            id,
            content,
            editor: Some(entity),
            project_search: None,
            problems: None,
        });
        self.next_tab_id += 1;
        self.active = Some(self.tabs.len() - 1);
        id
    }

    pub(crate) fn push_settings_tab(&mut self, entity: Entity<SettingsView>, cx: &App) -> usize {
        let id = self.next_tab_id;
        let content = TabItem::from_settings(entity, cx);
        self.tabs.push(Tab {
            id,
            content,
            editor: None,
            project_search: None,
            problems: None,
        });
        self.next_tab_id += 1;
        self.active = Some(self.tabs.len() - 1);
        id
    }

    pub(crate) fn push_project_search_tab(
        &mut self,
        entity: Entity<ProjectSearchView>,
        cx: &App,
    ) -> usize {
        let id = self.next_tab_id;
        let content = TabItem::from_project_search(entity.clone(), cx);
        self.tabs.push(Tab {
            id,
            content,
            editor: None,
            project_search: Some(entity),
            problems: None,
        });
        self.next_tab_id += 1;
        self.active = Some(self.tabs.len() - 1);
        id
    }

    pub(crate) fn push_problems_tab(
        &mut self,
        entity: Entity<DiagnosticsPanel>,
        cx: &App,
    ) -> usize {
        let id = self.next_tab_id;
        let content = TabItem::from_problems(entity.clone(), cx);
        self.tabs.push(Tab {
            id,
            content,
            editor: None,
            project_search: None,
            problems: Some(entity),
        });
        self.next_tab_id += 1;
        self.active = Some(self.tabs.len() - 1);
        id
    }

    pub(crate) fn push_tab_raw(&mut self, mut tab: Tab) {
        tab.id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(tab);
        self.active = Some(self.tabs.len() - 1);
    }

    pub(crate) fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub(crate) fn active_tab(&self) -> Option<&Tab> {
        self.active.and_then(|ix| self.tabs.get(ix))
    }

    pub(crate) fn tab_at(&self, ix: usize) -> Option<&Tab> {
        self.tabs.get(ix)
    }

    pub(crate) fn tab_by_id(&self, id: usize) -> Option<(usize, &Tab)> {
        self.tabs.iter().enumerate().find(|(_, t)| t.id == id)
    }

    pub(crate) fn set_active(&mut self, ix: Option<usize>) {
        self.active = ix.map(|i| i.min(self.tabs.len().saturating_sub(1)));
    }

    pub(crate) fn remove_tab(&mut self, ix: usize) -> Option<Tab> {
        if ix >= self.tabs.len() {
            return None;
        }
        let tab = self.tabs.remove(ix);
        self.active = if self.tabs.is_empty() {
            None
        } else {
            Some(match self.active {
                Some(a) if a > ix => a - 1,
                Some(a) => a.min(self.tabs.len() - 1),
                None => 0,
            })
        };
        Some(tab)
    }

    #[allow(dead_code)]
    pub(crate) fn reorder_tab(&mut self, from: usize, to: usize) {
        if from == to || from >= self.tabs.len() {
            return;
        }
        let to = to.min(self.tabs.len() - 1);
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        // Remap active: the active tab's id follows it.
        if let Some(a) = self.active {
            self.active = Some(if a == from {
                to
            } else if from < to && a > from && a <= to {
                a - 1
            } else if from > to && a >= to && a < from {
                a + 1
            } else {
                a
            });
        }
    }

    // ── Search helpers ────────────────────────────────────────────────────────

    pub(crate) fn find_editor_by_path(&self, path: &Path, cx: &App) -> Option<usize> {
        self.tabs
            .iter()
            .position(|t| t.editor().is_some_and(|e| e.read(cx).doc.path == path))
    }

    pub(crate) fn find_settings_tab(&self) -> Option<usize> {
        self.tabs
            .iter()
            .position(|t| t.content.kind == TabKind::Settings)
    }

    pub(crate) fn find_project_search_tab(&self) -> Option<usize> {
        self.tabs
            .iter()
            .position(|t| t.content.kind == TabKind::ProjectSearch)
    }

    #[allow(dead_code)]
    pub(crate) fn find_problems_tab(&self) -> Option<usize> {
        self.tabs
            .iter()
            .position(|t| t.content.kind == TabKind::Problems)
    }

    pub(crate) fn all_editors(&self) -> impl Iterator<Item = &Entity<EditorView>> {
        self.tabs.iter().filter_map(|t| t.editor())
    }

    pub(crate) fn all_problems_panels(&self) -> impl Iterator<Item = &Entity<DiagnosticsPanel>> {
        self.tabs.iter().filter_map(|t| t.problems.as_ref())
    }
}

/// `Pane` implements `Render` so it can be used as a gpui view.
/// The actual pane content (tab strip + editor) is rendered by `Workspace`;
/// this stub keeps the entity valid.
impl Render for Pane {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
