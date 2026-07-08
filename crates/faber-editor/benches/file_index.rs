use divan::Bencher;
use faber_editor::file_index::{filter, FileEntry, FileIndexSnapshot, FinderQuery};
use std::path::PathBuf;

fn main() {
    divan::main();
}

const DIRS: &[&str] = &["src", "crates/core/src", "crates/app/src/views", "tests", "docs/guide"];
const STEMS: &[&str] =
    &["main", "workspace", "editor_view", "buffer", "outline", "search", "theme", "utils"];
const EXTS: &[&str] = &["rs", "toml", "md", "json"];

/// Deterministic synthetic index of `n` entries.
fn make_snapshot(n: usize) -> FileIndexSnapshot {
    let mut entries: Vec<FileEntry> = (0..n)
        .map(|i| {
            let rel_path = format!(
                "{}/{}_{i}.{}",
                DIRS[i % DIRS.len()],
                STEMS[i % STEMS.len()],
                EXTS[i % EXTS.len()],
            );
            let name_off = rel_path.rfind('/').map(|p| p + 1).unwrap_or(0) as u32;
            FileEntry { rel_path, name_off, is_ignored: false }
        })
        .collect();
    entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    FileIndexSnapshot { root: PathBuf::new(), entries, extensions: Vec::new(), truncated: false }
}

#[divan::bench(args = [10_000, 100_000])]
fn fuzzy_filter(b: Bencher, n: usize) {
    let snap = make_snapshot(n);
    let q = FinderQuery { text: "wrkspc".into(), ..Default::default() };
    b.bench(|| divan::black_box(filter(&snap, divan::black_box(&q), &[], 100)));
}

#[divan::bench(args = [10_000, 100_000])]
fn regex_filter(b: Bencher, n: usize) {
    let snap = make_snapshot(n);
    let q = FinderQuery { text: r".*_view.*\.rs".into(), regex: true, ..Default::default() };
    b.bench(|| divan::black_box(filter(&snap, divan::black_box(&q), &[], 100)));
}

#[divan::bench(args = [10_000, 100_000])]
fn history_idle_filter(b: Bencher, n: usize) {
    let snap = make_snapshot(n);
    let history: Vec<String> =
        snap.entries.iter().rev().take(200).map(|e| e.rel_path.clone()).collect();
    let q = FinderQuery::default();
    b.bench(|| divan::black_box(filter(&snap, divan::black_box(&q), &history, 100)));
}
