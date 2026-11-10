# spellbook

Flectar Mail checks spelling with the operating system's own spell checker
where there is one. Linux has no spell-check service, so there the app reads
the system's Hunspell dictionaries with
[spellbook](https://github.com/helix-editor/spellbook), a Rust port of the
Hunspell and Nuspell checker.

## Source

`crates/spellbook` is spellbook 0.4.2 from crates.io, checksum
`0d204abcbdf8e88729306a8d0ca01a79d8a49969fe1f84696909d6dbc4321c1a`, published
from revision `2d278335f592defadeb1133917481bf510b5e635`, plus the patches
listed in `upstream.json`. The release also contains its author's local
tooling, which is left out: `.claude/`, `.github/`, `.envrc`, `flake.lock`,
`flake.nix` and `shell.nix`.

`scripts/verify-spellbook.py` downloads the release, applies the patches and
checks that the result matches `crates/spellbook` exactly. CI runs it and the
crate's own tests, which include Hunspell's legacy test cases.

spellbook is licensed under MPL-2.0, which names the AGPL as a compatible
secondary license. Its files keep that license.

## 0001-word-index.patch

Parsed into memory, a Hunspell dictionary keeps 2 MiB (English) to 18 MiB
(German) of private memory for as long as it is loaded. This patch lets
spellbook read a dictionary's stems in place from a word index instead, a
compact file that `Dictionary::word_index` writes from a parsed dictionary.
The app writes it once per dictionary and memory-maps it. The stems then sit
in page cache, which the system can reclaim, and only the `.aff` rules are
parsed into memory.

- `Dictionary::new_with_word_index` opens a dictionary from its `.aff` file
  and an index. The index carries a hash of the `.aff` file it was written
  for, and every read from it is bounds checked, so a stale or damaged file is
  refused or skipped rather than trusted.
- The checker and suggester take a borrowed `Flags` view where they took
  `&FlagSet`, so a stem's flags come from memory or straight from the index.
  The checking logic is unchanged.
- Words passed to `add` are kept in memory, and `remove_stem` only affects
  those.
- `memcmp` is declared with the signature rustc now expects of the C
  function.
- The legacy test cases also run against an index of each case's dictionary,
  and a test compares the vendored en_US dictionary both ways: every stem,
  each in four other forms, and the suggestions for common misspellings.

Measured with LibreOffice's dictionaries in a release build, opening the
index, checking a sentence, suggesting for one word and closing:

| Dictionary | Index | Open | Private memory kept parsed | Private memory while open indexed | Check per word |
| --- | --- | --- | --- | --- | --- |
| en_US | 969 KiB | 0.06 ms | 2.1 MiB | 8 KiB | 0.45 µs |
| es_ES | 1178 KiB | 3.1 ms | 4.8 MiB | 1.6 MiB | 1.3 µs |
| ru_RU | 4654 KiB | 0.8 ms | 11.0 MiB | 420 KiB | 2.2 µs |
| de_DE | 6099 KiB | 0.24 ms | 17.8 MiB | 92 KiB | 2.4 µs |

Suggestions are the same. Tied ngram suggestions come in index order, which
is fixed, where a parsed dictionary orders them by its randomly seeded
hasher.
