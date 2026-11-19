# Flathub packaging

`com.flectar.mail.yml` is the manifest Flathub builds. It matches the bundle
manifest in `packaging/flatpak` with three differences, and
`scripts/test-release.py` fails if anything else drifts between them:

- It builds a release tag from GitHub instead of the local checkout.
- It leaves out the prebuilt PDFium library, because Flathub only accepts
  software built from source. PDF attachments offer to be downloaded instead
  of previewed.
- It leaves out the GitHub update check, because Flathub updates the app.

The `flathub` job in `.github/workflows/native-platforms.yml` builds the
checkout with Flathub's own builder (`flathub-build` from
`org.flatpak.Builder`) and runs `flatpak-builder-lint` on the manifest and on
the built repository, so a pull request finds Flathub's errors before a
release does.

## Updating Flathub after a release

Flathub keeps the manifest in its `flathub/com.flectar.mail` repository. Fill
in the tag and its full commit hash, then copy both files there:

```bash
python3 scripts/flathub-manifest.py \
  --tag v0.1.0 --commit "$(git rev-list -n 1 v0.1.0)" target/flathub/release
```

The output directory holds `com.flectar.mail.yml` and a copy of
`packaging/flatpak/cargo-sources.json`, which every lockfile change refreshes.
