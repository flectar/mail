# Outlines before monochrome bitmap strikes

Vello's GPU renderer sends every font with a bitmap strike down its color glyph
path, and takes the strike for every glyph it covers, at any size, even when
the glyph has an outline. Calibri and Cambria pair their outlines with
monochrome `EBLC`/`EBDT` strikes. Their byte-aligned masks are skipped as
unsupported, so the glyph is never drawn, and bit-aligned masks are drawn
white. In Flectar Mail's GPU mode, mail set in Calibri lost most of its letters
(Mail #342, [Vello #2019](https://github.com/linebender/vello/issues/2019)).
The CPU mode was fine because Vello CPU falls back to the outline.

## Sources and adaptation

`crates/vello` is the published 0.11.0 archive, revision
`f3000c8d9e7a9c7e6abb09b9587238bf8b4860ff`, plus one patch, which is ours.
`0001-outlines-before-monochrome-strikes.patch` ignores `EBDT` strikes in a
font that has outlines, and draws a glyph's outline instead of a monochrome or
grayscale mask from any other strike. Skia does the same unless embedded
bitmaps are turned on, in its FreeType backend (`FT_LOAD_NO_BITMAP`) and in its
Fontations one. Color bitmaps, such as emoji, and glyphs that have only a
bitmap draw as before. Vello 0.12.0 and upstream main have the same code.

`Cargo.toml.orig` is kept unchanged for provenance. The standalone lockfile is
omitted, and the application lockfiles resolve the patched crate.

## Verification and removal

```sh
python3 scripts/verify-vello.py
```

The verifier downloads the checksum-pinned archive, checks its revision,
applies the patch, and compares every file with the vendored crate. It also
checks that the root and OpenHarmony workspaces patch the crate and that their
lockfiles resolve it locally. `issue_342_bitmap_strikes_draw_from_outlines` in
`src/renderer/regression.rs` paints the test fonts described in
`resources/test-fonts/README.md`.

When a Vello release fixes this, remove the vendored crate, this directory, the
two manifest patches, the root workspace exclusion, the `.gitignore` and
`.gitattributes` entries, the verifier, its CI step, and its paragraph in
`THIRD_PARTY_NOTICES.md`. Then update the two lockfiles and
`packaging/flatpak/cargo-sources.json`, and keep the regression tests.
