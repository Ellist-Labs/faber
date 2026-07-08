use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use faber_settings::AutoSave;

use faber_editor::{
    buffer::Document,
    file_index::{self, FileIndexSnapshot},
    project::{FileTree, VisibleRow},
    save::save,
};
use gpui::{
    AnyElement, App, ClipboardItem, Context, Div, Entity, FocusHandle, Focusable, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, PathPromptOptions, Pixels, Point, PromptLevel,
    Render, ScrollStrategy, SharedString, Stateful, Task, UniformListScrollHandle, Window,
    anchored, deferred, div, img, prelude::*, px, svg,
};

use crate::editor_view::{EditorEvent, EditorView};
use crate::file_finder::FileFinderView;
use crate::project_search_view::ProjectSearchView;
use crate::settings_view::{SettingsStore, SettingsView};
use crate::sidebar::{SidebarItem, SidebarItemKind, SidebarState, default_items};
use crate::theme::{ActiveTheme, RuntimeTheme};
use crate::ui::{IconName, ScrollbarDrag, h_flex, v_flex};
use crate::ui::scrollbar::update_drag;
use crate::welcome_view::render_welcome;
use crate::{
    CloseFile, CloseFolder, CloseTab, NewFile, NextTab, OpenFile, OpenFileFinder,
    OpenFileFinderPreview, OpenFolder, OpenProjectSearch, OpenSettings, PrevTab, ProjectRoot,
    Quit, SaveFile, ToggleBottomPanel, ToggleRightPanel, ToggleSidebar,
};

/// Rescan the file index when the cached snapshot is older than this.
const INDEX_STALE_AFTER: Duration = Duration::from_secs(5);

/// Background-scanned project file index, shared with the file finder.
#[derive(Default)]
pub(crate) struct FileIndexState {
    normal: Option<Arc<FileIndexSnapshot>>,
    full: Option<Arc<FileIndexSnapshot>>,
    normal_scanned_at: Option<Instant>,
    full_scanned_at: Option<Instant>,
    scanning_normal: bool,
    scanning_full: bool,
}

pub enum TabContent {
    Editor(Entity<EditorView>),
    Settings(Entity<SettingsView>),
    ProjectSearch(Entity<ProjectSearchView>),
}

struct TabMenu {
    tab_id: usize,
    pos: Point<Pixels>,
}

type MenuClickFn = Box<dyn Fn(&MouseDownEvent, &mut Window, &mut App)>;

/// A single row in the tab context menu. Disabled rows render dimmed and
/// ignore clicks.
#[derive(IntoElement)]
struct ContextMenuItem {
    label: SharedString,
    enabled: bool,
    on_click: MenuClickFn,
}

impl RenderOnce for ContextMenuItem {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.theme().clone();
        div()
            .id(self.label.clone())
            .flex()
            .items_center()
            .px(px(t.sp5))
            .py(px(t.sp2))
            .text_color(if self.enabled { t.text } else { t.text_subtle })
            .text_size(px(t.font_size_caption))
            .font_family(t.ui_family.clone())
            .when(self.enabled, |el| el.cursor_pointer().hover(|s| s.bg(t.line_highlight)))
            .when(self.enabled, |el| {
                el.on_mouse_down(MouseButton::Left, move |e, w, cx| {
                    (self.on_click)(e, w, cx)
                })
            })
            .child(self.label)
    }
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

    /// (title, dirty) for the tab strip.
    fn title(&self, cx: &App) -> (String, bool) {
        match &self.content {
            TabContent::Editor(e) => {
                let doc = &e.read(cx).doc;
                (Workspace::doc_display_name(doc), doc.dirty)
            }
            TabContent::Settings(_) => (rust_i18n::t!("tab.settings").to_string(), false),
            TabContent::ProjectSearch(_) => (rust_i18n::t!("tab.search").to_string(), false),
        }
    }

    fn tab_focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.content {
            TabContent::Editor(e) => e.read(cx).focus_handle.clone(),
            TabContent::Settings(s) => s.read(cx).focus_handle.clone(),
            TabContent::ProjectSearch(p) => p.read(cx).focus_handle.clone(),
        }
    }
}

pub struct Workspace {
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active: Option<usize>,
    next_tab_id: usize,
    root_folder: Option<PathBuf>,
    focus_handle: FocusHandle,
    pub(crate) sidebar_items: Vec<SidebarItem>,
    pub(crate) sidebar: SidebarState,
    pub(crate) sidebar_resizing: bool,
    pub(crate) tree: Option<FileTree>,
    pub(crate) visible_rows: Vec<VisibleRow>,
    pub(crate) tree_scroll: UniformListScrollHandle,
    pub(crate) tree_scrollbar_drag: Option<ScrollbarDrag>,
    pub(crate) bottom_open: bool,
    pub(crate) right_open: bool,
    tab_menu: Option<TabMenu>,
    /// Bumped on every edit; a debounced auto-save fires only if no newer
    /// edit arrived while its timer slept.
    autosave_generation: u64,
    file_index: FileIndexState,
    file_finder: Option<Entity<FileFinderView>>,
}

impl Workspace {
    pub fn new(paths: &[String], window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut ws = Self {
            tabs: Vec::new(),
            active: None,
            next_tab_id: 0,
            root_folder: None,
            focus_handle: cx.focus_handle(),
            sidebar_items: default_items(),
            sidebar: SidebarState::default(),
            sidebar_resizing: false,
            tree: None,
            visible_rows: Vec::new(),
            tree_scroll: UniformListScrollHandle::new(),
            tree_scrollbar_drag: None,
            bottom_open: false,
            right_open: false,
            tab_menu: None,
            autosave_generation: 0,
            file_index: FileIndexState::default(),
            file_finder: None,
        };
        for path in paths {
            let editor = cx.new(|cx| EditorView::new(path, cx));
            ws.push_editor_tab(editor, cx);
        }
        cx.observe_window_activation(window, |ws, window, cx| {
            let auto_save = cx.global::<SettingsStore>().0.auto_save;
            if !window.is_window_active()
                && matches!(auto_save, AutoSave::OnWindowChange | AutoSave::OnFocusChange)
            {
                ws.save_all_dirty(cx);
            }
        })
        .detach();
        ws
    }

