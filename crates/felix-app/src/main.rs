mod editor_view;
mod theme;
mod ui;

use editor_view::EditorView;
use gpui::{
    App, Application, Bounds, KeyBinding, Window, WindowBounds, WindowOptions, actions,
    prelude::*, px, size,
};
use std::{env, time::Instant};

use theme::RuntimeTheme;

// ── actions ────────────────────────────────────────────────────────────────────

actions!(
    editor,
    [
        MoveLeft, MoveRight, MoveUp, MoveDown,
        MoveWordLeft, MoveWordRight,
        MoveLineStart, MoveLineEnd,
        MoveDocStart, MoveDocEnd,
        MovePageUp, MovePageDown,
        SelectLeft, SelectRight, SelectUp, SelectDown,
        SelectWordLeft, SelectWordRight,
        SelectLineStart, SelectLineEnd,
        SelectDocStart, SelectDocEnd,
        SelectAll,
        Backspace, Delete,
        DeleteWordLeft, DeleteWordRight,
        Tab, Enter,
        Copy, Cut, Paste,
        Undo, Redo,
        Save,
        OpenSearch, OpenReplace, CloseSearch,
        FindNext, FindPrev,
        ReplaceOne, ReplaceAll,
        SearchBackspace, ReplaceBackspace,
    ]
);

// ── main ───────────────────────────────────────────────────────────────────────

fn main() {
    let start = Instant::now();
    let path = env::args().nth(1).unwrap_or_else(|| "src/main.rs".into());

    Application::new().run(move |cx: &mut App| {
        cx.set_global(RuntimeTheme::from(felix_theme::default::felix_dark()));
        register_keybindings(cx);

        let bounds = Bounds::centered(None, size(px(1024.), px(768.)), cx);

        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| EditorView::new(&path, cx)),
            )
            .unwrap();

        window
            .update(cx, |view, window, cx| {
                window.focus(&view.focus_handle);
                cx.activate(true);
            })
            .unwrap();

        println!("FELIX_READY startup_ms={}", start.elapsed().as_millis());
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
        KeyBinding::new("cmd-s", Save, Some("Editor")),
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
        // Replace bar
        KeyBinding::new("escape", CloseSearch, Some("ReplaceBar")),
        KeyBinding::new("enter", ReplaceOne, Some("ReplaceBar")),
        KeyBinding::new("cmd-enter", ReplaceAll, Some("ReplaceBar")),
        KeyBinding::new("backspace", ReplaceBackspace, Some("ReplaceBar")),
    ]);
}
