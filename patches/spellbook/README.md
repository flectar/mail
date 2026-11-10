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
