use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use faber_core::pane_tree::{
    Axis, DropZone, Member, PaneGroup, PaneId, Rect as PaneRect, SplitDirection, Vec2 as PaneVec2,
};
use faber_settings::AutoSave;

use faber_editor::{
    buffer::Document,
    project::{FileTree, VisibleRow},
    save::save,
};
use gpui::{
    AnyElement, App, ClipboardItem, Context, Div, DragMoveEvent, Entity, FocusHandle, Focusable,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, PathPromptOptions, Render,
    ScrollStrategy, SharedString, Stateful, Task, UniformListScrollHandle, Window, anchored,
    deferred, div, img, prelude::*, px, relative, svg,
};

use crate::editor_view::{EditorEvent, EditorView};
use crate::file_finder::FileFinderView;
use crate::lsp_status::LspStatus;
use crate::pane::{Pane, TabKind, TabMenu};
use crate::project_search_view::ProjectSearchView;
use crate::settings_view::{SettingsStore, SettingsView};
use crate::sidebar::{SidebarItem, SidebarItemKind, SidebarState, default_items};
use crate::theme::{ActiveTheme, RuntimeTheme};
use crate::ui::scrollbar::update_drag;
use crate::ui::{
    IconName, ScrollbarDrag, h_flex, modal_backdrop, modal_container, popover_container, v_flex,
};
use crate::welcome_view::render_welcome;
use crate::{
    AppStateStore, CfConfirm, CfDismiss, CloseFile, CloseFolder, CloseTab, CloseWindow, NewFile,
    NextTab, OpenFile, OpenFileFinder, OpenFileFinderPreview, OpenFolder, OpenLanguagePicker,
    OpenProblems, OpenProjectSearch, OpenSettings, PrevTab, ProjectRoot, Quit, ReindexProject,
    SaveAll, SaveAs, SaveFile, SplitDown, SplitLeft, SplitRight, SplitUp, ToggleBottomPanel,
    ToggleLspStatus, ToggleRightPanel, ToggleSidebar,
};
use faber_lang::{LanguageId as LspLanguageId, LanguageRegistry};
use faber_lsp::adapter::{LspAdapter, RustAnalyzerAdapter};
use faber_lsp::manager::LspManager;

// ── Index status ──────────────────────────────────────────────────────────────

pub struct IndexStatus {
    pub current_progress: Option<faber_index::progress::ProgressEvent>,
}

impl IndexStatus {
    fn new() -> Self {
        Self {
            current_progress: None,
        }
    }
}

// ── Confirm modal ─────────────────────────────────────────────────────────────

type ConfirmCallback =
    Box<dyn FnOnce(&mut Workspace, usize, &mut Window, &mut Context<Workspace>) + 'static>;

#[derive(Clone)]
struct ConfirmButton {
    label: SharedString,
}

struct ConfirmSpec {
    message: SharedString,
    buttons: Vec<ConfirmButton>,
    default_ix: usize,
    destructive_ix: Option<usize>,
    on_answer: ConfirmCallback,
}

struct ConfirmState {
    message: SharedString,
    buttons: Vec<ConfirmButton>,
    default_ix: usize,
    destructive_ix: Option<usize>,
    focus_handle: FocusHandle,
    on_answer: ConfirmCallback,
}

// ── Pane resize & drag types ──────────────────────────────────────────────────

#[derive(Clone)]
struct PaneResize {
    axis_path: Vec<usize>,
    divider_ix: usize,
    axis: Axis,
    start_cursor: gpui::Point<gpui::Pixels>,
}

#[derive(Clone)]
struct DraggedTab {
    source_pane: PaneId,
    tab_id: usize,
    title: String,
}

struct TabGhost {
    title: String,
}

impl Render for TabGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<RuntimeTheme>().clone();
        div()
            .px_3()
            .h(px(30.))
            .flex()
            .items_center()
            .bg(t.bg_elevated)
            .border_1()
            .border_color(t.accent)
            .rounded(px(t.radius_sm))
            .text_size(px(t.font_size_caption))
            .font_family(t.ui_family.clone())
            .text_color(t.text)
            .opacity(0.8)
            .child(self.title.clone())
    }
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
            .when(self.enabled, |el| {
                el.cursor_pointer().hover(|s| s.bg(t.line_highlight))
            })
            .when(self.enabled, |el| {
                el.on_mouse_down(MouseButton::Left, move |e, w, cx| (self.on_click)(e, w, cx))
            })
            .child(self.label)
    }
}

#[allow(dead_code)]
pub struct Workspace {
    /// Pane layout tree (single pane during this commit; split panes in later commits).
    pub(crate) pane_group: PaneGroup<PaneId>,
    pub(crate) panes: HashMap<PaneId, Entity<Pane>>,
    pub(crate) focused_pane: PaneId,
    next_pane_id: u64,
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
    /// Bumped on every edit; a debounced auto-save fires only if no newer
    /// edit arrived while its timer slept.
    autosave_generation: u64,
    index_engine: Option<Arc<faber_index::engine::IndexEngine>>,
    pub(crate) files_handle:
        Option<faber_index::module::SnapshotHandle<faber_index::files::FileIndexSnapshot>>,
    pub(crate) symbols_handle:
        Option<faber_index::module::SnapshotHandle<faber_index::SymbolsSnapshot>>,
    /// Keeps the filesystem watcher threads alive for the current root folder.
    _fs_watcher: Option<faber_index::watcher::FsWatcher>,
    index_status: gpui::Entity<IndexStatus>,
    lsp_manager: Option<Arc<LspManager>>,
    lsp_status: gpui::Entity<LspStatus>,
    status_bar: Entity<crate::status_bar::StatusBar>,
    file_finder: Option<Entity<FileFinderView>>,
    symbol_finder: Option<Entity<crate::symbol_finder::SymbolFinderView>>,
    confirm: Option<ConfirmState>,
    pane_resize: Option<PaneResize>,
    drop_hover: Option<(PaneId, DropZone)>,
    pub(crate) lsp_overlay_open: bool,
    pub(crate) lsp_overlay_pos: gpui::Point<gpui::Pixels>,
    active_doc_info: Entity<crate::status_bar::ActiveDocInfo>,
    language_picker: Option<Entity<crate::language_picker::LanguagePickerView>>,
}

impl Workspace {
    pub fn new(
        paths: &[String],
        session: Option<&faber_settings::state::LastSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let first_pane_id = PaneId(0);
        let first_pane = cx.new(|cx| Pane::new(first_pane_id, cx));
        let mut panes = HashMap::new();
        panes.insert(first_pane_id, first_pane);

        let index_status = cx.new(|_| IndexStatus::new());
        let lsp_status = cx.new(|_| LspStatus::new());
        let active_doc_info = cx.new(|_| crate::status_bar::ActiveDocInfo::new());
        let status_bar = cx.new(|_| crate::status_bar::StatusBar::new());
        {
            let item =
                cx.new(|cx| crate::status_bar::IndexingStatusItem::new(index_status.clone(), cx));
            status_bar.update(cx, |bar, _| bar.push_right(item.into()));
        }
        {
            let ws_weak = cx.entity().downgrade();
            let item = cx.new(|cx| {
                crate::status_bar::LanguageStatusItem::new(active_doc_info.clone(), ws_weak, cx)
            });
            status_bar.update(cx, |bar, _| bar.push_right(item.into()));
        }
        {
            let ws_weak = cx.entity().downgrade();
            let item =
                cx.new(|cx| crate::status_bar::LspStatusItem::new(lsp_status.clone(), ws_weak, cx));
            status_bar.update(cx, |bar, _| bar.push_right(item.into()));
        }
        {
            let item =
                cx.new(|cx| crate::status_bar::DiagnosticsStatusItem::new(lsp_status.clone(), cx));
            status_bar.update(cx, |bar, _| bar.push_right(item.into()));
        }

        let mut ws = Self {
            pane_group: PaneGroup::single(first_pane_id),
            panes,
            focused_pane: first_pane_id,
            next_pane_id: 1,
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
            autosave_generation: 0,
            index_engine: None,
            files_handle: None,
            symbols_handle: None,
            _fs_watcher: None,
            index_status,
            lsp_manager: None,
            lsp_status,
            status_bar,
            file_finder: None,
            symbol_finder: None,
            confirm: None,
            pane_resize: None,
            drop_hover: None,
            lsp_overlay_open: false,
            lsp_overlay_pos: gpui::point(gpui::px(0.), gpui::px(0.)),
            active_doc_info,
            language_picker: None,
        };
        if !paths.is_empty() {
            // Detect project root from the first path so the LSP can start.
            if let Some(first) = paths.first() {
                let root = Self::detect_project_root(Path::new(first));
                ws.set_root_folder(root.clone(), cx);
                ws.check_and_show_trust_modal(&root, window, cx);
            }
            for path in paths {
                let editor = cx.new(|cx| EditorView::new(path, cx));
                ws.push_editor_tab(editor, cx);
            }
            ws.activate_tab(0, window, cx);
        } else if let Some(sess) = session {
            if let Some(root) = sess.root.as_deref() {
                let root_path = PathBuf::from(root);
                if root_path.exists() {
                    ws.set_root_folder(root_path.clone(), cx);
                    ws.check_and_show_trust_modal(&root_path, window, cx);
                }
            }
            for file in &sess.files {
                let file_path = PathBuf::from(file);
                if file_path.exists() {
                    let editor = cx.new(|cx| EditorView::new(file, cx));
                    ws.push_editor_tab(editor, cx);
                }
            }
            if !ws.pane().read(cx).is_empty() {
                ws.activate_tab(0, window, cx);
            }
        }
        cx.observe_window_activation(window, |ws, window, cx| {
            let auto_save = cx.global::<SettingsStore>().0.auto_save;
            if !window.is_window_active()
                && matches!(
                    auto_save,
                    AutoSave::OnWindowChange | AutoSave::OnFocusChange
                )
            {
                ws.save_all_dirty(cx);
            }
        })
        .detach();
        ws
    }

    /// Returns the currently focused pane entity.
    pub(crate) fn pane(&self) -> &Entity<Pane> {
        &self.panes[&self.focused_pane]
    }

    /// All editor entities across all panes (for save-all, search scoping, etc.)
    pub(crate) fn all_editors(&self, cx: &App) -> Vec<Entity<EditorView>> {
        self.panes
            .values()
            .flat_map(|p| p.read(cx).all_editors().cloned().collect::<Vec<_>>())
            .collect()
    }

    /// Find an open editor by its file path (across all panes). Returns the
    /// pane id and tab index if found.
    pub(crate) fn find_open_editor(&self, path: &Path, cx: &App) -> Option<Entity<EditorView>> {
        for p in self.panes.values() {
            let pane = p.read(cx);
            if let Some(ix) = pane.find_editor_by_path(path, cx) {
                return pane.tab_at(ix)?.editor().cloned();
            }
        }
        None
    }

