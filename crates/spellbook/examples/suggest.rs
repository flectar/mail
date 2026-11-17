/*
Most basic example for the suggester for quick debugging.

This example doesn't check whether the input word is in the dictionary first.

## Usage

```
$ cargo run --example suggest ansi
Compiled the dictionary in 127ms
Suggestions for "ansi": "ANSI", "ans", "anti", "ans i" (checked in 1367µs)
```
*/
use std::time::Instant;

use spellbook::Dictionary;

const EN_US_AFF: &str = include_str!("../vendor/en_US/en_US.aff");
const EN_US_DIC: &str = include_str!("../vendor/en_US/en_US.dic");

fn main() {
    let mut args = std::env::args().skip(1);
    let word = match args.next() {
        Some(arg) => arg,
        None => {
            eprintln!("Usage: suggest WORD");
            std::process::exit(1);
        }
    };

    let now = Instant::now();
    let dict = Dictionary::new(EN_US_AFF, EN_US_DIC).unwrap();
    println!("Compiled the dictionary in {:?}", now.elapsed());

    let mut suggestions = Vec::with_capacity(5);
    let now = Instant::now();
    dict.suggest(&word, &mut suggestions);
    let time = now.elapsed();
    if suggestions.is_empty() {
        println!("No suggestions found for \"{word}\" (checked in {time:?})");
    } else {
        let suggestions = suggestions
            .into_iter()
            .fold(String::new(), |mut s, suggestion| {
                if !s.is_empty() {
                    s.push_str(", ");
                }
                s.push('"');
                s.push_str(&suggestion);
                s.push('"');
                s
            });
        println!("Suggestions for \"{word}\": {suggestions} (checked in {time:?})");
    }
}
