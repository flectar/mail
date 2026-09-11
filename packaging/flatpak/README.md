# Flatpak preview packaging

The manifest builds Flectar Mail from the checked-out source without network
access during compilation. Install the 25.08 Freedesktop SDK and Rust extension,
then run `./scripts/build-flatpak.sh`. The resulting standalone test bundle is
`target/flatpak/flectar-mail.flatpak`.

The Flatpak-specific Cargo feature disables close-to-tray. Slint's current tray
backend owns a legacy StatusNotifier D-Bus name outside the application ID,
which Flathub rejects for new apps. Native packages retain tray support.

`cargo-sources.json` was generated from `Cargo.lock` with the official
[`flatpak-cargo-generator.py`](https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo)
at commit `1fc32195e3e60fe5c97f0af646dec7a99df5962b`. Run the generator again whenever
`Cargo.lock` changes. The release test suite rejects missing, extra, or stale
crate archives and checksums.

This preview manifest uses the local checkout so the release workflow always
packages the commit being tested. A future Flathub repository must replace that
source with the checksum-pinned stable release archive. It must also build
PDFium from source or disable PDF preview because Flathub does not accept this
third-party prebuilt PDFium archive for an app submission.
