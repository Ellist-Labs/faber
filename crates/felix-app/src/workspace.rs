use std::path::{Path, PathBuf};
use std::time::Duration;

use felix_settings::AutoSave;

use felix_editor::{
    buffer::Document,
    project::{FileTree, VisibleRow},
    save::save,
};
use gpui::{
    AnyElement, App, Context, Div, Entity, FocusHandle, Focusable, IntoElement, MouseButton,
    PathPromptOptions, PromptLevel, Render, Stateful, Task, Window, div, img, prelude::*, px,
    svg,
};

use crate::editor_view::{EditorEvent, EditorView};
use crate::settings_view::{SettingsStore, SettingsView};
use crate::sidebar::{SidebarItem, SidebarItemKind, SidebarState, default_items};
use crate::theme::RuntimeTheme;
use crate::ui::{IconName, h_flex, v_flex};
use crate::welcome_view::render_welcome;
use crate::{
    CloseFile, CloseFolder, CloseTab, NewFile, NextTab, OpenFile, OpenFolder, OpenSettings,
    PrevTab, Quit, SaveFile, ToggleSidebar,
};

pub enum TabContent {
    Editor(Entity<EditorView>),
    Settings(Entity<SettingsView>),
}

pub struct Tab {
    pub id: usize,
    pub content: TabContent,
}

impl Tab {
    pub(crate) fn editor(&self) -> Option<&Entity<EditorView>> {
        match &self.content {
            TabContent::Editor(e) => Some(e),
            TabContent::Settings(_) => None,
        }
    }

    /// (title, dirty) for the tab strip.
    fn title(&self, cx: &App) -> (String, bool) {
        match &self.content {
            TabContent::Editor(e) => {
                let doc = &e.read(cx).doc;
                (Workspace::doc_display_name(doc), doc.dirty)
            }
            TabContent::Settings(_) => ("Settings".to_string(), false),
        }
    }

    fn tab_focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.content {
            TabContent::Editor(e) => e.read(cx).focus_handle.clone(),
            TabContent::Settings(s) => s.read(cx).focus_handle.clone(),
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
    pub(crate) tree: Option<FileTree>,
    pub(crate) visible_rows: Vec<VisibleRow>,
    /// Bumped on every edit; a debounced auto-save fires only if no newer
    /// edit arrived while its timer slept.
    autosave_generation: u64,
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
            tree: None,
            visible_rows: Vec::new(),
            autosave_generation: 0,
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
                ed.doc.dirty = false;
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
        if cx.global::<SettingsStore>().0.auto_save == AutoSave::OnFocusChange {
            if let Some(prev) = self.active.filter(|&prev| prev != ix) {
                if let Some(editor) = self.tabs.get(prev).and_then(|t| t.editor()).cloned() {
                    Self::save_doc_now(&editor, cx);
                }
            }
        }
        self.active = Some(ix);
        self.focus_active(window, cx);
        cx.notify();
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
            .map_or_else(|| "untitled".to_string(), |n| n.to_string_lossy().to_string())
    }

    // ── folder / explorer ──────────────────────────────────────────────────────

    fn set_root_folder(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        match FileTree::new(folder.clone()) {
            Ok(tree) => {
                self.visible_rows = tree.visible();
                self.tree = Some(tree);
                self.root_folder = Some(folder);
                self.sidebar.open = true;
                self.sidebar.active = SidebarItemKind::Explorer;
            }
            Err(err) => eprintln!("felix: can't open folder {}: {err}", folder.display()),
        }
        cx.notify();
    }

    pub(crate) fn toggle_tree_node(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(tree) = &mut self.tree {
            match tree.toggle(path) {
                Ok(()) => self.visible_rows = tree.visible(),
                Err(err) => eprintln!("felix: can't read {}: {err}", path.display()),
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
                        ed.doc.assign_path(path);
                        let ok = save(&ed.doc.rope, &ed.doc.path).is_ok();
                        if ok {
                            ed.doc.dirty = false;
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
                    ed.doc.dirty = false;
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
            &format!("Save changes to {name}?"),
            None,
            &["Save", "Don't Save", "Cancel"],
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
        let editor = cx.new(|cx| EditorView::from_doc(Document::empty_untitled(), cx));
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

    fn on_close_file(&mut self, _: &CloseFile, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.active {
            self.request_close_tab(ix, window, cx);
        }
    }

    fn on_close_folder(&mut self, _: &CloseFolder, _: &mut Window, cx: &mut Context<Self>) {
        self.root_folder = None;
        self.tree = None;
        self.visible_rows.clear();
        cx.notify();
    }

    fn on_toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar.open = !self.sidebar.open;
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
        let rx = window.prompt(
            PromptLevel::Warning,
            &format!("{} file(s) have unsaved changes.", dirty.len()),
            None,
            &["Save All & Quit", "Quit Without Saving", "Cancel"],
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
        };

        let indicator = div()
            .size(px(7.0))
            .flex_shrink_0()
            .rounded_full()
            .when(dirty, |el| el.bg(t.dirty));

        h_flex()
            .id(tab.id)
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
            .child(icon)
            .child(title)
            .child(indicator)
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

    fn render_tab_bar(&self, t: &RuntimeTheme, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .h(px(30.0))
            .flex_shrink_0()
            .bg(t.bg_elevated)
            .border_b_1()
            .border_color(t.separator)
            .children((0..self.tabs.len()).map(|ix| self.render_tab(ix, t, cx)))
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
            });

        let main = v_flex()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .child(self.render_tab_bar(&t, cx))
            .map(|el| match content {
                Some(view) => el.child(div().flex_1().min_h(px(0.)).child(view)),
                None => el.child(render_welcome(&t)),
            });

        div()
            .flex()
            .flex_row()
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
            .on_action(cx.listener(Self::on_open_settings))
            .on_action(cx.listener(Self::on_quit))
            .child(self.render_activity_bar(&t, cx))
            .when(self.sidebar.open, |el| el.child(self.render_sidebar_panel(&t, cx)))
            .child(main)
    }
}