    fn push_editor_tab(&mut self, editor: Entity<EditorView>, cx: &mut Context<Self>) {
        cx.subscribe(&editor, |ws, _, _: &EditorEvent, cx| ws.on_editor_edited(cx)).detach();
        self.tabs.push(Tab { id: self.next_tab_id, content: TabContent::Editor(editor) });
        self.next_tab_id += 1;
        self.active = Some(self.tabs.len() - 1);
    }

    // ── auto-save ──────────────────────────────────────────────────────────────

    fn on_editor_edited(&mut self, cx: &mut Context<Self>) {
        let settings = &cx.global::<SettingsStore>().0;
        if settings.auto_save != AutoSave::AfterDelay {
            return;
        }
        let delay = Duration::from_millis(settings.auto_save_delay_ms);
        self.autosave_generation += 1;
        let generation = self.autosave_generation;
        cx.spawn(async move |ws, cx| {
            cx.background_executor().timer(delay).await;
            ws.update(cx, |ws, cx| {
                if ws.autosave_generation == generation {
                    ws.save_all_dirty(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Navigate the active editor to `line`, moving the cursor and scrolling.
    pub(crate) fn outline_navigate(&mut self, line: usize, cx: &mut Context<Self>) {
        if let Some(editor) = self.active.and_then(|i| self.tabs.get(i)).and_then(|t| t.editor()) {
            editor.update(cx, |ev, _cx| {
                let char_idx = ev.line_starts.get(line).copied().unwrap_or(0);
                ev.sel.head = char_idx;
                ev.sel.anchor = char_idx;
                ev.scroll_handle.scroll_to_item(line, gpui::ScrollStrategy::Top);
            });
        }
    }

    /// Saves every dirty document that has a path; untitled docs are skipped
    /// (VS Code behavior). Never touches content or history.
    fn save_all_dirty(&mut self, cx: &mut Context<Self>) {
        let editors: Vec<Entity<EditorView>> =
            self.tabs.iter().filter_map(|t| t.editor()).cloned().collect();
        for editor in editors {
            Self::save_doc_now(&editor, cx);
        }
    }

    fn save_doc_now(editor: &Entity<EditorView>, cx: &mut App) {
        editor.update(cx, |ed, cx| {
            if ed.doc.dirty && !ed.doc.is_untitled() && save(&ed.doc.rope, &ed.doc.path).is_ok() {
                ed.doc.mark_saved();
                cx.notify();
            }
        });
    }

    pub fn open_path(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let existing = self
            .tabs
            .iter()
            .position(|tab| tab.editor().is_some_and(|e| e.read(cx).doc.path == path));
        if let Some(ix) = existing {
            self.activate_tab(ix, window, cx);
            return;
        }
        let path_str = path.to_string_lossy().to_string();
        let editor = cx.new(|cx| EditorView::new(&path_str, cx));
        self.push_editor_tab(editor, cx);
        self.activate_tab(self.tabs.len() - 1, window, cx);
    }

    pub fn activate_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        // Leaving a tab counts as a focus change for auto-save purposes.
        if cx.global::<SettingsStore>().0.auto_save == AutoSave::OnFocusChange
            && let Some(prev) = self.active.filter(|&prev| prev != ix)
                && let Some(editor) = self.tabs.get(prev).and_then(|t| t.editor()).cloned() {
                    Self::save_doc_now(&editor, cx);
                }
        self.active = Some(ix);
        self.focus_active(window, cx);
        cx.notify();
        // Reveal the newly active file in the explorer tree.
        let path = self.active_editor_path(cx);
        if let Some(path) = path {
            self.reveal_in_tree(&path, cx);
        }
    }

    pub fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.tabs.remove(ix);
        self.active = if self.tabs.is_empty() {
            None
        } else {
            Some(match self.active {
                Some(a) if a > ix => a - 1,
                Some(a) => a.min(self.tabs.len() - 1),
                None => 0,
            })
        };
        self.focus_active(window, cx);
        cx.notify();
    }

    fn close_tab_by_id(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.tabs.iter().position(|t| t.id == id) {
            self.close_tab(ix, window, cx);
        }
    }

    pub fn focus_active(&self, window: &mut Window, cx: &App) {
        match self.active.and_then(|ix| self.tabs.get(ix)) {
            Some(tab) => window.focus(&tab.tab_focus_handle(cx)),
            None => window.focus(&self.focus_handle),
        }
    }

    fn doc_display_name(doc: &Document) -> String {
        doc.path
            .file_name()
            .map_or_else(|| rust_i18n::t!("editor.untitled").to_string(), |n| n.to_string_lossy().to_string())
    }

    /// Returns the file path of the active editor tab, or `None` for untitled/Settings tabs.
    pub(crate) fn active_editor_path(&self, cx: &App) -> Option<PathBuf> {
        let ix = self.active?;
        let tab = self.tabs.get(ix)?;
        let editor = tab.editor()?;
        let path = editor.read(cx).doc.path.clone();
        if path.as_os_str().is_empty() { None } else { Some(path) }
    }

    /// Expand the tree so `path` is visible, scroll the explorer to it, and notify.
    /// No-op when no folder is open or `path` is outside the root.
    pub(crate) fn reveal_in_tree(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(root) = self.root_folder.clone() else { return; };
        if !path.starts_with(&root) { return; }
        let rows = if let Some(tree) = &mut self.tree {
            if tree.reveal(path).is_err() { return; }
            Some(tree.visible())
        } else {
            None
        };
        if let Some(rows) = rows {
            let scroll_ix = rows.iter().position(|r| r.path == path);
            self.visible_rows = rows;
            if let Some(ix) = scroll_ix {
                self.tree_scroll.scroll_to_item(ix, ScrollStrategy::Top);
            }
            cx.notify();
        }
    }

    // ── folder / explorer ──────────────────────────────────────────────────────

    fn set_root_folder(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        match FileTree::new(folder.clone()) {
            Ok(tree) => {
                self.visible_rows = tree.visible();
                self.tree = Some(tree);
                self.root_folder = Some(folder.clone());
                self.sidebar.open = true;
                self.sidebar.active = SidebarItemKind::Explorer;
                cx.set_global(ProjectRoot(Some(folder)));
                self.file_index = FileIndexState::default();
                self.kick_index_scan(false, cx);
            }
            Err(err) => eprintln!("faber: can't open folder {}: {err}", folder.display()),
        }
        cx.notify();
    }

    // ── file index / finder ────────────────────────────────────────────────────

    pub(crate) fn root_folder(&self) -> Option<&PathBuf> {
        self.root_folder.as_ref()
    }

    /// Best snapshot for the requested mode. Falls back to the normal snapshot
    /// while the full scan is still running (stale-while-revalidate).
    pub(crate) fn index_snapshot(&self, include_ignored: bool) -> Option<Arc<FileIndexSnapshot>> {
        if include_ignored {
            self.file_index.full.clone().or_else(|| self.file_index.normal.clone())
        } else {
            self.file_index.normal.clone()
        }
    }

    /// Lazily scan the gitignored-included index the first time it's needed.
    pub(crate) fn ensure_full_index(&mut self, cx: &mut Context<Self>) {
        if self.file_index.full.is_none() {
            self.kick_index_scan(true, cx);
        }
    }

    fn kick_index_scan(&mut self, include_ignored: bool, cx: &mut Context<Self>) {
        let Some(root) = self.root_folder.clone() else { return };
        let scanning = if include_ignored {
            &mut self.file_index.scanning_full
        } else {
            &mut self.file_index.scanning_normal
        };
        if *scanning {
            return;
        }
        *scanning = true;
        cx.spawn(async move |ws, cx| {
            let snapshot = cx
                .background_executor()
                .spawn(async move { Arc::new(file_index::scan(&root, include_ignored)) })
                .await;
            ws.update(cx, |ws, cx| {
                if include_ignored {
                    ws.file_index.full = Some(snapshot);
                    ws.file_index.scanning_full = false;
                    ws.file_index.full_scanned_at = Some(Instant::now());
                } else {
                    ws.file_index.normal = Some(snapshot);
                    ws.file_index.scanning_normal = false;
                    ws.file_index.normal_scanned_at = Some(Instant::now());
                }
                if let Some(finder) = &ws.file_finder {
                    finder.update(cx, |f, cx| f.on_index_updated(cx));
                }
            })
            .ok();
        })
        .detach();
    }

    /// Rescan if the cached index is stale; the finder shows the cached list
    /// instantly and re-filters when the fresh scan lands.
    fn revalidate_index(&mut self, cx: &mut Context<Self>) {
        let normal_stale = self
            .file_index
            .normal_scanned_at
            .is_none_or(|at| at.elapsed() > INDEX_STALE_AFTER);
        let full_stale = self
            .file_index
            .full_scanned_at
            .is_none_or(|at| at.elapsed() > INDEX_STALE_AFTER);
        if normal_stale {
            self.kick_index_scan(false, cx);
        }
        if full_stale && self.file_index.full.is_some() {
            self.kick_index_scan(true, cx);
        }
    }

    fn open_file_finder(&mut self, preview: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(finder) = &self.file_finder {
            // Already open: cmd-p again dismisses (Zed behaviour); the
            // preview variant just enables the preview pane.
            if preview {
                finder.update(cx, |f, cx| f.enable_preview(cx));
                window.focus(&finder.read(cx).focus_handle);
            } else {
                self.close_file_finder(window, cx);
            }
            return;
        }
        self.revalidate_index(cx);
        let ws = cx.entity().downgrade();
        let finder = cx.new(|cx| FileFinderView::new(ws, preview, cx));
        window.focus(&finder.read(cx).focus_handle);
        self.file_finder = Some(finder);
        cx.notify();
    }

    pub(crate) fn close_file_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_finder.take().is_some() {
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    fn on_open_file_finder(&mut self, _: &OpenFileFinder, window: &mut Window, cx: &mut Context<Self>) {
        self.open_file_finder(false, window, cx);
    }

    fn on_open_file_finder_preview(
        &mut self,
        _: &OpenFileFinderPreview,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_file_finder(true, window, cx);
    }

    pub(crate) fn toggle_tree_node(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(tree) = &mut self.tree {
            match tree.toggle(path) {
                Ok(()) => self.visible_rows = tree.visible(),
                Err(err) => eprintln!("faber: can't read {}: {err}", path.display()),
            }
        }
        cx.notify();
    }

    // ── save / close flows ─────────────────────────────────────────────────────

    /// Save the given editor's document. Untitled docs get a native Save As
    /// dialog; the task resolves to whether the document ended up saved.
    fn save_editor(
        &self,
        editor: &Entity<EditorView>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        if editor.read(cx).doc.is_untitled() {
            let dir = self
                .root_folder
                .clone()
                .or_else(|| std::env::var("HOME").ok().map(PathBuf::from))
                .unwrap_or_else(|| PathBuf::from("."));
            let rx = cx.prompt_for_new_path(&dir, Some("untitled.txt"));
            let editor = editor.clone();
            cx.spawn_in(window, async move |_, cx| {
                let Ok(Ok(Some(path))) = rx.await else { return false };
                editor
                    .update(cx, |ed, cx| {
                        ed.doc.assign_path(path, &ed.registry);
                        let ok = save(&ed.doc.rope, &ed.doc.path).is_ok();
                        if ok {
                            ed.doc.mark_saved();
                        }
                        ed.rebuild_line_cache();
                        cx.notify();
                        ok
                    })
                    .unwrap_or(false)
            })
        } else {
            let ok = editor.update(cx, |ed, cx| {
                let ok = save(&ed.doc.rope, &ed.doc.path).is_ok();
                if ok {
                    ed.doc.mark_saved();
                }
                cx.notify();
                ok
            });
            Task::ready(ok)
        }
    }

    /// Close a tab, prompting to save first if the document is dirty.
    fn request_close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix) else { return };
        let tab_id = tab.id;
        let Some(editor) = tab.editor().cloned() else {
            self.close_tab(ix, window, cx);
            return;
        };
        if !editor.read(cx).doc.dirty {
            self.close_tab(ix, window, cx);
            return;
        }
        let name = Self::doc_display_name(&editor.read(cx).doc);
        let rx = window.prompt(
            PromptLevel::Warning,
            &rust_i18n::t!("dialog.save_changes", name = name),
            None,
            &[
                rust_i18n::t!("dialog.save").as_ref(),
                rust_i18n::t!("dialog.dont_save").as_ref(),
                rust_i18n::t!("dialog.cancel").as_ref(),
            ],
            cx,
        );
        cx.spawn_in(window, async move |ws, cx| {
            let Ok(answer) = rx.await else { return };
            match answer {
                0 => {
                    let Ok(saved) =
                        ws.update_in(cx, |ws, window, cx| ws.save_editor(&editor, window, cx))
                    else {
                        return;
                    };
                    if saved.await {
                        ws.update_in(cx, |ws, window, cx| ws.close_tab_by_id(tab_id, window, cx))
                            .ok();
                    }
                }
                1 => {
                    ws.update_in(cx, |ws, window, cx| ws.close_tab_by_id(tab_id, window, cx))
                        .ok();
                }
                _ => {}
            }
        })
        .detach();
    }

    // ── action handlers ────────────────────────────────────────────────────────

    fn on_new_file(&mut self, _: &NewFile, window: &mut Window, cx: &mut Context<Self>) {
        let registry = cx.global::<crate::Registry>().0.clone();
        let editor =
            cx.new(|cx| EditorView::from_doc(Document::empty_untitled(), registry, cx));
        self.push_editor_tab(editor, cx);
        self.activate_tab(self.tabs.len() - 1, window, cx);
    }

    fn on_open_file(&mut self, _: &OpenFile, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        cx.spawn_in(window, async move |ws, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            ws.update_in(cx, |ws, window, cx| {
                for path in paths {
                    ws.open_path(&path, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn on_open_folder(&mut self, _: &OpenFolder, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |ws, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(folder) = paths.into_iter().next() else { return };
            ws.update_in(cx, |ws, _, cx| ws.set_root_folder(folder, cx)).ok();
        })
        .detach();
    }

    fn on_save_file(&mut self, _: &SaveFile, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) =
            self.active.and_then(|ix| self.tabs.get(ix)).and_then(|tab| tab.editor()).cloned()
        {
            self.save_editor(&editor, window, cx).detach();
        }
    }

    fn on_open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        let existing = self
            .tabs
            .iter()
            .position(|tab| matches!(tab.content, TabContent::Settings(_)));
        if let Some(ix) = existing {
            self.activate_tab(ix, window, cx);
            return;
        }
        let view = cx.new(SettingsView::new);
        self.tabs.push(Tab { id: self.next_tab_id, content: TabContent::Settings(view) });
        self.next_tab_id += 1;
        self.activate_tab(self.tabs.len() - 1, window, cx);
    }

    pub(crate) fn on_open_project_search(
        &mut self,
        _: &OpenProjectSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Prefill with active editor selection text if any.
        let prefill = self
            .active
            .and_then(|ix| self.tabs.get(ix))
            .and_then(|t| t.editor())
            .and_then(|e| {
                let ev = e.read(cx);
                let sel = &ev.sel;
                if sel.anchor == sel.head {
                    None
                } else {
                    let start = sel.anchor.min(sel.head);
                    let end = sel.anchor.max(sel.head);
                    Some(ev.doc.rope.slice(start..end).to_string())
                }
            })
            .unwrap_or_default();

        let existing =
            self.tabs.iter().position(|t| matches!(t.content, TabContent::ProjectSearch(_)));
        if let Some(ix) = existing {
            // Toggle closed if already the active tab (mirrors Cmd+F in-file behaviour).
            if self.active == Some(ix) {
                self.close_tab(ix, window, cx);
                return;
            }
            self.activate_tab(ix, window, cx);
            if !prefill.is_empty() {
                if let TabContent::ProjectSearch(view) = &self.tabs[ix].content {
                    view.update(cx, |psv, cx| {
                        psv.set_query(prefill, cx);
                    });
                }
            }
            if let TabContent::ProjectSearch(view) = &self.tabs[ix].content {
                let qh = view.read(cx).query_handle.clone();
                window.focus(&qh);
            }
            return;
        }
        let ws_entity = cx.entity();
        let view = cx.new(|cx| ProjectSearchView::new(ws_entity.downgrade(), prefill, cx));
        self.tabs.push(Tab { id: self.next_tab_id, content: TabContent::ProjectSearch(view.clone()) });
        self.next_tab_id += 1;
        self.activate_tab(self.tabs.len() - 1, window, cx);
        let qh = view.read(cx).query_handle.clone();
        window.focus(&qh);
    }

    /// Open `path` at `line`/`col` (0-based). Re-uses an existing editor tab
    /// if the file is already open; otherwise opens it first.
    pub(crate) fn navigate_to(
        &mut self,
        path: &Path,
        line: usize,
        col: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_path(path, window, cx);
        if let Some(editor) =
            self.active.and_then(|i| self.tabs.get(i)).and_then(|t| t.editor()).cloned()
        {
            editor.update(cx, |ev, cx| {
                let char_idx = ev
                    .line_starts
                    .get(line)
                    .map(|&ls| ls + col)
                    .unwrap_or(ev.doc.rope.len_chars().saturating_sub(1));
                ev.sel.head = char_idx;
                ev.sel.anchor = char_idx;
                ev.scroll_handle.scroll_to_item(line, gpui::ScrollStrategy::Center);
                ev.flash_line = Some(line);
                cx.spawn(async move |view, cx| {
                    cx.background_executor().timer(Duration::from_millis(800)).await;
                    view.update(cx, |ev, cx| {
                        ev.flash_line = None;
                        cx.notify();
                    }).ok();
                }).detach();
                cx.notify();
            });
        }
    }

    pub(crate) fn collapse_tree_all(&mut self, cx: &mut Context<Self>) {
        if let Some(tree) = &mut self.tree {
            tree.collapse_all();
            self.visible_rows = tree.visible();
        }
        cx.notify();
    }

    pub(crate) fn expand_tree_all(&mut self, cx: &mut Context<Self>) {
        if let Some(tree) = &mut self.tree {
            let _ = tree.expand_all();
            self.visible_rows = tree.visible();
        }
        cx.notify();
    }

    pub(crate) fn refresh_tree(&mut self, cx: &mut Context<Self>) {
        if let Some(tree) = &mut self.tree {
            let _ = tree.refresh();
            self.visible_rows = tree.visible();
        }
        self.kick_index_scan(false, cx);
        if self.file_index.full.is_some() {
            self.kick_index_scan(true, cx);
        }
        cx.notify();
    }

    fn on_close_file(&mut self, _: &CloseFile, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.active {
            self.request_close_tab(ix, window, cx);
        }
    }

    fn on_close_folder(&mut self, _: &CloseFolder, _: &mut Window, cx: &mut Context<Self>) {
        self.root_folder = None;
        self.tree = None;
        self.visible_rows.clear();
        cx.set_global(ProjectRoot(None));
        cx.notify();
    }

    fn on_toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar.open = !self.sidebar.open;
        cx.notify();
    }

    fn on_toggle_bottom_panel(&mut self, _: &ToggleBottomPanel, _: &mut Window, cx: &mut Context<Self>) {
        self.bottom_open = !self.bottom_open;
        cx.notify();
    }

    fn on_toggle_right_panel(&mut self, _: &ToggleRightPanel, _: &mut Window, cx: &mut Context<Self>) {
        self.right_open = !self.right_open;
        cx.notify();
    }

    fn on_quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        let dirty: Vec<Entity<EditorView>> = self
            .tabs
            .iter()
            .filter_map(|t| t.editor())
            .filter(|e| e.read(cx).doc.dirty)
            .cloned()
            .collect();
        if dirty.is_empty() {
            cx.quit();
            return;
        }
        let count = dirty.len();
        let rx = window.prompt(
            PromptLevel::Warning,
            &rust_i18n::t!("dialog.unsaved_count", count = count),
            None,
            &[
                rust_i18n::t!("dialog.save_all_quit").as_ref(),
                rust_i18n::t!("dialog.quit_without_saving").as_ref(),
                rust_i18n::t!("dialog.cancel").as_ref(),
            ],
            cx,
        );
        cx.spawn_in(window, async move |ws, cx| {
            let Ok(answer) = rx.await else { return };
            match answer {
                0 => {
                    for editor in dirty {
                        let Ok(saved) =
                            ws.update_in(cx, |ws, window, cx| ws.save_editor(&editor, window, cx))
                        else {
                            return;
                        };
                        if !saved.await {
                            return; // cancelled a Save As dialog — abort quit
                        }
                    }
                    ws.update_in(cx, |_, _, cx| cx.quit()).ok();
                }
                1 => {
                    ws.update_in(cx, |_, _, cx| cx.quit()).ok();
                }
                _ => {}
            }
        })
        .detach();
    }

    fn on_close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.active {
            self.request_close_tab(ix, window, cx);
        }
    }

    /// Close all tabs except the one with `keep_id`.
    fn close_other_tabs(&mut self, keep_id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<usize> =
            self.tabs.iter().filter(|t| t.id != keep_id).map(|t| t.id).collect();
        for id in ids {
            if let Some(ix) = self.tabs.iter().position(|t| t.id == id) {
                self.request_close_tab(ix, window, cx);
            }
        }
    }

    /// Close all tabs.
    fn close_all_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<usize> = self.tabs.iter().map(|t| t.id).collect();
        for id in ids {
            if let Some(ix) = self.tabs.iter().position(|t| t.id == id) {
                self.request_close_tab(ix, window, cx);
            }
        }
    }

    /// Close all tabs to the left of `anchor_id`.
    fn close_tabs_to_left(&mut self, anchor_id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let anchor_ix = self.tabs.iter().position(|t| t.id == anchor_id).unwrap_or(0);
        let ids: Vec<usize> = self.tabs[..anchor_ix].iter().map(|t| t.id).collect();
        for id in ids {
            if let Some(ix) = self.tabs.iter().position(|t| t.id == id) {
                self.request_close_tab(ix, window, cx);
            }
        }
    }

    /// Close all tabs to the right of `anchor_id`.
    fn close_tabs_to_right(&mut self, anchor_id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let anchor_ix = self.tabs.iter().position(|t| t.id == anchor_id).unwrap_or(0);
        let ids: Vec<usize> = self.tabs[anchor_ix + 1..].iter().map(|t| t.id).collect();
        for id in ids {
            if let Some(ix) = self.tabs.iter().position(|t| t.id == id) {
                self.request_close_tab(ix, window, cx);
            }
        }
    }

    fn on_next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.active {
            let next = (ix + 1) % self.tabs.len();
            self.activate_tab(next, window, cx);
        }
    }

    fn on_prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.active {
            let prev = if ix == 0 { self.tabs.len() - 1 } else { ix - 1 };
            self.activate_tab(prev, window, cx);
        }
    }

