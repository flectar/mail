# Slint winit backend patches

## Wayland file drops

Slint 1.19 still runs on winit 0.30, which doesn't report files dragged onto a
window on Wayland. Files dragged from a file manager onto Flectar Mail did
nothing there, in the Flatpak too. The clipboard's own data device takes those
drags instead (see `patches/smithay-clipboard/README.md`), and this patch hands
them to the window as the `HoveredFile`, `HoveredFileCancelled` and
`DroppedFile` events winit gives on other platforms. Slint's own winit 0.31
branch reports them natively.

## Smooth and tear-free frames on Windows

In CPU mode on Windows, scrolling tore (Mail #343). In about a quarter of the
frames of a recording, the top of the window showed the new frame and the
bottom the previous one, with the line slowly moving. Animations didn't run
smoothly either. Two causes add up.

Softbuffer blits each frame into the window with GDI. Since Windows 7, GDI
draws straight into the surface that DWM composes from, and nothing marks the
end of a frame, so a composition during the blit shows half of each. A swap
chain hands DWM complete frames, which is why GPU mode never tore. Chromium's
software output and Firefox's software WebRender present CPU-rendered frames
through a DXGI swap chain for this reason, and GTK can too.

The frame throttle also started a frame every whole number of milliseconds
from the monitor's refresh rate, every 16 ms at 60 Hz instead of every 16.67
ms. Frames slipped against the display by about 0.67 ms each, so every 25th one
was shown twice or not at all, and the blits kept passing through DWM's
compositions.

## Windows that fit the screen

winit creates a window at the default position with the default size and only
then gives it the size Slint asks for. Windows cascades that position off the
top left of a monitor for a window of the default size, so a window sized for a
larger screen opened partly below the screen edge or behind the taskbar
(Mail #433). The main window asks for 1320 by 800 logical pixels. That is
taller than a 1366 by 768 screen, and at 125% scale on a 1920 by 1080 screen it
leaves no room for the offset Windows places it at. Its client area also came
out taller than asked by the caption, because winit adds a standard frame to
the size and Flectar Mail's window draws its own title bar. The bottom of the
window, with the last rows of the mail list, stayed out of reach until the
window was moved. In alpha.7 that part also stayed white once the window was
moved onto the screen, which Slint 1.19 and the DXGI presenter fixed.
Microsoft's guidance for window placement is to move such a window until all
of it is visible on its monitor, and to shrink a resizable window that is
larger than the monitor until it fits. Chromium sizes its first window to the
work area in the same way.

## Sources and adaptation

`crates/i-slint-backend-winit` is the published 1.19.0 archive, revision
`e436f546f71481d786cbc2a74316b1be1ef678c1`, plus five patches.
`0001-dependency-scanner.patch` lists five dependencies that the release
declares but does not import, so `cargo machete` can scan the crate. The other
four are ours.

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

`0004-windows-software-present-through-dxgi.patch` presents the software
renderer's frames through a two-buffer flip model swap chain, as Chromium's
software output does. Slint draws into a CPU-readable staging texture of a WARP
device, which needs no graphics driver. The damage is copied into the back
buffer, and `Present1` hands DWM the whole frame with its dirty rectangles. All
windows share one device. Where Direct3D 11 or DXGI isn't available, when the
first present fails, and when `SLINT_SOFTWARE_GDI` is set, softbuffer draws
with GDI as before. GDI no longer draws into a window once a flip model swap
chain presented to it, even after the swap chain is gone, so a present that
fails later gets a new swap chain instead. The swap chain's two buffers add two
window-sized surfaces, and the staging texture takes the place of softbuffer's
bitmap. The Windows packaging job runs `scripts/test-clean-exit.py` with both
presenters and prints the app's memory for each.

`0005-windows-fit-new-windows-into-the-work-area.patch` checks a window on
Windows before its first frame, after Slint gave it its preferred size. When
the window doesn't fit in the work area of the monitor that Windows put it on,
it moves inside and shrinks as far as needed. The patch sets the outer frame
itself, from the borders the window actually has, so the client area gets the
size Slint asked for. Flectar Mail's window keeps only the side and bottom
borders of the standard frame. A minimized window is left alone. A window shown
again after it was hidden keeps the place the user gave it, unless it is on no
monitor at all, for example after the monitor it was on was disconnected. The
Windows packaging job's `scripts/test-clean-exit.py` fails when a window of the
app doesn't fit the work area of its monitor. The runners' screen is 1024 by
768 pixels.

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
lists. The Windows patches stay until a Slint release paces and presents
software frames the same way. Remove the vendored crate when none is left.
