use std::path::Path;

use gpui::{App, Context, Entity, FocusHandle, Render, Window, div, prelude::*};

use faber_core::pane_tree::PaneId;

use crate::editor_view::EditorView;
use crate::project_search_view::ProjectSearchView;
use crate::settings_view::SettingsView;

// ── Tab types (moved from workspace.rs) ──────────────────────────────────────

pub enum TabContent {
    Editor(Entity<EditorView>),
    Settings(Entity<SettingsView>),
    ProjectSearch(Entity<ProjectSearchView>),
}

pub(crate) struct TabMenu {
    pub tab_id: usize,
    pub pos: gpui::Point<gpui::Pixels>,
}

pub struct Tab {
    pub id: usize,
    pub content: TabContent,
}

impl Tab {
    pub(crate) fn editor(&self) -> Option<&Entity<EditorView>> {
        match &self.content {
            TabContent::Editor(e) => Some(e),
            TabContent::Settings(_) | TabContent::ProjectSearch(_) => None,
        }
    }

    /// Returns `(title, dirty)` for the tab strip.
    pub(crate) fn title(&self, cx: &App) -> (String, bool) {
        match &self.content {
            TabContent::Editor(e) => {
                let doc = &e.read(cx).doc;
                let name = doc
                    .path
                    .file_name()
                    .map_or_else(
                        || rust_i18n::t!("editor.untitled").to_string(),
                        |n| n.to_string_lossy().to_string(),
                    );
                (name, doc.dirty)
            }
            TabContent::Settings(_) => (rust_i18n::t!("tab.settings").to_string(), false),
            TabContent::ProjectSearch(_) => (rust_i18n::t!("tab.search").to_string(), false),
        }
    }

    pub(crate) fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.content {
            TabContent::Editor(e) => e.read(cx).focus_handle.clone(),
            TabContent::Settings(s) => s.read(cx).focus_handle.clone(),
            TabContent::ProjectSearch(p) => p.read(cx).focus_handle.clone(),
        }
    }
}

// ── Pane entity ───────────────────────────────────────────────────────────────

/// A single editor pane: an ordered list of tabs with one active.
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

    /// Add a tab and make it active. Returns the new tab's id.
    pub(crate) fn push_tab(&mut self, content: TabContent) -> usize {
        let id = self.next_tab_id;
        self.tabs.push(Tab { id, content });
        self.next_tab_id += 1;
        self.active = Some(self.tabs.len() - 1);
        id
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

    /// Set active index, clamping to the current length.
    /// Passing `None` clears the selection.
    pub(crate) fn set_active(&mut self, ix: Option<usize>) {
        self.active = ix.map(|i| i.min(self.tabs.len().saturating_sub(1)));
    }

    /// Remove the tab at `ix` and remap `active` correctly.
    /// Returns the removed tab, or `None` if out of bounds.
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

    /// Move a tab from `from` to `to` (clamped). Remaps `active` so the
    /// logically-selected tab follows its id.
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
        self.tabs.iter().position(|t| {
            t.editor().is_some_and(|e| e.read(cx).doc.path == path)
        })
    }

    pub(crate) fn find_settings_tab(&self) -> Option<usize> {
        self.tabs
            .iter()
            .position(|t| matches!(t.content, TabContent::Settings(_)))
    }

    pub(crate) fn find_project_search_tab(&self) -> Option<usize> {
        self.tabs
            .iter()
            .position(|t| matches!(t.content, TabContent::ProjectSearch(_)))
    }

    /// All editor entities in this pane (in order).
    pub(crate) fn all_editors(&self) -> impl Iterator<Item = &Entity<EditorView>> {
        self.tabs.iter().filter_map(|t| t.editor())
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
