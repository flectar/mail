# Rounded clips far out of view

Slint 1.19's software renderer clips to the border radius of `Clip` items, which
it didn't before. Two places of that code convert a rect to the renderer's `i16`
coordinates before cutting it to what is in view. A rounded clip scrolled more
than 32767 physical pixels away, such as an avatar in a long conversation, or a
rectangle inside a rounded clip that is taller than that, then panics in
euclid's `cast`. Flectar Mail's CPU mode, the default, crashed on long messages
and threads. The reader tests in `src/reader_validation.rs` caught it.

## Sources and adaptation

`crates/i-slint-renderer-software` is the published 1.19.0 archive, revision
`e436f546f71481d786cbc2a74316b1be1ef678c1`, plus one patch, which is ours.
`0001-clip-far-out-of-view.patch` cuts a rounded clip's rect to the current clip,
grown by its largest corner radius and border, before the conversion. Outside
that band its corners can't touch anything drawn, so what is in view doesn't
change, and a clip with nothing in view leaves an empty rect, which hides its
children as before. The rectangle check of the `TargetPixelBuffer` hooks now
cuts the rectangle to the clip before the conversion too, as the texture check
next to it already did. Upstream master has the same code.

`Cargo.toml.orig` is kept unchanged for provenance. The standalone lockfile is
omitted, and the application lockfiles resolve the patched crate.

## Verification and removal

```sh
python3 scripts/verify-slint-renderer-software.py
```

The verifier downloads the checksum-pinned archive, checks its revision,
applies the patch, and compares every file with the vendored crate. It also
checks that the root, Android and OpenHarmony workspaces patch the crate and
that their lockfiles resolve it locally for Slint 1.19.0.

When a Slint release fixes this, remove the vendored crate, this directory, the
three manifest patches, the root workspace exclusion, the `.gitignore` and
`.gitattributes` entries, the verifier, its CI step, and its paragraph in
`THIRD_PARTY_NOTICES.md`. Then update the three lockfiles and
`packaging/flatpak/cargo-sources.json`, and keep the regression tests.
