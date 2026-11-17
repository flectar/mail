# Wayland file drops

winit 0.30 has no drag and drop on Wayland, so files dragged from a file
manager onto Flectar Mail did nothing there, in the Flatpak too. A second
`wl_data_device` just for drops isn't enough, since GNOME (Mutter), Hyprland
and KWin before 6.7.5 send a client's drags to one of its data devices only,
and Mutter can take the clipboard's device out of service when another
appears. COSMIC answers the same problem by adding drag and drop to the
clipboard's device, and so does this patch.

## Sources and adaptation

`crates/smithay-clipboard` is the published 0.7.2 archive, revision
`ca11fd0bf6f71654af952ea4d710e8f1c828e600`, plus one patch, which is ours.
0.7.2 uses SCTK 0.19 like winit 0.30 (see `patches/slint-winit/README.md`).

`0001-file-drops.patch` adds `Clipboard::with_drops` and a `DropHandler`. The
clipboard thread takes drags over any of the client's surfaces that carry a
document portal key (`application/vnd.portal.filetransfer`, or
`application/vnd.portal.files` from GTK before 4.8.1) or a `text/uri-list`.
Links come as a URI list too, so a drag without a key counts as files only
once its list names local ones, and is refused otherwise. On a drop the handler
turns the key into files a sandboxed app can read, through the FileTransfer
portal, before the offer finishes, since a source may end the transfer then.
Without a key, or when the portal can't resolve it, the drop gives the local
files the list names, and the app says so when a sandbox hides them. Sources
that never close their end are given up after 10 seconds.
`crates/i-slint-backend-winit` connects the handler to the event loop
(`0002-wayland-file-drops.patch` there).

The archive's lockfile is kept, so its tests run with `--locked`.

## Verification and removal

```sh
python3 scripts/verify-smithay-clipboard.py
cargo test --manifest-path crates/smithay-clipboard/Cargo.toml --lib --locked
```

The verifier downloads the checksum-pinned archive, checks its revision,
applies the patch, and compares every file with the vendored crate. It also
checks that the root, Android and OHOS workspaces patch the crate and that
their lockfiles resolve it locally.

When Flectar Mail moves to a Slint release on winit 0.31, which reports drops
on Wayland itself, remove this vendored crate and `crates/i-slint-backend-winit`,
their directories in `patches`, their manifest patches in the three
workspaces, their `.gitignore` and `.gitattributes` entries, their notices in
`THIRD_PARTY_NOTICES.md`, both verifiers, and their CI steps. Then update the
three lockfiles and `packaging/flatpak/cargo-sources.json`.
