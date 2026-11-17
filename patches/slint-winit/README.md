# Slint winit backend patches

## Wayland file drops

Slint 1.19 still runs on winit 0.30, which doesn't report files dragged onto a
window on Wayland. Files dragged from a file manager onto Flectar Mail did
nothing there, in the Flatpak too. The clipboard's own data device takes those
drags instead (see `patches/smithay-clipboard/README.md`), and this patch hands
them to the window as the `HoveredFile`, `HoveredFileCancelled` and
`DroppedFile` events winit gives on other platforms. Slint's own winit 0.31
branch reports them natively.

## Frame pacing on Windows

The frame throttle started a frame every whole number of milliseconds from the
monitor's refresh rate, every 16 ms at 60 Hz instead of every 16.67 ms. Frames
slipped against the display by about 0.67 ms each, so on Windows every 25th one
was shown twice or not at all, and animations didn't run smoothly (Mail #343).

## Sources and adaptation

`crates/i-slint-backend-winit` is the published 1.19.0 archive, revision
`e436f546f71481d786cbc2a74316b1be1ef678c1`, plus three patches.
`0001-dependency-scanner.patch` lists five dependencies that the release
declares but does not import, so `cargo machete` can scan the crate. The other
two are ours.

`0002-wayland-file-drops.patch` gives the clipboard a drop handler that sends
each drag to the event loop, which finds the window by its Wayland surface and
dispatches the winit events. For a drop with a document portal key, the handler
asks the FileTransfer portal for files a sandboxed app can read.

`0003-frame-pacing-on-the-compositor-clock.patch` computes the throttle's
period in nanoseconds on every platform. On Windows it also starts each frame
at DWM's next vblank, which `DwmGetCompositionTimingInfo` gives, as Chromium
paces its software frames and winit #2412 proposes. When DWM has no timing, or
follows a monitor with another refresh rate than the window's, the plain
period is used.

`Cargo.toml.orig` is kept unchanged for provenance. The standalone lockfile is
omitted, and the application lockfiles resolve the patched manifest.

Slint 1.19 reaches the Wayland clipboard through smithay-clipboard on the event
loop's own connection
([Slint #14248](https://github.com/slint-ui/slint/pull/14248)), which Flectar
Mail carried as a backport for Slint 1.18 (Mail #108). smithay-clipboard is
pinned to 0.7.2, which uses SCTK 0.19 like winit 0.30. Version 0.7.3 moved to
SCTK 0.20 and would build a second copy.

The reader's rich-text copy and primary selection still go through arboard
(`src/reader_clipboard.rs`). Where arboard has no backend, rich copy falls back
to plain text through Slint.

## Verification and removal

```sh
python3 scripts/verify-slint-winit.py
```

The verifier downloads the checksum-pinned archive, checks its revision,
applies the patches, and compares every file with the vendored crate. It also
checks that the root, Android and OpenHarmony workspaces patch the crate, that
their lockfiles resolve it locally for Slint 1.19.0, and the smithay-clipboard
pin.

When Flectar Mail moves to a Slint release on winit 0.31, the file drop patch
goes together with smithay-clipboard, as `patches/smithay-clipboard/README.md`
lists. The Windows patch stays until a Slint release paces software frames
the same way. Remove the vendored crate when none is left.
