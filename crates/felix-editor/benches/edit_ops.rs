use divan::Bencher;
use felix_core::transaction::ChangeSet;
use felix_editor::{buffer::Document, search::Query};
use ropey::Rope;

fn main() {
    divan::main();
}

const SEED: &str = include_str!("../../felix-app/src/main.rs");

fn make_fixture(target_lines: usize) -> String {
    let seed_lines = SEED.lines().count();
    let reps = (target_lines / seed_lines) + 1;
    SEED.repeat(reps)
}

/// Insert a single char near the middle of a medium file, including incremental reparse.
#[divan::bench]
fn edit_insert_mid(b: Bencher) {
    let content = make_fixture(5_000);
    b.with_inputs(|| Document::from_str(&content))
        .bench_values(|mut doc| {
            let mid = doc.len_chars() / 2;
            doc.insert(mid, "x");
            divan::black_box(doc.tree.root_node().descendant_count())
        });
}

/// Delete one char near the middle, including incremental reparse.
#[divan::bench]
fn edit_delete_mid(b: Bencher) {
    let content = make_fixture(5_000);
    b.with_inputs(|| Document::from_str(&content))
        .bench_values(|mut doc| {
            let mid = doc.len_chars() / 2;
            doc.delete(mid..mid + 1);
            divan::black_box(doc.tree.root_node().descendant_count())
        });
}

/// Build and apply a ChangeSet insert at mid-document (ChangeSet hot path).
#[divan::bench]
fn changeset_insert_mid(b: Bencher) {
    let content = make_fixture(5_000);
    let n = content.chars().count();
    let mid = n / 2;
    b.with_inputs(|| Rope::from_str(&content))
        .bench_values(|mut rope| {
            let cs = ChangeSet::from_changes(n, [(mid, mid, "x".into())]);
            cs.apply(&mut rope);
            divan::black_box(rope.len_chars())
        });
}

/// Compose two ChangeSets (sequential edits).
#[divan::bench]
fn changeset_compose(b: Bencher) {
    let content = make_fixture(5_000);
    let n = content.chars().count();
    let mid = n / 2;
    let a = ChangeSet::from_changes(n, [(mid, mid, "x".into())]);
    let b_cs = ChangeSet::from_changes(a.len_after, [(mid + 1, mid + 1, "y".into())]);
    b.bench(|| {
        let composed = a.clone().compose(b_cs.clone());
        divan::black_box(composed.len_after)
    });
}

/// Scan all matches for a short common pattern in a medium file.
#[divan::bench]
fn search_scan(b: Bencher) {
    let content = make_fixture(5_000);
    let rope = Rope::from_str(&content);
    let query = Query::new("fn ");
    b.bench(|| {
        let matches = query.all_matches(&rope);
        divan::black_box(matches.len())
    });
}
