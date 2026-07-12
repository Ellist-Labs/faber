mod assets;
mod buffer_view;
mod editor_logic;
mod editor_view;
mod file_finder;
mod file_icon_data;
mod file_icons;
mod file_preview;
mod hover_popover;
mod http;
mod i18n;
mod input_helpers;
mod language_picker;
mod lsp_status;
mod markdown_preview;
mod pane;
mod panels;
mod project_search_view;
mod settings_view;
mod sidebar;
mod status_bar;
mod symbol_finder;
mod symbol_overlay;
mod theme;
mod ui;
mod welcome_view;
mod workspace;

rust_i18n::i18n!("locales", fallback = "en");

use gpui::{
    App, Application, Bounds, Global, KeyBinding, Menu, MenuItem, TitlebarOptions, WindowBounds,
    WindowOptions, actions, point, prelude::*, px, size,
};
use std::{env, path::PathBuf, sync::Arc};

use faber_editor::LanguageRegistry;

pub struct ProjectRoot(pub Option<PathBuf>);
impl Global for ProjectRoot {}

/// Shared, process-wide language registry built once at startup and injected
/// everywhere via the GPUI global store.
#[derive(Clone)]
pub struct Registry(pub Arc<LanguageRegistry>);
impl Global for Registry {}

pub struct AppStateStore(pub faber_settings::state::AppState);
impl Global for AppStateStore {}

use workspace::Workspace;

// ── actions ────────────────────────────────────────────────────────────────────

actions!(
    markdown,
    [
        TogglePreview,
        BoldSelection,
        ItalicSelection,
        ToggleCheckbox,
    ]
);

actions!(
    editor,
    [
        MoveLeft,
        MoveRight,
        MoveUp,
        MoveDown,
        MoveWordLeft,
        MoveWordRight,
        MoveLineStart,
        MoveLineEnd,
        MoveDocStart,
        MoveDocEnd,
        MovePageUp,
        MovePageDown,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectWordLeft,
        SelectWordRight,
        SelectLineStart,
        SelectLineEnd,
        SelectDocStart,
        SelectDocEnd,
        SelectAll,
        Backspace,
        Delete,
        DeleteWordLeft,
        DeleteWordRight,
        DeleteToLineStart,
        DeleteToLineEnd,
        DeleteLine,
        Tab,
        Enter,
        Copy,
        Cut,
        Paste,
        Undo,
        Redo,
        OpenSearch,
        OpenReplace,
        CloseSearch,
        FindNext,
        FindPrev,
        ReplaceOne,
        ReplaceAll,
        SearchBackspace,
        ReplaceBackspace,
        ToggleSearchCase,
        ToggleSearchWholeWord,
        ToggleSearchRegex,
        ToggleReplace,
        InputMoveLeft,
        InputMoveRight,
        InputMoveStart,
        InputMoveEnd,
        GoToDefinition,
        FindReferences,
    ]
);

actions!(
    workspace,
    [
        CloseTab,
        NextTab,
        PrevTab,
        NewFile,
        OpenFile,
        OpenFolder,
        SaveFile,
        SaveAs,
        SaveAll,
        CloseFile,
        CloseFolder,
        CloseWindow,
        ToggleSidebar,
        ToggleBottomPanel,
        ToggleRightPanel,
        OpenSettings,
        OpenProjectSearch,
        OpenFileFinder,
        OpenFileFinderPreview,
        OpenSymbolFinder,
        OpenProblems,
        ToggleLspStatus,
        OpenLanguagePicker,
        Quit,
        SplitLeft,
        SplitRight,
        SplitUp,
        SplitDown,
        ReindexProject,
    ]
);

actions!(
    language_picker,
    [LpDismiss, LpConfirm, LpSelectNext, LpSelectPrev,]
);

actions!(
    symbol_finder,
    [
        SfDismiss,
        SfConfirm,
        SfSelectNext,
        SfSelectPrev,
        SfBackspace,
        SfMoveLeft,
        SfMoveRight,
        SfMoveStart,
        SfMoveEnd,
    ]
);

actions!(
    file_finder,
    [
        FfDismiss,
        FfConfirm,
        FfSelectNext,
        FfSelectPrev,
        FfBackspace,
        FfMoveLeft,
        FfMoveRight,
        FfMoveStart,
        FfMoveEnd,
        FfToggleCase,
        FfToggleWholeWord,
        FfToggleRegex,
        FfToggleIgnored,
        FfTogglePreview,
    ]
);

