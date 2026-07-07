use divan::Bencher;
use faber_editor::{make_rust_parser, node_count, parse_source, reparse_source};
use tree_sitter::{InputEdit, Point};

fn main() {
    divan::main();
}

const SEED: &str = include_str!("../../faber-app/src/main.rs");

fn make_fixture(target_lines: usize) -> String {
    let seed_lines = SEED.lines().count();
    let reps = (target_lines / seed_lines) + 1;
    SEED.repeat(reps)
}

/// Full parse from scratch — includes parser construction overhead.
#[divan::bench]
fn parse_fresh_medium(b: Bencher) {
    let content = make_fixture(5_000);
    b.bench(|| {
        let mut parser = make_rust_parser();
        let tree = parse_source(&mut parser, divan::black_box(&content));
        divan::black_box(node_count(&tree))
    });
}

/// Incremental reparse after a small edit prepended to the file.
/// Input setup (parse old tree) is outside the timing window.
#[divan::bench]
fn reparse_small_edit(b: Bencher) {
    let content = make_fixture(5_000);
    let insertion = "// edited\n";
    let new_source = format!("{}{}", insertion, content);
    let edit = InputEdit {
        start_byte: 0,
        old_end_byte: 0,
        new_end_byte: insertion.len(),
        start_position: Point { row: 0, column: 0 },
        old_end_position: Point { row: 0, column: 0 },
        new_end_position: Point { row: 1, column: 0 },
    };

    b.with_inputs(|| {
        let mut parser = make_rust_parser();
        let old_tree = parse_source(&mut parser, &content);
        (parser, old_tree)
    })
    .bench_values(|(mut parser, old_tree)| {
        let tree = reparse_source(&mut parser, &old_tree, &edit, divan::black_box(&new_source));
        divan::black_box(node_count(&tree))
    });
}
