use divan::Bencher;
use faber_editor::{LanguageRegistry, buffer::Document};
use std::path::Path;

fn main() {
    divan::main();
}

const SEED: &str = include_str!("../../faber-app/src/main.rs");

fn make_fixture(target_lines: usize) -> String {
    let seed_lines = SEED.lines().count();
    let reps = (target_lines / seed_lines) + 1;
    SEED.repeat(reps)
}

fn rust_doc(content: &str) -> Document {
    let reg = LanguageRegistry::with_defaults();
    let lang = reg.language_for_path(Path::new("_.rs")).expect("rust language");
    Document::from_str(content, Some(&lang))
}

/// Full highlight computation on first open — cold path.
#[divan::bench]
fn highlight_open_medium(b: Bencher) {
    let content = make_fixture(5_000);
    b.bench(|| {
        let doc = rust_doc(divan::black_box(&content));
        divan::black_box(doc.len_lines())
    });
}

/// Incremental highlight after a single character insertion.
/// Document construction is outside the timing window.
#[divan::bench]
fn highlight_incremental_insert(b: Bencher) {
    let content = make_fixture(5_000);
    b.bench_local(|| {
        let mut doc = rust_doc(&content);
        doc.insert(0, "x");
        divan::black_box(doc.len_lines())
    });
}

/// Highlight a large file — stress the query cursor.
#[divan::bench]
fn highlight_open_large(b: Bencher) {
    let content = make_fixture(20_000);
    b.bench(|| {
        let doc = rust_doc(divan::black_box(&content));
        divan::black_box(doc.len_lines())
    });
}
