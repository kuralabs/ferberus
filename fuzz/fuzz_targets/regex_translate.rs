//! The regex translator must never panic, and whatever it emits must be a
//! valid pattern for the `regex` crate (or be refused for its size only).
//!
//!     cargo +nightly-2026-11-10 fuzz run regex_translate -- -max_total_time=60

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    // first line: the pattern, rest: a subject
    let (pattern, subject) = text.split_once('\n').unwrap_or((text, ""));
    if let Ok(translated) = ferberus_regex::translate(pattern) {
        match ferberus_regex::build(&translated) {
            Ok(regex) => {
                if subject.is_ascii() {
                    let _ = regex.is_match(subject.as_bytes());
                }
            }
            Err(ferberus_regex::BuildError::CompiledTooBig(_)) => {}
            Err(error) => panic!("pattern {pattern:?} was translated to invalid {translated:?}: {error}"),
        }
    }
});
