/*
An example that checks prose piped in via stdin.

This is meant to get a rough idea of how performant and correct the checker is. The way this
example tokenizes input is very basic.

```
cat "Grapes of Wrath.txt" | cargo run --release --example prose
```
*/

use std::time::Instant;

use spellbook::Dictionary;

const EN_US_AFF: &str = include_str!("../vendor/en_US/en_US.aff");
const EN_US_DIC: &str = include_str!("../vendor/en_US/en_US.dic");

fn main() {
    if std::env::args().nth(1).is_some() {
        eprintln!("This example takes no arguments. Pipe in prose through stdin.");
        std::process::exit(1);
    }
    let mut total_words = 0;
    let mut misspelled = 0;

    let now = Instant::now();
    let dict = Dictionary::new(EN_US_AFF, EN_US_DIC).unwrap();
    println!("Compiled the dictionary in {}ms", now.elapsed().as_millis());

    let now = Instant::now();
    for line in std::io::stdin().lines() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                eprintln!("Failed to read line from stdin: {err}");
                std::process::exit(1);
            }
        };
        for word in line.split_whitespace().flat_map(|s| s.split("--")) {
            let word = word.trim_matches(|ch: char| !ch.is_ascii_alphabetic());
            total_words += 1;

            if !dict.check(word) {
                eprintln!("* {word}");
                misspelled += 1;
            }
        }
    }

    println!(
        "Finished in {}ms: {misspelled}/{total_words} words misspelled.",
        now.elapsed().as_millis()
    );
}
