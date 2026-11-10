# Comparisons to other spellcheckers

Spellbook is mainly a rewrite of Nuspell so the ways Spellbook diverges from Nuspell are the most notable:

* Instead of a custom hash table implementation Spellbook uses [`hashbrown`].
* Spellbook uses a different string and slice representation for word-list stems and flagsets. See the [internals] document for more info.
* Nuspell switches to UTF-32 for ngram suggestions while Spellbook consistently uses UTF-8. This performs better in Rust as the standard library has optimized routines for UTF-8 strings.

## Performance comparisons of "Hunspell-like" `check`

When it comes to performance I mainly care about the time it takes to check a word - dictionary initialization (`new`) and suggestion (`suggest`) don't happen often enough to be really concerning. To measure `check`/`spell` we use `cargo bench` benchmarks as of Rust nightly-2024-08-21 (arbitrarily). Benchmarks for Spellbook can be seen in the `benches/check.rs` file in this repository. Benchmarks for Hunspell and Nuspell can be found in [`the-mikedavis/ffi-dictionaries`](https://github.com/the-mikedavis/ffi-dictionaries/tree/3b1dc8fb4caf1961a958011d0255ed7d31696616/benches)'s `benches` directory. Note that these use `c"word"` sigils so the benchmark doesn't pay the cost of converting from a Rust string to a C/C++ string. These tests are run against the stock en_US dictionary.

| Test name                      | Word              | Hunspell                     | Nuspell                      | Spellbook                  |
|--------------------------------|-------------------|------------------------------|------------------------------|----------------------------|
| `breaks`                       | light-weight-like | 4,433.77 ns/iter (+/- 44.80) | 1,250.27 ns/iter (+/- 23.00) | 865.61 ns/iter (+/- 31.54) |
| `compound_word`                | 20000th           | 3,432.38 ns/iter (+/- 49.68) |   736.53 ns/iter (+/- 15.57) | 708.11 ns/iter (+/- 20.72) |
| `in_dictionary_word`           | earth             |   212.30 ns/iter (+/- 3.67)  |   126.97 ns/iter (+/- 7.39)  |  65.84 ns/iter (+/- 2.12)  |
| `incorrect_prefix`             | reearth           | 1,175.15 ns/iter (+/- 12.86) |   520.51 ns/iter (+/- 19.37) | 493.24 ns/iter (+/- 18.18) |
| `number`                       | 8675,309.0        |   239.64 ns/iter (+/- 6.47)  |    92.12 ns/iter (+/- 1.29)  |  41.10 ns/iter (+/- 1.13)  |
| `titlecase_in_dictionary_word` | Earth             |   827.88 ns/iter (+/- 13.48) |   513.78 ns/iter (+/- 9.10)  | 314.48 ns/iter (+/- 8.61)  |
| `uppercase_in_dictionary_word` | EARTH             | 1,426.46 ns/iter (+/- 15.70) |   280.78 ns/iter (+/- 8.03)  | 182.43 ns/iter (+/- 6.41)  |
| `word_with_prefix`             | unearth           |   288.74 ns/iter (+/- 7.82)  |   924.66 ns/iter (+/- 35.69) | 539.69 ns/iter (+/- 10.97) |
| `word_with_prefix_and_suffix`  | unearthed         |   448.48 ns/iter (+/- 9.64)  |   534.85 ns/iter (+/- 14.11) | 395.25 ns/iter (+/- 9.71)  |
| `word_with_suffix`             | earthly           |   236.94 ns/iter (+/- 6.17)  |   141.47 ns/iter (+/- 2.98)  |  69.81 ns/iter (+/- 7.60)  |

When it comes to memory, Valgrind's [DHAT](https://valgrind.org/docs/manual/dh-manual.html) is a good tool for reporting total allocations and breaking down heap interaction. We'll run `vaglind --tool=dhat <check-binary> hello` for an example binary that checks the given word against en_US.

Hunspell:

```
Total:     3,325,493 bytes in 90,736 blocks
At t-gmax: 3,164,250 bytes in 90,144 blocks
At t-end:  0 bytes in 0 blocks
Reads:     16,733,982 bytes
Writes:    6,264,702 bytes
```

Nuspell:

```
Total:     4,437,599 bytes in 50,419 blocks
At t-gmax: 4,208,644 bytes in 49,870 blocks
At t-end:  3,486 bytes in 6 blocks
Reads:     16,320,481 bytes
Writes:    8,027,585 bytes
```

Spellbook:

```
Total:     2,580,386 bytes in 44,741 blocks
At t-gmax: 2,189,633 bytes in 947 blocks
At t-end:  0 bytes in 0 blocks
Reads:     1,728,454 bytes
Writes:    2,105,758 bytes
```

### Analysis

Mostly I am familiar with Nuspell so I'll be talking about Spellbook vs. Nuspell in this section.

