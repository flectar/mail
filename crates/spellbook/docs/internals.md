# Internals

## Data structures

### Boxed slices

By default Spellbook prefers boxed slices (`Box<[T]>`) and boxed strs (`Box<str>`) rather than their resizable counterparts `Vec<T>` and `String`. Boxed slices can be used as drop-in replacements for the most part. For example you can index into a boxed slice and iterate on the elements. The difference is that boxed slices have a fixed size once created: you can push to a `Vec` but not a `Box<[T]>`. They also discard any excess capacity and don't need to track length and capacity separately, saving a very small amount of memory per instance.

```rust
type Stem = UmbraString;
```

`Box<str>` was the representation for stems in the dictionary but this type has been changed to an even further optimized structure based on a "German string." A deep dive into the optimization can be found [here](https://the-mikedavis.github.io/posts/german-string-optimizations-in-spellbook/).

### Flag sets

```rust
struct FlagSet(UmbraSlice<Flag>);
```

As mentioned above, `FlagSet` uses a "German string" inspired optimization to store small sets inline. You can imagine this type as basically `Box<[Flag]>` though. Further discussion of the `FlagSet` optimization in particular can be found [here](https://the-mikedavis.github.io/posts/german-string-optimizations-in-spellbook/#bonus-points-the-flagset-can-also-be-german).

Words in the dictionary are associated with any number of flags, like `adventure/DRSMZG` mentioned above. The order of the flags as written in the dictionary isn't important. We need a way to look up whether a flag exists in that set quickly. The right tool for the job might seem like a `HashSet<Flag>` or a `BTreeSet<Flag>`. Those are mutable though so they carry some extra overhead. A dictionary contains many many flag sets and the overhead adds up. So what we use instead is an optimized version of a sorted `Box<[Flag]>` and look up flags with `slice::contains` or `slice::binary_search` depending on length.

Binary searching a small slice is a tiny bit slower than `slice::contains` but we prefer `slice::binary_search` for its consistent performance on outlier flag sets. See [`benches/slice-contains.rs`](../benches/slice-contains.rs) for more details.

### Flags

```rust
type Flag = core::num::NonZeroU16;
```

Spellbook represents flags with non-zero `u16`s. Non-zero numbers are special core types in Rust that enable a memory layout optimization: the size of a `Option<NonZeroU16>` is the same as the size of a `u16` - you don't pay for the `Option`. This optimization isn't useful for flags in flag sets. Flags are also used in `.aff` files to mark stems as having some special properties though. For example `en_US` uses `ONLYINCOMPOUND c` to declare that stems in the dictionary with the `c` flag are only valid when used in a compound, for example `1th/tc`, `2th/tc` or `3th/tc`. These stems are only correct when used in a compound like "11th", "12th" or "13th" and aren't correct alone. Internally Spellbook keeps a bunch of these `Option<Flag>`s so the layout optimization saves a small amount of space.

By default, flags are encoded in a dictionary with the `UTF-8` flag type. For `en_US` that means that each character after the `/` in a word in the dictionary and any flags declared in `en_US.aff` are converted to a `u16` (and then `NonZeroU16`). A 16 bit integer can't fit all unicode characters (UTF-8 characters may be up to 32 bits) but the lower 16 bits of UTF-8 are more than sufficient for declaring flags. Flags are only used to specify properties with the `.aff` file rather than stems. There are other encoding for flags used by some dictionaries. See the `FlagType` enum for more details.

### Word list

```rust
type WordList = HashBag<Stem, FlagSet>;
```

The word list is one of the two central data structures. It's a lookup table for the pairs of `(stem, flag_set)` defined in a dictionary's `.dic` file. We need to look up whether a word is in the dictionary (and what flags it has) very quickly. A natural choice might be `HashMap<String, FlagSet>` or `BTreeSet<String, FlagSet>`. Unlike flag sets and boxed slices and strs mentioned above, it's ok for this type to be resizable. There's only one instance of it in a dictionary and the API can support adding words to the dictionary to enable building a personal dictionary feature. Instead the snag with this type is that there can be duplicate stems in the dictionary with different flag sets. Merging the flag sets together isn't correct: the combination of flags might allow one prefix/suffix to apply but not work in a compounds while another entry provides a different prefix/suffix which can compound.

So what we need is something closer to `HashMap<String, Vec<FlagSet>>`. The extra `Vec` is more overhead though that isn't necessary in most cases since duplicate stems are fairly rare. In other languages like C++ this is where a [multi map](https://en.cppreference.com/w/cpp/container/unordered_multimap) might fit. It's the same idea as a hash map but allows for duplicate keys. Building a type like this in Rust is actually pretty straightforward with the [`hashbrown`] `HashTable` API. Insertion is slightly simpler than a `HashMap`: we don't need to check if the key is already in the table, we can just blindly insert. Reading from the table works very similarly to `HashMap::get`. Lookup in a regular hash map can stop searching the table when the first entry matching the key is found. For a multi map though we continue to look up until we find an entry that doesn't match.

See the implementation details for this in [`src/hash_bag.rs`](../src/hash_bag.rs).

### Affix index

Affixes (i.e. prefixes and suffixes) are stored in an "index" that allows quick lookup. For example `en_US` has prefixes like these:

```
PFX C Y 1
PFX C   0     de          .

PFX E Y 1
PFX E   0     dis         .
```

Which might apply to a stem in the dictionary like `pose/CAKEGDS` to allow words "depose" and "dispose". When checking "depose" we look up in the set of prefixes to find any where the input word starts with the "add" part (for example `"depose".starts_with("de")`).

A [prefix tree](https://en.wikipedia.org/wiki/Trie) would allow very quick lookup. Trees and graph-like structures are not the most straightforward things to write in Rust though. Luckily Nuspell has a trick for this type which works well in Rust. Instead of a tree, we collect the set of prefixes into a `Box<[Prefix]>` table sorted by the "add" part of a prefix/suffix ("de" or "dis" above, for example). We can then binary search based on whether a prefix matches (`str::starts_with`). There are some additional optimizations like an extra lookup table that maps the first character in a prefix to the starting index in the `Box<[Prefix]>` table so that we can jump to the right region of the table quickly.

## Unsafe code

Spellbook uses `unsafe` in three ways:

1. Small-string/slice optimizations. The `umbra_slice` module uses `unsafe` to interpret itself as either an inline or allocated string/slice.
2. UTF-8 manipulation. Spellbook manipulates UTF-8 encoded strings as bytes in some cases for performance reasons. For example when checking German sharps, Spellbook might replace "ss" with "ß". These two strings have the same UTF-8 length (2) so the bytes can be overwritten directly. This kind of edit can't be done as efficiently in safe Rust.
3. A `CharsStr` type in the ngram suggester (`src/suggester/ngram.rs`) indexes into its underlying `str` without bounds checks for performance reasons.

These uses of `unsafe` could theoretically be eliminated:

1. The `Stem` and `FlagSlice` types could switch from `umbra_slice` types to `Box<str>` and `Box<[Flag]>` respectively with the tradeoff of significantly higher total dictionary memory size (around 25% more for `en_US`).
2. String edits could be done using safe methods only for an unknown performance hit to the checker and likely a larger hit to the suggester.
3. `CharsStr` could use checked lookups into its underlying `str` for a small performance hit.

But eliminating `unsafe` is not really interesting to me. The uses rely on solid assumptions are typically documented with "SAFETY" comments.

[`hashbrown`]: https://github.com/rust-lang/hashbrown
