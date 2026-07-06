use divan::Bencher;
use ropey::Rope;

fn main() {
    divan::main();
}

const SEED: &str = include_str!("../src/main.rs");

fn make_fixture(target_lines: usize) -> String {
    let seed_lines = SEED.lines().count();
    let reps = (target_lines / seed_lines) + 1;
    SEED.repeat(reps)
}

#[divan::bench]
fn rope_from_str_medium(b: Bencher) {
    let content = make_fixture(5_000);
    b.bench(|| {
        divan::black_box(Rope::from_str(divan::black_box(&content)));
    });
}

#[divan::bench]
fn rope_insert_middle(b: Bencher) {
    let content = make_fixture(5_000);
    let mid = Rope::from_str(&content).len_chars() / 2;
    b.with_inputs(|| Rope::from_str(&content))
        .bench_values(|mut rope| {
            rope.insert(mid, "hello world\n");
            divan::black_box(rope)
        });
}

#[divan::bench]
fn rope_remove_chunk(b: Bencher) {
    let content = make_fixture(5_000);
    let rope = Rope::from_str(&content);
    let mid = rope.len_chars() / 2;
    b.with_inputs(|| Rope::from_str(&content))
        .bench_values(|mut rope| {
            rope.remove(mid..mid + 100);
            divan::black_box(rope)
        });
}

#[divan::bench]
fn rope_line_iter(b: Bencher) {
    let content = make_fixture(5_000);
    let rope = Rope::from_str(&content);
    b.bench(|| {
        let count = rope.lines().count();
        divan::black_box(count)
    });
}