The `check` code is basically a rewrite so they should perform very similarly. One major difference that might affect lookup time is the main lookup table. It's meant to be a hash multi-map, like a `HashMap<String, FlagSet>` but allowing duplicate keys. Nuspell rolls its own hash table type for this while Spellbook uses `hashbrown::HashTable` which has SIMD optimizations for searching. Spellbook also uses `foldhash` by default which is quite fast while Nuspell uses `std::hash` (implementation-specific). This sometimes happens with Rust rewrites: it's a pain to take a dependency in C/C++ so C/C++ libraries/tools might leave performance on the table by not taking advantage of available high-performance dependencies. To confirm or deny this suspicion one could replace Nuspell's `Word_List` type with an adaptation from Google's `SwissTable` library (on which `hashbrown` is based).

Otherwise I suspect that Rust's standard library has better optimizations for string searching and equality, as I know it uses `memchr` and SIMD operations when available.

When it comes to memory, Spellbook is optimized to save memory by cutting out unnecessary bytes from the common string type used in the lookup table, as well as small-string and small-slice optimizations for the stem and flagsets. The [internals] document has more details.

## ZSpell

[`pluots/zspell`](https://github.com/pluots/zspell) is an interesting alternative to the Hunspell-like spellcheckers mentioned above. ZSpell also takes the `.dic` and `.aff` Hunspell-style dictionary files. At time of writing ZSpell doesn't support suggestions. The interesting part of ZSpell is how it checks words instead.

ZSpell expands affixes during instantiation of a dictionary. (See the `README.md` doc in this repository for a basic intro on affixes.) The "classic" spellcheckers mentioned above contain a subset of the possible dictionary words in a main lookup table. For example Spellbook's table includes "adventure" but not some of its conjugations made possible by prefixes/suffixes like "adventurer" or "adventured". In contrast, ZSpell expands each stem so that its tables include "adventure", "adventures", "adventurer", "adventured", "adventuring" and more. When checking a word, ZSpell performs a lookup into a handful of hash maps, short-circuiting if a word is found.

The benefit is a basically constant-time `Dictionary::check_word` performance:

| Test name                      | Word              | ZSpell                       | Notes... |
|--------------------------------|-------------------|------------------------------|----------|
| `breaks`                       | light-weight-like | N/A                          | ZSpell has a custom tokenization/breaking strategy not based on Hunspell |
| `compound_word`                | 20000th           | N/A                          | ZSpell does not support compounds |
| `in_dictionary_word`           | earth             |   46.86 ns/iter (+/- 1.35)   | |
| `incorrect_prefix`             | reearth           |   62.47 ns/iter (+/- 1.14)   | |
| `number`                       | 8675,309.0        | N/A                          | ZSpell does not detect/support numbers |
| `titlecase_in_dictionary_word` | Earth             |   50.51 ns/iter (+/- 0.42)   | |
| `uppercase_in_dictionary_word` | EARTH             |   52.63 ns/iter (+/- 0.46)   | |
| `word_with_prefix`             | unearth           |   54.52 ns/iter (+/- 1.84)   | |
| `word_with_prefix_and_suffix`  | unearthed         |   61.13 ns/iter (+/- 2.08)   | |
| `word_with_suffix`             | earthly           |   54.94 ns/iter (+/- 1.52)   | |

This comes with costs however. Behold the `DHAT` output for an example `check` binary run:

```
Total:     83,209,456 bytes in 731,177 blocks
At t-gmax: 57,081,051 bytes in 347,038 blocks
At t-end:  246,796 bytes in 459 blocks
Reads:     130,487,585 bytes
Writes:    69,845,862 bytes
```

So the tradeoff is much more memory usage. There's also a correctness issue with compounds: "20000th" from the benchmark fails to check. Checking compounds involves slicing up the input word and checking the components to see if they are compound components laid out in a pattern declared by the `.aff` file. This part of Hunspell/Nuspell is not implemented by ZSpell.

For `en_US` specifically you might accept these tradeoffs. It's more memory but the check time is nearly constant. `en_US` only uses compounds for numbers, for example "7th", "21st" or "20000th" from the benchmark. If you have a large corpus to check, don't care much for memory and can skip compounds then it's not a bad tradeoff.

The approach of expanding affixes is not scalable however and the tradeoff becomes worse with other Hunspell dictionaries. `en_US` is quite slim and simple with 50,000 stems, 7 prefixes and 16 suffixes. Brazilian Portuguese (`pt_BR`) is a far more complicated real-world dictionary weighing in at over 312,000 stems, 47 prefixes and 57 suffixes. Even with Spellbook this dictionary takes a hefty 100ms to initialize but with ZSpell, initialization runs for more than six minutes and consumes more than 100GB of memory before I kill it.

The reason I mention ZSpell specifically in the comparison is it's a good example of the strategy taken by other implementations that consume Hunspell dictionary files. [Harper](https://github.com/elijah-potter/harper) and [Vale](https://github.com/errata-ai/vale) are two other projects in the wild that expand affixes in their checkers.

[`hashbrown`]: https://github.com/rust-lang/hashbrown
[internals]: ./internals.md
