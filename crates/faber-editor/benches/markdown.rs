//! Bench: markdown parse throughput — bounds the "instant toggle" requirement.
//! Run with: `cargo bench -p faber-editor --bench markdown`

use divan::{Bencher, black_box};
use faber_editor::markdown::parse_markdown;
use faber_lang::LanguageRegistry;
use ropey::Rope;

fn make_medium_doc() -> String {
    let mut doc = String::with_capacity(50_000);
    for i in 0..80 {
        doc.push_str(&format!("# Heading {i}\n\n"));
        doc.push_str("Some paragraph text with **bold** and *italic* content.\n\n");
        doc.push_str(&format!("1. Item one of {i}\n2. Item two\n3. Item three\n\n"));
        doc.push_str("| Col A | Col B | Col C |\n|---|---|---|\n| a | b | c |\n\n");
        doc.push_str("```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n\n");
        doc.push_str("- [ ] Task one\n- [x] Task done\n\n");
    }
    doc
}

fn make_large_doc() -> String {
    let medium = make_medium_doc();
    medium.repeat(5) // ~5× larger, tests scaling
}

fn make_code_heavy_doc() -> String {
    let mut doc = String::with_capacity(30_000);
    for _ in 0..20 {
        doc.push_str("```rust\nfn fib(n: u64) -> u64 {\n    match n {\n        0 => 0,\n        1 => 1,\n        _ => fib(n-1) + fib(n-2),\n    }\n}\n```\n\n");
    }
    doc
}

fn main() {
    divan::main();
}

#[divan::bench]
fn parse_medium(b: Bencher) {
    let doc = make_medium_doc();
    let rope = Rope::from_str(&doc);
    let reg = LanguageRegistry::with_defaults();
    b.bench(|| {
        black_box(parse_markdown(black_box(&doc), black_box(&rope), black_box(&reg)));
    });
}

#[divan::bench]
fn parse_large(b: Bencher) {
    let doc = make_large_doc();
    let rope = Rope::from_str(&doc);
    let reg = LanguageRegistry::with_defaults();
    b.bench(|| {
        black_box(parse_markdown(black_box(&doc), black_box(&rope), black_box(&reg)));
    });
}

#[divan::bench]
fn parse_code_heavy(b: Bencher) {
    let doc = make_code_heavy_doc();
    let rope = Rope::from_str(&doc);
    let reg = LanguageRegistry::with_defaults();
    b.bench(|| {
        black_box(parse_markdown(black_box(&doc), black_box(&rope), black_box(&reg)));
    });
}
