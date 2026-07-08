use divan::Bencher;
use faber_editor::project_search::{ProjectSearchQuery, run};
use std::io::Write;

fn main() {
    divan::main();
}

/// Write N copies of a source file to a temp dir and return its path.
fn make_fixtures(n: usize) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let seed = include_str!("../../faber-app/src/main.rs");
    for i in 0..n {
        let path = dir.path().join(format!("file_{i}.rs"));
        let mut f = std::fs::File::create(&path).expect("file");
        f.write_all(seed.as_bytes()).expect("write");
    }
    dir
}

#[divan::bench]
fn literal_10_files(b: Bencher) {
    let dir = make_fixtures(10);
    let q = ProjectSearchQuery::new("fn ");
    b.bench(|| {
        let mut results = Vec::new();
        run(divan::black_box(&q), divan::black_box(dir.path()), |r| {
            results.push(r);
            true
        });
        divan::black_box(results)
    });
}

#[divan::bench]
fn regex_10_files(b: Bencher) {
    let dir = make_fixtures(10);
    let mut q = ProjectSearchQuery::new(r"fn\s+\w+");
    q.regex = true;
    b.bench(|| {
        let mut results = Vec::new();
        run(divan::black_box(&q), divan::black_box(dir.path()), |r| {
            results.push(r);
            true
        });
        divan::black_box(results)
    });
}

#[divan::bench]
fn case_sensitive_10_files(b: Bencher) {
    let dir = make_fixtures(10);
    let mut q = ProjectSearchQuery::new("pub fn");
    q.case_sensitive = true;
    b.bench(|| {
        let mut results = Vec::new();
        run(divan::black_box(&q), divan::black_box(dir.path()), |r| {
            results.push(r);
            true
        });
        divan::black_box(results)
    });
}