actions!(
    project_search,
    [
        PsInputBackspace,
        PsInputMoveLeft,
        PsInputMoveRight,
        PsInputMoveStart,
        PsInputMoveEnd,
    ]
);

actions!(confirm, [CfConfirm, CfDismiss]);

// ── logging ────────────────────────────────────────────────────────────────────

/// Debug builds log at `debug` for faber crates (overridable via RUST_LOG);
/// release builds stay at `warn` so the log facade costs nothing in normal use.
fn init_logging() {
    let default_filter = if cfg!(debug_assertions) {
        "warn,faber=debug,faber_lsp=debug,faber_editor=debug,faber_index=debug"
    } else {
        "warn"
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default_filter))
        .format_timestamp_millis()
        .init();
}

// ── main ───────────────────────────────────────────────────────────────────────

fn main() {
    init_logging();
    let paths: Vec<String> = env::args().skip(1).collect();

    Application::new()
        .with_assets(assets::Assets)
        // Real http client (gpui defaults to a null one) — powers remote image
        // loading, e.g. badges in hover documentation.
        .with_http_client(http::UreqHttpClient::new())
        .run(move |cx: &mut App| {
            let settings = faber_settings::load();
            let state = faber_settings::state::load();
            let session: Option<faber_settings::state::LastSession> =
                if settings.reopen_last_session && paths.is_empty() {
                    state.last_session.clone()
                } else {
                    None
                };
            cx.set_global(settings_view::SettingsStore(settings));
            cx.set_global(AppStateStore(state));
            cx.set_global(ProjectRoot(None));
            cx.set_global(Registry(Arc::new(LanguageRegistry::with_defaults())));
            register_keybindings(cx);
            i18n::apply(cx);
            theme::apply_settings(cx);

            let bounds = Bounds::centered(None, size(px(1024.), px(768.)), cx);

            let window = cx
                .open_window(
                    WindowOptions {
                        titlebar: Some(TitlebarOptions {
                            title: None,
                            appears_transparent: true,
                            traffic_light_position: Some(point(px(14.), px(12.))),
                        }),
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        ..Default::default()
                    },
                    |window, cx| cx.new(|cx| Workspace::new(&paths, session.as_ref(), window, cx)),
                )
                .unwrap();

            window
                .update(cx, |view, window, cx| {
                    view.focus_active(window, cx);
                    cx.activate(true);
                })
                .unwrap();
        });
}