    // ── rendering ──────────────────────────────────────────────────────────────

    fn render_tab(&self, ix: usize, t: &RuntimeTheme, cx: &mut Context<Self>) -> Stateful<Div> {
        let tab = &self.tabs[ix];
        let (title, dirty) = tab.title(cx);
        let is_active = self.active == Some(ix);

        let icon: AnyElement = match &tab.content {
            TabContent::Editor(_) => img(crate::file_icons::icon_for_file(&title))
                .size(px(14.0))
                .flex_shrink_0()
                .into_any_element(),
            TabContent::Settings(_) => svg()
                .path(IconName::Settings.path())
                .size(px(14.0))
                .flex_shrink_0()
                .text_color(t.text_muted)
                .into_any_element(),
            TabContent::ProjectSearch(_) => svg()
                .path(IconName::Search.path())
                .size(px(14.0))
                .flex_shrink_0()
                .text_color(t.text_muted)
                .into_any_element(),
        };

        let tab_id = tab.id;
        h_flex()
            .id(tab.id)
            .flex_shrink_0()
            .gap_2()
            .px_3()
            .h_full()
            .border_r_1()
            .border_color(t.separator)
            .when(is_active, |el| el.bg(t.bg))
            .when(!is_active, |el| el.bg(t.bg_elevated))
            .font_family(t.ui_family.clone())
            .text_size(px(t.font_size_caption))
            .text_color(if is_active { t.text } else { t.text_muted })
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |ws, _, window, cx| ws.activate_tab(ix, window, cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |ws, ev: &MouseDownEvent, _, cx| {
                    ws.tab_menu = Some(TabMenu { tab_id, pos: ev.position });
                    cx.notify();
                }),
            )
            .child(icon)
            .child(title)
            .when(dirty, |el| {
                el.child(
                    div()
                        .size(px(7.0))
                        .flex_shrink_0()
                        .rounded_full()
                        .bg(t.dirty),
                )
            })
            .child(
                svg()
                    .path(IconName::Close.path())
                    .size(px(13.0))
                    .flex_shrink_0()
                    .text_color(t.text_subtle)
                    .hover(|s| s.text_color(t.text))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |ws, _, window, cx| {
                            cx.stop_propagation();
                            ws.request_close_tab(ix, window, cx);
                        }),
                    ),
            )
    }

    fn render_context_menu(&self, t: &RuntimeTheme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.tab_menu.as_ref()?;
        let tab_id = menu.tab_id;
        let pos = menu.pos;

        let tab_ix = self.tabs.iter().position(|t| t.id == tab_id)?;
        let path = self.tabs.get(tab_ix)
            .and_then(|t| t.editor())
            .map(|e| e.read(cx).doc.path.clone())
            .filter(|p| !p.as_os_str().is_empty());
        let has_path = path.is_some();
        let tab_count = self.tabs.len();
        let has_left = tab_ix > 0;
        let has_right = tab_ix + 1 < tab_count;
        let has_others = tab_count > 1;
        let root = self.root_folder.clone();

        let ws = cx.entity();

        // ── helper: build a single menu item ─────────────────────────────────
        let item = |label: SharedString, enabled: bool, on_click: MenuClickFn| {
            ContextMenuItem { label, enabled, on_click }
        };
        let sep = || div().h(px(1.)).mx(px(t.sp2)).my(px(t.sp1)).bg(t.separator);

        // ── close group ──────────────────────────────────────────────────────
        let close_item = {
            let ws = ws.clone();
            item(rust_i18n::t!("tab_menu.close").into(), true, Box::new(move |_, window, cx| {
                ws.update(cx, |ws, cx| {
                    ws.tab_menu = None;
                    if let Some(ix) = ws.tabs.iter().position(|t| t.id == tab_id) {
                        ws.request_close_tab(ix, window, cx);
                    }
                });
            }))
        };

        let close_others = {
            let ws = ws.clone();
            item(rust_i18n::t!("tab_menu.close_others").into(), has_others, Box::new(move |_, window, cx| {
                ws.update(cx, |ws, cx| {
                    ws.tab_menu = None;
                    ws.close_other_tabs(tab_id, window, cx);
                });
            }))
        };

        let close_all = {
            let ws = ws.clone();
            item(rust_i18n::t!("tab_menu.close_all").into(), true, Box::new(move |_, window, cx| {
                ws.update(cx, |ws, cx| {
                    ws.tab_menu = None;
                    ws.close_all_tabs(window, cx);
                });
            }))
        };

        let close_left = {
            let ws = ws.clone();
            item(rust_i18n::t!("tab_menu.close_left").into(), has_left, Box::new(move |_, window, cx| {
                ws.update(cx, |ws, cx| {
                    ws.tab_menu = None;
                    ws.close_tabs_to_left(tab_id, window, cx);
                });
            }))
        };

        let close_right = {
            let ws = ws.clone();
            item(rust_i18n::t!("tab_menu.close_right").into(), has_right, Box::new(move |_, window, cx| {
                ws.update(cx, |ws, cx| {
                    ws.tab_menu = None;
                    ws.close_tabs_to_right(tab_id, window, cx);
                });
            }))
        };

        // ── copy group ───────────────────────────────────────────────────────
        let copy_path = {
            let ws = ws.clone();
            let p = path.clone();
            item(rust_i18n::t!("tab_menu.copy_path").into(), has_path, Box::new(move |_, _, cx| {
                let Some(p) = p.clone() else { return };
                ws.update(cx, |ws, cx| {
                    ws.tab_menu = None;
                    cx.write_to_clipboard(ClipboardItem::new_string(p.display().to_string()));
                    cx.notify();
                });
            }))
        };

        let copy_rel = {
            let ws = ws.clone();
            let rel = match (&root, &path) {
                (Some(r), Some(p)) => p.strip_prefix(r).ok()
                    .map(|rel| rel.display().to_string())
                    .unwrap_or_else(|| p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()),
                (None, Some(p)) => p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                _ => String::new(),
            };
            item(rust_i18n::t!("tab_menu.copy_relative_path").into(), has_path, Box::new(move |_, _, cx| {
                let rel = rel.clone();
                ws.update(cx, |ws, cx| {
                    ws.tab_menu = None;
                    cx.write_to_clipboard(ClipboardItem::new_string(rel));
                    cx.notify();
                });
            }))
        };

        // ── reveal group ─────────────────────────────────────────────────────
        let reveal_finder = {
            let ws = ws.clone();
            let p = path.clone();
            item(rust_i18n::t!("tab_menu.reveal_in_finder").into(), has_path, Box::new(move |_, _, cx| {
                let Some(p) = p.clone() else { return };
                ws.update(cx, |ws, cx| {
                    ws.tab_menu = None;
                    cx.reveal_path(&p);
                    cx.notify();
                });
            }))
        };

        let reveal_explorer = {
            let ws = ws.clone();
            let p = path.clone();
            item(rust_i18n::t!("tab_menu.reveal_in_explorer").into(), has_path, Box::new(move |_, _, cx| {
                let Some(p) = p.clone() else { return };
                ws.update(cx, |ws, cx| {
                    ws.tab_menu = None;
                    ws.sidebar.open = true;
                    ws.sidebar.active = SidebarItemKind::Explorer;
                    ws.reveal_in_tree(&p, cx);
                    cx.notify();
                });
            }))
        };

        let items: Vec<ContextMenuItem> = vec![close_item, close_others, close_all, close_left, close_right];
        let copy_items: Vec<ContextMenuItem> = vec![copy_path, copy_rel];
        let reveal_items: Vec<ContextMenuItem> = vec![reveal_finder, reveal_explorer];

        let menu_div = v_flex()
            .id("tab-ctx-menu")
            .occlude()
            .on_mouse_down_out(cx.listener(|ws, _, _, cx| {
                ws.tab_menu = None;
                cx.notify();
            }))
            .bg(t.bg_overlay)
            .border_1()
            .border_color(t.border)
            .rounded(px(t.radius_md))
            .py(px(t.sp2))
            .min_w(px(200.))
            .children(items)
            .child(sep())
            .children(copy_items)
            .child(sep())
            .children(reveal_items);

        Some(
            deferred(anchored().position(pos).snap_to_window().child(menu_div))
                .with_priority(1)
                .into_any_element(),
        )
    }

    fn render_tab_bar(&self, t: &RuntimeTheme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("tab-bar")
            .flex()
            .flex_row()
            .h(px(30.0))
            .flex_shrink_0()
            .overflow_x_scroll()
            .bg(t.bg_elevated)
            .border_b_1()
            .border_color(t.separator)
            .children((0..self.tabs.len()).map(|ix| self.render_tab(ix, t, cx)))
    }

    fn render_titlebar(&self, t: &RuntimeTheme, cx: &mut Context<Self>) -> impl IntoElement {
        let sidebar_open = self.sidebar.open;
        let bottom_open = self.bottom_open;
        let right_open = self.right_open;
        let hover_bg = t.line_highlight;
        let active_bg = t.line_highlight;
        let accent = t.accent;
        let text_subtle = t.text_subtle;
        let radius = t.radius_sm;

        let make_btn = |id: &'static str, icon: IconName, active: bool, color: gpui::Hsla| {
            div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .size(px(28.))
                .rounded(px(radius))
                .cursor_pointer()
                .when(active, move |el| el.bg(active_bg))
                .hover(move |s| s.bg(hover_bg))
                .child(svg().path(icon.path()).size(px(15.)).text_color(color))
        };

        let left_btn = make_btn(
            "titlebar-left-panel",
            IconName::PanelLeft,
            sidebar_open,
            if sidebar_open { accent } else { text_subtle },
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|ws, _, window, cx| {
                cx.stop_propagation();
                ws.on_toggle_sidebar(&ToggleSidebar, window, cx);
            }),
        );

        let right_btn = make_btn(
            "titlebar-right-panel",
            IconName::PanelRight,
            right_open,
            if right_open { accent } else { text_subtle },
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|ws, _, window, cx| {
                cx.stop_propagation();
                ws.on_toggle_right_panel(&ToggleRightPanel, window, cx);
            }),
        );

        let bottom_btn = make_btn(
            "titlebar-bottom-panel",
            IconName::PanelBottom,
            bottom_open,
            if bottom_open { accent } else { text_subtle },
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|ws, _, window, cx| {
                cx.stop_propagation();
                ws.on_toggle_bottom_panel(&ToggleBottomPanel, window, cx);
            }),
        );

        let settings_btn = make_btn("titlebar-settings", IconName::Settings, false, text_subtle)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|ws, _, window, cx| {
                    cx.stop_propagation();
                    ws.on_open_settings(&OpenSettings, window, cx);
                }),
            );

        h_flex()
            .id("titlebar")
            .h(px(36.))
            .flex_shrink_0()
            .px_2()
            .bg(t.bg_elevated)
            .border_b_1()
            .border_color(t.separator)
            .on_mouse_down(MouseButton::Left, |_, window, _cx| {
                window.start_window_move();
            })
            .child(div().w(px(72.)).flex_shrink_0())
            .child(div().flex_1())
            .child(h_flex().gap_1().child(left_btn).child(right_btn).child(bottom_btn).child(settings_btn))
            .child(div().w_2())
    }

    fn render_right_panel(&self, t: &RuntimeTheme) -> impl IntoElement {
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
                    .child(rust_i18n::t!("panel.panel").to_string()),
            )
            .child(
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .text_size(px(t.font_size_caption))
                    .text_color(t.text_muted)
                    .font_family(t.ui_family.clone())
                    .child(rust_i18n::t!("panel.coming_soon").to_string()),
            )
    }

    fn render_bottom_panel(&self, t: &RuntimeTheme) -> impl IntoElement {
        v_flex()
            .w_full()
            .h(px(180.))
            .flex_shrink_0()
            .bg(t.bg_elevated)
            .border_t_1()
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
                    .child(rust_i18n::t!("panel.terminal").to_string()),
            )
            .child(
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .text_size(px(t.font_size_caption))
                    .text_color(t.text_muted)
                    .font_family(t.ui_family.clone())
                    .child(rust_i18n::t!("panel.coming_soon").to_string()),
            )
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<RuntimeTheme>().clone();

        let content: Option<AnyElement> =
            self.active.and_then(|ix| self.tabs.get(ix)).map(|tab| match &tab.content {
                TabContent::Editor(e) => e.clone().into_any_element(),
                TabContent::Settings(s) => s.clone().into_any_element(),
                TabContent::ProjectSearch(p) => p.clone().into_any_element(),
            });

        let base = div()
            .size_full()
            .bg(t.bg)
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_close_tab))
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_prev_tab))
            .on_action(cx.listener(Self::on_new_file))
            .on_action(cx.listener(Self::on_open_file))
            .on_action(cx.listener(Self::on_open_folder))
            .on_action(cx.listener(Self::on_save_file))
            .on_action(cx.listener(Self::on_close_file))
            .on_action(cx.listener(Self::on_close_folder))
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_toggle_bottom_panel))
            .on_action(cx.listener(Self::on_toggle_right_panel))
            .on_action(cx.listener(Self::on_open_settings))
            .on_action(cx.listener(Self::on_open_project_search))
            .on_action(cx.listener(Self::on_open_file_finder))
            .on_action(cx.listener(Self::on_open_file_finder_preview))
            .on_action(cx.listener(Self::on_quit))
            .when(self.sidebar_resizing, |el| {
                el.on_mouse_move(cx.listener(|ws, event: &MouseMoveEvent, _, cx| {
                    use crate::sidebar::ACTIVITY_BAR_W;
                    let x = f32::from(event.position.x);
                    ws.sidebar.width = (x - ACTIVITY_BAR_W).clamp(160.0, 600.0);
                    cx.notify();
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|ws, _, _, cx| {
                        ws.sidebar_resizing = false;
                        cx.notify();
                    }),
                )
            })
            .when(self.tree_scrollbar_drag.is_some(), |el| {
                el.on_mouse_move(cx.listener(|ws, ev: &MouseMoveEvent, _, cx| {
                    if let Some(ref drag) = ws.tree_scrollbar_drag {
                        let handle = ws.tree_scroll.0.borrow().base_handle.clone();
                        update_drag(drag, ev, &handle);
                        cx.notify();
                    }
                }))
                .on_mouse_up(MouseButton::Left, cx.listener(|ws, _, _, cx| {
                    ws.tree_scrollbar_drag = None;
                    cx.notify();
                }))
            });

        // Empty state: show welcome screen only when no folder and no tabs are open.
        if content.is_none() && self.root_folder.is_none() {
            return base
                .flex()
                .items_center()
                .justify_center()
                .child(render_welcome(&t))
                .when_some(self.file_finder.clone(), |el, finder| el.relative().child(finder))
                .into_any();
        }

        let main = v_flex()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .child(self.render_tab_bar(&t, cx))
            .child(div().flex_1().min_h(px(0.)).when_some(content, |el, c| el.child(c)));

        let body_row = h_flex()
            .flex_1()
            .min_h(px(0.))
            .child(self.render_activity_bar(&t, cx))
            .when(self.sidebar.open, |el| {
                el.child(self.render_sidebar_panel(&t, cx))
                    .child(self.render_sidebar_resize_handle(&t, cx))
            })
            .child(main)
            .when(self.right_open, |el| el.child(self.render_right_panel(&t)));

        let body = v_flex()
            .flex_1()
            .min_h(px(0.))
            .child(body_row)
            .when(self.bottom_open, |el| el.child(self.render_bottom_panel(&t)));

        let root = base
            .flex()
            .flex_col()
            .relative()
            .child(self.render_titlebar(&t, cx))
            .child(body)
            .map(|el| match self.render_context_menu(&t, cx) {
                Some(menu) => el.child(menu),
                None => el,
            })
            .when_some(self.file_finder.clone(), |el, finder| el.child(finder));

        root.into_any()
    }
}
