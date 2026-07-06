use gpui::{
    App, Application, Bounds, Context, SharedString, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, rgb, size,
};
use ropey::Rope;
use std::{env, fs};
use tree_sitter::Parser;

struct EditorView {
    file_path: SharedString,
    lines: Vec<SharedString>,
    line_count: usize,
    node_count: usize,
}

impl EditorView {
    fn new(path: &str) -> Self {
        let source = fs::read_to_string(path).unwrap_or_default();
        let rope = Rope::from_str(&source);

        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("failed to load Rust grammar");
        let tree = parser.parse(&source, None).expect("parse failed");
        let node_count = tree.root_node().descendant_count();

        let lines: Vec<SharedString> = rope
            .lines()
            .enumerate()
            .map(|(i, line)| {
                let text = line.to_string();
                let text = text.trim_end_matches('\n').trim_end_matches('\r');
                format!("{:>4}  {}", i + 1, text).into()
            })
            .collect();

        let line_count = lines.len();

        Self {
            file_path: path.to_string().into(),
            lines,
            line_count,
            node_count,
        }
    }
}

impl Render for EditorView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let header = div()
            .flex()
            .flex_row()
            .justify_between()
            .px_4()
            .py_2()
            .bg(rgb(0x1e1e2e))
            .border_b_1()
            .border_color(rgb(0x313244))
            .child(
                div()
                    .text_color(rgb(0xcdd6f4))
                    .child(self.file_path.clone()),
            )
            .child(
                div()
                    .text_color(rgb(0x6c7086))
                    .child(format!(
                        "{} lines  •  {} nodes",
                        self.line_count, self.node_count
                    )),
            );

        let content = div()
            .flex_1()
            .id("editor-scroll")
            .overflow_scroll()
            .px_4()
            .py_2()
            .bg(rgb(0x1e1e2e))
            .children(self.lines.iter().map(|line| {
                div()
                    .text_color(rgb(0xcdd6f4))
                    .font_family("Menlo")
                    .text_sm()
                    .child(line.clone())
            }));

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x1e1e2e))
            .child(header)
            .child(content)
    }
}

fn main() {
    let path = env::args().nth(1).unwrap_or_else(|| "src/main.rs".into());

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1024.), px(768.)), cx);
        let view = EditorView::new(&path);

        println!(
            "felix: opened '{}' — {} lines, {} tree-sitter nodes",
            view.file_path, view.line_count, view.node_count
        );

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| view),
        )
        .unwrap();
        cx.activate(true);
    });
}
