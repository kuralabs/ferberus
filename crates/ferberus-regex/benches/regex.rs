//! Micro-benchmarks of the regex fast path (`cargo bench -p ferberus-regex`).

use divan::{Bencher, black_box};

const PATTERNS: &[(&str, &str, &str)] = &[
    ("email", r"[a-zA-Z0-9_.+-]+@[a-zA-Z0-9-]+\.[a-zA-Z0-9-.]+$", "user1234@example.com"),
    ("uuid", r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$", "0123abcd-0123-4567-89ab-0123456789ab"),
    ("date", r"\d{4}-\d{2}-\d{2}$", "2026-11-16"),
    ("slug", r"^[a-z0-9]+(?:-[a-z0-9]+)*$", "some-slug-with-several-parts"),
    ("word", r"(?i)(alpha|beta|gamma)\w*$", "Gamma123"),
];

fn main() {
    divan::main();
}

#[divan::bench(args = PATTERNS.iter().map(|p| p.0))]
fn translate(bencher: Bencher, name: &str) {
    let pattern = PATTERNS.iter().find(|p| p.0 == name).unwrap().1;
    bencher.bench(|| ferberus_regex::translate(black_box(pattern)));
}

#[divan::bench(args = PATTERNS.iter().map(|p| p.0))]
fn compile(bencher: Bencher, name: &str) {
    let pattern = PATTERNS.iter().find(|p| p.0 == name).unwrap().1;
    bencher.bench(|| ferberus_regex::compile(black_box(pattern)));
}

#[divan::bench(args = PATTERNS.iter().map(|p| p.0))]
fn is_match(bencher: Bencher, name: &str) {
    let (_, pattern, subject) = PATTERNS.iter().find(|p| p.0 == name).unwrap();
    let regex = ferberus_regex::compile(pattern).unwrap();
    bencher.bench(|| regex.is_match(black_box(subject.as_bytes())));
}

#[divan::bench(args = [64, 1024, 4096, 65536])]
fn is_match_long_subject(bencher: Bencher, len: usize) {
    let regex = ferberus_regex::compile(r"(?:[a-z]+ )*[a-z]+$").unwrap();
    let subject = "word ".repeat(len / 5) + "end";
    bencher.bench(|| regex.is_match(black_box(subject.as_bytes())));
}
