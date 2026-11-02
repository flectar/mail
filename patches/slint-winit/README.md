# Wayland clipboard backport

Slint 1.18 replaced copypasta with arboard
([Slint #12373](https://github.com/slint-ui/slint/pull/12373)). On Wayland,
arboard needs the data-control protocol on a connection of its own. GNOME and
COSMIC do not offer it by default, and compositors hide it from sandboxed
clients. arboard then falls back to X11, which a Flatpak with
`--socket=fallback-x11` does not have on Wayland. Slint silently uses a no-op
clipboard, so copy and paste do nothing (Mail #108).

## Sources and adaptation

`crates/i-slint-backend-winit` is the published 1.18.1 archive, revision
`372cf0ee5577c3dfec309a45e7b778ba4e81b734`, plus two patches.
`0001-wayland-clipboard.patch` is
[Slint #14248](https://github.com/slint-ui/slint/pull/14248) at
`cb91c1927cd8c17649ba445d28d3a0c2a0041238`, adapted to 1.18.1's `cfg_if`
block and normalized manifest. On Wayland, smithay-clipboard uses the event
loop's own `wl_display` with the core `wl_data_device` and primary-selection
protocols, which every compositor offers to sandboxed clients too. arboard
stays for X11. The upstream changelog and lockfile hunks are omitted.
`0002-dependency-scanner.patch` lists five dependencies that the release
declares but does not import, so `cargo machete` can scan the crate.
`Cargo.toml.orig` is kept unchanged for provenance. The standalone lockfile is
omitted; both application lockfiles resolve the patched manifest.

smithay-clipboard is pinned to 0.7.2, which uses SCTK 0.19 like winit 0.30.
Version 0.7.3 moved to SCTK 0.20 and would build a second copy.

The reader's rich-text copy and primary selection still go through arboard
(`src/reader_clipboard.rs`). Where arboard has no backend, rich copy falls back
to plain text through Slint.

## Verification and removal

```sh
python3 scripts/verify-slint-winit.py
```

The verifier downloads the checksum-pinned archive, checks its revision,
applies the patch, and compares every file with the vendored crate. It also
checks the desktop/Android patch wiring, that both lockfiles resolve the local
crate for Slint 1.18.1, and the smithay-clipboard pin.

When a Slint release includes #14248, remove the vendored crate, this
directory, both manifest patches, the `.gitignore` and `.gitattributes`
entries, the verifier, and its CI step. Then update both lockfiles and
`packaging/flatpak/cargo-sources.json`.
