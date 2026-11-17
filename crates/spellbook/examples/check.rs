/*
Most basic example for the checker for quick debugging.

## Usage

```
$ cargo run --example check hello
Compiled the dictionary in 113ms
"hello" is in the dictionary (checked in 3µs)
$ cargo run --example check helol
Compiled the dictionary in 110ms
"helol" is NOT in the dictionary (checked in 21µs)
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
            eprintln!("Usage: check WORD");
            std::process::exit(1);
        }
    };

    let now = Instant::now();
    let dict = Dictionary::new(EN_US_AFF, EN_US_DIC).unwrap();
    println!("Compiled the dictionary in {:?}", now.elapsed());

    let now = Instant::now();
    if dict.check(&word) {
        println!(
            "\"{word}\" is in the dictionary (checked in {:?})",
            now.elapsed()
        );
    } else {
        eprintln!(
            "\"{word}\" is NOT in the dictionary (checked in {:?})",
            now.elapsed()
        );
        std::process::exit(1);
    }
}
