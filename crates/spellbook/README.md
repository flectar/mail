# Spellbook

[![Crates.io](https://img.shields.io/crates/v/spellbook.svg)](https://crates.io/crates/spellbook)
[![Documentation](https://docs.rs/spellbook/badge.svg)](https://docs.rs/spellbook)

Spellbook is a Rust spellchecking library compatible with the Hunspell dictionary format.

```rust
fn main() {
    let aff = std::fs::read_to_string("en_US.aff").unwrap();
    let dic = std::fs::read_to_string("en_US.dic").unwrap();
    let dict = spellbook::Dictionary::new(&aff, &dic).unwrap();

    let word = std::env::args().nth(1).expect("expected a word to check");

    if dict.check(&word) {
        println!("{word:?} is in the dictionary.");
    } else {
        let mut suggestions = Vec::new();
        dict.suggest(&word, &mut suggestions);
        eprintln!("{word:?} is NOT in the dictionary. Did you mean {suggestions:?}?");
        std::process::exit(1);
    }
}
```

Spellbook aims to be minimal: it is `no_std` and only requires [`hashbrown`] as a dependency. (Note that [`foldhash`] is included by default, see the feature flags section below.)

### Maturity

Spellbook is a work in progress and might see breaking changes to any part of the API as well as updates to the MSRV and dependencies.

Currently the `check` API works well for `en_US` - a relatively simple dictionary - though it should work reasonably well for most other dictionaries. Some dictionaries which use complex compounding directives may work less well.

The `suggest` API was added in v0.2.0 and should behave the same as Nuspell's `suggest`. (Meaning that phonetic suggestions are not implemented.)

Spellbook should be considered to be in _alpha_. Almost all of the Hunspell test corpus tested by Nuspell is passing.

### Feature flags

Spellbook follows [`hashbrown`] by including a `default-hasher` feature flag which is enabled by default. Like Hashbrown v0.15+, the default hasher is [`foldhash`].

A non-cryptographic hash significantly improves the time it takes to initialize a dictionary and check and suggest words. Denial-of-service attacks are not usually relevant for this use-case since you would usually not take dictionary files as arbitrary inputs, so a non-cryptographic hash is probably ok. (I am not a cryptologist.) Note that Hashbrown v0.14 and lower used [`ahash`](https://github.com/tkaitchuck/aHash) instead of foldhash. In my runs of the Spellbook benchmarks there is no perceptible performance difference between `foldhash` and `ahash`.

If you wish to use a different hasher you may turn this default feature off:

```toml
[dependencies]
spellbook = { version = "1.0", default-features = false }
```

and specify a hasher of your choosing instead:

```rust
use std::hash::BuildHasherDefault;
type Dictionary = spellbook::Dictionary<BuildHasherDefault<ahash::AHasher>>;
```

### Spellbook in practice

Spellbook is used "in the wild" in these projects:

* [`codebook`](https://github.com/blopker/codebook) uses [`tree-sitter`](https://github.com/tree-sitter/tree-sitter) and Spellbook for a programming-language-friendly spell checking extension for the Zed editor and a LSP language server
* [`cargo-spellcheck`](https://github.com/drahnr/cargo-spellcheck) uses Spellbook as an optional back end under the `spellbook` feature flag for a CLI spell checking tool for [`cargo`](https://github.com/rust-lang/cargo) Rust projects

### How does it work?

For a more in depth overview, check out [`@zverok`]'s blog series [Rebuilding the spellchecker][zverok-blog].

Hunspell dictionaries are split into two files: `<lang>.dic` and `<lang>.aff`.
The `.dic` file has a listing of stems and flags associated with that stem. For example `en_US.dic` contains the word `adventure/DRSMZG` meaning that "adventure" is a stem in the dictionary with flags `D`, `R`, `S`, `M`, `Z` and `G`.
The `.aff` file contains a bunch of rules to use when determining if a word is correct or figuring out which words to suggest. The most intuitive of these are prefixes and suffixes. `en_US` contains suffixes like `R` and `G`:

```
SFX R Y 4
SFX R   0     r          e
SFX R   y     ier        [^aeiou]y
SFX R   0     er         [aeiou]y
SFX R   0     er         [^ey]

SFX G Y 2
SFX G   e     ing        e
SFX G   0     ing        [^e]
```

Since "adventure" has these flags, these suffixes can be applied. The rules are structured as tables that define the flag (like `R`), what to strip from the end of the word (`0` for nothing), what to add to the end (`er` for example) and under what condition the suffix applies (matches `[^aeiou]y` meaning not 'a' 'e' 'i' 'o' 'u' and then 'y' for example). When checking a word like "adventurer" you find any suffixes where the "add" portion of the suffix matches the ending of the word and check if the condition applies. The first clause of `R` applies since the "adventure" ends in 'e', and we add a 'r' to the end. When checking this happens in reverse. Starting with a word like "adventurer" we strip the 'r' and check the condition. Similarly with `G`, the first clause matches "adventuring" because "adventure" ends with 'e' and we add an "ing".

Hunspell dictionaries use these prefixing and suffixing rules to compress the dictionary. Without prefixes and suffixes we'd need a big set of every possible conjugation of every word in the dictionary. That might be possible with the gigabytes of RAM we have today but it certainly isn't efficient.

Another way Hunspell dictionaries "compress" words like this is compounding. For example with the COMPOUNDRULE directive:

```
# compound rules:
# 1. [0-9]*1[0-9]th (10th, 11th, 12th, 56714th, etc.)
# 2. [0-9]*[02-9](1st|2nd|3rd|[4-9]th) (21st, 22nd, 123rd, 1234th, etc.)
COMPOUNDRULE 2
COMPOUNDRULE n*1t
COMPOUNDRULE n*mp
```

`en_US.dic` has words for digits like `0/nm`, `0th/pt`, `1/n1`, `1st/p`, etc. The COMPOUNDRULE directive describes a regex-like pattern using flags and `*` (zero-or-more) and `?` (zero-or-one) modifiers. For example the first compound rule in the table `n*1t` allows a word like "10th": it matches the `n` flag zero times and then "1" (the stem of the `1` flag in the `.dic` file) and "0th". The `n*` modifier at the front allows adding any number of any other digit, so this rule also allows words like "110th" or "10000th".

### Other docs

* An overview of [internals](./docs/internals.md)
* [Comparisons](./docs/compare.md) to other spellcheckers
* [Development and contributing notes](./docs/CONTRIBUTING.md)

### Credits

* [`@zverok`]'s [blog series on rebuilding Hunspell][zverok-blog] was an invaluable resource during early prototypes. The old [`spylls`](https://github.com/zverok/spylls)-like prototype can be found on the `spylls` branch.
* Ultimately [Nuspell](https://github.com/nuspell/nuspell)'s codebase became the reference for Spellbook though as C++ idioms mesh better with Rust than Python's. Nuspell's code is in great shape and is much more readable than Hunspell so for now Spellbook is essentially a Rust rewrite of Nuspell (though we may diverge in the future).
* The parser for `.dic` and `.aff` files is loosely based on [ZSpell](https://github.com/pluots/zspell).

[`hashbrown`]: https://github.com/rust-lang/hashbrown
[`foldhash`]: https://github.com/orlp/foldhash
[`@zverok`]: https://github.com/zverok
[zverok-blog]: https://zverok.space/spellchecker.html