    fn push_editor_tab(&mut self, editor: Entity<EditorView>, cx: &mut Context<Self>) {
        cx.subscribe(&editor, |ws, _, _: &EditorEvent, cx| {
            ws.on_editor_edited(cx)
        })
        .detach();
        // Wire the shared diagnostic store and LSP manager so squiggles + hover work.
        if let Some(mgr) = &self.lsp_manager {
            let store = mgr.diagnostic_store();
            let mgr_arc = Arc::clone(mgr);
            editor.update(cx, |ev, _cx| {
                ev.diagnostic_store = Some(store);
                ev.lsp_manager = Some(mgr_arc);
            });
        }
        self.panes[&self.focused_pane].update(cx, |pane: &mut Pane, cx| {
            pane.push_editor_tab(editor.clone(), cx);
        });
        if let Some(mgr) = &self.lsp_manager {
            let registry = cx.global::<crate::Registry>().0.clone();
            let (_version, path, text) = editor.read(cx).doc.lsp_sync_info();
            if let Some((uri, lang_id)) = Self::doc_uri_and_lang(&path, &registry) {
                let root = self.root_folder.clone();
                let mgr = Arc::clone(mgr);
                std::thread::spawn(move || {
                    if let Some(root) = root
                        && let Err(e) = mgr.ensure_server_for_language(&lang_id, &root)
                    {
                        log::error!("lsp: ensure failed: {e}");
                    }
                    mgr.on_document_opened(uri, lang_id, &text);
                });
            }
        }
    }

    // ── auto-save ──────────────────────────────────────────────────────────────

    fn on_editor_edited(&mut self, cx: &mut Context<Self>) {
        // Notify LSP of document change (unconditional).
        if let Some(mgr) = &self.lsp_manager {
            let editor = self
                .pane()
                .read(cx)
                .active_tab()
                .and_then(|t| t.editor())
                .cloned();
            if let Some(editor) = editor {
                let registry = cx.global::<crate::Registry>().0.clone();
                let (_version, path, text) = editor.read(cx).doc.lsp_sync_info();
                if let Some((uri, _lang_id)) = Self::doc_uri_and_lang(&path, &registry) {
                    mgr.on_document_changed(uri, &text);
                }
            }
        }

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
        let editor = self
            .pane()
            .read(cx)
            .active_tab()
            .and_then(|t| t.editor())
            .cloned();
        if let Some(editor) = editor {
            editor.update(cx, |ev, _cx| {
                let char_idx = ev.line_starts.get(line).copied().unwrap_or(0);
                ev.sel.head = char_idx;
                ev.sel.anchor = char_idx;
                ev.scroll_handle
                    .scroll_to_item(line, gpui::ScrollStrategy::Top);
            });
        }
    }

    /// Saves every dirty document that has a path; untitled docs are skipped
    /// (VS Code behavior). Never touches content or history.
    fn save_all_dirty(&mut self, cx: &mut Context<Self>) {
        let editors = self.all_editors(cx);
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
        // Check if already open in the focused pane.
        let existing = self.pane().read(cx).find_editor_by_path(path, cx);
        if let Some(ix) = existing {
            self.activate_tab(ix, window, cx);
            return;
        }
        let path_str = path.to_string_lossy().to_string();
        let editor = cx.new(|cx| EditorView::new(&path_str, cx));
        self.push_editor_tab(editor, cx);
        let new_ix = self.pane().read(cx).tab_count() - 1;
        self.activate_tab(new_ix, window, cx);
        if !path.as_os_str().is_empty() {
            let abs = path_str.clone();
            self.record_state_change(cx, move |s| s.record_recent_file(&abs));
        }
    }

