# Android backend patches

Slint 1.19.0's Android backend stops with `todo!()` when Android reports a
pointer scroll, which a mouse wheel or a touchpad sends
([Slint #14494](https://github.com/slint-ui/slint/issues/14494)). Any wheel or
two-finger touchpad scroll then aborts Flectar Mail on Android, for example on a
Chromebook, with Samsung DeX, or on a tablet with a trackpad keyboard.

## Sources and adaptation

`crates/i-slint-backend-android-activity` is the published 1.19.0 archive,
revision `e436f546f71481d786cbc2a74316b1be1ef678c1`, plus two patches.
`0001-pointer-scroll.patch` is
[Slint #14495](https://github.com/slint-ui/slint/pull/14495) at
`04fe0cc568e4f1b90fc63f6ee8f66d33423c513d`, which applies to 1.19.0 unchanged.
It turns the scroll axes into a Slint `PointerScrolled` event at the pointer,
50 logical pixels per wheel step.
`0002-back-as-close-request.patch` is our own. Since Slint 1.18 the Android
backend sends Back as a `Key.Back` key and finishes the activity when no
element takes it. Flectar Mail steps back through its screens from the
window's close request, which is how Back reached it before, and a key needs
an element with focus, which a phone often lacks after a tap. So a Back no
element takes now goes to the app as a close request, on Android 14 and later
through `OnBackInvokedCallback` and before that as `KEYCODE_BACK`, and the
activity finishes only when the app lets the window close.
`Cargo.toml.orig` is kept unchanged for provenance. The standalone lockfile is
omitted, and the Android lockfile resolves the patched crate. Only the Android
app links this backend, so only `platform/android/Cargo.toml` patches it.

## Verification and removal

```sh
python3 scripts/verify-slint-android-activity.py
```

The verifier downloads the checksum-pinned archive, checks its revision,
applies the patch, and compares every file with the vendored crate. It also
checks the Android manifest patch and that the Android lockfile resolves the
local crate for Slint 1.19.0.

When a Slint release includes #14495 and Flectar Mail takes Back as a key
itself, remove the vendored crate, this
directory, the manifest patch, the root workspace exclusion, the `.gitignore`
and `.gitattributes` entries, the verifier, its CI step, and its paragraph in
`THIRD_PARTY_NOTICES.md`. Then update the Android lockfile.
