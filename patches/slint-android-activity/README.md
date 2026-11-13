# Android pointer scroll backport

Slint 1.18.1's Android backend stops with `todo!()` when Android reports a
pointer scroll, which a mouse wheel or a touchpad sends
([Slint #14494](https://github.com/slint-ui/slint/issues/14494)). Any wheel or
two-finger touchpad scroll then aborts Flectar Mail on Android, for example on a
Chromebook, with Samsung DeX, or on a tablet with a trackpad keyboard.

## Sources and adaptation

`crates/i-slint-backend-android-activity` is the published 1.18.1 archive,
revision `372cf0ee5577c3dfec309a45e7b778ba4e81b734`, plus two patches.
`0001-pointer-scroll.patch` is
[Slint #14495](https://github.com/slint-ui/slint/pull/14495) at
`04fe0cc568e4f1b90fc63f6ee8f66d33423c513d`, adapted to 1.18.1's import list. It
turns the scroll axes into a Slint `PointerScrolled` event at the pointer, 50
logical pixels per wheel step. `0002-deprecated-fetch-update.patch` allows the
deprecation Rust 1.100 reports for `AtomicBool::fetch_update` inside jni's
`bind_java_type!` in `javahelper.rs`. Cargo caps lints for crates from the
registry, so only the vendored copy meets the lint job's `-D warnings`.
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
local crate for Slint 1.18.1.

When a Slint release includes #14495, remove the vendored crate, this
directory, the manifest patch, the root workspace exclusion, the `.gitignore`
and `.gitattributes` entries, the verifier, its CI step, and its paragraph in
`THIRD_PARTY_NOTICES.md`. Then update the Android lockfile.