fn register_keybindings(cx: &mut App) {
    cx.bind_keys([
        // Movement
        KeyBinding::new("left", MoveLeft, Some("Editor")),
        KeyBinding::new("right", MoveRight, Some("Editor")),
        KeyBinding::new("up", MoveUp, Some("Editor")),
        KeyBinding::new("down", MoveDown, Some("Editor")),
        KeyBinding::new("alt-left", MoveWordLeft, Some("Editor")),
        KeyBinding::new("alt-right", MoveWordRight, Some("Editor")),
        KeyBinding::new("cmd-left", MoveLineStart, Some("Editor")),
        KeyBinding::new("cmd-right", MoveLineEnd, Some("Editor")),
        KeyBinding::new("home", MoveLineStart, Some("Editor")),
        KeyBinding::new("end", MoveLineEnd, Some("Editor")),
        KeyBinding::new("cmd-up", MoveDocStart, Some("Editor")),
        KeyBinding::new("cmd-down", MoveDocEnd, Some("Editor")),
        KeyBinding::new("pageup", MovePageUp, Some("Editor")),
        KeyBinding::new("pagedown", MovePageDown, Some("Editor")),
        // Selection
        KeyBinding::new("shift-left", SelectLeft, Some("Editor")),
        KeyBinding::new("shift-right", SelectRight, Some("Editor")),
        KeyBinding::new("shift-up", SelectUp, Some("Editor")),
        KeyBinding::new("shift-down", SelectDown, Some("Editor")),
        KeyBinding::new("shift-alt-left", SelectWordLeft, Some("Editor")),
        KeyBinding::new("shift-alt-right", SelectWordRight, Some("Editor")),
        KeyBinding::new("shift-cmd-left", SelectLineStart, Some("Editor")),
        KeyBinding::new("shift-cmd-right", SelectLineEnd, Some("Editor")),
        KeyBinding::new("shift-home", SelectLineStart, Some("Editor")),
        KeyBinding::new("shift-end", SelectLineEnd, Some("Editor")),
        KeyBinding::new("shift-cmd-up", SelectDocStart, Some("Editor")),
        KeyBinding::new("shift-cmd-down", SelectDocEnd, Some("Editor")),
        KeyBinding::new("cmd-a", SelectAll, Some("Editor")),
        // Editing
        KeyBinding::new("backspace", Backspace, Some("Editor")),
        KeyBinding::new("delete", Delete, Some("Editor")),
        KeyBinding::new("alt-backspace", DeleteWordLeft, Some("Editor")),
        KeyBinding::new("alt-delete", DeleteWordRight, Some("Editor")),
        // cmd-backspace/delete/shift-k handled directly in on_key_down to bypass
        // macOS NSTextInputClient interception of these selectors.
        KeyBinding::new("tab", Tab, Some("Editor")),
        KeyBinding::new("enter", Enter, Some("Editor")),
        // Clipboard
        KeyBinding::new("cmd-c", Copy, Some("Editor")),
        KeyBinding::new("cmd-x", Cut, Some("Editor")),
        KeyBinding::new("cmd-v", Paste, Some("Editor")),
        // Undo/redo
        KeyBinding::new("cmd-z", Undo, Some("Editor")),
        KeyBinding::new("cmd-shift-z", Redo, Some("Editor")),
        // File
        KeyBinding::new("cmd-n", NewFile, Some("Workspace")),
        KeyBinding::new("cmd-o", OpenFile, Some("Workspace")),
        KeyBinding::new("cmd-shift-o", OpenFolder, Some("Workspace")),
        KeyBinding::new("cmd-s", SaveFile, Some("Workspace")),
        KeyBinding::new("cmd-shift-s", SaveAs, Some("Workspace")),
        KeyBinding::new("cmd-alt-s", SaveAll, Some("Workspace")),
        KeyBinding::new("cmd-shift-w", CloseWindow, Some("Workspace")),
        KeyBinding::new("cmd-,", OpenSettings, Some("Workspace")),
        KeyBinding::new("cmd-q", Quit, Some("Workspace")),
        // Sidebar / panels
        KeyBinding::new("cmd-b", ToggleSidebar, Some("Workspace")),
        KeyBinding::new("cmd-1", ToggleSidebar, Some("Workspace")),
        KeyBinding::new("escape", ToggleSidebar, Some("Sidebar")),
        KeyBinding::new("cmd-j", ToggleBottomPanel, Some("Workspace")),
        KeyBinding::new("ctrl-cmd-b", ToggleRightPanel, Some("Workspace")),
        // Tabs
        KeyBinding::new("cmd-w", CloseTab, Some("Workspace")),
        KeyBinding::new("ctrl-tab", NextTab, Some("Workspace")),
        KeyBinding::new("ctrl-shift-tab", PrevTab, Some("Workspace")),
        // Split panes
        KeyBinding::new("cmd-alt-left", SplitLeft, Some("Workspace")),
        KeyBinding::new("cmd-alt-right", SplitRight, Some("Workspace")),
        KeyBinding::new("cmd-alt-up", SplitUp, Some("Workspace")),
        KeyBinding::new("cmd-alt-down", SplitDown, Some("Workspace")),
        // Markdown — cmd-b shadows ToggleSidebar while markdown editor is focused (VS Code parity)
        KeyBinding::new("cmd-shift-v", TogglePreview, Some("Editor && markdown")),
        KeyBinding::new("cmd-b", BoldSelection, Some("Editor && markdown")),
        KeyBinding::new("cmd-i", ItalicSelection, Some("Editor && markdown")),
        KeyBinding::new("cmd-shift-x", ToggleCheckbox, Some("Editor && markdown")),
        // LSP navigation
        KeyBinding::new("f12", GoToDefinition, Some("Editor")),
        KeyBinding::new("shift-f12", FindReferences, Some("Editor")),
        // Project search
        KeyBinding::new("cmd-shift-f", OpenProjectSearch, Some("Workspace")),
        // Symbol finder
        KeyBinding::new("cmd-t", OpenSymbolFinder, Some("Workspace")),
        // Problems panel
        KeyBinding::new("cmd-shift-m", OpenProblems, Some("Workspace")),
        KeyBinding::new("cmd-shift-p", OpenProblems, Some("Workspace")),
        KeyBinding::new("escape", SfDismiss, Some("SymbolFinder")),
        KeyBinding::new("enter", SfConfirm, Some("SymbolFinder")),
        KeyBinding::new("down", SfSelectNext, Some("SymbolFinder")),
        KeyBinding::new("up", SfSelectPrev, Some("SymbolFinder")),
        KeyBinding::new("ctrl-n", SfSelectNext, Some("SymbolFinder")),
        KeyBinding::new("ctrl-p", SfSelectPrev, Some("SymbolFinder")),
        KeyBinding::new("backspace", SfBackspace, Some("SymbolFinder")),
        KeyBinding::new("left", SfMoveLeft, Some("SymbolFinder")),
        KeyBinding::new("right", SfMoveRight, Some("SymbolFinder")),
        KeyBinding::new("cmd-left", SfMoveStart, Some("SymbolFinder")),
        KeyBinding::new("cmd-right", SfMoveEnd, Some("SymbolFinder")),
        KeyBinding::new("home", SfMoveStart, Some("SymbolFinder")),
        KeyBinding::new("end", SfMoveEnd, Some("SymbolFinder")),
        // File finder
        KeyBinding::new("cmd-p", OpenFileFinder, Some("Workspace")),
        KeyBinding::new("cmd-alt-p", OpenFileFinderPreview, Some("Workspace")),
        KeyBinding::new("escape", FfDismiss, Some("FileFinder")),
        KeyBinding::new("enter", FfConfirm, Some("FileFinder")),
        KeyBinding::new("down", FfSelectNext, Some("FileFinder")),
        KeyBinding::new("up", FfSelectPrev, Some("FileFinder")),
        KeyBinding::new("ctrl-n", FfSelectNext, Some("FileFinder")),
        KeyBinding::new("ctrl-p", FfSelectPrev, Some("FileFinder")),
        KeyBinding::new("backspace", FfBackspace, Some("FileFinder")),
        KeyBinding::new("left", FfMoveLeft, Some("FileFinder")),
        KeyBinding::new("right", FfMoveRight, Some("FileFinder")),
        KeyBinding::new("cmd-left", FfMoveStart, Some("FileFinder")),
        KeyBinding::new("cmd-right", FfMoveEnd, Some("FileFinder")),
        KeyBinding::new("home", FfMoveStart, Some("FileFinder")),
        KeyBinding::new("end", FfMoveEnd, Some("FileFinder")),
        KeyBinding::new("cmd-alt-c", FfToggleCase, Some("FileFinder")),
        KeyBinding::new("cmd-alt-w", FfToggleWholeWord, Some("FileFinder")),
        KeyBinding::new("cmd-alt-x", FfToggleRegex, Some("FileFinder")),
        KeyBinding::new("cmd-alt-i", FfToggleIgnored, Some("FileFinder")),
        KeyBinding::new("cmd-alt-p", FfTogglePreview, Some("FileFinder")),
        // Search
        KeyBinding::new("cmd-f", OpenSearch, Some("Editor")),
        KeyBinding::new("cmd-alt-f", OpenReplace, Some("Editor")),
        KeyBinding::new("cmd-g", FindNext, None),
        KeyBinding::new("cmd-shift-g", FindPrev, None),
        // Search bar
        KeyBinding::new("escape", CloseSearch, Some("SearchBar")),
        KeyBinding::new("enter", FindNext, Some("SearchBar")),
        KeyBinding::new("shift-enter", FindPrev, Some("SearchBar")),
        KeyBinding::new("backspace", SearchBackspace, Some("SearchBar")),
        KeyBinding::new("left", InputMoveLeft, Some("SearchBar")),
        KeyBinding::new("right", InputMoveRight, Some("SearchBar")),
        KeyBinding::new("cmd-left", InputMoveStart, Some("SearchBar")),
        KeyBinding::new("cmd-right", InputMoveEnd, Some("SearchBar")),
        KeyBinding::new("home", InputMoveStart, Some("SearchBar")),
        KeyBinding::new("end", InputMoveEnd, Some("SearchBar")),
        KeyBinding::new("cmd-alt-c", ToggleSearchCase, Some("SearchBar")),
        KeyBinding::new("cmd-alt-w", ToggleSearchWholeWord, Some("SearchBar")),
        KeyBinding::new("cmd-alt-x", ToggleSearchRegex, Some("SearchBar")),
        KeyBinding::new("cmd-alt-f", ToggleReplace, Some("SearchBar")),
        // Project search inputs
        KeyBinding::new("backspace", PsInputBackspace, Some("ProjectSearch")),
        KeyBinding::new("left", PsInputMoveLeft, Some("ProjectSearch")),
        KeyBinding::new("right", PsInputMoveRight, Some("ProjectSearch")),
        KeyBinding::new("cmd-left", PsInputMoveStart, Some("ProjectSearch")),
        KeyBinding::new("cmd-right", PsInputMoveEnd, Some("ProjectSearch")),
        KeyBinding::new("home", PsInputMoveStart, Some("ProjectSearch")),
        KeyBinding::new("end", PsInputMoveEnd, Some("ProjectSearch")),
        // LSP / language
        KeyBinding::new("cmd-shift-l", ToggleLspStatus, Some("Workspace")),
        KeyBinding::new("cmd-k m", OpenLanguagePicker, Some("Workspace")),
        // Language picker
        KeyBinding::new("escape", LpDismiss, Some("LanguagePicker")),
        KeyBinding::new("enter", LpConfirm, Some("LanguagePicker")),
        KeyBinding::new("down", LpSelectNext, Some("LanguagePicker")),
        KeyBinding::new("up", LpSelectPrev, Some("LanguagePicker")),
        KeyBinding::new("ctrl-n", LpSelectNext, Some("LanguagePicker")),
        KeyBinding::new("ctrl-p", LpSelectPrev, Some("LanguagePicker")),
        // Confirm modal
        KeyBinding::new("enter", CfConfirm, Some("ConfirmModal")),
        KeyBinding::new("escape", CfDismiss, Some("ConfirmModal")),
        // Replace bar
        KeyBinding::new("escape", CloseSearch, Some("ReplaceBar")),
        KeyBinding::new("enter", ReplaceOne, Some("ReplaceBar")),
        KeyBinding::new("cmd-enter", ReplaceAll, Some("ReplaceBar")),
        KeyBinding::new("backspace", ReplaceBackspace, Some("ReplaceBar")),
        KeyBinding::new("left", InputMoveLeft, Some("ReplaceBar")),
        KeyBinding::new("right", InputMoveRight, Some("ReplaceBar")),
        KeyBinding::new("cmd-left", InputMoveStart, Some("ReplaceBar")),
        KeyBinding::new("cmd-right", InputMoveEnd, Some("ReplaceBar")),
        KeyBinding::new("home", InputMoveStart, Some("ReplaceBar")),
        KeyBinding::new("end", InputMoveEnd, Some("ReplaceBar")),
    ]);
}

pub(crate) fn register_menus(cx: &mut App) {
    use rust_i18n::t;
    cx.set_menus(vec![
        Menu {
            name: "Faber".into(),
            items: vec![
                MenuItem::action(t!("menu.settings").to_string(), OpenSettings),
                MenuItem::separator(),
                MenuItem::action(t!("menu.quit").to_string(), Quit),
            ],
        },
        Menu {
            name: t!("menu.file").to_string().into(),
            items: vec![
                MenuItem::action(t!("menu.new_file").to_string(), NewFile),
                MenuItem::separator(),
                MenuItem::action(t!("menu.open_file").to_string(), OpenFile),
                MenuItem::action(t!("menu.open_folder").to_string(), OpenFolder),
                MenuItem::separator(),
                MenuItem::action(t!("menu.save").to_string(), SaveFile),
                MenuItem::action(t!("menu.save_as").to_string(), SaveAs),
                MenuItem::action(t!("menu.save_all").to_string(), SaveAll),
                MenuItem::separator(),
                MenuItem::action(t!("menu.close_window").to_string(), CloseWindow),
                MenuItem::separator(),
                MenuItem::action(t!("menu.problems").to_string(), OpenProblems),
            ],
        },
    ]);
}
