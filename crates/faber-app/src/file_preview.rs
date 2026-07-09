use std::path::{Path, PathBuf};

use faber_editor::{LanguageRegistry, buffer::Document};
use gpui::{
    AnyElement, Entity, ScrollStrategy, SharedString, UniformListScrollHandle, canvas, div,
    prelude::*, px, uniform_list,
};
use rust_i18n::t;

use crate::editor_view::EditorView;
use crate::file_finder::FileFinderView;
use crate::theme::RuntimeTheme;
use crate::ui::v_flex;

pub const PREVIEW_MAX_BYTES: u64 = 1024 * 1024;
pub const PREVIEW_MAX_LINES: usize = 2000;

/// Result of reading a candidate file on the background executor.
pub enum LoadedFile {
    Text(String),
    TooLarge,
    Binary,
    Unreadable,
}

pub fn load_file(path: &Path) -> LoadedFile {
    match std::fs::metadata(path) {
        Ok(m) if m.len() > PREVIEW_MAX_BYTES => return LoadedFile::TooLarge,
        Err(_) => return LoadedFile::Unreadable,
        _ => {}
    }
    let Ok(bytes) = std::fs::read(path) else {
        return LoadedFile::Unreadable;
    };
    if bytes.contains(&0u8) {
        return LoadedFile::Binary;
    }
    match String::from_utf8(bytes) {
        Ok(text) => LoadedFile::Text(text),
        Err(_) => LoadedFile::Binary,
    }
}

pub enum PreviewContent {
    Empty,
    Loading,
    TooLarge,
    Binary,
    Unreadable,
    Doc {
        doc: Box<Document>,
        lines: Vec<SharedString>,
    },
}

/// Read-only syntax-highlighted preview pane state, embedded in the finder.
pub struct FilePreview {
    pub content: PreviewContent,
    pub scroll: UniformListScrollHandle,
    /// Path the current `content` was produced from.
    pub path: Option<PathBuf>,
    /// Bumped per selection change; stale loads are dropped.
    pub epoch: u64,
}

impl FilePreview {
    pub fn new() -> Self {
        Self {
            content: PreviewContent::Empty,
            scroll: UniformListScrollHandle::new(),
            path: None,
            epoch: 0,
        }
    }

    /// Build the highlighted document from text loaded off-thread. Parsing
    /// happens here (main thread); inputs are capped at PREVIEW_MAX_BYTES.
    pub fn set_loaded(&mut self, path: PathBuf, loaded: LoadedFile, registry: &LanguageRegistry) {
        self.content = match loaded {
            LoadedFile::TooLarge => PreviewContent::TooLarge,
            LoadedFile::Binary => PreviewContent::Binary,
            LoadedFile::Unreadable => PreviewContent::Unreadable,
            LoadedFile::Text(text) => {
                let language = registry.language_for_path(&path);
                let doc = Document::from_str(&text, language.as_ref());
                let lines: Vec<SharedString> = text
                    .split('\n')
                    .take(PREVIEW_MAX_LINES)
                    .map(|l| SharedString::from(l.trim_end_matches('\r').to_string()))
                    .collect();
                PreviewContent::Doc {
                    doc: Box::new(doc),
                    lines,
                }
            }
        };
        self.path = Some(path);
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }
}

/// Render the preview pane. `finder` is read inside the uniform_list closure
/// so line content stays borrowed from the view, not cloned per frame.
pub fn render_preview(
    finder: Entity<FileFinderView>,
    preview: &FilePreview,
    t: &RuntimeTheme,
) -> AnyElement {
    let hint = |key: &str| {
        div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .font_family(t.ui_family.clone())
            .text_size(px(t.font_size_caption))
            .text_color(t.text_muted)
            .child(t!(key).to_string())
            .into_any_element()
    };

    let body: AnyElement = match &preview.content {
        PreviewContent::Empty => hint("file_finder.preview_empty"),
        PreviewContent::Loading => hint("file_finder.preview_loading"),
        PreviewContent::TooLarge => hint("file_finder.preview_too_large"),
        PreviewContent::Binary => hint("file_finder.preview_binary"),
        PreviewContent::Unreadable => hint("file_finder.preview_unreadable"),
        PreviewContent::Doc { lines, .. } => {
            let count = lines.len();
            let t2 = t.clone();
            let line_h = px(t.line_height_code);
            let font_sz = px(t.font_size_code);
            uniform_list("finder-preview-lines", count, move |range, _window, cx| {
                let view = finder.read(cx);
                let PreviewContent::Doc { doc, lines } = &view.preview.content else {
                    return Vec::new();
                };
                range
                    .map(|i| {
                        let text = lines[i].clone();
                        let runs =
                            EditorView::build_text_runs(&text, doc.highlight_spans(i), &t2, &[]);
                        div()
                            .h(line_h)
                            .w_full()
                            .child(
                                canvas(
                                    move |_bounds, window, _cx| {
                                        window.text_system().shape_line(text, font_sz, &runs, None)
                                    },
                                    move |bounds, shaped, window, cx| {
                                        let _ = shaped.paint(bounds.origin, line_h, window, cx);
                                    },
                                )
                                .size_full(),
                            )
                            .into_any_element()
                    })
                    .collect()
            })
            .flex_1()
            .px_2()
            .track_scroll(preview.scroll.clone())
            .into_any_element()
        }
    };

    v_flex()
        .flex_1()
        .min_w(px(0.))
        .min_h(px(0.))
        .bg(t.bg)
        .child(body)
        .into_any_element()
}
