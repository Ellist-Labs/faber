use divan::Bencher;
use felix_editor::buffer::Document;

fn main() {
    divan::main();
}

const SEED: &str = include_str!("../../felix-app/src/main.rs");

fn make_fixture(target_lines: usize) -> String {
    let seed_lines = SEED.lines().count();
    let reps = (target_lines / seed_lines) + 1;
    SEED.repeat(reps)
}

/// Full highlight computation on first open — cold path.
#[divan::bench]
fn highlight_open_medium(b: Bencher) {
    let content = make_fixture(5_000);
    b.bench(|| {
        let doc = Document::from_str(divan::black_box(&content));
        divan::black_box(doc.highlight_cache.lines.len())
    });
}

/// Incremental highlight after a single character insertion.
/// Document construction is outside the timing window.
#[divan::bench]
fn highlight_incremental_insert(b: Bencher) {
    let content = make_fixture(5_000);
    b.bench_local(|| {
        let mut doc = Document::from_str(&content);
        doc.insert(0, "x");
        divan::black_box(doc.highlight_cache.lines.len())
    });
}

/// Highlight a large file — stress the query cursor.
#[divan::bench]
fn highlight_open_large(b: Bencher) {
    let content = make_fixture(20_000);
    b.bench(|| {
        let doc = Document::from_str(divan::black_box(&content));
        divan::black_box(doc.highlight_cache.lines.len())
    });
}
