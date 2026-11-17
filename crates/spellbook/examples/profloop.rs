//! A tight loop over `check` or `suggest`.
//!
//! This is intended as a target for `perf` to sample. It does the same work many times so the
//! profiler collects enough samples to attribute cost to individual functions.
//!
//! # Usage
//!
//! ```text
//! cargo run --release --example profloop -- <mode> <aff> <dic> <iters> <word>...
//! ```
//!
//! - `mode`: `check` or `suggest`
//! - `aff` / `dic`: paths to the dictionary's `.aff` and `.dic` files
//! - `iters`: how many times to repeat the whole word list
//! - `word...`: one or more words to check/suggest each iteration
//!
//! Example:
//!
//! ```text
//! cargo run --release --example profloop -- suggest vendor/fr_FR/fr.aff vendor/fr_FR/fr.dic 40 impécable réceptioniste
//! ```
//!
//! # Collecting a profile with `perf`
//!
//! Build with debug symbols in release mode (`-g` gives the profiler function names and line
//! numbers without disabling optimizations), then record and report:
//!
//! ```sh
//! RUSTFLAGS="-g" cargo build --release --example profloop
//! perf record -g -o perf.data -- \
//!     ./target/release/examples/profloop \
//!     suggest vendor/fr_FR/fr.aff vendor/fr_FR/fr.dic 40 impécable réceptioniste
//!
//! # Self-time (flat) profile: which functions burn CPU directly.
//! perf report -i perf.data --stdio --no-children
//!
//! # Call-graph view (cost including callees):
//! perf report -i perf.data --stdio
//! ```
//!
//! Note: `perf` may need `kernel.perf_event_paranoid` lowered
//! (`sudo sysctl kernel.perf_event_paranoid=1`) to sample without root.

use spellbook::Dictionary;

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().expect("mode (check|suggest)");
    let aff_path = args.next().expect("aff path");
    let dic_path = args.next().expect("dic path");
    let iters: u32 = args
        .next()
        .expect("iters")
        .parse()
        .expect("iters must be a number");
    let words: Vec<String> = args.collect();
    assert!(!words.is_empty(), "expected at least one word");

    let aff = std::fs::read_to_string(&aff_path).unwrap();
    let dic = std::fs::read_to_string(&dic_path).unwrap();
    let dict = Dictionary::new(&aff, &dic).unwrap();

    // `total` is accumulated and printed so the optimizer can't elide the work.
    let mut total = 0usize;
    match mode.as_str() {
        "check" => {
            for _ in 0..iters {
                for word in &words {
                    total += dict.check(word) as usize;
                }
            }
        }
        "suggest" => {
            let mut suggestions = Vec::new();
            for _ in 0..iters {
                for word in &words {
                    dict.suggest(word, &mut suggestions);
                    total += suggestions.len();
                }
            }
        }
        other => panic!("unknown mode {other:?}, expected `check` or `suggest`"),
    }

    println!(
        "done ({mode}, {iters} iters, {} words): accumulator={total}",
        words.len()
    );
}
