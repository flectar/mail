# Slanted italics from a `slnt` axis

When a font has no italic face, fontique 0.12 decides how to fake one. For a
variable font with a `slnt` axis and no `ital` axis it asks for `slnt` 14. A
positive `slnt` slants counter-clockwise, though, so the request points the
wrong way, and fonts whose axis only goes right-leaning clamp it to upright.
Italic text in such a font stayed upright. The bundled Google Sans Flex is one
(its `slnt` axis runs from -10 to 0). It is the last sans-serif fallback for
message text, and on OpenHarmony, where fontique finds no system fonts, the
only one. The same went for CSS obliques.

## Sources and adaptation

`crates/fontique` is the published 0.12.0 archive, revision
`6e1143dda817360dd3a2a64f9133c6e5dfabd881`, plus one patch.
`0001-negate-synthesized-slant.patch` backports upstream
[linebender/parley#922](https://github.com/linebender/parley/pull/922), which
negates the synthesized `slnt` values. Parley 0.13 contains it.

Slint 1.19, Parley 0.12 and the application share this one crate, so the patch
covers the message reader and the interface alike. `Cargo.toml.orig` is kept
unchanged for provenance. The standalone lockfile is omitted, and the
application lockfiles resolve the patched crate.

## Verification and removal

```sh
python3 scripts/verify-fontique.py
```

The verifier downloads the checksum-pinned archive, checks its revision,
applies the patch, and compares every file with the vendored crate. It also
checks that the root, Android and OpenHarmony workspaces patch the crate and
that their lockfiles resolve it locally.

When Slint and Blitz move to Parley 0.13, remove the vendored crate, this
directory, the three manifest patches, the root workspace exclusion, the
`.gitignore` and `.gitattributes` entries, the verifier, its CI step, and the
fontique section of `THIRD_PARTY_NOTICES.md`. Then update the three lockfiles
and `packaging/flatpak/cargo-sources.json`, and keep the regression test.