    pub fn activate_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.pane().read(cx).tab_count() {
            return;
        }
        // Leaving a tab counts as a focus change for auto-save purposes.
        let prev_editor = {
            let pane = self.pane().read(cx);
            pane.active
                .filter(|&prev| prev != ix)
                .and_then(|prev| pane.tab_at(prev).and_then(|t| t.editor()).cloned())
        };
        if cx.global::<SettingsStore>().0.auto_save == AutoSave::OnFocusChange
            && let Some(editor) = prev_editor
        {
            Self::save_doc_now(&editor, cx);
        }
        self.panes[&self.focused_pane].update(cx, |p: &mut Pane, _| p.set_active(Some(ix)));
        self.right_open = self.active_is_markdown(cx);
        self.update_active_doc_info(cx);
        self.focus_active(window, cx);
        cx.notify();
        // Reveal the newly active file in the explorer tree.
        let path = self.active_editor_path(cx);
        if let Some(path) = path {
            self.reveal_in_tree(&path, cx);
        }
    }

    pub fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let pane_id = self.focused_pane;
        // Capture the closing editor's path before removal so we can decide
        // whether to notify LSP afterwards.
        let closing_path = {
            let pane = self.panes[&pane_id].read(cx);
            pane.tab_at(ix)
                .and_then(|tab| tab.editor.as_ref())
                .map(|e| e.read(cx).doc.path.clone())
        };
        self.panes[&pane_id].update(cx, |p: &mut Pane, _| {
            p.remove_tab(ix);
        });
        // Send didClose only when this was the last open editor for the path.
        // The tab is already removed, so any remaining match means the file is
        // still visible in another pane and the server must keep tracking it.
        if let Some(path) = closing_path
            && let Some(mgr) = &self.lsp_manager
        {
            let registry = cx.global::<crate::Registry>().0.clone();
            let still_open_elsewhere = self.find_open_editor(&path, cx).is_some();
            if !still_open_elsewhere
                && let Some((uri, _)) = Self::doc_uri_and_lang(&path, &registry)
            {
                mgr.on_document_closed(uri);
            }
        }
        // Collapse the pane if this emptied it (a no-op for the last remaining pane,
        // which stays to show the welcome screen).
        if self.panes[&pane_id].read(cx).is_empty() {
            self.collapse_pane(pane_id, window, cx);
        } else {
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    /// Dismiss the focused pane's tab context menu.
    fn close_tab_menu(&mut self, cx: &mut Context<Self>) {
        self.panes[&self.focused_pane].update(cx, |p: &mut Pane, _| p.tab_menu = None);
    }

    fn close_tab_by_id(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.pane().read(cx).tab_by_id(id).map(|(ix, _)| ix);
        if let Some(ix) = ix {
            self.close_tab(ix, window, cx);
        }
    }

    pub(crate) fn activate_tab_in(
        &mut self,
        pane_id: PaneId,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focused_pane = pane_id;
        self.activate_tab(ix, window, cx);
    }

    fn request_close_tab_in(
        &mut self,
        pane_id: PaneId,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focused_pane = pane_id;
        self.request_close_tab(ix, window, cx);
    }

    pub fn focus_active(&self, window: &mut Window, cx: &App) {
        let pane = self.pane().read(cx);
        match pane.active_tab() {
            Some(tab) => window.focus(&tab.focus_handle(cx)),
            None => window.focus(&self.focus_handle),
        }
    }

    fn doc_display_name(doc: &Document) -> String {
        doc.path.file_name().map_or_else(
            || rust_i18n::t!("editor.untitled").to_string(),
            |n| n.to_string_lossy().to_string(),
        )
    }

    /// Returns the file path of the active editor tab, or `None` for untitled/Settings tabs.
    pub(crate) fn active_editor_path(&self, cx: &App) -> Option<PathBuf> {
        let pane = self.pane().read(cx);
        let editor = pane.active_tab()?.editor()?;
        let path = editor.read(cx).doc.path.clone();
        if path.as_os_str().is_empty() {
            None
        } else {
            Some(path)
        }
    }

    /// Expand the tree so `path` is visible, scroll the explorer to it, and notify.
    /// No-op when no folder is open or `path` is outside the root.
    pub(crate) fn reveal_in_tree(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(root) = self.root_folder.clone() else {
            return;
        };
        if !path.starts_with(&root) {
            return;
        }
        let rows = if let Some(tree) = &mut self.tree {
            if tree.reveal(path).is_err() {
                return;
            }
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

    /// Walk up from `file` looking for a directory that contains `.git` or
    /// `Cargo.toml`; if nothing is found returns the file's parent directory.
    fn detect_project_root(file: &Path) -> PathBuf {
        let start = file.parent().unwrap_or(file);
        let mut dir = start;
        loop {
            if dir.join(".git").exists() || dir.join("Cargo.toml").exists() {
                return dir.to_path_buf();
            }
            match dir.parent() {
                Some(p) => dir = p,
                None => return start.to_path_buf(),
            }
        }
    }

    pub(crate) fn set_root_folder(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        match FileTree::new(folder.clone()) {
            Ok(tree) => {
                self.visible_rows = tree.visible();
                self.tree = Some(tree);
                self.root_folder = Some(folder.clone());
                self.sidebar.open = true;
                self.sidebar.active = SidebarItemKind::Explorer;
                cx.set_global(ProjectRoot(Some(folder.clone())));
                self.index_engine = None;
                self.files_handle = None;
                self.symbols_handle = None;
                self._fs_watcher = None; // drops the old watcher threads
                self.start_index_engine(cx);
                self.start_lsp_manager(cx);
                let abs = folder.to_string_lossy().to_string();
                self.record_state_change(cx, move |s| s.record_recent_project(&abs));
            }
            Err(err) => eprintln!("faber: can't open folder {}: {err}", folder.display()),
        }
        cx.notify();
    }

    fn check_and_show_trust_modal(
        &mut self,
        folder: &PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let app_state = cx.global::<AppStateStore>().0.clone();
        if app_state.is_trusted(folder) {
            // Already trusted — LSP manager was created with trusted=true.
            return;
        }
        let folder = folder.clone();
        self.show_confirm(
            ConfirmSpec {
                message: rust_i18n::t!("trust.message").into(),
                buttons: vec![
                    ConfirmButton {
                        label: rust_i18n::t!("trust.btn_trust").into(),
                    },
                    ConfirmButton {
                        label: rust_i18n::t!("trust.btn_restricted").into(),
                    },
                ],
                default_ix: 0,
                destructive_ix: None,
                on_answer: Box::new(move |ws, ix, _window, cx| {
                    if ix != 0 {
                        return; // "Open Restricted" — leave LSP gated off
                    }
                    // Persist trust and unlock the manager.
                    let folder_clone = folder.clone();
                    ws.record_state_change(cx, move |s| s.trust_project(&folder_clone));
                    if let Some(mgr) = &ws.lsp_manager {
                        mgr.set_trusted(true);
                        // Kick ensure+didOpen for any editor already open.
                        let registry = cx.global::<crate::Registry>().0.clone();
                        let root = ws.root_folder.clone();
                        let mgr = Arc::clone(mgr);
                        let open_docs: Vec<(url::Url, faber_lang::LanguageId, String)> = ws
                            .panes
                            .values()
                            .flat_map(|pane| {
                                pane.read(cx).all_editors().cloned().collect::<Vec<_>>()
                            })
                            .filter_map(|editor| {
                                let (_v, path, text) = editor.read(cx).doc.lsp_sync_info();
                                let (uri, lang_id) = Self::doc_uri_and_lang(&path, &registry)?;
                                Some((uri, lang_id, text))
                            })
                            .collect();
                        std::thread::spawn(move || {
                            let Some(root) = root else { return };
                            for (uri, lang_id, text) in open_docs {
                                if let Err(e) = mgr.ensure_server_for_language(&lang_id, &root) {
                                    log::error!("lsp: ensure after trust failed: {e}");
                                    continue;
                                }
                                mgr.on_document_opened(uri, lang_id, &text);
                            }
                        });
                    }
                }),
            },
            window,
            cx,
        );
    }

    fn record_state_change(
        &self,
        cx: &mut Context<Self>,
        update: impl FnOnce(&mut faber_settings::state::AppState),
    ) {
        let mut state = cx.global::<AppStateStore>().0.clone();
        update(&mut state);
        let files: Vec<String> = self
            .all_editors(cx)
            .into_iter()
            .filter_map(|e| {
                let p = e.read(cx).doc.path.to_string_lossy().to_string();
                if p.is_empty() { None } else { Some(p) }
            })
            .collect();
        let root = self
            .root_folder
            .as_ref()
            .map(|p| p.to_string_lossy().to_string());
        state.set_last_session(root, files);
        let settings = cx.global::<crate::settings_view::SettingsStore>().0.clone();
        if settings.restore_split_layout {
            let layout = self.serialize_layout(cx);
            state.set_last_session_layout(Some(layout));
        }
        let state_to_save = state.clone();
        cx.set_global(AppStateStore(state));
        cx.background_executor()
            .spawn(async move {
                if let Err(err) = faber_settings::state::save(&state_to_save) {
                    eprintln!("faber: can't save state: {err}");
                }
            })
            .detach();
    }

    // ── file index / finder ────────────────────────────────────────────────────

    pub(crate) fn root_folder(&self) -> Option<&PathBuf> {
        self.root_folder.as_ref()
    }

    /// No-op kept for call-site compatibility; the engine indexes everything in one pass.
    pub(crate) fn ensure_full_index(&mut self, _cx: &mut Context<Self>) {}

    /// Clone the index store `Arc` for off-thread symbol queries.
    pub(crate) fn index_store_arc(&self) -> Option<Arc<faber_index::store::IndexStore>> {
        self.index_engine.as_ref().map(|e| e.store_arc())
    }

    /// Start (or restart) the index engine for the current root folder.
    pub(crate) fn start_index_engine(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root_folder.clone() else {
            return;
        };
        let registry = cx.global::<crate::Registry>().0.clone();

        let mut engine = match faber_index::engine::IndexEngine::new(root.clone(), registry.clone())
        {
            Ok(e) => e,
            Err(e) => {
                log::error!("index engine init failed: {e}");
                return;
            }
        };

        let files_handle = engine.register(faber_index::engine::FilesModule);
        self.files_handle = Some(files_handle);

        let symbols_handle = engine.register(faber_index::SymbolsModule::new(registry));
        self.symbols_handle = Some(symbols_handle);

        let progress_rx = engine.progress();
        let engine = Arc::new(engine);
        engine.clone().start();
        engine.request(faber_index::trigger::IndexTrigger::FolderOpened);

        // Wire the filesystem watcher: external edits feed incremental ExternalChanges
        // triggers, so the finder, symbol index, and search stay fresh automatically.
        let engine_for_watcher = engine.clone();
        self._fs_watcher = faber_index::watcher::FsWatcher::start(&root, move |trigger| {
            engine_for_watcher.request(trigger);
        })
        .map_err(|e| log::warn!("fs watcher failed to start: {e}"))
        .ok();

        self.index_engine = Some(engine);

        // Drain ProgressReceiver on a background polling loop → update Entity<IndexStatus>.
        let status = self.index_status.clone();
        cx.spawn(async move |_this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(50))
                    .await;
                if let Some(ev) = progress_rx.try_recv()
                    && status
                        .update(cx, |s, cx| {
                            s.current_progress = Some(ev);
                            cx.notify();
                        })
                        .is_err()
                {
                    break; // workspace entity dropped; stop draining.
                }
            }
        })
        .detach();
    }

    pub(crate) fn start_lsp_manager(&mut self, cx: &mut Context<Self>) {
        let settings = cx.global::<SettingsStore>().0.lsp.clone();
        let app_state = cx.global::<AppStateStore>().0.clone();
        let trusted = self
            .root_folder
            .as_ref()
            .map(|r| app_state.is_trusted(r))
            .unwrap_or(false);

        let manager = LspManager::new(default_lsp_adapters(), settings, trusted);
        self.lsp_manager = Some(Arc::clone(&manager));

        let lsp_status = self.lsp_status.clone();
        let manager_weak = Arc::downgrade(&manager);

        cx.spawn(async move |this, cx| {
            let mut last_diag_gen: u64 = 0;
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(50))
                    .await;
                let Some(mgr) = manager_weak.upgrade() else {
                    break;
                };
                let statuses = mgr.server_states();
                let diag_store = mgr.diagnostic_store();
                let error_count =
                    diag_store.count_by_severity(faber_lsp::diagnostics::Severity::Error);
                let warning_count =
                    diag_store.count_by_severity(faber_lsp::diagnostics::Severity::Warning);
                if lsp_status
                    .update(cx, |s, cx| {
                        s.statuses = statuses;
                        s.error_count = error_count;
                        s.warning_count = warning_count;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                // Notify EditorViews + DiagnosticsPanels to repaint if diagnostics changed.
                let diag_gen = diag_store.generation();
                if diag_gen != last_diag_gen {
                    last_diag_gen = diag_gen;
                    let _ = this.update(cx, |ws, cx| {
                        let editors: Vec<_> = ws
                            .panes
                            .values()
                            .flat_map(|pane| {
                                pane.read(cx).all_editors().cloned().collect::<Vec<_>>()
                            })
                            .collect();
                        for ev in editors {
                            ev.update(cx, |_, cx| cx.notify());
                        }
                        let panels: Vec<_> = ws
                            .panes
                            .values()
                            .flat_map(|pane| {
                                pane.read(cx)
                                    .all_problems_panels()
                                    .cloned()
                                    .collect::<Vec<_>>()
                            })
                            .collect();
                        for panel in panels {
                            panel.update(cx, |_, cx| cx.notify());
                        }
                    });
                }
            }
        })
        .detach();
    }

    fn doc_uri_and_lang(
        path: &std::path::Path,
        registry: &LanguageRegistry,
    ) -> Option<(url::Url, LspLanguageId)> {
        let path_str = path.to_string_lossy();
        if path_str == "<memory>" || path.as_os_str().is_empty() {
            return None;
        }
        let uri = url::Url::from_file_path(path).ok()?;
        let lang = registry.language_for_path(path)?;
        let lang_id = LspLanguageId::new(lang.id.as_str());
        Some((uri, lang_id))
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
        let ws = cx.entity().downgrade();
        let index_status = self.index_status.clone();
        let finder = cx.new(|cx| FileFinderView::new(ws, index_status, preview, cx));
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

    // ── symbol finder ──────────────────────────────────────────────────────────

    pub(crate) fn open_symbol_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.symbol_finder.is_some() {
            // Already open: cmd-t again dismisses (same pattern as file finder).
            self.close_symbol_finder(window, cx);
            return;
        }
        let ws = cx.entity().downgrade();
        let finder = cx.new(|cx| crate::symbol_finder::SymbolFinderView::new(ws, cx));
        window.focus(&finder.read(cx).focus_handle);
        self.symbol_finder = Some(finder);
        cx.notify();
    }

    pub(crate) fn close_symbol_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.symbol_finder.take().is_some() {
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    // ── Language picker ────────────────────────────────────────────────────────

    pub(crate) fn open_language_picker(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::FocusHandle> {
        if self.language_picker.is_some() {
            return None;
        }
        let registry = cx.global::<crate::Registry>().0.clone();
        let mut languages: Vec<_> = registry.languages().to_vec();
        languages.sort_by(|a, b| a.name.cmp(&b.name));

        let current_lang = self.active_doc_info.read(cx).language.clone();
        let current_lang_id = current_lang.map(|l| l.id.clone());

        let ws = cx.entity().downgrade();
        let picker = cx.new(|cx| {
            crate::language_picker::LanguagePickerView::new(languages, current_lang_id, ws, cx)
        });
        let fh = picker.read(cx).focus_handle.clone();
        self.language_picker = Some(picker);
        cx.notify();
        Some(fh)
    }

    pub(crate) fn close_language_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.language_picker.take().is_some() {
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    pub(crate) fn apply_language_override(
        &mut self,
        lang: Option<std::sync::Arc<faber_lang::Language>>,
        cx: &mut Context<Self>,
    ) {
        let editor = self
            .pane()
            .read(cx)
            .active_tab()
            .and_then(|t| t.editor())
            .cloned();
        let Some(editor) = editor else { return };

        editor.update(cx, |ev, _cx| {
            ev.doc.set_language(lang.clone());
        });

        // Update active doc info.
        self.active_doc_info.update(cx, |info, cx| {
            info.language = lang.clone();
            cx.notify();
        });

        // Re-trigger LSP for new language.
        if let Some(mgr) = &self.lsp_manager {
            let registry = cx.global::<crate::Registry>().0.clone();
            let (_version, path, text) = editor.read(cx).doc.lsp_sync_info();
            if let Some((uri, lang_id)) = Self::doc_uri_and_lang(&path, &registry) {
                let root = self.root_folder.clone();
                let mgr = Arc::clone(mgr);
                std::thread::spawn(move || {
                    if let Some(root) = root
                        && let Err(e) = mgr.ensure_server_for_language(&lang_id, &root)
                    {
                        log::error!("lsp: ensure failed after language override: {e}");
                    }
                    mgr.on_document_opened(uri, lang_id, &text);
                });
            }
        }

        cx.notify();
    }

    fn update_active_doc_info(&mut self, cx: &mut Context<Self>) {
        let editor = self
            .pane()
            .read(cx)
            .active_tab()
            .and_then(|t| t.editor())
            .cloned();
        let lang = editor.and_then(|e| {
            let doc = &e.read(cx).doc;
            let registry = cx.global::<crate::Registry>().0.clone();
            registry.language_for_path(&doc.path)
        });
        self.active_doc_info.update(cx, |info, cx| {
            info.language = lang;
            cx.notify();
        });
    }

    // ── Confirm modal ──────────────────────────────────────────────────────────

    fn show_confirm(&mut self, spec: ConfirmSpec, window: &mut Window, cx: &mut Context<Self>) {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle);
        self.confirm = Some(ConfirmState {
            message: spec.message,
            buttons: spec.buttons,
            default_ix: spec.default_ix,
            destructive_ix: spec.destructive_ix,
            focus_handle,
            on_answer: spec.on_answer,
        });
        cx.notify();
    }

    fn on_confirm_answer(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(state) = self.confirm.take() {
            (state.on_answer)(self, ix, window, cx);
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    fn on_cf_confirm(&mut self, _: &CfConfirm, window: &mut Window, cx: &mut Context<Self>) {
        let default_ix = self.confirm.as_ref().map(|s| s.default_ix).unwrap_or(0);
        self.on_confirm_answer(default_ix, window, cx);
    }

    fn on_cf_dismiss(&mut self, _: &CfDismiss, window: &mut Window, cx: &mut Context<Self>) {
        let cancel_ix = self
            .confirm
            .as_ref()
            .map(|s| s.buttons.len().saturating_sub(1))
            .unwrap_or(0);
        self.on_confirm_answer(cancel_ix, window, cx);
    }

    fn render_lsp_overlay(&self, t: &RuntimeTheme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.lsp_overlay_open {
            return None;
        }

        use faber_lsp::server::ServerState;
        let mgr = self.lsp_manager.clone();
        let ws_entity = cx.entity().downgrade();

        // ── header ────────────────────────────────────────────────────────────
        let ws_dismiss = ws_entity.clone();
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .mb(px(8.))
            .child(
                div()
                    .font_family(t.ui_family.clone())
                    .text_size(px(t.font_size_caption))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(t.text_muted)
                    .child(rust_i18n::t!("lsp_overlay.title").to_string()),
            )
            .child(
                div()
                    .cursor_pointer()
                    .text_color(t.text_muted)
                    .text_size(px(t.font_size_caption))
                    .font_family(t.ui_family.clone())
                    .px(px(4.))
                    .child("✕")
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        if let Some(ws) = ws_dismiss.upgrade() {
                            ws.update(cx, |ws, cx| {
                                ws.lsp_overlay_open = false;
                                cx.notify();
                            });
                        }
                    }),
            );

        // ── server rows ───────────────────────────────────────────────────────
        // Always show all registered adapters so users can restart a stopped server.
        let all_statuses = mgr.as_ref().map(|m| m.all_server_statuses()).unwrap_or_default();
        let rows: Vec<AnyElement> = if all_statuses.is_empty() {
            vec![
                div()
                    .font_family(t.ui_family.clone())
                    .text_size(px(t.font_size_body))
                    .text_color(t.text_subtle)
                    .py(px(4.))
                    .child(rust_i18n::t!("lsp_overlay.no_servers").to_string())
                    .into_any_element(),
            ]
        } else {
            all_statuses
                .iter()
                .map(|s| {
                    let (state_text, dot_color) = match &s.state {
                        ServerState::Downloading => {
                            let msg = s.download_msg.as_deref().unwrap_or("Downloading...");
                            (msg.to_owned(), t.warning)
                        }
                        ServerState::Starting | ServerState::Initializing => {
                            (rust_i18n::t!("lsp_overlay.starting").to_string(), t.warning)
                        }
                        ServerState::Running => {
                            (rust_i18n::t!("lsp_overlay.running").to_string(), t.success)
                        }
                        ServerState::Restarting { attempt } => (
                            format!("{} ({})", rust_i18n::t!("lsp_overlay.restarting"), attempt),
                            t.warning,
                        ),
                        ServerState::Error(_) => {
                            (rust_i18n::t!("lsp_overlay.error").to_string(), t.error)
                        }
                        ServerState::Stopped => (
                            rust_i18n::t!("lsp_overlay.stopped").to_string(),
                            t.text_subtle,
                        ),
                    };

                    let server_id = s.server_id.clone();
                    let mgr_btn = mgr.clone();
                    let ws_stop = ws_entity.clone();

                    let is_running = matches!(s.state, ServerState::Running);
                    let is_error = matches!(s.state, ServerState::Error(_));
                    let is_stopped = matches!(s.state, ServerState::Stopped);

                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .py(px(8.))
                        .border_b_1()
                        .border_color(t.separator)
                        // server name row
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .size(px(7.))
                                        .rounded_full()
                                        .bg(dot_color)
                                        .flex_shrink_0(),
                                )
                                .child(
                                    div()
                                        .font_family(t.ui_family.clone())
                                        .text_size(px(t.font_size_body))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(t.text)
                                        .child(s.server_id.clone()),
                                ),
                        )
                        // state + action buttons row
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .pl(px(13.))
                                .child(
                                    div()
                                        .font_family(t.ui_family.clone())
                                        .text_size(px(t.font_size_caption))
                                        .text_color(dot_color)
                                        .child(state_text),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(8.))
                                        // Start — only when Stopped
                                        .when(is_stopped, |el| {
                                            let sid = server_id.clone();
                                            let mgr = mgr_btn.clone();
                                            el.child(
                                                div()
                                                    .cursor_pointer()
                                                    .text_size(px(t.font_size_caption))
                                                    .font_family(t.ui_family.clone())
                                                    .text_color(t.text_muted)
                                                    .child(
                                                        rust_i18n::t!("lsp_overlay.start")
                                                            .to_string(),
                                                    )
                                                    .on_mouse_down(MouseButton::Left, move |_, _, _cx| {
                                                        if let Some(m) = &mgr {
                                                            m.restart_server(&sid);
                                                        }
                                                    }),
                                            )
                                        })
                                        // Restart — when Running or Error
                                        .when(is_running || is_error, |el| {
                                            let sid = server_id.clone();
                                            let mgr = mgr_btn.clone();
                                            el.child(
                                                div()
                                                    .cursor_pointer()
                                                    .text_size(px(t.font_size_caption))
                                                    .font_family(t.ui_family.clone())
                                                    .text_color(t.text_muted)
                                                    .child(
                                                        rust_i18n::t!("lsp_overlay.restart")
                                                            .to_string(),
                                                    )
                                                    .on_mouse_down(MouseButton::Left, move |_, _, _cx| {
                                                        if let Some(m) = &mgr {
                                                            m.restart_server(&sid);
                                                        }
                                                    }),
                                            )
                                        })
                                        // Stop — only when Running
                                        .when(is_running, |el| {
                                            let sid = server_id.clone();
                                            let mgr = mgr_btn.clone();
                                            el.child(
                                                div()
                                                    .cursor_pointer()
                                                    .text_size(px(t.font_size_caption))
                                                    .font_family(t.ui_family.clone())
                                                    .text_color(t.text_muted)
                                                    .child(
                                                        rust_i18n::t!("lsp_overlay.stop")
                                                            .to_string(),
                                                    )
                                                    .on_mouse_down(MouseButton::Left, {
                                                        let ws = ws_stop.clone();
                                                        move |_, _, cx| {
                                                            if let Some(m) = &mgr {
                                                                m.stop_server(&sid);
                                                            }
                                                            if let Some(ws) = ws.upgrade() {
                                                                ws.update(cx, |ws, cx| {
                                                                    ws.lsp_overlay_open = false;
                                                                    cx.notify();
                                                                });
                                                            }
                                                        }
                                                    }),
                                            )
                                        }),
                                ),
                        )
                        .into_any_element()
                })
                .collect()
        };

        let panel = popover_container("lsp-overlay", t)
            .w(px(280.))
            .p(px(12.))
            .child(header)
            .children(rows);

        let pos = self.lsp_overlay_pos;
        Some(
            deferred(
                anchored()
                    .position(pos)
                    .anchor(gpui::Corner::BottomRight)
                    .snap_to_window_with_margin(px(8.))
                    .child(panel),
            )
            .with_priority(2)
            .into_any(),
        )
    }

    fn render_confirm_modal(
        &mut self,
        t: &RuntimeTheme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let state = self.confirm.as_ref()?;

        let focus_handle = state.focus_handle.clone();
        let message = state.message.clone();
        let default_ix = state.default_ix;
        let destructive_ix = state.destructive_ix;
        let buttons: Vec<ConfirmButton> = state.buttons.clone();

        let button_els: Vec<AnyElement> = buttons
            .into_iter()
            .enumerate()
            .map(|(ix, btn)| {
                let is_default = ix == default_ix;
                let is_destructive = destructive_ix == Some(ix);
                let t = t.clone();
                div()
                    .id(("cf-btn", ix))
                    .px(px(t.sp5))
                    .py(px(t.sp2))
                    .rounded(px(t.radius_sm))
                    .cursor_pointer()
                    .font_family(t.ui_family.clone())
                    .text_size(px(t.font_size_caption))
                    .border_1()
                    .when(is_default, |el| {
                        el.border_color(t.border_focus)
                            .bg(t.accent)
                            .text_color(t.text_on_accent)
                    })
                    .when(!is_default, |el| {
                        el.border_color(t.border)
                            .text_color(if is_destructive { t.error } else { t.text })
                            .hover(move |s| s.bg(t.line_highlight))
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |ws, _, window, cx| {
                            ws.on_confirm_answer(ix, window, cx);
                        }),
                    )
                    .child(btn.label)
                    .into_any()
            })
            .collect();

        let modal = modal_container("cf-modal", t)
            .w(px(440.))
            .key_context("ConfirmModal")
            .track_focus(&focus_handle)
            .on_action(cx.listener(Self::on_cf_confirm))
            .on_action(cx.listener(Self::on_cf_dismiss))
            .child(
                div()
                    .px(px(t.sp6))
                    .py(px(t.sp5))
                    .font_family(t.ui_family.clone())
                    .text_size(px(t.font_size_body))
                    .text_color(t.text)
                    .child(message),
            )
            .child(
                h_flex()
                    .px(px(t.sp4))
                    .py(px(t.sp4))
                    .gap(px(t.sp3))
                    .justify_end()
                    .border_t_1()
                    .border_color(t.separator)
                    .children(button_els),
            );

        Some(
            deferred(
                modal_backdrop("cf-backdrop", t)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|ws, _, window, cx| {
                            let cancel_ix = ws
                                .confirm
                                .as_ref()
                                .map(|s| s.buttons.len().saturating_sub(1))
                                .unwrap_or(0);
                            ws.on_confirm_answer(cancel_ix, window, cx);
                        }),
                    )
                    .child(modal),
            )
            .with_priority(3)
            .into_any(),
        )
    }

    fn on_open_symbol_finder(
        &mut self,
        _: &crate::OpenSymbolFinder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_symbol_finder(window, cx);
    }

    fn on_open_file_finder(
        &mut self,
        _: &OpenFileFinder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            return self.save_editor_as(editor, window, cx);
        }
        let abs_path = editor.read(cx).doc.path.clone();
        let ok = editor.update(cx, |ed, cx| {
            let ok = save(&ed.doc.rope, &ed.doc.path).is_ok();
            if ok {
                ed.doc.mark_saved();
            }
            cx.notify();
            ok
        });
        if ok && let Some(engine) = &self.index_engine {
            engine.request(faber_index::trigger::IndexTrigger::FileSaved(abs_path));
        }
        Task::ready(ok)
    }

    /// Prompt the user for a path and save the document there.
    fn save_editor_as(
        &self,
        editor: &Entity<EditorView>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        let dir = editor
            .read(cx)
            .doc
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .or_else(|| self.root_folder.clone())
            .or_else(|| std::env::var("HOME").ok().map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."));
        let hint = editor
            .read(cx)
            .doc
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "untitled.txt".to_string());
        let rx = cx.prompt_for_new_path(&dir, Some(hint.as_str()));
        let editor = editor.clone();
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(path))) = rx.await else {
                return false;
            };
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
    }

    /// Close a tab, prompting to save first if the document is dirty.
    fn request_close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        // Extract tab data in a block so the pane borrow drops before any mutable use.
        let (tab_id, editor) = {
            let pane = self.pane().read(cx);
            let Some(tab) = pane.tab_at(ix) else { return };
            (tab.id, tab.editor().cloned())
        };
        let Some(editor) = editor else {
            self.close_tab(ix, window, cx);
            return;
        };
        if !editor.read(cx).doc.dirty {
            self.close_tab(ix, window, cx);
            return;
        }
        let name = Self::doc_display_name(&editor.read(cx).doc);
        self.show_confirm(
            ConfirmSpec {
                message: rust_i18n::t!("dialog.save_changes", name = name)
                    .to_string()
                    .into(),
                buttons: vec![
                    ConfirmButton {
                        label: rust_i18n::t!("dialog.save").to_string().into(),
                    },
                    ConfirmButton {
                        label: rust_i18n::t!("dialog.dont_save").to_string().into(),
                    },
                    ConfirmButton {
                        label: rust_i18n::t!("dialog.cancel").to_string().into(),
                    },
                ],
                default_ix: 0,
                destructive_ix: Some(1),
                on_answer: Box::new(move |ws, answer, window, cx| match answer {
                    0 => {
                        let ws_entity = cx.entity();
                        let task = ws.save_editor(&editor, window, cx);
                        cx.spawn_in(window, async move |_, cx| {
                            if task.await {
                                ws_entity
                                    .update_in(cx, |ws, window, cx| {
                                        ws.close_tab_by_id(tab_id, window, cx)
                                    })
                                    .ok();
                            }
                        })
                        .detach();
                    }
                    1 => ws.close_tab_by_id(tab_id, window, cx),
                    _ => {}
                }),
            },
            window,
            cx,
        );
    }

    // ── action handlers ────────────────────────────────────────────────────────

    fn on_new_file(&mut self, _: &NewFile, window: &mut Window, cx: &mut Context<Self>) {
        let registry = cx.global::<crate::Registry>().0.clone();
        let editor = cx.new(|cx| EditorView::from_doc(Document::empty_untitled(), registry, cx));
        self.push_editor_tab(editor, cx);
        let new_ix = self.pane().read(cx).tab_count() - 1;
        self.activate_tab(new_ix, window, cx);
    }

    fn on_open_file(&mut self, _: &OpenFile, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        cx.spawn_in(window, async move |ws, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
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
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(folder) = paths.into_iter().next() else {
                return;
            };
            ws.update_in(cx, |ws, window, cx| {
                ws.set_root_folder(folder.clone(), cx);
                ws.check_and_show_trust_modal(&folder, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn on_save_file(&mut self, _: &SaveFile, window: &mut Window, cx: &mut Context<Self>) {
        let editor = self
            .pane()
            .read(cx)
            .active_tab()
            .and_then(|t| t.editor())
            .cloned();
        if let Some(editor) = editor {
            self.save_editor(&editor, window, cx).detach();
        }
    }

    fn on_save_as(&mut self, _: &SaveAs, window: &mut Window, cx: &mut Context<Self>) {
        let editor = self
            .pane()
            .read(cx)
            .active_tab()
            .and_then(|t| t.editor())
            .cloned();
        if let Some(editor) = editor {
            self.save_editor_as(&editor, window, cx).detach();
        }
    }

    fn on_save_all(&mut self, _: &SaveAll, _: &mut Window, cx: &mut Context<Self>) {
        self.save_all_dirty(cx);
    }

    fn on_close_window(&mut self, _: &CloseWindow, window: &mut Window, cx: &mut Context<Self>) {
        self.on_quit(&Quit, window, cx);
    }

    fn on_reindex_project(
        &mut self,
        _: &ReindexProject,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        if let Some(engine) = &self.index_engine {
            engine.request(faber_index::trigger::IndexTrigger::Manual);
        }
    }

    fn on_open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        let existing = self.pane().read(cx).find_settings_tab();
        if let Some(ix) = existing {
            self.activate_tab(ix, window, cx);
            return;
        }
        let view = cx.new(SettingsView::new);
        self.panes[&self.focused_pane].update(cx, |p: &mut Pane, cx| {
            p.push_settings_tab(view, cx);
        });
        let new_ix = self.pane().read(cx).tab_count() - 1;
        self.activate_tab(new_ix, window, cx);
    }

    fn on_toggle_lsp_status(
        &mut self,
        _: &ToggleLspStatus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.lsp_overlay_open = !self.lsp_overlay_open;
        if self.lsp_overlay_open {
            let size = window.viewport_size();
            self.lsp_overlay_pos = gpui::point(size.width - px(8.), size.height - px(30.));
        }
        cx.notify();
    }

    fn on_open_language_picker(
        &mut self,
        _: &OpenLanguagePicker,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(fh) = self.open_language_picker(cx) {
            window.focus(&fh);
        }
    }

    fn on_open_problems(&mut self, _: &OpenProblems, window: &mut Window, cx: &mut Context<Self>) {
        let existing = self.pane().read(cx).find_problems_tab();
        if let Some(ix) = existing {
            self.activate_tab(ix, window, cx);
            return;
        }
        let store = self.lsp_manager.as_ref().map(|m| m.diagnostic_store());
        let ws_weak = cx.weak_entity();
        let panel = cx.new(|cx| {
            let mut p = crate::panels::diagnostics_panel::DiagnosticsPanel::new(cx);
            if let Some(s) = store {
                p.set_store(s, ws_weak);
            }
            p
        });
        self.panes[&self.focused_pane].update(cx, |p: &mut Pane, cx| {
            p.push_problems_tab(panel, cx);
        });
        let new_ix = self.pane().read(cx).tab_count() - 1;
        self.activate_tab(new_ix, window, cx);
    }

    pub(crate) fn on_open_project_search(
        &mut self,
        _: &OpenProjectSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Prefill with active editor selection text if any.
        let prefill = self
            .pane()
            .read(cx)
            .active_tab()
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

        let existing = self.pane().read(cx).find_project_search_tab();
        let active_ix = self.pane().read(cx).active;
        if let Some(ix) = existing {
            // Toggle closed if already the active tab (mirrors Cmd+F in-file behaviour).
            if active_ix == Some(ix) {
                self.close_tab(ix, window, cx);
                return;
            }
            self.activate_tab(ix, window, cx);
            // Prefill query and focus.
            let ps_view = {
                let pane = self.pane().read(cx);
                pane.tab_at(ix).and_then(|t| t.project_search.clone())
            };
            if let Some(view) = ps_view {
                if !prefill.is_empty() {
                    view.update(cx, |psv, cx| psv.set_query(prefill, cx));
                }
                let qh = view.read(cx).query_handle.clone();
                window.focus(&qh);
            }
            return;
        }
        let ws_entity = cx.entity();
        let view = cx.new(|cx| ProjectSearchView::new(ws_entity.downgrade(), prefill, cx));
        self.panes[&self.focused_pane].update(cx, |p: &mut Pane, cx| {
            p.push_project_search_tab(view.clone(), cx);
        });
        let new_ix = self.pane().read(cx).tab_count() - 1;
        self.activate_tab(new_ix, window, cx);
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
        if let Some(editor) = self
            .pane()
            .read(cx)
            .active_tab()
            .and_then(|t| t.editor())
            .cloned()
        {
            editor.update(cx, |ev, cx| {
                let char_idx = ev
                    .line_starts
                    .get(line)
                    .map(|&ls| ls + col)
                    .unwrap_or(ev.doc.rope.len_chars().saturating_sub(1));
                ev.sel.head = char_idx;
                ev.sel.anchor = char_idx;
                ev.scroll_handle
                    .scroll_to_item(line, gpui::ScrollStrategy::Center);
                ev.flash_line = Some(line);
                cx.spawn(async move |view, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(800))
                        .await;
                    view.update(cx, |ev, cx| {
                        ev.flash_line = None;
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
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
        if let Some(engine) = &self.index_engine {
            engine.request(faber_index::trigger::IndexTrigger::FolderOpened);
        }
        cx.notify();
    }

    fn on_close_file(&mut self, _: &CloseFile, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.pane().read(cx).active;
        if let Some(ix) = ix {
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

    fn on_toggle_bottom_panel(
        &mut self,
        _: &ToggleBottomPanel,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.bottom_open = !self.bottom_open;
        cx.notify();
    }

    fn on_toggle_right_panel(
        &mut self,
        _: &ToggleRightPanel,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.right_open = !self.right_open;
        cx.notify();
    }

    fn on_quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        let editors = self.all_editors(cx);
        let dirty: Vec<Entity<EditorView>> = editors
            .into_iter()
            .filter(|e| e.read(cx).doc.dirty)
            .collect();
        if dirty.is_empty() {
            cx.quit();
            return;
        }
        let count = dirty.len();
        let msg = if count == 1 {
            rust_i18n::t!("dialog.unsaved_count_one", count = count)
        } else {
            rust_i18n::t!("dialog.unsaved_count_other", count = count)
        };
        self.show_confirm(
            ConfirmSpec {
                message: msg.to_string().into(),
                buttons: vec![
                    ConfirmButton {
                        label: rust_i18n::t!("dialog.save_all_quit").to_string().into(),
                    },
                    ConfirmButton {
                        label: rust_i18n::t!("dialog.quit_without_saving")
                            .to_string()
                            .into(),
                    },
                    ConfirmButton {
                        label: rust_i18n::t!("dialog.cancel").to_string().into(),
                    },
                ],
                default_ix: 0,
                destructive_ix: Some(1),
                on_answer: Box::new(move |ws, answer, window, cx| {
                    match answer {
                        0 => {
                            let ws_entity = cx.entity();
                            let tasks: Vec<_> = dirty
                                .iter()
                                .map(|editor| ws.save_editor(editor, window, cx))
                                .collect();
                            cx.spawn_in(window, async move |_, cx| {
                                for task in tasks {
                                    if !task.await {
                                        return; // cancelled a Save As dialog — abort quit
                                    }
                                }
                                ws_entity.update_in(cx, |_, _, cx| cx.quit()).ok();
                            })
                            .detach();
                        }
                        1 => cx.quit(),
                        _ => {}
                    }
                }),
            },
            window,
            cx,
        );
    }

    fn on_close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.pane().read(cx).active;
        if let Some(ix) = ix {
            self.request_close_tab(ix, window, cx);
        }
    }

    /// Close all tabs except the one with `keep_id`.
    fn close_other_tabs(&mut self, keep_id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<usize> = self
            .pane()
            .read(cx)
            .tabs
            .iter()
            .filter(|t| t.id != keep_id)
            .map(|t| t.id)
            .collect();
        for id in ids {
            let ix = self.pane().read(cx).tab_by_id(id).map(|(ix, _)| ix);
            if let Some(ix) = ix {
                self.request_close_tab(ix, window, cx);
            }
        }
    }

    /// Close all tabs.
    fn close_all_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<usize> = self.pane().read(cx).tabs.iter().map(|t| t.id).collect();
        for id in ids {
            let ix = self.pane().read(cx).tab_by_id(id).map(|(ix, _)| ix);
            if let Some(ix) = ix {
                self.request_close_tab(ix, window, cx);
            }
        }
    }

    /// Close all tabs to the left of `anchor_id`.
    fn close_tabs_to_left(
        &mut self,
        anchor_id: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ids: Vec<usize> = {
            let pane = self.pane().read(cx);
            let anchor_ix = pane
                .tabs
                .iter()
                .position(|t| t.id == anchor_id)
                .unwrap_or(0);
            pane.tabs[..anchor_ix].iter().map(|t| t.id).collect()
        };
        for id in ids {
            let ix = self.pane().read(cx).tab_by_id(id).map(|(ix, _)| ix);
            if let Some(ix) = ix {
                self.request_close_tab(ix, window, cx);
            }
        }
    }

    /// Close all tabs to the right of `anchor_id`.
    fn close_tabs_to_right(
        &mut self,
        anchor_id: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ids: Vec<usize> = {
            let pane = self.pane().read(cx);
            let anchor_ix = pane
                .tabs
                .iter()
                .position(|t| t.id == anchor_id)
                .unwrap_or(0);
            pane.tabs[anchor_ix + 1..].iter().map(|t| t.id).collect()
        };
        for id in ids {
            let ix = self.pane().read(cx).tab_by_id(id).map(|(ix, _)| ix);
            if let Some(ix) = ix {
                self.request_close_tab(ix, window, cx);
            }
        }
    }

    fn on_next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        let (active, count) = {
            let pane = self.pane().read(cx);
            (pane.active, pane.tab_count())
        };
        if let Some(ix) = active {
            let next = (ix + 1) % count;
            self.activate_tab(next, window, cx);
        }
    }

    fn on_prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        let (active, count) = {
            let pane = self.pane().read(cx);
            (pane.active, pane.tab_count())
        };
        if let Some(ix) = active {
            let prev = if ix == 0 { count - 1 } else { ix - 1 };
            self.activate_tab(prev, window, cx);
        }
    }

    // ── rendering ──────────────────────────────────────────────────────────────

    fn render_context_menu(&self, t: &RuntimeTheme, cx: &mut Context<Self>) -> Option<AnyElement> {
        // Read all pane data before any mutable cx use.
        let (tab_id, pos, tab_ix, path, tab_count) = {
            let pane = self.pane().read(cx);
            let menu = pane.tab_menu.as_ref()?;
            let tab_id = menu.tab_id;
            let pos = menu.pos;
            let tab_ix = pane.tabs.iter().position(|t| t.id == tab_id)?;
            let path = pane
                .tabs
                .get(tab_ix)
                .and_then(|t| t.editor())
                .map(|e| e.read(cx).doc.path.clone())
                .filter(|p| !p.as_os_str().is_empty());
            let tab_count = pane.tabs.len();
            (tab_id, pos, tab_ix, path, tab_count)
        };
        let has_path = path.is_some();
        let has_left = tab_ix > 0;
        let has_right = tab_ix + 1 < tab_count;
        let has_others = tab_count > 1;
        let root = self.root_folder.clone();

        let ws = cx.entity();

        // ── helper: build a single menu item ─────────────────────────────────
        let item = |label: SharedString, enabled: bool, on_click: MenuClickFn| ContextMenuItem {
            label,
            enabled,
            on_click,
        };
        let sep = || div().h(px(1.)).mx(px(t.sp2)).my(px(t.sp1)).bg(t.separator);

        // ── close group ──────────────────────────────────────────────────────
        let close_item = {
            let ws = ws.clone();
            item(
                rust_i18n::t!("tab_menu.close").into(),
                true,
                Box::new(move |_, window, cx| {
                    ws.update(cx, |ws, cx| {
                        ws.close_tab_menu(cx);
                        let ix = ws.pane().read(cx).tab_by_id(tab_id).map(|(ix, _)| ix);
                        if let Some(ix) = ix {
                            ws.request_close_tab(ix, window, cx);
                        }
                    });
                }),
            )
        };

        let close_others = {
            let ws = ws.clone();
            item(
                rust_i18n::t!("tab_menu.close_others").into(),
                has_others,
                Box::new(move |_, window, cx| {
                    ws.update(cx, |ws, cx| {
                        ws.close_tab_menu(cx);
                        ws.close_other_tabs(tab_id, window, cx);
                    });
                }),
            )
        };

        let close_all = {
            let ws = ws.clone();
            item(
                rust_i18n::t!("tab_menu.close_all").into(),
                true,
                Box::new(move |_, window, cx| {
                    ws.update(cx, |ws, cx| {
                        ws.close_tab_menu(cx);
                        ws.close_all_tabs(window, cx);
                    });
                }),
            )
        };

        let close_left = {
            let ws = ws.clone();
            item(
                rust_i18n::t!("tab_menu.close_left").into(),
                has_left,
                Box::new(move |_, window, cx| {
                    ws.update(cx, |ws, cx| {
                        ws.close_tab_menu(cx);
                        ws.close_tabs_to_left(tab_id, window, cx);
                    });
                }),
            )
        };

        let close_right = {
            let ws = ws.clone();
            item(
                rust_i18n::t!("tab_menu.close_right").into(),
                has_right,
                Box::new(move |_, window, cx| {
                    ws.update(cx, |ws, cx| {
                        ws.close_tab_menu(cx);
                        ws.close_tabs_to_right(tab_id, window, cx);
                    });
                }),
            )
        };

        // ── copy group ───────────────────────────────────────────────────────
        let copy_path = {
            let ws = ws.clone();
            let p = path.clone();
            item(
                rust_i18n::t!("tab_menu.copy_path").into(),
                has_path,
                Box::new(move |_, _, cx| {
                    let Some(p) = p.clone() else { return };
                    ws.update(cx, |ws, cx| {
                        ws.close_tab_menu(cx);
                        cx.write_to_clipboard(ClipboardItem::new_string(p.display().to_string()));
                        cx.notify();
                    });
                }),
            )
        };

        let copy_rel = {
            let ws = ws.clone();
            let rel = match (&root, &path) {
                (Some(r), Some(p)) => p
                    .strip_prefix(r)
                    .ok()
                    .map(|rel| rel.display().to_string())
                    .unwrap_or_else(|| {
                        p.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default()
                    }),
                (None, Some(p)) => p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
                _ => String::new(),
            };
            item(
                rust_i18n::t!("tab_menu.copy_relative_path").into(),
                has_path,
                Box::new(move |_, _, cx| {
                    let rel = rel.clone();
                    ws.update(cx, |ws, cx| {
                        ws.close_tab_menu(cx);
                        cx.write_to_clipboard(ClipboardItem::new_string(rel));
                        cx.notify();
                    });
                }),
            )
        };

        // ── reveal group ─────────────────────────────────────────────────────
        let reveal_finder = {
            let ws = ws.clone();
            let p = path.clone();
            item(
                rust_i18n::t!("tab_menu.reveal_in_finder").into(),
                has_path,
                Box::new(move |_, _, cx| {
                    let Some(p) = p.clone() else { return };
                    ws.update(cx, |ws, cx| {
                        ws.close_tab_menu(cx);
                        cx.reveal_path(&p);
                        cx.notify();
                    });
                }),
            )
        };

        let reveal_explorer = {
            let ws = ws.clone();
            let p = path.clone();
            item(
                rust_i18n::t!("tab_menu.reveal_in_explorer").into(),
                has_path,
                Box::new(move |_, _, cx| {
                    let Some(p) = p.clone() else { return };
                    ws.update(cx, |ws, cx| {
                        ws.close_tab_menu(cx);
                        ws.sidebar.open = true;
                        ws.sidebar.active = SidebarItemKind::Explorer;
                        ws.reveal_in_tree(&p, cx);
                        cx.notify();
                    });
                }),
            )
        };

        let split_items: Vec<ContextMenuItem> = [
            (rust_i18n::t!("tab_menu.split_left"), SplitDirection::Left),
            (rust_i18n::t!("tab_menu.split_right"), SplitDirection::Right),
            (rust_i18n::t!("tab_menu.split_up"), SplitDirection::Up),
            (rust_i18n::t!("tab_menu.split_down"), SplitDirection::Down),
        ]
        .into_iter()
        .map(|(label, dir)| {
            let ws = ws.clone();
            item(
                label.into(),
                true,
                Box::new(move |_, window, cx| {
                    ws.update(cx, |ws, cx| {
                        ws.close_tab_menu(cx);
                        ws.split_focused(dir, window, cx);
                    });
                }),
            )
        })
        .collect();

        let items: Vec<ContextMenuItem> =
            vec![close_item, close_others, close_all, close_left, close_right];
        let copy_items: Vec<ContextMenuItem> = vec![copy_path, copy_rel];
        let reveal_items: Vec<ContextMenuItem> = vec![reveal_finder, reveal_explorer];

        let menu_div = popover_container("tab-ctx-menu", t)
            .on_mouse_down_out(cx.listener(|ws, _, _, cx| {
                ws.close_tab_menu(cx);
                cx.notify();
            }))
            .flex()
            .flex_col()
            .py(px(t.sp2))
            .min_w(px(200.))
            .children(items)
            .child(sep())
            .children(split_items)
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
            .child(
                h_flex()
                    .gap_1()
                    .child(left_btn)
                    .child(right_btn)
                    .child(bottom_btn)
                    .child(settings_btn),
            )
            .child(div().w_2())
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

    // ── Split / pane management ───────────────────────────────────────────────

    fn split_focused(&mut self, dir: SplitDirection, window: &mut Window, cx: &mut Context<Self>) {
        let new_pane_id = PaneId(self.next_pane_id);
        self.next_pane_id += 1;
        let new_pane = cx.new(|cx| Pane::new(new_pane_id, cx));
        let registry = cx.global::<crate::Registry>().0.clone();
        let editor = cx.new(|cx| {
            EditorView::from_doc(
                faber_editor::buffer::Document::empty_untitled(),
                registry,
                cx,
            )
        });
        cx.subscribe(&editor, |ws, _, _: &EditorEvent, cx| {
            ws.on_editor_edited(cx)
        })
        .detach();
        new_pane.update(cx, |p: &mut Pane, cx| {
            p.push_editor_tab(editor, cx);
        });
        self.panes.insert(new_pane_id, new_pane);
        self.pane_group.split(self.focused_pane, new_pane_id, dir);
        self.focused_pane = new_pane_id;
        self.focus_active(window, cx);
        cx.notify();
    }

    fn move_tab(
        &mut self,
        source_pane: PaneId,
        tab_id: usize,
        target_pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ix = {
            let pane = self.panes[&source_pane].read(cx);
            let Some((ix, _)) = pane.tab_by_id(tab_id) else {
                return;
            };
            ix
        };
        let mut removed_tab = None;
        self.panes[&source_pane].update(cx, |p: &mut Pane, _| {
            removed_tab = p.remove_tab(ix);
        });
        let Some(tab) = removed_tab else { return };
        if let Some(e) = tab.editor.as_ref() {
            cx.subscribe(e, |ws, _, _: &EditorEvent, cx| ws.on_editor_edited(cx))
                .detach();
        }
        self.panes[&target_pane].update(cx, |p: &mut Pane, _| {
            p.push_tab_raw(tab);
        });
        self.focused_pane = target_pane;
        let new_ix = self.panes[&target_pane].read(cx).tab_count() - 1;
        self.activate_tab_in(target_pane, new_ix, window, cx);
        if self.panes.contains_key(&source_pane) && self.panes[&source_pane].read(cx).is_empty() {
            self.collapse_pane(source_pane, window, cx);
        }
    }

    fn collapse_pane(&mut self, pane_id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(neighbor_id) = self.pane_group.remove_pane(pane_id) {
            self.panes.remove(&pane_id);
            self.focused_pane = neighbor_id;
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    fn on_tab_drop(
        &mut self,
        dragged: DraggedTab,
        target_pane: PaneId,
        zone: DropZone,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match zone {
            DropZone::Center => {
                if dragged.source_pane != target_pane {
                    self.move_tab(dragged.source_pane, dragged.tab_id, target_pane, window, cx);
                }
            }
            DropZone::Edge(dir) => {
                let source_tab_count = self.panes[&dragged.source_pane].read(cx).tab_count();
                if dragged.source_pane == target_pane && source_tab_count == 1 {
                    return;
                }
                let new_pane_id = PaneId(self.next_pane_id);
                self.next_pane_id += 1;
                let new_pane = cx.new(|cx| Pane::new(new_pane_id, cx));
                self.panes.insert(new_pane_id, new_pane);
                self.pane_group.split(target_pane, new_pane_id, dir);
                self.move_tab(dragged.source_pane, dragged.tab_id, new_pane_id, window, cx);
            }
        }
        cx.notify();
    }

    // ── Split action handlers ─────────────────────────────────────────────────

    fn on_split_left(&mut self, _: &SplitLeft, w: &mut Window, cx: &mut Context<Self>) {
        self.split_focused(SplitDirection::Left, w, cx);
    }

    fn on_split_right(&mut self, _: &SplitRight, w: &mut Window, cx: &mut Context<Self>) {
        self.split_focused(SplitDirection::Right, w, cx);
    }

    fn on_split_up(&mut self, _: &SplitUp, w: &mut Window, cx: &mut Context<Self>) {
        self.split_focused(SplitDirection::Up, w, cx);
    }

    fn on_split_down(&mut self, _: &SplitDown, w: &mut Window, cx: &mut Context<Self>) {
        self.split_focused(SplitDirection::Down, w, cx);
    }

    // ── Multi-pane rendering ──────────────────────────────────────────────────

    fn render_tab_for(
        &self,
        pane_id: PaneId,
        ix: usize,
        t: &RuntimeTheme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let (title, dirty, is_active, tab_id, icon) = {
            let pane = self.panes[&pane_id].read(cx);
            let tab = &pane.tabs[ix];
            let (title, dirty) = tab.title(cx);
            let is_active = pane.active == Some(ix);
            let icon: AnyElement = match tab.content.kind {
                TabKind::Editor => img(crate::file_icons::icon_for_file(&title))
                    .size(px(14.0))
                    .flex_shrink_0()
                    .into_any_element(),
                TabKind::Settings => svg()
                    .path(IconName::Settings.path())
                    .size(px(14.0))
                    .flex_shrink_0()
                    .text_color(t.text_muted)
                    .into_any_element(),
                TabKind::ProjectSearch => svg()
                    .path(IconName::Search.path())
                    .size(px(14.0))
                    .flex_shrink_0()
                    .text_color(t.text_muted)
                    .into_any_element(),
                TabKind::Problems => svg()
                    .path(IconName::Search.path())
                    .size(px(14.0))
                    .flex_shrink_0()
                    .text_color(t.text_muted)
                    .into_any_element(),
            };
            (title, dirty, is_active, tab.id, icon)
        };

        let element_id = SharedString::from(format!("tab-{}-{}", pane_id.0, tab_id));

        h_flex()
            .id(element_id)
            .group("tab")
            .flex_shrink_0()
            .max_w(px(170.))
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
            .on_drag(
                DraggedTab {
                    source_pane: pane_id,
                    tab_id,
                    title: title.clone(),
                },
                |dragged: &DraggedTab, _point, _window, cx| {
                    let title = dragged.title.clone();
                    cx.new(|_| TabGhost { title })
                },
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |ws, _, window, cx| ws.activate_tab_in(pane_id, ix, window, cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |ws, ev: &MouseDownEvent, _, cx| {
                    ws.panes[&pane_id].update(cx, |p: &mut Pane, _| {
                        p.tab_menu = Some(TabMenu {
                            tab_id,
                            pos: ev.position,
                        });
                    });
                    ws.focused_pane = pane_id;
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |ws, _, window, cx| {
                    ws.request_close_tab_in(pane_id, ix, window, cx)
                }),
            )
            .child(icon)
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(title),
            )
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
                    .size(px(16.0))
                    .flex_shrink_0()
                    .text_color(gpui::transparent_black())
                    .group_hover("tab", |s| s.text_color(t.text_subtle))
                    .hover(|s| s.text_color(t.text))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |ws, _, window, cx| {
                            cx.stop_propagation();
                            ws.request_close_tab_in(pane_id, ix, window, cx);
                        }),
                    ),
            )
    }

    fn render_tab_bar_for(
        &self,
        pane_id: PaneId,
        t: &RuntimeTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tab_count = self.panes[&pane_id].read(cx).tab_count();
        let bar_id = SharedString::from(format!("tab-bar-{}", pane_id.0));
        div()
            .id(bar_id)
            .flex()
            .flex_row()
            .h(px(30.0))
            .flex_shrink_0()
            .overflow_x_scroll()
            .bg(t.bg_elevated)
            .border_b_1()
            .border_color(t.separator)
            .on_drop::<DraggedTab>(cx.listener(move |ws, dragged: &DraggedTab, window, cx| {
                ws.drop_hover = None;
                if dragged.source_pane != pane_id {
                    let d = dragged.clone();
                    ws.move_tab(d.source_pane, d.tab_id, pane_id, window, cx);
                }
                cx.notify();
            }))
            .children((0..tab_count).map(|ix| self.render_tab_for(pane_id, ix, t, cx)))
    }

    fn render_sash(
        &self,
        axis_path: Vec<usize>,
        divider_ix: usize,
        axis: Axis,
        t: &RuntimeTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_h = matches!(axis, Axis::Horizontal);
        let sash_id = SharedString::from(format!(
            "sash-{}-{}",
            axis_path
                .iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join("-"),
            divider_ix
        ));
        let accent = t.accent;
        let sep = t.separator;
        div()
            .id(sash_id)
            .flex_shrink_0()
            .when(is_h, |el| el.w(px(4.)).h_full().cursor_ew_resize())
            .when(!is_h, |el| el.h(px(4.)).w_full().cursor_ns_resize())
            .bg(sep)
            .hover(move |s| s.bg(accent))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |ws, ev: &MouseDownEvent, _, cx| {
                    ws.pane_resize = Some(PaneResize {
                        axis_path: axis_path.clone(),
                        divider_ix,
                        axis,
                        start_cursor: ev.position,
                    });
                    cx.notify();
                }),
            )
    }

    fn render_pane_area(
        &self,
        pane_id: PaneId,
        t: &RuntimeTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let content: Option<AnyElement> = {
            let pane = self.panes[&pane_id].read(cx);
            pane.active_tab()
                .map(|tab| tab.content.view.clone().into_any_element())
        };

        let tab_bar = self.render_tab_bar_for(pane_id, t, cx);

        // Drop-zone highlight for this pane, shown only while a tab is actually
        // being dragged (so an aborted drag can't leave a stale overlay).
        let hover_zone = if cx.has_active_drag() {
            self.drop_hover
                .filter(|(id, _)| *id == pane_id)
                .map(|(_, z)| z)
        } else {
            None
        };

        v_flex()
            .flex_1()
            .h_full()
            .min_w(px(0.))
            .min_h(px(0.))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |ws, _, _, cx| {
                    ws.focused_pane = pane_id;
                    cx.notify();
                }),
            )
            .child(tab_bar)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .on_drag_move::<DraggedTab>(cx.listener(
                        move |ws, ev: &DragMoveEvent<DraggedTab>, _, cx| {
                            // Every pane's handler fires on each move (capture phase),
                            // so gate on whether the cursor is actually over THIS pane.
                            let bounds = ev.bounds;
                            let cursor = ev.event.position;
                            let pane_rect = PaneRect {
                                x: f32::from(bounds.origin.x),
                                y: f32::from(bounds.origin.y),
                                w: f32::from(bounds.size.width),
                                h: f32::from(bounds.size.height),
                            };
                            let cursor_v = PaneVec2 {
                                x: f32::from(cursor.x),
                                y: f32::from(cursor.y),
                            };
                            if pane_rect.contains(cursor_v) {
                                let zone =
                                    faber_core::pane_tree::drop_zone(pane_rect, cursor_v, 0.25);
                                if ws.drop_hover != Some((pane_id, zone)) {
                                    ws.drop_hover = Some((pane_id, zone));
                                    cx.notify();
                                }
                            } else if matches!(ws.drop_hover, Some((p, _)) if p == pane_id) {
                                ws.drop_hover = None;
                                cx.notify();
                            }
                        },
                    ))
                    .on_drop::<DraggedTab>(cx.listener(
                        move |ws, dragged: &DraggedTab, window, cx| {
                            let zone = ws
                                .drop_hover
                                .take()
                                .map(|(_, z)| z)
                                .unwrap_or(DropZone::Center);
                            let d = dragged.clone();
                            ws.on_tab_drop(d, pane_id, zone, window, cx);
                        },
                    ))
                    .when_some(content, |el, c| el.child(c))
                    .when_some(hover_zone, |el, zone| {
                        el.child(deferred(Self::drop_overlay(zone, t)))
                    }),
            )
            .into_any_element()
    }

    /// Translucent accent rect covering the region a dragged tab would occupy:
    /// the whole body for `Center`, or the matching half for an `Edge` split.
    fn drop_overlay(zone: DropZone, t: &RuntimeTheme) -> Div {
        let mut el = div()
            .absolute()
            .bg(t.accent)
            .opacity(0.25)
            .border_2()
            .border_color(t.accent);
        el = match zone {
            DropZone::Center => el.top_0().left_0().w_full().h_full(),
            DropZone::Edge(SplitDirection::Left) => el.top_0().left_0().h_full().w(relative(0.5)),
            DropZone::Edge(SplitDirection::Right) => el.top_0().right_0().h_full().w(relative(0.5)),
            DropZone::Edge(SplitDirection::Up) => el.top_0().left_0().w_full().h(relative(0.5)),
            DropZone::Edge(SplitDirection::Down) => {
                el.bottom_0().left_0().w_full().h(relative(0.5))
            }
        };
        el
    }

    fn render_pane_group(
        &self,
        member: Member<PaneId>,
        axis_path: Vec<usize>,
        t: &RuntimeTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match member {
            Member::Pane(id) => self.render_pane_area(id, t, cx),
            Member::Axis(axis_node) => {
                let is_h = matches!(axis_node.axis, Axis::Horizontal);
                let axis = axis_node.axis;
                // Build axis containers without `items_center` (h_flex applies it),
                // so children stretch on the cross axis and fill the pane slot.
                let container: Div = if is_h {
                    div().flex().flex_row()
                } else {
                    div().flex().flex_col()
                };
                let mut container = container.flex_1().h_full().min_w(px(0.)).min_h(px(0.));
                for (i, (member, flex)) in axis_node
                    .members
                    .into_iter()
                    .zip(axis_node.flexes.iter())
                    .enumerate()
                {
                    if i > 0 {
                        let sash = self.render_sash(axis_path.clone(), i - 1, axis, t, cx);
                        container = container.child(sash);
                    }
                    let mut child_path = axis_path.clone();
                    child_path.push(i);
                    let child_elem = self.render_pane_group(member, child_path, t, cx);
                    let mut wrapper = div().flex().min_w(px(0.)).min_h(px(0.)).overflow_hidden();
                    wrapper.style().flex_grow = Some(*flex);
                    wrapper.style().flex_basis = Some(px(0.).into());
                    let wrapper = wrapper.child(child_elem);
                    container = container.child(wrapper);
                }
                container.into_any_element()
            }
        }
    }

    // ── Layout persistence helpers ────────────────────────────────────────────

    fn serialize_layout(&self, cx: &App) -> faber_settings::state::SerializedLayout {
        use faber_core::pane_tree::SerializedMember;
        use faber_settings::state::{SerializedLayout, SerializedNode, SerializedPane};

        fn convert(m: SerializedMember<SerializedPane>) -> SerializedNode {
            match m {
                SerializedMember::Pane(p) => SerializedNode::Pane(p),
                SerializedMember::Axis {
                    axis,
                    members,
                    flexes,
                } => SerializedNode::Axis {
                    axis: match axis {
                        Axis::Horizontal => "horizontal".to_string(),
                        Axis::Vertical => "vertical".to_string(),
                    },
                    members: members.into_iter().map(convert).collect(),
                    flexes,
                },
            }
        }

        let root_member = self.pane_group.to_serialized(&|id: PaneId| {
            let pane = self.panes[&id].read(cx);
            let files: Vec<String> = pane
                .tabs
                .iter()
                .filter_map(|tab| tab.editor())
                .map(|e| e.read(cx).doc.path.to_string_lossy().to_string())
                .filter(|p| !p.is_empty())
                .collect();
            let active = pane.active.unwrap_or(0);
            SerializedPane { files, active }
        });
        SerializedLayout {
            root: convert(root_member),
        }
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
            .on_action(cx.listener(Self::on_save_as))
            .on_action(cx.listener(Self::on_save_all))
            .on_action(cx.listener(Self::on_close_window))
            .on_action(cx.listener(Self::on_close_file))
            .on_action(cx.listener(Self::on_close_folder))
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_toggle_bottom_panel))
            .on_action(cx.listener(Self::on_toggle_right_panel))
            .on_action(cx.listener(Self::on_open_settings))
            .on_action(cx.listener(Self::on_open_problems))
            .on_action(cx.listener(Self::on_toggle_lsp_status))
            .on_action(cx.listener(Self::on_open_language_picker))
            .on_action(cx.listener(Self::on_reindex_project))
            .on_action(cx.listener(Self::on_open_project_search))
            .on_action(cx.listener(Self::on_open_file_finder))
            .on_action(cx.listener(Self::on_open_file_finder_preview))
            .on_action(cx.listener(Self::on_open_symbol_finder))
            .on_action(cx.listener(Self::on_quit))
            .on_action(cx.listener(Self::on_split_left))
            .on_action(cx.listener(Self::on_split_right))
            .on_action(cx.listener(Self::on_split_up))
            .on_action(cx.listener(Self::on_split_down))
            .when(self.pane_resize.is_some(), |el| {
                let axis = self.pane_resize.as_ref().map(|r| r.axis);
                el.when(axis == Some(Axis::Horizontal), |el| el.cursor_ew_resize())
                    .when(axis == Some(Axis::Vertical), |el| el.cursor_ns_resize())
                    .on_mouse_move(cx.listener(|ws, ev: &MouseMoveEvent, window, cx| {
                        if let Some(ref resize) = ws.pane_resize {
                            let delta = ev.position - resize.start_cursor;
                            let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
                            let shift = if resize.axis == Axis::Horizontal {
                                dx
                            } else {
                                dy
                            };
                            let vp = window.viewport_size();
                            let container_px = if resize.axis == Axis::Horizontal {
                                f32::from(vp.width)
                            } else {
                                f32::from(vp.height)
                            };
                            ws.pane_group.resize(
                                &resize.axis_path,
                                resize.divider_ix,
                                shift,
                                container_px,
                                0.1,
                            );
                            ws.pane_resize = Some(PaneResize {
                                start_cursor: ev.position,
                                ..ws.pane_resize.clone().unwrap()
                            });
                            cx.notify();
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|ws, _, _, cx| {
                            ws.pane_resize = None;
                            cx.notify();
                        }),
                    )
            })
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
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|ws, _, _, cx| {
                        ws.tree_scrollbar_drag = None;
                        cx.notify();
                    }),
                )
            });

        // Empty state: show welcome screen only when no folder and all panes are empty.
        let all_empty = self.panes.values().all(|p| p.read(cx).is_empty());
        if all_empty && self.root_folder.is_none() {
            let (recent_projects, recent_files) = {
                let store = cx.global::<AppStateStore>();
                (
                    store.0.recent_projects.clone(),
                    store.0.recent_files.clone(),
                )
            };
            return base
                .flex()
                .items_center()
                .justify_center()
                .child(render_welcome(
                    &t,
                    &recent_projects,
                    &recent_files,
                    &cx.entity(),
                ))
                .when_some(self.file_finder.clone(), |el, finder| {
                    el.relative().child(finder)
                })
                .when_some(self.symbol_finder.clone(), |el, finder| {
                    el.relative().child(finder)
                })
                .when_some(self.language_picker.clone(), |el, picker| {
                    el.relative().child(picker)
                })
                .map(|el| match self.render_confirm_modal(&t, cx) {
                    Some(modal) => el.relative().child(modal),
                    None => el,
                })
                .map(|el| match self.render_lsp_overlay(&t, cx) {
                    Some(overlay) => el.child(overlay),
                    None => el,
                })
                .into_any();
        }

        let root_member = self.pane_group.root.clone();
        let main = self.render_pane_group(root_member, vec![], &t, cx);

        let body_row = h_flex()
            .flex_1()
            .min_h(px(0.))
            .child(self.render_activity_bar(&t, cx))
            .when(self.sidebar.open, |el| {
                el.child(self.render_sidebar_panel(&t, cx))
                    .child(self.render_sidebar_resize_handle(&t, cx))
            })
            .child(main)
            .when(self.right_open, |el| {
                el.child(self.render_right_panel(&t, cx))
            });

        let body = v_flex()
            .flex_1()
            .min_h(px(0.))
            .child(body_row)
            .when(self.bottom_open, |el| {
                el.child(self.render_bottom_panel(&t))
            });

        let root = base
            .flex()
            .flex_col()
            .relative()
            .child(self.render_titlebar(&t, cx))
            .child(body)
            .child(self.status_bar.clone())
            .map(|el| match self.render_context_menu(&t, cx) {
                Some(menu) => el.child(menu),
                None => el,
            })
            .when_some(self.file_finder.clone(), |el, finder| el.child(finder))
            .when_some(self.symbol_finder.clone(), |el, finder| el.child(finder))
            .when_some(self.language_picker.clone(), |el, picker| el.child(picker))
            .map(|el| match self.render_confirm_modal(&t, cx) {
                Some(modal) => el.child(modal),
                None => el,
            })
            .map(|el| match self.render_lsp_overlay(&t, cx) {
                Some(overlay) => el.child(overlay),
                None => el,
            });

        root.into_any()
    }
}

/// All LSP adapters active by default. Add new language servers here — one line each.
fn default_lsp_adapters() -> Vec<Box<dyn LspAdapter>> {
    vec![
        Box::new(RustAnalyzerAdapter),
        // Box::new(TypeScriptLanguageServerAdapter),  // add future adapters here
    ]
}
