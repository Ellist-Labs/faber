use felix::{load_rope, make_rust_parser, node_count, parse_source};
use gpui::{
    App, Application, Bounds, Context, SharedString, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, rgb, size,
};
use std::{env, time::Instant};

struct EditorView {
    file_path: SharedString,
    lines: Vec<SharedString>,
    line_count: usize,
    node_count: usize,
}

impl EditorView {
    fn new(path: &str) -> Self {
        let (source, rope) = load_rope(path).unwrap_or_default();
        let mut parser = make_rust_parser();
        let tree = parse_source(&mut parser, &source);

        let lines: Vec<SharedString> = rope
            .lines()
            .enumerate()
            .map(|(i, line)| {
                let text = line.to_string();
                let text = text.trim_end_matches('\n').trim_end_matches('\r');
                format!("{:>4}  {}", i + 1, text).into()
            })
            .collect();

        Self {
            file_path: path.to_string().into(),
            line_count: lines.len(),
            node_count: node_count(&tree),
            lines,
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
            .child(div().text_color(rgb(0xcdd6f4)).child(self.file_path.clone()))
            .child(
                div().text_color(rgb(0x6c7086)).child(format!(
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
    let start = Instant::now();
    let path = env::args().nth(1).unwrap_or_else(|| "src/main.rs".into());

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1024.), px(768.)), cx);
        let view = EditorView::new(&path);

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| view),
        )
        .unwrap();
        cx.activate(true);

        // Print after activation so perf/macro.sh can measure startup_ms
        println!("FELIX_READY startup_ms={}", start.elapsed().as_millis());
    });
}
